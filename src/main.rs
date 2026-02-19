#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod chunked;
mod cli;
mod config;
mod curl_compat;
mod handler;
mod mcp;
mod types;
mod websocket;
mod writer;

use config::VERSION;
use rmcp::ServiceExt;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::AsyncBufReadExt;
use tokio::sync::{mpsc, RwLock};
use tokio_util::sync::CancellationToken;
use types::*;

const OUTPUT_CHANNEL_CAPACITY: usize = 16_384;

fn default_response_save_dir() -> String {
    let dir: PathBuf = std::env::temp_dir()
        .join("afh")
        .join(uuid::Uuid::new_v4().to_string());
    dir.to_string_lossy().into_owned()
}

pub struct App {
    pub config: RwLock<RuntimeConfig>,
    pub client: RwLock<reqwest::Client>,
    pub writer: mpsc::Sender<Output>,
    pub in_flight: RwLock<HashMap<String, CancellationToken>>,
    pub ws_connections: RwLock<HashMap<String, mpsc::UnboundedSender<WsCommand>>>,
    pub request_count: AtomicU64,
    pub start_time: Instant,
}

#[tokio::main]
async fn main() {
    let mode = cli::parse_args();
    match mode {
        cli::Mode::Cli(req) => run_cli(*req).await,
        cli::Mode::Pipe(init_config) => run_pipe(*init_config).await,
        cli::Mode::Mcp => run_mcp().await,
    }
}

// ---------------------------------------------------------------------------
// CLI mode: one request, JSON output, exit
// ---------------------------------------------------------------------------

async fn run_cli(req: cli::CliRequest) {
    let output_format = req.output_format;

    // Auto-generate temp response_save_dir; user --response-save-dir overrides via config_overrides
    let tmp_save_dir = default_response_save_dir();
    if let Err(e) = std::fs::create_dir_all(&tmp_save_dir) {
        eprintln!("fatal: create response_save_dir: {e}");
        std::process::exit(1);
    }

    let log_categories = req.log_categories;
    let emit_startup = log_categories.contains(&"startup".to_string());

    let mut config = RuntimeConfig::new(tmp_save_dir.clone());
    config.apply_update(req.config_overrides);

    // If user provided --response-save-dir, ensure it exists
    if config.response_save_dir != tmp_save_dir {
        if let Err(e) = std::fs::create_dir_all(&config.response_save_dir) {
            eprintln!("fatal: create response_save_dir: {e}");
            std::process::exit(1);
        }
    }

    let client = match config.build_client() {
        Ok(c) => c,
        Err(e) => {
            let err = serde_json::json!({
                "code": "error",
                "error_code": "invalid_request",
                "error": format!("build client: {e}"),
                "retryable": false,
                "trace": {"duration_ms": 0}
            });
            let json = agent_first_data::output_json(&err);
            println!("{json}");
            std::process::exit(1);
        }
    };

    let (writer_tx, mut writer_rx) = mpsc::channel::<Output>(OUTPUT_CHANNEL_CAPACITY);

    // Emit startup only if requested via --log startup or --verbose
    if emit_startup {
        let argv: Vec<String> = std::env::args().collect();
        let _ = writer_tx.try_send(make_log(
            "startup",
            vec![
                ("version", serde_json::Value::String(VERSION.to_string())),
                ("argv", serde_json::to_value(&argv).unwrap_or_default()),
                ("config", serde_json::to_value(&config).unwrap_or_default()),
            ],
        ));
    }

    let app = Arc::new(App {
        config: RwLock::new(config),
        client: RwLock::new(client),
        writer: writer_tx,
        in_flight: RwLock::new(HashMap::new()),
        ws_connections: RwLock::new(HashMap::new()),
        request_count: AtomicU64::new(0),
        start_time: Instant::now(),
    });

    // Spawn the request task — it holds a clone of the Arc
    let app2 = app.clone();
    tokio::spawn(async move {
        handler::execute_request(
            &app2,
            "cli".to_string(),
            None,
            req.method,
            req.url,
            req.headers,
            req.body,
            req.body_base64,
            req.body_file,
            req.body_multipart,
            req.body_urlencoded,
            req.options,
        )
        .await;
    });

    // Drop our Arc so the channel closes when the request task finishes
    drop(app);

    // Read outputs, write to stdout (stripped of id/tag), track exit code
    let mut had_error = false;
    while let Some(output) = writer_rx.recv().await {
        if matches!(&output, Output::Error { .. }) {
            had_error = true;
        }
        cli::write_cli_output(&output, output_format);
    }

    // Clean up empty temp download_dir
    let _ = std::fs::remove_dir(&tmp_save_dir);

    std::process::exit(if had_error { 1 } else { 0 });
}

// ---------------------------------------------------------------------------
// MCP mode: Model Context Protocol server over stdio
// ---------------------------------------------------------------------------

async fn run_mcp() {
    let save_dir = default_response_save_dir();
    if let Err(e) = std::fs::create_dir_all(&save_dir) {
        eprintln!("fatal: create response_save_dir: {e}");
        std::process::exit(1);
    }

    let config = RuntimeConfig::new(save_dir);
    let client = match config.build_client() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("fatal: build client: {e}");
            std::process::exit(1);
        }
    };

    // The shared app holds config/client state for the MCP session.
    // Each http_request tool call creates its own per-call App clone with a
    // dedicated writer channel, sharing the config/client snapshot at call time.
    let (writer_tx, _writer_rx) = mpsc::channel::<Output>(OUTPUT_CHANNEL_CAPACITY);
    let app = Arc::new(App {
        config: RwLock::new(config),
        client: RwLock::new(client),
        writer: writer_tx,
        in_flight: RwLock::new(HashMap::new()),
        ws_connections: RwLock::new(HashMap::new()),
        request_count: AtomicU64::new(0),
        start_time: Instant::now(),
    });

    let service = match mcp::AfhMcp::new(app).serve(rmcp::transport::stdio()).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("fatal: MCP serve failed: {e}");
            std::process::exit(1);
        }
    };

    if let Err(e) = service.waiting().await {
        eprintln!("MCP server error: {e}");
        std::process::exit(1);
    }
}

// ---------------------------------------------------------------------------
// Pipe mode: JSONL stdin/stdout (existing behavior)
// ---------------------------------------------------------------------------

async fn run_pipe(init_config: ConfigPatch) {
    let save_dir = default_response_save_dir();
    if let Err(e) = std::fs::create_dir_all(&save_dir) {
        eprintln!("fatal: create response_save_dir: {e}");
        std::process::exit(1);
    }

    let mut config = RuntimeConfig::new(save_dir);
    config.apply_update(init_config);
    let client = match config.build_client() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("fatal: build client: {e}");
            std::process::exit(1);
        }
    };

    let (writer_tx, writer_rx) = mpsc::channel::<Output>(OUTPUT_CHANNEL_CAPACITY);

    // Spawn writer task
    tokio::spawn(writer::writer_task(writer_rx));

    // Send startup log if enabled (default: off, agent must configure log:["startup"])
    if config.log.contains(&"startup".to_string()) {
        let argv: Vec<String> = std::env::args().collect();
        let _ = writer_tx.try_send(make_log(
            "startup",
            vec![
                ("version", serde_json::Value::String(VERSION.to_string())),
                ("argv", serde_json::to_value(&argv).unwrap_or_default()),
                ("config", serde_json::to_value(&config).unwrap_or_default()),
            ],
        ));
    }

    let app = Arc::new(App {
        config: RwLock::new(config),
        client: RwLock::new(client),
        writer: writer_tx,
        in_flight: RwLock::new(HashMap::new()),
        ws_connections: RwLock::new(HashMap::new()),
        request_count: AtomicU64::new(0),
        start_time: Instant::now(),
    });

    // Track spawned request tasks for graceful shutdown
    let mut handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();

    // Read JSONL from stdin
    let stdin = tokio::io::stdin();
    let reader = tokio::io::BufReader::new(stdin);
    let mut lines = reader.lines();

    loop {
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => break, // EOF
            Err(_) => break,
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let input: Input = match serde_json::from_str(trimmed) {
            Ok(i) => i,
            Err(e) => {
                let _ = app
                    .writer
                    .send(make_error(
                        None,
                        None,
                        ErrorInfo::invalid_request(format!("parse error: {e}")),
                        Trace::error_only(0),
                    ))
                    .await;
                continue;
            }
        };

        match input {
            Input::Request {
                id,
                tag,
                method,
                url,
                headers,
                body,
                body_base64,
                body_file,
                body_multipart,
                body_urlencoded,
                options,
            } => {
                let app = app.clone();
                let handle = tokio::spawn(async move {
                    handler::execute_request(
                        &app,
                        id,
                        tag,
                        method,
                        url,
                        headers,
                        body,
                        body_base64,
                        body_file,
                        body_multipart,
                        body_urlencoded,
                        options,
                    )
                    .await;
                });
                handles.push(handle);
                // Drop finished task handles so long-lived sessions don't grow unbounded.
                handles.retain(|h| !h.is_finished());
            }
            Input::Config(patch) => {
                handle_config(&app, patch).await;
            }
            Input::Send {
                id,
                data,
                data_base64,
            } => {
                handle_send(&app, &id, data, data_base64).await;
            }
            Input::Cancel { id } => {
                handle_cancel(&app, &id).await;
            }
            Input::Ping => {
                handle_ping(&app).await;
            }
            Input::Close => {
                break;
            }
        }
    }

    // Graceful shutdown

    // Close all WebSocket connections
    {
        let ws_conns = app.ws_connections.read().await;
        for tx in ws_conns.values() {
            let _ = tx.send(WsCommand::Close);
        }
    }

    // Wait for all in-flight tasks (up to 5 seconds)
    let shutdown_deadline = tokio::time::sleep(std::time::Duration::from_secs(5));
    tokio::pin!(shutdown_deadline);
    tokio::select! {
        _ = futures::future::join_all(&mut handles) => {}
        _ = &mut shutdown_deadline => {}
    }

    let uptime_s = app.start_time.elapsed().as_secs();
    let requests_total = app.request_count.load(std::sync::atomic::Ordering::Relaxed);

    let _ = app
        .writer
        .send(Output::Close {
            message: "shutdown".to_string(),
            trace: CloseTrace {
                uptime_s,
                requests_total,
            },
        })
        .await;

    // Give writer a moment to flush
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
}

// ---------------------------------------------------------------------------
// Pipe mode command handlers
// ---------------------------------------------------------------------------

async fn handle_config(app: &Arc<App>, patch: ConfigPatch) {
    // Validate response_save_dir if being changed
    if let Some(ref dir) = patch.response_save_dir {
        if let Err(e) = std::fs::create_dir_all(dir) {
            let _ = app
                .writer
                .send(make_error(
                    None,
                    None,
                    ErrorInfo::invalid_request(format!("response_save_dir: {e}")),
                    Trace::error_only(0),
                ))
                .await;
            return;
        }
    }

    let needs_rebuild = {
        let mut config = app.config.write().await;
        config.apply_update(patch)
    };

    if needs_rebuild {
        let config = app.config.read().await;
        match config.build_client() {
            Ok(new_client) => {
                drop(config);
                let mut client = app.client.write().await;
                *client = new_client;
            }
            Err(e) => {
                let _ = app
                    .writer
                    .send(make_error(
                        None,
                        None,
                        ErrorInfo::invalid_request(format!("rebuild client: {e}")),
                        Trace::error_only(0),
                    ))
                    .await;
                return;
            }
        }
    }

    // Echo full config
    let config = app.config.read().await;
    let _ = app.writer.send(Output::Config(config.clone())).await;
}

async fn handle_ping(app: &App) {
    let uptime_s = app.start_time.elapsed().as_secs();
    let requests_total = app.request_count.load(std::sync::atomic::Ordering::Relaxed);
    let connections_active = app
        .in_flight
        .try_read()
        .map(|m| m.len() as u64)
        .unwrap_or(0);
    let _ = app
        .writer
        .send(Output::Pong {
            trace: PongTrace {
                uptime_s,
                requests_total,
                connections_active,
            },
        })
        .await;
}

async fn handle_send(
    app: &App,
    id: &str,
    data: Option<serde_json::Value>,
    data_base64: Option<String>,
) {
    let ws_conns = app.ws_connections.read().await;
    if let Some(tx) = ws_conns.get(id) {
        let _ = tx.send(WsCommand::Send { data, data_base64 });
    } else {
        let _ = app
            .writer
            .send(make_error(
                Some(id.to_string()),
                None,
                ErrorInfo::invalid_request("no active websocket connection with this id"),
                Trace::error_only(0),
            ))
            .await;
    }
}

async fn handle_cancel(app: &App, id: &str) {
    // Try HTTP in-flight first
    {
        let in_flight = app.in_flight.read().await;
        if let Some(token) = in_flight.get(id) {
            token.cancel();
            return;
        }
    }
    // Try WebSocket
    {
        let ws_conns = app.ws_connections.read().await;
        if let Some(tx) = ws_conns.get(id) {
            let _ = tx.send(WsCommand::Close);
            return;
        }
    }
    let _ = app
        .writer
        .send(make_error(
            Some(id.to_string()),
            None,
            ErrorInfo::invalid_request("no active request or websocket connection with this id"),
            Trace::error_only(0),
        ))
        .await;
}
