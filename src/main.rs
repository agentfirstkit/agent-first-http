#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::print_stderr,
        clippy::disallowed_methods,
        clippy::disallowed_macros
    )
)]

mod chunked;
mod cli;
mod config;
mod curl_compat;
mod handler;
mod types;
mod websocket;
mod writer;

use agent_first_data::{cli_output, OutputFormat};
use config::VERSION;
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
        .join("afhttp")
        .join(uuid::Uuid::new_v4().to_string());
    dir.to_string_lossy().into_owned()
}

fn emit_startup_error_and_exit(message: impl AsRef<str>, hint: Option<&str>) -> ! {
    let mut obj = serde_json::Map::new();
    obj.insert("code".into(), serde_json::json!("error"));
    obj.insert("error_code".into(), serde_json::json!("internal_error"));
    obj.insert("error".into(), serde_json::json!(message.as_ref()));
    if let Some(h) = hint {
        obj.insert("hint".into(), serde_json::json!(h));
    }
    obj.insert("retryable".into(), serde_json::json!(false));
    obj.insert("trace".into(), serde_json::json!({"duration_ms": 0}));
    println!(
        "{}",
        cli_output(&serde_json::Value::Object(obj), OutputFormat::Json)
    );
    std::process::exit(1);
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
        cli::Mode::Pipe(init) => run_pipe(*init).await,
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
        emit_startup_error_and_exit(format!("create response_save_dir: {e}"), None);
    }

    let log_categories = req.log_categories;
    let emit_startup = log_categories.contains(&"startup".to_string());

    let mut config = RuntimeConfig::new(tmp_save_dir.clone());
    config.apply_update(req.config_overrides);

    // If user provided --response-save-dir, ensure it exists
    if config.response_save_dir != tmp_save_dir {
        if let Err(e) = std::fs::create_dir_all(&config.response_save_dir) {
            emit_startup_error_and_exit(format!("create response_save_dir: {e}"), None);
        }
    }

    let client = match config.build_client() {
        Ok(c) => c,
        Err(e) => {
            emit_startup_error_and_exit(format!("build client: {e}"), None);
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

    // Dry-run: emit the request details without executing
    if req.dry_run {
        let dry = Output::DryRun {
            method: req.method,
            url: req.url,
            headers: req.headers,
            body: req.body,
            trace: Trace::error_only(0),
        };
        cli::write_cli_output(&dry, output_format);
        return;
    }

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
// Pipe mode: structured stdin/stdout
// ---------------------------------------------------------------------------

async fn run_pipe(init: cli::PipeInit) {
    let cli::PipeInit {
        config: init_config,
        output_format,
    } = init;
    let save_dir = default_response_save_dir();
    if let Err(e) = std::fs::create_dir_all(&save_dir) {
        emit_startup_error_and_exit(format!("create response_save_dir: {e}"), None);
    }

    let mut config = RuntimeConfig::new(save_dir);
    config.apply_update(init_config);
    let client = match config.build_client() {
        Ok(c) => c,
        Err(e) => {
            emit_startup_error_and_exit(format!("build client: {e}"), None);
        }
    };

    let (writer_tx, writer_rx) = mpsc::channel::<Output>(OUTPUT_CHANNEL_CAPACITY);

    // Spawn writer task
    tokio::spawn(writer::writer_task(writer_rx, output_format));

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

    // Cancel all active requests first so each in-flight id can converge toward
    // a terminal event before we emit process close.
    {
        let in_flight = app.in_flight.read().await;
        for token in in_flight.values() {
            token.cancel();
        }
    }

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
    let mut shutdown_timed_out = false;
    tokio::select! {
        _ = futures::future::join_all(&mut handles) => {}
        _ = &mut shutdown_deadline => {
            shutdown_timed_out = true;
        }
    }

    // Forced shutdown fallback: abort unfinished tasks and emit terminal cancelled
    // events for any ids still tracked as in-flight.
    if shutdown_timed_out {
        for handle in &handles {
            if !handle.is_finished() {
                handle.abort();
            }
        }
        let remaining_ids: Vec<String> = {
            let mut in_flight = app.in_flight.write().await;
            let ids = in_flight.keys().cloned().collect::<Vec<_>>();
            in_flight.clear();
            ids
        };
        for id in remaining_ids {
            let _ = app
                .writer
                .send(make_error(
                    Some(id),
                    None,
                    ErrorInfo::cancelled(),
                    Trace::error_only(0),
                ))
                .await;
        }
        app.ws_connections.write().await.clear();
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

    let (needs_rebuild, previous_config) = {
        let mut config = app.config.write().await;
        let previous = config.clone();
        let needs = config.apply_update(patch);
        (needs, previous)
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
                drop(config);
                let mut config = app.config.write().await;
                *config = previous_config;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::RuntimeConfig;

    async fn test_app() -> (Arc<App>, mpsc::Receiver<Output>) {
        let save_dir = std::env::temp_dir()
            .join(format!("afhttp-main-test-{}", std::process::id()))
            .to_string_lossy()
            .into_owned();
        let config = RuntimeConfig::new(save_dir);
        let client = config.build_client().expect("build client");
        let (tx, rx) = mpsc::channel(32);
        let app = Arc::new(App {
            config: RwLock::new(config),
            client: RwLock::new(client),
            writer: tx,
            in_flight: RwLock::new(HashMap::new()),
            ws_connections: RwLock::new(HashMap::new()),
            request_count: AtomicU64::new(0),
            start_time: Instant::now(),
        });
        (app, rx)
    }

    #[test]
    fn default_response_save_dir_is_temp_afh_path() {
        let path = default_response_save_dir();
        assert!(path.contains("/afhttp/"));
        assert!(path.starts_with(std::env::temp_dir().to_string_lossy().as_ref()));
    }

    #[tokio::test]
    async fn handle_ping_emits_pong() {
        let (app, mut rx) = test_app().await;
        handle_ping(&app).await;
        let out = rx.recv().await.expect("output");
        assert!(matches!(out, Output::Pong { .. }));
    }

    #[tokio::test]
    async fn handle_send_unknown_id_emits_error() {
        let (app, mut rx) = test_app().await;
        handle_send(&app, "missing", None, None).await;
        let out = rx.recv().await.expect("output");
        assert!(matches!(out, Output::Error { .. }));
    }

    #[tokio::test]
    async fn handle_send_known_ws_id_forwards_command() {
        let (app, _rx) = test_app().await;
        let (tx, mut ws_rx) = mpsc::unbounded_channel();
        app.ws_connections
            .write()
            .await
            .insert("ws1".to_string(), tx);
        handle_send(
            &app,
            "ws1",
            Some(serde_json::json!({"a":1})),
            Some("aA==".to_string()),
        )
        .await;
        let cmd = ws_rx.recv().await.expect("ws cmd");
        assert!(matches!(cmd, WsCommand::Send { .. }));
    }

    #[tokio::test]
    async fn handle_cancel_cancels_http_and_ws() {
        let (app, mut rx) = test_app().await;
        let token = CancellationToken::new();
        app.in_flight
            .write()
            .await
            .insert("r1".to_string(), token.clone());
        handle_cancel(&app, "r1").await;
        assert!(token.is_cancelled());

        let (tx, mut ws_rx) = mpsc::unbounded_channel();
        app.ws_connections
            .write()
            .await
            .insert("ws1".to_string(), tx);
        handle_cancel(&app, "ws1").await;
        let cmd = ws_rx.recv().await.expect("ws close");
        assert!(matches!(cmd, WsCommand::Close));

        handle_cancel(&app, "missing").await;
        let out = rx.recv().await.expect("error output");
        assert!(matches!(out, Output::Error { .. }));
    }

    #[tokio::test]
    async fn handle_config_updates_and_echoes_config() {
        let (app, mut rx) = test_app().await;
        handle_config(
            &app,
            ConfigPatch {
                timeout_connect_s: Some(12),
                request_concurrency_limit: Some(7),
                ..ConfigPatch::default()
            },
        )
        .await;
        let out = rx.recv().await.expect("config output");
        match out {
            Output::Config(cfg) => {
                assert_eq!(cfg.timeout_connect_s, 12);
                assert_eq!(cfg.request_concurrency_limit, 7);
            }
            _ => panic!("expected Output::Config"),
        }
    }

    #[tokio::test]
    async fn handle_config_rebuild_failure_rolls_back_config() {
        let (app, mut rx) = test_app().await;
        let before = app.config.read().await.clone();
        handle_config(
            &app,
            ConfigPatch {
                proxy: Some("not a valid proxy".to_string()),
                ..ConfigPatch::default()
            },
        )
        .await;
        let out = rx.recv().await.expect("error output");
        assert!(matches!(out, Output::Error { .. }));
        let after = app.config.read().await.clone();
        assert_eq!(after.proxy, before.proxy);
        assert_eq!(after.timeout_connect_s, before.timeout_connect_s);
    }

    #[tokio::test]
    async fn handle_config_invalid_response_save_dir_emits_error_and_keeps_config() {
        let (app, mut rx) = test_app().await;
        let before = app.config.read().await.clone();

        let bad_path = std::env::temp_dir().join(format!(
            "afhttp-config-file-{}-{}.tmp",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        std::fs::write(&bad_path, b"x").expect("seed file");
        let bad_path_str = bad_path.to_string_lossy().into_owned();

        handle_config(
            &app,
            ConfigPatch {
                response_save_dir: Some(bad_path_str),
                timeout_connect_s: Some(99),
                ..ConfigPatch::default()
            },
        )
        .await;

        let out = rx.recv().await.expect("error output");
        assert!(matches!(out, Output::Error { .. }));
        let after = app.config.read().await.clone();
        assert_eq!(after.response_save_dir, before.response_save_dir);
        assert_eq!(after.timeout_connect_s, before.timeout_connect_s);

        let _ = std::fs::remove_file(bad_path);
    }
}
