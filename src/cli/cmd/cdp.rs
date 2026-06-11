//! `afhttp cdp` subcommand. Raw CDP method invocation.

use clap::Args as ClapArgs;
use serde::Serialize;

use crate::cli::output;
use crate::sdk::Client;
use crate::shared::error::{Error, ErrorCode};
use crate::shared::ids::TabId;
use crate::shared::time::parse_duration;

#[derive(Serialize)]
struct CdpResult {
    result: serde_json::Value,
}

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// CDP method name (e.g. Runtime.evaluate).
    pub method: String,
    /// CDP endpoint of the running host (e.g. `ws://127.0.0.1:9222`). Falls back to `AFHTTP_ENDPOINT_URL`.
    #[arg(long = "endpoint-url", env = "AFHTTP_ENDPOINT_URL")]
    pub endpoint: String,
    /// Bearer token, if the host was started with `--token-secret`.
    /// Falls back to `AFHTTP_TOKEN_SECRET`.
    #[arg(long = "token-secret", env = "AFHTTP_TOKEN_SECRET")]
    pub token: Option<String>,
    /// CDP target id (tab) to drive.
    #[arg(long)]
    pub tab: String,
    /// JSON literal, or `@-` to read from stdin.
    #[arg(long, value_name = "JSON|@-")]
    pub params: Option<String>,
    /// "<event>:<timeout>" — wait for a CDP event before exiting.
    #[arg(long = "wait-event")]
    pub wait: Option<String>,
}

pub async fn run(args: Args) -> Result<(), Error> {
    let mut client = Client::connect(&args.endpoint)?;
    if let Some(t) = args.token.as_deref() {
        client = client.with_token(t);
    }
    let params = if let Some(raw) = args.params {
        if raw == "@-" {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .map_err(|e| Error::new(ErrorCode::IoError, format!("read stdin: {e}")))?;
            serde_json::from_str(&buf).map_err(|e| {
                Error::new(
                    ErrorCode::InvalidArgument,
                    format!("--params @-: invalid JSON: {e}"),
                )
            })?
        } else {
            serde_json::from_str(&raw).map_err(|e| {
                Error::new(
                    ErrorCode::InvalidArgument,
                    format!("--params: invalid JSON: {e}"),
                )
            })?
        }
    } else {
        serde_json::Value::Object(Default::default())
    };
    let mut req = client
        .cdp(args.method)
        .tab(TabId::new(args.tab))
        .params(params);
    if let Some(spec) = args.wait {
        let (ev, timeout) = spec.rsplit_once(':').ok_or_else(|| {
            Error::new(
                ErrorCode::InvalidArgument,
                "--wait-event: expected <event>:<timeout>",
            )
        })?;
        let d = parse_duration(timeout)?;
        req = req.wait_for(ev, d);
    }
    let value = req.send().await?;
    output::emit("cdp", &CdpResult { result: value })
}
