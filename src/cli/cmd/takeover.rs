//! `afhttp takeover` workflows. These commands prepare a browser tab and print
//! the URL a human should open for manual login/captcha/2FA handling.

use clap::{Args as ClapArgs, Subcommand};
use serde::Serialize;

use crate::cli::output;
use crate::sdk::endpoint::Endpoint;
use crate::sdk::Client;
use crate::shared::error::{Error, ErrorCode};
use crate::shared::ids::TabId;

#[derive(ClapArgs, Debug)]
pub struct Args {
    #[command(subcommand)]
    pub sub: TakeoverSub,
}

#[derive(Subcommand, Debug)]
pub enum TakeoverSub {
    /// Open a persistent tab, navigate it, and print the takeover URL.
    Prepare(PrepareArgs),
}

#[derive(ClapArgs, Debug)]
pub struct PrepareArgs {
    /// URL to open in the takeover tab.
    pub url: String,
    /// CDP endpoint of the running host. Defaults to the container host port.
    #[arg(long = "endpoint-url", default_value = "ws://127.0.0.1:9222")]
    pub endpoint: String,
    /// Bearer token, if the host was started with `--token-secret`.
    #[arg(long = "token-secret")]
    pub token: Option<String>,
    /// Prefer the real display takeover URL and warn when the host lacks it.
    #[arg(long = "hard-site")]
    pub hard_site: bool,
}

#[derive(Debug, Serialize)]
struct PrepareResult {
    url: String,
    endpoint: String,
    tab_id: String,
    screencast_url: String,
    display_url: String,
    recommended_url: String,
    recommended_url_kind: String,
    display_provider: Option<String>,
    hard_site: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

#[derive(Debug, Clone)]
struct TakeoverUrls {
    screencast_url: String,
    display_url: String,
}

pub async fn run(args: Args) -> Result<(), Error> {
    match args.sub {
        TakeoverSub::Prepare(a) => prepare(a).await,
    }
}

async fn prepare(args: PrepareArgs) -> Result<(), Error> {
    let mut client = Client::connect(&args.endpoint)?;
    if let Some(token) = args.token.as_deref() {
        client = client.with_token(token);
    }
    let urls = build_urls(&args.endpoint, args.token.as_deref())?;
    let mut warnings = Vec::new();

    let health = client.health().await.map_err(|e| {
        Error::new(
            e.error_code,
            format!(
                "takeover prepare could not reach host at {}; run `afhttp container status` or `afhttp container install` for a local host; for hard sites use `afhttp container install --from-source --with kasmvnc -- --takeover display --display-provider kasmvnc`: {}",
                args.endpoint, e.detail
            ),
        )
        .with_retryable(e.retryable)
    })?;
    if health.status != "ok" {
        if let Some(backend_error) = health.backend_error {
            return Err(Error::new(
                backend_error.error_code,
                format!(
                    "takeover prepare requires a ready browser backend; /health status={} backend_error={}",
                    health.status, backend_error.error
                ),
            ));
        }
        warnings.push(format!(
            "/health status was {}; attempting to prepare a tab anyway",
            health.status
        ));
    }

    let mut recommended_url = urls.screencast_url.clone();
    let mut recommended_url_kind = "screencast_url".to_string();
    let mut display_provider = None;
    match client.capabilities().await {
        Ok(caps) => {
            if caps.ops_panel.display {
                recommended_url = urls.display_url.clone();
                recommended_url_kind = "display_url".into();
                display_provider = caps.ops_panel.display_provider;
            } else if args.hard_site {
                warnings.push(
                    "hard-site requested, but this host does not expose display takeover; start a host with `--takeover display --display-provider kasmvnc` for more reliable human input. This is not a captcha bypass.".into(),
                );
            } else if caps.backend.family == "camoufox" || !caps.ops_panel.screencast {
                warnings.push(
                    "screencast takeover may be unavailable or limited for this backend; use a real-display takeover host for hard sites. This is not a captcha bypass.".into(),
                );
            }
        }
        Err(e) => warnings.push(format!(
            "could not read /capabilities; defaulting recommended_url to screencast_url: {}",
            e.detail
        )),
    }

    let tab_id = create_target(&client).await?;
    let tab = TabId::new(tab_id.clone());
    let _ = client.cdp("Page.enable").tab(tab.clone()).send().await;
    let navigate = client
        .cdp("Page.navigate")
        .tab(tab)
        .params(serde_json::json!({ "url": args.url }))
        .send()
        .await?;
    if let Some(err) = navigate.get("errorText").and_then(|v| v.as_str()) {
        if !err.is_empty() {
            return Err(Error::new(
                ErrorCode::NavigationTimeout,
                format!("Page.navigate for takeover tab returned {err}"),
            ));
        }
    }

    output::emit(
        "takeover_prepare",
        &PrepareResult {
            url: args.url,
            endpoint: args.endpoint,
            tab_id,
            screencast_url: urls.screencast_url,
            display_url: urls.display_url,
            recommended_url,
            recommended_url_kind,
            display_provider,
            hard_site: args.hard_site,
            warnings,
        },
    )
}

async fn create_target(client: &Client) -> Result<String, Error> {
    let target = client
        .cdp("Target.createTarget")
        .params(serde_json::json!({"url": "about:blank"}))
        .send()
        .await?;
    target
        .get("targetId")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| Error::new(ErrorCode::CdpError, "Target.createTarget: missing targetId"))
}

fn build_urls(endpoint: &str, token: Option<&str>) -> Result<TakeoverUrls, Error> {
    let endpoint = Endpoint::parse(endpoint)?;
    let base = endpoint.http_base();
    let mut screencast_url = url::Url::parse(&format!("{base}/ops/screencast")).map_err(|e| {
        Error::new(
            ErrorCode::InvalidEndpoint,
            format!("takeover screencast URL from endpoint {base:?}: {e}"),
        )
    })?;
    let mut display_url = url::Url::parse(&format!("{base}/ops/display")).map_err(|e| {
        Error::new(
            ErrorCode::InvalidEndpoint,
            format!("takeover display URL from endpoint {base:?}: {e}"),
        )
    })?;
    if let Some(token) = token {
        screencast_url
            .query_pairs_mut()
            .append_pair("token_secret", token);
        display_url
            .query_pairs_mut()
            .append_pair("token_secret", token);
    }
    let screencast_url = screencast_url.to_string();
    Ok(TakeoverUrls {
        screencast_url,
        display_url: display_url.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takeover_urls_encode_token() {
        let urls = build_urls("http://localhost:9222", Some("a+b&c%20")).unwrap();
        assert_eq!(
            urls.screencast_url,
            "http://localhost:9222/ops/screencast?token_secret=a%2Bb%26c%2520"
        );
        assert_eq!(
            urls.display_url,
            "http://localhost:9222/ops/display?token_secret=a%2Bb%26c%2520"
        );
    }

    #[test]
    fn prepare_result_exposes_recommended_url() {
        let value = serde_json::to_value(PrepareResult {
            url: "https://example.com".into(),
            endpoint: "ws://127.0.0.1:9222".into(),
            tab_id: "tab-1".into(),
            screencast_url: "http://127.0.0.1:9222/ops/screencast".into(),
            display_url: "http://127.0.0.1:9222/ops/display".into(),
            recommended_url: "http://127.0.0.1:9222/ops/display".into(),
            recommended_url_kind: "display_url".into(),
            display_provider: Some("kasmvnc".into()),
            hard_site: true,
            warnings: Vec::new(),
        })
        .unwrap();
        assert_eq!(value["code"], serde_json::Value::Null);
        assert!(value.get("panel_url").is_none());
        assert_eq!(value["recommended_url_kind"], "display_url");
    }
}
