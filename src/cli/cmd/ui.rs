//! `afhttp ui` subcommand. Prints the ops panel URL for an endpoint.

use clap::Args as ClapArgs;
use serde::Serialize;

use crate::cli::output;
use crate::sdk::endpoint::Endpoint;
use crate::sdk::Client;
use crate::shared::error::Error;

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// CDP endpoint of the running host.
    #[arg(long = "endpoint-url")]
    pub endpoint: String,
    /// Bearer token, if the host requires one (appended to the panel URLs).
    #[arg(long = "token-secret")]
    pub token: Option<String>,
}

#[derive(Serialize)]
struct UiResult {
    screencast_url: String,
    display_url: String,
    recommended_url: String,
    recommended_url_kind: String,
    display_provider: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

pub async fn run(args: Args) -> Result<(), Error> {
    let result = build_result(&args.endpoint, args.token.as_deref()).await?;
    output::emit("ui", &result)
}

async fn build_result(endpoint: &str, token: Option<&str>) -> Result<UiResult, Error> {
    let mut result = build_urls(endpoint, token)?;
    let mut client = Client::connect(endpoint)?;
    if let Some(token) = token {
        client = client.with_token(token);
    }
    match client.capabilities().await {
        Ok(caps) => {
            if caps.ops_panel.display {
                result.recommended_url = result.display_url.clone();
                result.recommended_url_kind = "display_url".into();
                result.display_provider = caps.ops_panel.display_provider;
            } else if caps.backend.family == "camoufox" || !caps.ops_panel.screencast {
                result.warnings.push(
                    "screencast takeover is unavailable or limited for this backend; for hard sites start the host with `--takeover display --display-provider kasmvnc` and use display_url. This is not a captcha bypass.".into(),
                );
            }
        }
        Err(e) => {
            result.warnings.push(format!(
                "could not read host capabilities; defaulting recommended_url to screencast_url: {}",
                e.detail
            ));
        }
    }
    Ok(result)
}

fn build_urls(endpoint: &str, token: Option<&str>) -> Result<UiResult, Error> {
    let endpoint = Endpoint::parse(endpoint)?;
    let base = endpoint.http_base();
    let mut screencast_url = url::Url::parse(&format!("{base}/ops/screencast")).map_err(|e| {
        crate::shared::error::Error::new(
            crate::shared::error::ErrorCode::InvalidEndpoint,
            format!("ui screencast URL from endpoint {base:?}: {e}"),
        )
    })?;
    let mut display_url = url::Url::parse(&format!("{base}/ops/display")).map_err(|e| {
        crate::shared::error::Error::new(
            crate::shared::error::ErrorCode::InvalidEndpoint,
            format!("ui display URL from endpoint {base:?}: {e}"),
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
    let display_url = display_url.to_string();
    Ok(UiResult {
        recommended_url: screencast_url.clone(),
        recommended_url_kind: "screencast_url".into(),
        screencast_url,
        display_url,
        display_provider: None,
        warnings: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_result_has_no_legacy_url_aliases() {
        let value = serde_json::to_value(UiResult {
            screencast_url: "http://localhost:9222/ops/screencast".into(),
            display_url: "http://localhost:9222/ops/display".into(),
            recommended_url: "http://localhost:9222/ops/screencast".into(),
            recommended_url_kind: "screencast_url".into(),
            display_provider: None,
            warnings: Vec::new(),
        })
        .unwrap();
        assert!(value.get("url").is_none());
        assert!(value.get("panel_url").is_none());
        assert!(value.get("screencast_url").is_some());
        assert!(value.get("display_url").is_some());
        assert!(value.get("recommended_url").is_some());
        assert!(value.get("display_provider").is_some());
    }

    #[test]
    fn ui_token_query_is_percent_encoded() {
        let result = build_urls("http://localhost:9222", Some("a+b&c%20")).unwrap();
        assert_eq!(
            result.screencast_url,
            "http://localhost:9222/ops/screencast?token_secret=a%2Bb%26c%2520"
        );
        assert_eq!(
            result.display_url,
            "http://localhost:9222/ops/display?token_secret=a%2Bb%26c%2520"
        );
    }
}
