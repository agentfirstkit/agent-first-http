//! `afhttp cdp` subcommand. Raw CDP method invocation.

use serde::Serialize;

use crate::cli::connect::Connection;
use crate::cli::output;
use crate::shared::error::{Error, ErrorCode};
use crate::shared::ids::TabId;
use crate::shared::time::parse_duration;

/// Nothing but the capture, which is what lets it go out unredacted.
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
    // The browser's answer, unaltered: an evaluated `{"api_secret": ...}` or
    // `{"next_url": ...}` is the page's data, not a credential afhttp holds.
    output::emit_raw_capture("cdp", &CdpResult { result: value })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `run` writes, end to end through the emitter: the AFDATA `_secret`
    /// redactor and the `_url` walk both leave a raw result alone.
    #[test]
    fn a_raw_cdp_result_is_emitted_as_the_browser_gave_it() {
        let result = serde_json::json!({"result": {"value": {
            "api_secret": "page-own-secret",
            "next_url": "https://a.test/cb?token=page-own",
            "password": "hunter2",
        }}});
        let mut out = Vec::new();
        crate::shared::afdata::emit_unredacted(
            &mut out,
            "cdp",
            &CdpResult {
                result: result.clone(),
            },
        )
        .unwrap();
        let event: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(event["result"]["result"], result, "{event}");
        assert_eq!(event["result"]["code"], "cdp");

        // The default emitter would have altered it; that is what the raw
        // route avoids.
        let mut redacted = Vec::new();
        crate::shared::afdata::emit(&mut redacted, "cdp", &CdpResult { result }).unwrap();
        let redacted: serde_json::Value = serde_json::from_slice(&redacted).unwrap();
        assert_ne!(
            redacted["result"]["result"]["result"]["value"]["api_secret"],
            "page-own-secret"
        );
    }
}
