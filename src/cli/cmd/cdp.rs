//! `afhttp cdp` subcommand. Raw CDP method invocation.

use serde::Serialize;

use crate::cli::connect::Connection;
use crate::cli::output;
use crate::shared::error::{Error, ErrorCode};
use crate::shared::ids::TabId;
use crate::shared::time::parse_duration;

#[derive(Serialize)]
struct CdpResult {
    result: serde_json::Value,
}

#[derive(Debug)]
pub struct Args {
    pub method: String,
    pub connection: Connection,
    pub tab: String,
    pub params: Option<String>,
    pub wait: Option<String>,
}

pub async fn run(args: Args) -> Result<(), Error> {
    let client = args.connection.client().await?;
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
