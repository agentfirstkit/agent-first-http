//! `afhttp tabs` — list and close CDP targets.

use serde_json::Value;

use crate::cli::output;
use crate::sdk::Client;
use crate::shared::error::{Error, ErrorCode};

#[derive(Debug)]
pub struct Args {
    pub sub: TabsSub,
}

#[derive(Debug)]
pub enum TabsSub {
    List(EndpointArgs),
    Close(CloseArgs),
}

#[derive(Debug)]
pub struct EndpointArgs {
    pub endpoint: String,
    pub token: Option<String>,
}

#[derive(Debug)]
pub struct CloseArgs {
    pub tab: String,
    pub endpoint: String,
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
    if args.tab.trim().is_empty() {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            "tabs close: target id must not be empty",
        ));
    }
    let client = build_client(&args.endpoint, args.token)?;
    let response = client
        .cdp("Target.closeTarget")
        .params(serde_json::json!({ "targetId": args.tab }))
        .send()
        .await?;
    let success = response
        .get("success")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let payload = serde_json::json!({
        "code": "tab_closed",
        "target_id": args.tab,
        "success": success,
    });
    output::emit("tab_closed", &payload)
}
