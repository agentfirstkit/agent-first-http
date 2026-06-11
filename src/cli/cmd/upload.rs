//! `afhttp upload` subcommand — inject a local file into an `<input type=file>`
//! using the privileged CDP `DOM.setFileInputFiles` primitive.

use std::path::PathBuf;

use clap::Args as ClapArgs;
use serde::Serialize;

use crate::cli::output;
use crate::sdk::Client;
use crate::shared::error::{Error, ErrorCode};
use crate::shared::ids::TabId;

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// CDP endpoint of the running host (e.g. `ws://127.0.0.1:9222`). Falls back to `AFHTTP_ENDPOINT_URL`.
    #[arg(long = "endpoint-url", env = "AFHTTP_ENDPOINT_URL")]
    pub endpoint: String,
    /// Bearer token, if the host was started with `--token-secret`.
    /// Falls back to `AFHTTP_TOKEN_SECRET`.
    #[arg(long = "token-secret", env = "AFHTTP_TOKEN_SECRET")]
    pub token: Option<String>,
    /// CDP target id (tab) to operate in.
    #[arg(long)]
    pub tab: String,
    /// CSS selector for the `<input type=file>` element.
    #[arg(long)]
    pub selector: String,
    /// Local file path to upload.
    #[arg(long)]
    pub file: PathBuf,
}

#[derive(Serialize)]
struct UploadResult {
    tab_id: String,
    selector: String,
    path: String,
    size_bytes: u64,
}

pub async fn run(args: Args) -> Result<(), Error> {
    let file_meta = tokio::fs::metadata(&args.file).await.map_err(|e| {
        Error::new(
            ErrorCode::IoError,
            format!("upload: stat {}: {e}", args.file.display()),
        )
    })?;
    let bytes = file_meta.len();

    let abs_path = tokio::fs::canonicalize(&args.file).await.map_err(|e| {
        Error::new(
            ErrorCode::IoError,
            format!("upload: canonicalize {}: {e}", args.file.display()),
        )
    })?;
    let path_str = abs_path.to_string_lossy().to_string();

    let mut client = Client::connect(&args.endpoint)?;
    if let Some(t) = args.token.as_deref() {
        client = client.with_token(t);
    }

    let conn = client.cdp_connection().await?;
    let tab = TabId::new(args.tab.clone());
    let session_id = crate::sdk::cdp::session::attach_to_target(&conn, tab.as_str()).await?;

    conn.send("DOM.enable", &serde_json::json!({}), Some(&session_id))
        .await?;
    let document = conn
        .send(
            "DOM.getDocument",
            &serde_json::json!({"depth": 1, "pierce": true}),
            Some(&session_id),
        )
        .await?;
    let root_id = document["root"]["nodeId"].as_i64().ok_or_else(|| {
        Error::new(
            ErrorCode::CdpError,
            "upload: DOM.getDocument missing root nodeId",
        )
    })?;
    let query = conn
        .send(
            "DOM.querySelector",
            &serde_json::json!({"nodeId": root_id, "selector": args.selector}),
            Some(&session_id),
        )
        .await?;
    let node_id = query["nodeId"]
        .as_i64()
        .filter(|id| *id > 0)
        .ok_or_else(|| {
            Error::new(
                ErrorCode::InvalidArgument,
                format!(
                    "upload: selector {:?} did not match an element",
                    args.selector
                ),
            )
        })?;
    ensure_file_input(&conn, &session_id, node_id, &args.selector).await?;
    conn.send(
        "DOM.setFileInputFiles",
        &serde_json::json!({
            "files": [path_str],
            "nodeId": node_id,
        }),
        Some(&session_id),
    )
    .await?;

    let _ = crate::sdk::cdp::session::detach_from_target(&conn, &session_id).await;

    output::emit(
        "upload",
        &UploadResult {
            tab_id: args.tab,
            selector: args.selector,
            path: path_str,
            size_bytes: bytes,
        },
    )
}

async fn ensure_file_input(
    conn: &crate::sdk::cdp::ws_client::Connection,
    session_id: &str,
    node_id: i64,
    selector: &str,
) -> Result<(), Error> {
    let described = conn
        .send(
            "DOM.describeNode",
            &serde_json::json!({"nodeId": node_id}),
            Some(session_id),
        )
        .await?;
    let node = &described["node"];
    let is_input = node["nodeName"]
        .as_str()
        .is_some_and(|name| name.eq_ignore_ascii_case("input"));
    let attrs = node["attributes"].as_array().cloned().unwrap_or_default();
    let mut is_file = false;
    for pair in attrs.chunks(2) {
        if pair.first().and_then(|v| v.as_str()) == Some("type")
            && pair
                .get(1)
                .and_then(|v| v.as_str())
                .is_some_and(|value| value.eq_ignore_ascii_case("file"))
        {
            is_file = true;
        }
    }
    if is_input && is_file {
        Ok(())
    } else {
        Err(Error::new(
            ErrorCode::InvalidArgument,
            format!("upload: selector {selector:?} must resolve to <input type=file>"),
        ))
    }
}
