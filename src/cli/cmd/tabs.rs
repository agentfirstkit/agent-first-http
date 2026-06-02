//! `afhttp tabs` — list and close CDP targets.

use clap::{Args as ClapArgs, Subcommand};
use serde_json::Value;

use crate::cli::output;
use crate::sdk::Client;
use crate::shared::error::{Error, ErrorCode};

#[derive(ClapArgs, Debug)]
pub struct Args {
    #[command(subcommand)]
    pub sub: TabsSub,
}

#[derive(Subcommand, Debug)]
pub enum TabsSub {
    /// List currently-attached CDP targets.
    List(EndpointArgs),
    /// Close a target by its CDP target id.
    Close(CloseArgs),
}

#[derive(ClapArgs, Debug)]
pub struct EndpointArgs {
    /// CDP endpoint URL (e.g. `ws://127.0.0.1:9222`).
    #[arg(long = "endpoint-url")]
    pub endpoint: String,
    /// Bearer token, if the host was started with `--token-secret`.
    #[arg(long = "token-secret")]
    pub token: Option<String>,
}

#[derive(ClapArgs, Debug)]
pub struct CloseArgs {
    /// CDP target id to close (e.g. `41A0F1E0FD…`).
    pub id: String,
    /// CDP endpoint URL (e.g. `ws://127.0.0.1:9222`).
    #[arg(long = "endpoint-url")]
    pub endpoint: String,
    /// Bearer token, if the host was started with `--token-secret`.
    #[arg(long = "token-secret")]
    pub token: Option<String>,
}

pub async fn run(args: Args) -> Result<(), Error> {
    match args.sub {
        TabsSub::List(a) => list(a).await,
        TabsSub::Close(a) => close(a).await,
    }
}

fn build_client(endpoint: &str, token: Option<String>) -> Result<Client, Error> {
    let mut client = Client::connect(endpoint)?;
    if let Some(t) = token {
        client = client.with_token(t);
    }
    Ok(client)
}

async fn list(args: EndpointArgs) -> Result<(), Error> {
    let client = build_client(&args.endpoint, args.token)?;
    let response = client.cdp("Target.getTargets").send().await?;
    // `Client.cdp(...).send()` unwraps the JSON-RPC `result` envelope, so
    // `response` is the inner method result and we read `targetInfos`
    // off it directly.
    let targets = response
        .get("targetInfos")
        .cloned()
        .unwrap_or(Value::Array(Vec::new()));
    let payload = serde_json::json!({
        "code": "tabs",
        "targets": targets,
    });
    output::emit("tabs", &payload)
}

async fn close(args: CloseArgs) -> Result<(), Error> {
    if args.id.trim().is_empty() {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            "tabs close: target id must not be empty",
        ));
    }
    let client = build_client(&args.endpoint, args.token)?;
    let response = client
        .cdp("Target.closeTarget")
        .params(serde_json::json!({ "targetId": args.id }))
        .send()
        .await?;
    let success = response
        .get("success")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let payload = serde_json::json!({
        "code": "tab_closed",
        "target_id": args.id,
        "success": success,
    });
    output::emit("tab_closed", &payload)
}
