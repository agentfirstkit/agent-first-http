//! `afhttp panel` subcommand. Prints a short-lived takeover URL for an endpoint.

use clap::Args as ClapArgs;
use serde::Serialize;

use crate::cli::output;
use crate::sdk::Client;
use crate::shared::error::Error;

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// CDP endpoint of the running host (e.g. ws://127.0.0.1:9222). Falls back
    /// to `AFHTTP_ENDPOINT_URL`.
    #[arg(long = "endpoint-url", env = "AFHTTP_ENDPOINT_URL")]
    pub endpoint: String,
    /// Bearer token, if the host requires one. Falls back to
    /// `AFHTTP_TOKEN_SECRET`.
    #[arg(long = "token-secret", env = "AFHTTP_TOKEN_SECRET")]
    pub token: Option<String>,
}

#[derive(Serialize)]
struct PanelResult {
    takeover_url: String,
    takeover_url_expires_at_rfc3339: String,
    takeover_url_ttl_s: u64,
    takeover_url_scope: String,
    provider: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

pub async fn run(args: Args) -> Result<(), Error> {
    let result = build_result(&args.endpoint, args.token.as_deref()).await?;
    output::emit("panel", &result)
}

async fn build_result(endpoint: &str, token: Option<&str>) -> Result<PanelResult, Error> {
    let mut client = Client::connect(endpoint)?;
    if let Some(token) = token {
        client = client.with_token(token);
    }
    let handoff = client.takeover_handoff(None, None).await?;
    let mut warnings = Vec::new();
    let mut provider = None;
    match client.capabilities().await {
        Ok(caps) => {
            if caps.takeover.supported {
                provider = caps.takeover.provider;
            } else {
                warnings.push(
                    "this host has no takeover panel; build a takeover-ready host with `afhttp container install` and reconnect. This is not a captcha bypass.".into(),
                );
            }
        }
        Err(e) => {
            warnings.push(format!(
                "could not read host capabilities; returning the takeover_url anyway: {}",
                e.detail
            ));
        }
    }
    Ok(PanelResult {
        takeover_url: handoff.takeover_url,
        takeover_url_expires_at_rfc3339: handoff.takeover_url_expires_at_rfc3339,
        takeover_url_ttl_s: handoff.takeover_url_ttl_s,
        takeover_url_scope: handoff.takeover_url_scope,
        provider,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_result_exposes_takeover_url_only() {
        let value = serde_json::to_value(PanelResult {
            takeover_url: "http://localhost:9222/takeover/panel?handoff=h".into(),
            takeover_url_expires_at_rfc3339: "2026-06-11T00:00:00Z".into(),
            takeover_url_ttl_s: 900,
            takeover_url_scope: "takeover".into(),
            provider: Some("kasmvnc".into()),
            warnings: Vec::new(),
        })
        .unwrap();
        assert!(value.get("url").is_none());
        assert!(value.get("display_url").is_none());
        assert!(value.get("panel_url").is_none());
        assert!(value.get("token_secret").is_none());
        assert!(value["takeover_url"].as_str().unwrap().contains("handoff="));
        assert!(value.get("provider").is_some());
    }
}
