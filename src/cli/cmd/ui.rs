//! `afhttp ui` subcommand. Prints the ops panel URL for an endpoint.

use clap::Args as ClapArgs;
use serde::Serialize;

use crate::cli::output;
use crate::sdk::endpoint::Endpoint;
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
    panel_url: String,
    display_url: String,
}

pub async fn run(args: Args) -> Result<(), Error> {
    let result = build_result(&args.endpoint, args.token.as_deref())?;
    output::emit("ui", &result)
}

fn build_result(endpoint: &str, token: Option<&str>) -> Result<UiResult, Error> {
    let endpoint = Endpoint::parse(endpoint)?;
    let base = endpoint.http_base();
    let mut panel_url = url::Url::parse(&format!("{base}/ops")).map_err(|e| {
        crate::shared::error::Error::new(
            crate::shared::error::ErrorCode::InvalidEndpoint,
            format!("ui panel URL from endpoint {base:?}: {e}"),
        )
    })?;
    let mut display_url = url::Url::parse(&format!("{base}/ops/display")).map_err(|e| {
        crate::shared::error::Error::new(
            crate::shared::error::ErrorCode::InvalidEndpoint,
            format!("ui display URL from endpoint {base:?}: {e}"),
        )
    })?;
    if let Some(token) = token {
        panel_url
            .query_pairs_mut()
            .append_pair("token_secret", token);
        display_url
            .query_pairs_mut()
            .append_pair("token_secret", token);
    }
    Ok(UiResult {
        panel_url: panel_url.to_string(),
        display_url: display_url.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_result_has_no_url_alias() {
        let value = serde_json::to_value(UiResult {
            panel_url: "http://localhost:9222/ops".into(),
            display_url: "http://localhost:9222/ops/display".into(),
        })
        .unwrap();
        assert!(value.get("url").is_none());
        assert!(value.get("panel_url").is_some());
        assert!(value.get("display_url").is_some());
    }

    #[test]
    fn ui_token_query_is_percent_encoded() {
        let result = build_result("http://localhost:9222", Some("a+b&c%20")).unwrap();
        assert_eq!(
            result.panel_url,
            "http://localhost:9222/ops?token_secret=a%2Bb%26c%2520"
        );
        assert_eq!(
            result.display_url,
            "http://localhost:9222/ops/display?token_secret=a%2Bb%26c%2520"
        );
    }
}
