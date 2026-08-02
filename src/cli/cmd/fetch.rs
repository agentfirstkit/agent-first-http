//! `afhttp fetch` subcommand.

use std::path::PathBuf;
use std::time::Duration;

use crate::cli::output;
use crate::host::bootstrap::BrowserChoice;
use crate::sdk::fetch::{FetchCookie, NetworkBodies, RenderMode, Wait};
use crate::sdk::{Client, InlineConfig};
use crate::shared::artifacts::Artifact;
use crate::shared::error::{Error, ErrorCode};
use crate::shared::ids::TabId;

/// One fetch, already narrowed to a registered shape: `data` and `form` are
/// never both set, and `takeover` never coexists with [`RenderMode::None`].
#[derive(Debug)]
pub struct Args {
    pub url: String,
    pub endpoint: Option<String>,
    pub token: Option<String>,
    pub browser: BrowserChoice,
    pub browser_bin: Option<PathBuf>,
    pub render: RenderMode,
    pub tab: String,
    pub takeover: bool,
    pub profile: Option<String>,
    pub wait: String,
    pub headers: Vec<String>,
    pub cookies: Vec<String>,
    pub user_agent: Option<String>,
    pub evaluate_after_wait: Vec<String>,
    pub want: Vec<String>,
    pub method: String,
    pub data: Option<String>,
    pub form: Vec<String>,
    pub network_bodies: NetworkBodies,
    pub network_body_max_bytes: u64,
    pub readiness_idle_ms: u64,
    pub readiness_stable_ms: u64,
    pub readiness_min_text_bytes: u64,
    pub no_network_redact: bool,
    pub out: Option<PathBuf>,
    pub cookie_jar: Option<PathBuf>,
    pub no_cookie_jar: bool,
    pub observe_main_wait_ms: u64,
    pub max_response_bytes: u64,
    pub retry: u32,
    pub backoff_ms: u64,
    pub proxy: Option<String>,
    pub ca_cert: Option<PathBuf>,
    pub tls_insecure: bool,
    pub timeout_ms: u64,
    pub capture_ws: bool,
    pub capture_sse: bool,
}

pub async fn run(args: Args) -> Result<(), Error> {
    match run_inner(args).await {
        Ok(()) => Ok(()),
        Err(FetchRunError::Plain(err)) => {
            let _ = crate::shared::afdata::emit_process_error(&err);
            Err(err)
        }
        Err(FetchRunError::Emitted(err)) => Err(err),
    }
}

async fn run_inner(mut args: Args) -> Result<(), FetchRunError> {
    let render = args.render;
    let endpoint_was_preconfigured = args.endpoint.is_some();
    prepare_takeover_connection(&mut args, |token| async move {
        crate::cli::cmd::container::discover_default_takeover_host(token.as_deref()).await
    })
    .await?;
    // Resolve the host profile: explicit --profile wins; otherwise --takeover
    // derives the URL's registrable domain (eTLD+1) for per-domain isolation.
    let explicit_profile = args.profile.clone();
    let resolved_profile: Option<String> = if let Some(p) = explicit_profile.clone() {
        Some(p)
    } else if args.takeover {
        Some(default_profile_for_url(&args.url)?)
    } else {
        None
    };
    if resolved_profile.is_some() && args.endpoint.is_none() {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            "--profile (and --takeover profile derivation) switch the host's active profile and require a host; pass --endpoint-url or set AFHTTP_ENDPOINT_URL",
        )
        .into());
    }
    let takeover = args.takeover;
    let takeover_endpoint = args.endpoint.clone();
    let recommended_endpoint =
        takeover_recommended_endpoint(takeover_endpoint.as_deref(), endpoint_was_preconfigured);
    let takeover_token = args.token.clone();
    let wait = Wait::parse(&args.wait)?;
    let timeout = Duration::from_millis(args.timeout_ms);
    let network_bodies = args.network_bodies;
    let network_redact = !args.no_network_redact;

    let body_bytes = resolve_body(&args).await?;
    let want = resolve_want(&args.want);
    let mut client = build_client(&args, render).await?;
    if let Some(profile) = &resolved_profile {
        client = client.with_profile(profile.clone());
    }

    let mut builder = client
        .fetch(args.url.clone())
        .render(render)
        .wait(wait)
        .timeout(timeout)
        .network_bodies(network_bodies)
        .network_body_max_bytes(args.network_body_max_bytes)
        .readiness_idle_ms(args.readiness_idle_ms)
        .readiness_stable_ms(args.readiness_stable_ms)
        .readiness_min_text_bytes(args.readiness_min_text_bytes)
        .network_redact(network_redact)
        .method(args.method);
    if let Some(want) = want {
        builder = builder.want(want);
    }
    if let Some(bytes) = body_bytes {
        builder = builder.body(bytes);
    }
    for raw in &args.form {
        let (k, v) = raw.split_once('=').ok_or_else(|| {
            Error::new(
                ErrorCode::InvalidArgument,
                format!("--form: expected key=value, got {raw:?}"),
            )
        })?;
        builder = builder.form_field(k, v);
    }
    for raw in args.headers {
        let (name, value) = parse_header_arg(&raw)?;
        builder = builder.header(name, value);
    }
    for raw in args.cookies {
        builder = builder.cookie_full(parse_cookie_arg(&raw)?);
    }
    if let Some(user_agent) = args.user_agent {
        builder = builder.user_agent(user_agent);
    }
    for js in args.evaluate_after_wait {
        builder = builder.evaluate_after_wait(js);
    }
    if args.tab != "new" {
        builder = builder.tab(TabId::new(args.tab));
    }
    if takeover {
        // Keep the prepared tab open so a human can take it over.
        builder = builder.keep_tab_open(true);
    }
    if let Some(out) = args.out {
        builder = builder.out_dir(out);
    }
    builder = builder.observe_main_wait_ms(args.observe_main_wait_ms);
    builder = builder.max_response_bytes(args.max_response_bytes);
    builder = builder.retry(args.retry).backoff_ms(args.backoff_ms);
    if let Some(url) = args.proxy {
        builder = builder.proxy(url);
    }
    if let Some(path) = args.ca_cert {
        builder = builder.ca_cert(path);
    }
    if args.tls_insecure {
        builder = builder.tls_insecure(true);
    }
    if args.capture_ws {
        builder = builder.capture_ws(true);
    }
    if args.capture_sse {
        builder = builder.capture_sse(true);
    }
    if args.no_cookie_jar {
        builder = builder.no_cookie_jar();
    } else {
        let cookie_jar = args.cookie_jar.or_else(|| {
            std::env::var_os("AFHTTP_COOKIE_JAR")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        });
        if let Some(jar) = cookie_jar {
            builder = builder.cookie_jar(jar);
        }
    }

    match builder.send_detailed().await {
        Ok(mut result) => {
            if takeover
                && result.next_action.is_some()
                && let Some(endpoint) = takeover_endpoint.as_deref()
            {
                let mut handoff_client = Client::connect(endpoint)?;
                if let Some(token) = takeover_token.as_deref() {
                    handoff_client = handoff_client.with_token(token);
                }
                let tab_id = result.tab_id.as_ref().map(|t| t.as_str().to_string());
                let handoff = handoff_client
                    .takeover_handoff(None, tab_id.as_deref())
                    .await?;
                result.attach_takeover_with_context(
                    handoff.takeover_url_secret,
                    Some(handoff.takeover_url_expires_at_rfc3339),
                    Some(handoff.takeover_url_ttl_s),
                    Some(handoff.takeover_url_scope),
                    recommended_endpoint,
                    explicit_profile.as_deref(),
                );
            }
            if takeover {
                Ok(output::emit_revealing_takeover("fetch", &result)?)
            } else {
                Ok(output::emit("fetch", &result)?)
            }
        }
        Err(err) => {
            let trace = serde_json::to_value(&err.trace).map_err(|e| {
                Error::new(
                    ErrorCode::InternalError,
                    format!("serialize fetch error trace: {e}"),
                )
            })?;
            let err = err.into_error();
            crate::shared::afdata::emit_process_error_with(
                err.error_code.as_str(),
                &err.detail,
                serde_json::json!({"retryable": err.retryable}),
                trace,
            )?;
            Err(FetchRunError::Emitted(err))
        }
    }
}

/// Fill in the host `--takeover` needs when the caller did not name one.
///
/// That `--takeover` requires a browser render is not checked here: the
/// registry's takeover shapes pin `--render` to the modes that reach one, so an
/// unusable mix never becomes an invocation.
async fn prepare_takeover_connection<D, Fut>(args: &mut Args, discover: D) -> Result<(), Error>
where
    D: FnOnce(Option<String>) -> Fut,
    Fut: std::future::Future<Output = Result<crate::cli::cmd::container::LocalTakeoverHost, Error>>,
{
    if !args.takeover {
        return Ok(());
    }
    if args.endpoint.is_none() {
        let discovered = discover(args.token.clone()).await?;
        args.endpoint = Some(discovered.endpoint);
        if args.token.is_none() {
            args.token = discovered.token_secret;
        }
    }
    Ok(())
}

fn takeover_recommended_endpoint(
    endpoint: Option<&str>,
    endpoint_was_preconfigured: bool,
) -> Option<&str> {
    if endpoint_was_preconfigured {
        endpoint
    } else {
        None
    }
}

/// Derive a default host profile from a URL's registrable domain (eTLD+1), so
/// `fetch --takeover` gives each site its own isolated browser profile when
/// `--profile` is omitted. Falls back to the full host for public-suffix
/// tenants (e.g. `foo.github.io`) and bare IPs.
fn default_profile_for_url(raw_url: &str) -> Result<String, Error> {
    let parsed = url::Url::parse(raw_url).map_err(|e| {
        Error::new(
            ErrorCode::InvalidArgument,
            format!(
                "--takeover default profile needs a valid URL with a host; \
                 could not parse {raw_url:?}: {e}; pass --profile <name>"
            ),
        )
    })?;
    let host = parsed.host().ok_or_else(|| {
        Error::new(
            ErrorCode::InvalidArgument,
            format!(
                "--takeover default profile needs URL {raw_url:?} to include a host; \
                 pass --profile <name>"
            ),
        )
    })?;
    let (normalized_host, dns_name) = match host {
        url::Host::Domain(domain) => (normalize_profile_host(domain), true),
        url::Host::Ipv4(addr) => (addr.to_string(), false),
        url::Host::Ipv6(addr) => (addr.to_string(), false),
    };
    let profile = if dns_name {
        psl::domain_str(&normalized_host)
            .unwrap_or(&normalized_host)
            .to_string()
    } else {
        normalized_host.clone()
    };
    crate::sdk::profile::paths::validate_name(&profile).map_err(|e| {
        Error::new(
            e.error_code,
            format!(
                "derived --takeover profile {profile:?} from URL host {normalized_host:?} \
                 is invalid: {}; pass --profile <name>",
                e.detail
            ),
        )
    })?;
    Ok(profile)
}

fn normalize_profile_host(host: &str) -> String {
    host.trim_end_matches('.').to_ascii_lowercase()
}

enum FetchRunError {
    Plain(Error),
    Emitted(Error),
}

impl From<Error> for FetchRunError {
    fn from(err: Error) -> Self {
        Self::Plain(err)
    }
}

/// Resolve the request body from `--data` (literal, or `@path` to read a file).
///
/// `--form` fields are wired separately by the caller, and the registry's three
/// body shapes already guarantee the two never arrive together.
async fn resolve_body(args: &Args) -> Result<Option<Vec<u8>>, Error> {
    if let Some(data) = &args.data {
        if let Some(path) = data.strip_prefix('@') {
            return Ok(Some(tokio::fs::read(path).await.map_err(|e| {
                Error::new(ErrorCode::IoError, format!("--data @{path}: {e}"))
            })?));
        }
        return Ok(Some(data.as_bytes().to_vec()));
    }
    Ok(None)
}

/// Parse the `--want` tokens into an artifact set; `None` means use the
/// render-mode-aware default. The registry declares `--want` as a closed value
/// set, so every token that reaches here names an artifact.
fn resolve_want(want: &[String]) -> Option<std::collections::BTreeSet<Artifact>> {
    if want.is_empty() {
        return None;
    }
    Some(
        want.iter()
            .filter_map(|token| parse_artifact(token))
            .collect(),
    )
}

/// Construct the SDK client for this fetch: a remote connection when
/// `--endpoint-url` is set, the HTTP-only client for `--render none`, or an
/// inline ephemeral host otherwise (lazy for `auto`, eager for `always`).
async fn build_client(args: &Args, render: RenderMode) -> Result<Client, Error> {
    match args.endpoint.as_deref() {
        Some(ep) => {
            let mut c = Client::connect(ep)?;
            if let Some(t) = args.token.as_deref() {
                c = c.with_token(t);
            }
            Ok(c)
        }
        None if matches!(render, RenderMode::None) => Client::http_only(),
        None => {
            let cfg = InlineConfig {
                browser: args.browser.clone(),
                browser_bin: args.browser_bin.clone(),
            };
            if matches!(render, RenderMode::Auto) {
                Client::inline_ephemeral_lazy(cfg).await
            } else {
                Client::inline_ephemeral_with(cfg).await
            }
        }
    }
}

fn parse_artifact(token: &str) -> Option<Artifact> {
    Some(match token {
        "body" => Artifact::Body,
        "rendered_html" => Artifact::RenderedHtml,
        "text" => Artifact::Text,
        "content" => Artifact::Content,
        "content_json" => Artifact::ContentJson,
        "screenshot" => Artifact::Screenshot,
        "network" => Artifact::Network,
        "console" => Artifact::Console,
        "observation" => Artifact::Observation,
        "storage" => Artifact::Storage,
        _ => return None,
    })
}

fn parse_header_arg(raw: &str) -> Result<(String, String), Error> {
    let (name, value) = raw.split_once(':').ok_or_else(|| {
        Error::new(
            ErrorCode::InvalidArgument,
            format!("--header: expected K:V, got {raw:?}"),
        )
    })?;
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            format!("--header: header name must not be empty in {raw:?}"),
        ));
    }
    Ok((name.to_string(), value.trim_start().to_string()))
}

fn parse_cookie_arg(raw: &str) -> Result<FetchCookie, Error> {
    if !raw.contains('=') {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            format!("--cookie: expected Set-Cookie style name=value, got {raw:?}"),
        ));
    }
    let cookie = FetchCookie::parse(raw.to_string())
        .map_err(|e| Error::new(ErrorCode::InvalidArgument, format!("--cookie: {e}")))?
        .into_owned();
    if cookie.name().trim().is_empty() {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            format!("--cookie: cookie name must not be empty in {raw:?}"),
        ));
    }
    Ok(cookie)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::sdk::fetch::DEFAULT_NETWORK_BODY_MAX_BYTES;

    fn base_args(url: &str) -> Args {
        Args {
            url: url.to_string(),
            endpoint: None,
            token: None,
            browser: BrowserChoice::Auto,
            browser_bin: None,
            render: RenderMode::Auto,
            tab: "new".into(),
            takeover: false,
            profile: None,
            wait: "auto".into(),
            headers: Vec::new(),
            cookies: Vec::new(),
            user_agent: None,
            evaluate_after_wait: Vec::new(),
            want: Vec::new(),
            method: "GET".into(),
            data: None,
            form: Vec::new(),
            network_bodies: NetworkBodies::Off,
            network_body_max_bytes: DEFAULT_NETWORK_BODY_MAX_BYTES,
            readiness_idle_ms: 800,
            readiness_stable_ms: 500,
            readiness_min_text_bytes: 32,
            no_network_redact: false,
            out: None,
            cookie_jar: None,
            no_cookie_jar: false,
            observe_main_wait_ms: 500,
            max_response_bytes: 1_073_741_824,
            retry: 0,
            backoff_ms: 250,
            proxy: None,
            ca_cert: None,
            tls_insecure: false,
            timeout_ms: 30_000,
            capture_ws: false,
            capture_sse: false,
        }
    }

    #[tokio::test]
    async fn takeover_autodiscovery_fills_missing_endpoint_and_token() {
        let mut args = base_args("https://contabo.com");
        args.takeover = true;
        prepare_takeover_connection(&mut args, |token| async move {
            assert_eq!(token, None);
            Ok(crate::cli::cmd::container::LocalTakeoverHost {
                endpoint: "ws://127.0.0.1:9222".into(),
                token_secret: Some("secret".into()),
            })
        })
        .await
        .unwrap();

        assert_eq!(args.endpoint.as_deref(), Some("ws://127.0.0.1:9222"));
        assert_eq!(args.token.as_deref(), Some("secret"));
    }

    #[tokio::test]
    async fn takeover_autodiscovery_preserves_existing_token() {
        let mut args = base_args("https://contabo.com");
        args.takeover = true;
        args.token = Some("env-token".into());
        prepare_takeover_connection(&mut args, |token| async move {
            assert_eq!(token.as_deref(), Some("env-token"));
            Ok(crate::cli::cmd::container::LocalTakeoverHost {
                endpoint: "ws://127.0.0.1:9222".into(),
                token_secret: Some("container-token".into()),
            })
        })
        .await
        .unwrap();

        assert_eq!(args.endpoint.as_deref(), Some("ws://127.0.0.1:9222"));
        assert_eq!(args.token.as_deref(), Some("env-token"));
    }

    #[tokio::test]
    async fn takeover_autodiscovery_surfaces_failure() {
        let mut args = base_args("https://contabo.com");
        args.takeover = true;
        let err = prepare_takeover_connection(&mut args, |_| async {
            Err(Error::new(
                ErrorCode::InvalidArgument,
                "default local container `afhttp-host` is not running",
            ))
        })
        .await
        .err()
        .unwrap();

        assert_eq!(err.error_code, ErrorCode::InvalidArgument);
        assert!(err.detail.contains("afhttp-host"));
        assert!(args.endpoint.is_none());
    }

    /// Every value the registry lets `--want` carry names a real artifact, so
    /// the projection can be total.
    #[test]
    fn every_registry_want_value_names_an_artifact() {
        for token in crate::cli::spec::ARTIFACTS {
            assert!(parse_artifact(token).is_some(), "{token}");
        }
        let wanted = resolve_want(&["body".to_string(), "network".to_string()])
            .expect("an explicit want is a set");
        assert!(wanted.contains(&Artifact::Body));
        assert!(wanted.contains(&Artifact::Network));
        assert!(resolve_want(&[]).is_none(), "no want means the default set");
    }

    #[test]
    fn takeover_recommendation_omits_auto_discovered_endpoint() {
        assert_eq!(
            takeover_recommended_endpoint(Some("ws://127.0.0.1:9222"), false),
            None
        );
    }

    #[test]
    fn takeover_recommendation_keeps_preconfigured_endpoint() {
        assert_eq!(
            takeover_recommended_endpoint(Some("ws://127.0.0.1:9222"), true),
            Some("ws://127.0.0.1:9222")
        );
    }

    #[test]
    fn default_profile_uses_registrable_domain() {
        assert_eq!(
            default_profile_for_url("https://www.court.gov.cn/foo").unwrap(),
            "court.gov.cn"
        );
        assert_eq!(
            default_profile_for_url("https://accounts.google.com/foo").unwrap(),
            "google.com"
        );
        assert_eq!(
            default_profile_for_url("https://contabo.com").unwrap(),
            "contabo.com"
        );
    }

    #[test]
    fn default_profile_keeps_public_suffix_tenants_isolated() {
        assert_eq!(
            default_profile_for_url("https://foo.github.io/x").unwrap(),
            "foo.github.io"
        );
        assert_eq!(
            default_profile_for_url("https://tenant.vercel.app/x").unwrap(),
            "tenant.vercel.app"
        );
    }

    #[test]
    fn default_profile_normalizes_case_and_trailing_dot() {
        assert_eq!(
            default_profile_for_url("https://WWW.Example.COM./foo").unwrap(),
            "example.com"
        );
    }

    #[test]
    fn default_profile_falls_back_to_full_host_for_psl_misses_and_ips() {
        assert_eq!(
            default_profile_for_url("http://localhost:8080/foo").unwrap(),
            "localhost"
        );
        assert_eq!(
            default_profile_for_url("http://127.0.0.1:8080/foo").unwrap(),
            "127.0.0.1"
        );
    }

    #[test]
    fn default_profile_errors_when_host_is_missing() {
        assert!(default_profile_for_url("file:///tmp/page.html").is_err());
    }

    #[test]
    fn header_arg_accepts_colon_separator() {
        assert_eq!(
            parse_header_arg("X-Test: yes").unwrap(),
            ("X-Test".to_string(), "yes".to_string())
        );
    }

    #[test]
    fn header_arg_rejects_missing_colon() {
        let err = parse_header_arg("X-Test").err().unwrap();
        assert_eq!(err.error_code, ErrorCode::InvalidArgument);
    }

    #[test]
    fn cookie_arg_accepts_equals_separator() {
        let cookie = parse_cookie_arg("sid=abc=def").unwrap();
        assert_eq!(cookie.name_value(), ("sid", "abc=def"));
    }

    #[test]
    fn cookie_arg_accepts_full_set_cookie_attributes() {
        let cookie = parse_cookie_arg("sid=abc; Path=/; Secure; HttpOnly; SameSite=Lax").unwrap();
        assert_eq!(cookie.name_value(), ("sid", "abc"));
        assert_eq!(cookie.path(), Some("/"));
        assert_eq!(cookie.secure(), Some(true));
        assert_eq!(cookie.http_only(), Some(true));
        assert_eq!(cookie.same_site(), Some(cookie::SameSite::Lax));
    }

    #[test]
    fn cookie_arg_rejects_missing_equals() {
        let err = parse_cookie_arg("sid").err().unwrap();
        assert_eq!(err.error_code, ErrorCode::InvalidArgument);
    }
}
