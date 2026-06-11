//! `afhttp fetch` subcommand.

use std::path::PathBuf;
use std::time::Duration;

use clap::Args as ClapArgs;
use clap::ValueEnum;

use crate::cli::cmd::argenums::{BrowserArg, RenderArg};
use crate::cli::output;
use crate::sdk::fetch::{
    FetchCookie, NetworkBodies, RenderMode, Wait, DEFAULT_NETWORK_BODY_MAX_BYTES,
};
use crate::sdk::{Client, InlineConfig};
use crate::shared::artifacts::Artifact;
use crate::shared::error::{Error, ErrorCode};
use crate::shared::ids::TabId;

#[derive(ValueEnum, Debug, Clone, Copy, Default)]
pub enum NetworkBodiesArg {
    #[default]
    Off,
    Xhr,
    All,
}

impl From<NetworkBodiesArg> for NetworkBodies {
    fn from(v: NetworkBodiesArg) -> Self {
        match v {
            NetworkBodiesArg::Off => NetworkBodies::Off,
            NetworkBodiesArg::Xhr => NetworkBodies::Xhr,
            NetworkBodiesArg::All => NetworkBodies::All,
        }
    }
}

impl std::fmt::Display for NetworkBodiesArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Off => "off",
            Self::Xhr => "xhr",
            Self::All => "all",
        })
    }
}

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// URL to fetch.
    pub url: String,
    /// CDP endpoint of a running host. Omit to spawn an inline ephemeral host
    /// for ordinary browser fetches; with --takeover, omission discovers the
    /// standard local `afhttp-host`. Falls back to `AFHTTP_ENDPOINT_URL`.
    #[arg(
        long = "endpoint-url",
        env = "AFHTTP_ENDPOINT_URL",
        help_heading = "Connection"
    )]
    pub endpoint: Option<String>,
    /// Bearer token, if the host was started with `--token-secret`.
    /// Falls back to `AFHTTP_TOKEN_SECRET`.
    #[arg(
        long = "token-secret",
        env = "AFHTTP_TOKEN_SECRET",
        help_heading = "Connection"
    )]
    pub token: Option<String>,
    /// Browser backend for the inline host. Ignored when --endpoint-url is set
    /// (the host owns its browser).
    #[arg(long, default_value = "auto", help_heading = "Inline host")]
    pub browser: BrowserArg,
    /// Browser binary path for the inline host, for when auto-discovery can't
    /// find one. Ignored when --endpoint-url is set.
    #[arg(
        long = "browser-bin",
        value_name = "PATH",
        help_heading = "Inline host"
    )]
    pub browser_bin: Option<PathBuf>,
    /// Render strategy: none (HTTP fast path, no browser), auto (HTTP first,
    /// escalate to the browser on failure), or always (browser only).
    #[arg(long, default_value = "auto", help_heading = "Rendering")]
    pub render: RenderArg,
    /// Tab target: "new" allocates a temporary target and closes it after
    /// fetch; a CDP target id reuses that target and leaves it open (the same
    /// id `afhttp cdp`/`upload`/`tabs` accept).
    #[arg(
        long,
        default_value = "new",
        value_name = "new|<id>",
        help_heading = "Session"
    )]
    pub tab: String,
    /// Escalate to human takeover when a wall (captcha/login/2FA) is hit: keep
    /// a persistent tab open and return its short-lived takeover URL in
    /// `next_action`, plus a re-fetch command for the same tab once the human
    /// clears the wall. Uses `--endpoint-url` / `AFHTTP_ENDPOINT_URL` when set;
    /// otherwise discovers the standard local `afhttp-host` container (build
    /// one with `afhttp container install`).
    #[arg(long, help_heading = "Session")]
    pub takeover: bool,
    /// Host profile to use for this fetch. Switches the host's active profile
    /// if it differs (per-domain isolation), relaunching its browser. With
    /// `--takeover` and no `--profile`, the profile defaults to the URL's
    /// registrable domain (eTLD+1). Requires a host via `--endpoint-url`, or
    /// the standard local takeover host discovered by `--takeover`.
    #[arg(long, help_heading = "Session")]
    pub profile: Option<String>,
    /// Readiness signal before capture on the browser path:
    /// auto | load | idle | selector:<css> | selector-visible:<css> | ms:<n>.
    #[arg(long, default_value = "auto", help_heading = "Rendering")]
    pub wait: String,
    /// Add a request header (repeatable). Format: `Name:value` (a space after
    /// the colon is allowed).
    #[arg(long = "header", value_name = "NAME:VALUE", help_heading = "Request")]
    pub headers: Vec<String>,
    /// Add a request cookie (repeatable). Format: `name=value`.
    #[arg(long = "cookie", value_name = "NAME=VALUE", help_heading = "Request")]
    pub cookies: Vec<String>,
    /// Override the User-Agent header for this fetch.
    #[arg(long, help_heading = "Request")]
    pub user_agent: Option<String>,
    /// JavaScript to evaluate after the wait condition resolves (repeatable).
    /// Runs in page context before artifacts are captured.
    #[arg(long, value_name = "JS", help_heading = "Rendering")]
    pub evaluate_after_wait: Vec<String>,
    /// Artifacts to capture, comma-separated. Default: body, rendered_html,
    /// text, content, content_json, screenshot, network, console,
    /// observation. `content` is the agent-oriented composed page view
    /// (content.md); `content_json` its structured form with link/action
    /// candidates. `storage` is opt-in (sensitive: localStorage/IndexedDB).
    #[arg(long, value_delimiter = ',', help_heading = "Rendering")]
    pub want: Vec<String>,
    /// HTTP method. Common values: POST, PUT, PATCH, DELETE.
    #[arg(long, default_value = "GET", help_heading = "Request")]
    pub method: String,
    /// Request body as a string. Prefix with `@` to read from a file path
    /// (e.g. `--data @payload.json`). Mutually exclusive with `--form`.
    #[arg(long, value_name = "STRING|@FILE", help_heading = "Request")]
    pub data: Option<String>,
    /// Add a form field (repeatable). Sends body as
    /// `application/x-www-form-urlencoded`. Mutually exclusive with `--data`.
    /// Format: `name=value`.
    #[arg(long = "form", value_name = "NAME=VALUE", help_heading = "Request")]
    pub form: Vec<String>,
    /// Capture response bodies for network requests: off, xhr (XHR/fetch
    /// only), or all.
    #[arg(long, default_value_t = NetworkBodiesArg::Off, help_heading = "Network capture")]
    pub network_bodies: NetworkBodiesArg,
    /// Per-body cap for each captured network sub-request body, in bytes (see
    /// `--max-response-bytes` for the main HTTP-path response body).
    #[arg(long, default_value_t = DEFAULT_NETWORK_BODY_MAX_BYTES, help_heading = "Network capture")]
    pub network_body_max_bytes: u64,
    /// Network quiet window used by --wait auto, in milliseconds.
    #[arg(long, default_value_t = 800, help_heading = "Readiness tuning")]
    pub readiness_idle_ms: u64,
    /// DOM/text unchanged window used by --wait auto, in milliseconds.
    #[arg(long, default_value_t = 500, help_heading = "Readiness tuning")]
    pub readiness_stable_ms: u64,
    /// Low visible-text byte threshold for --wait auto quality warnings only.
    #[arg(long, default_value_t = 32, help_heading = "Readiness tuning")]
    pub readiness_min_text_bytes: u64,
    /// Disable redaction of sensitive values in network.json (redacted by
    /// default). Writes raw Authorization/Cookie headers and token-bearing
    /// query params to the artifact — only for trusted local debugging.
    #[arg(long, help_heading = "Network capture")]
    pub no_network_redact: bool,
    /// Directory to write artifacts into. Defaults to `afhttp-out` under the
    /// system temporary directory. Files persist there for inspection.
    #[arg(long, help_heading = "Output")]
    pub out: Option<PathBuf>,
    /// Override the cookie-jar path. The default — derived from the host's
    /// `GET /profile` — places the jar at `<profile-dir>/cookies.jar.json`.
    /// This override is rejected with `invalid_argument` if it does not
    /// match the host's profile path; the flag exists for tests and
    /// forensic tooling, not production sessions. Honors
    /// `AFHTTP_COOKIE_JAR` when omitted (same validation applies).
    #[arg(long, help_heading = "Cookies")]
    pub cookie_jar: Option<PathBuf>,
    /// Opt out of cookie-jar persistence for this fetch. No cookies are
    /// replayed from the jar and no `Set-Cookie` responses are merged
    /// back.
    #[arg(long, help_heading = "Cookies")]
    pub no_cookie_jar: bool,
    /// Upper bound on the browser-path wait for the main document network
    /// event, in milliseconds. Raise for slow networks or low-end machines.
    #[arg(long, default_value_t = 500, help_heading = "Readiness tuning")]
    pub observe_main_wait_ms: u64,
    /// Upper bound on the main HTTP-path response body, in bytes (see
    /// `--network-body-max-bytes` for captured network sub-request bodies).
    /// Default 1 GiB (`1073741824`). `0` disables the cap entirely. When the
    /// cap is hit, the fetch returns successfully with a
    /// `network_body_truncated` warning and the prefix bytes that
    /// were collected.
    #[arg(long, default_value_t = 1_073_741_824, help_heading = "HTTP transport")]
    pub max_response_bytes: u64,
    /// Number of additional attempts after the first. Retries fire
    /// only when the error has `retryable: true` (e.g.
    /// `host_unreachable`, `cdp_timeout`); non-retryable failures
    /// (`tls_error`, `wait_selector_unmatched`, etc.) short-circuit.
    /// Default 0 = single attempt.
    #[arg(long, default_value_t = 0, help_heading = "Retry")]
    pub retry: u32,
    /// Fixed delay between retries, in milliseconds.
    #[arg(long, default_value_t = 250, help_heading = "Retry")]
    pub backoff_ms: u64,
    /// Per-fetch upstream HTTP/HTTPS proxy for the HTTP fast path.
    /// The SDK never honors `HTTP_PROXY` from the environment; this
    /// flag is the only way to route an HTTP-path fetch through one.
    /// Format: `http://user:pass@host:port` or `socks5://host:port`.
    #[arg(long = "proxy-url", help_heading = "HTTP transport")]
    pub proxy: Option<String>,
    /// Path to a PEM file containing extra root CAs to trust for
    /// this fetch's HTTP path. Useful for self-signed staging or
    /// corporate MITM CAs.
    #[arg(long, help_heading = "HTTP transport")]
    pub ca_cert: Option<PathBuf>,
    /// Disable TLS certificate verification for this fetch's HTTP
    /// path. Dangerous; leaves the connection open to MITM. Use only
    /// against known-self-signed environments.
    #[arg(long, help_heading = "HTTP transport")]
    pub tls_insecure: bool,
    /// Overall fetch timeout, in milliseconds. Applies to both the HTTP fast
    /// path and the browser path.
    #[arg(
        long = "timeout-ms",
        default_value_t = 30_000,
        help_heading = "HTTP transport"
    )]
    pub timeout_ms: u64,
    /// Capture WebSocket frame payloads to network-bodies/<id>.frames.jsonl.
    /// Frames may carry bearer tokens, session IDs, and message content —
    /// treat the artifact as sensitive.
    #[arg(long, help_heading = "Network capture")]
    pub capture_ws: bool,
    /// Capture SSE event payloads to network-bodies/<id>.frames.jsonl. Events
    /// may carry PII; treat the artifact as sensitive.
    #[arg(long, help_heading = "Network capture")]
    pub capture_sse: bool,
}

pub async fn run(args: Args) -> Result<(), Error> {
    match run_inner(args).await {
        Ok(()) => Ok(()),
        Err(FetchRunError::Plain(err)) => {
            let stdout = std::io::stdout();
            let mut handle = stdout.lock();
            let _ = crate::shared::envelope::emit_error(&mut handle, &err);
            Err(err)
        }
        Err(FetchRunError::Emitted(err)) => Err(err),
    }
}

async fn run_inner(mut args: Args) -> Result<(), FetchRunError> {
    let render: RenderMode = args.render.into();
    prepare_takeover_connection(&mut args, render, |token| async move {
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
    let takeover_token = args.token.clone();
    let wait = Wait::parse(&args.wait)?;
    let timeout = Duration::from_millis(args.timeout_ms);
    let network_bodies = NetworkBodies::from(args.network_bodies);
    let network_redact = !args.no_network_redact;

    let body_bytes = resolve_body(&args).await?;
    let want = resolve_want(&args.want)?;
    let mut client = build_client(&args, render).await?;
    if let Some(profile) = &resolved_profile {
        client = client.with_profile(profile.clone());
    }

    let mut builder = client
        .fetch(args.url.clone())
        .render(render)
        .wait(wait)
        .timeout(timeout)
        .want(want)
        .network_bodies(network_bodies)
        .network_body_max_bytes(args.network_body_max_bytes)
        .readiness_idle_ms(args.readiness_idle_ms)
        .readiness_stable_ms(args.readiness_stable_ms)
        .readiness_min_text_bytes(args.readiness_min_text_bytes)
        .network_redact(network_redact)
        .method(args.method);
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
            if takeover && result.next_action.is_some() {
                if let Some(endpoint) = takeover_endpoint.as_deref() {
                    let mut handoff_client = Client::connect(endpoint)?;
                    if let Some(token) = takeover_token.as_deref() {
                        handoff_client = handoff_client.with_token(token);
                    }
                    let tab_id = result.tab_id.as_ref().map(|t| t.as_str().to_string());
                    let handoff = handoff_client
                        .takeover_handoff(None, tab_id.as_deref())
                        .await?;
                    result.attach_takeover_with_context(
                        handoff.takeover_url,
                        Some(handoff.takeover_url_expires_at_rfc3339),
                        Some(handoff.takeover_url_ttl_s),
                        Some(handoff.takeover_url_scope),
                        Some(endpoint),
                        explicit_profile.as_deref(),
                    );
                }
            }
            Ok(output::emit("fetch", &result)?)
        }
        Err(err) => {
            output::emit("error", &err)?;
            Err(FetchRunError::Emitted(err.into_error()))
        }
    }
}

async fn prepare_takeover_connection<D, Fut>(
    args: &mut Args,
    render: RenderMode,
    discover: D,
) -> Result<(), Error>
where
    D: FnOnce(Option<String>) -> Fut,
    Fut: std::future::Future<Output = Result<crate::cli::cmd::container::LocalTakeoverHost, Error>>,
{
    if !args.takeover {
        return Ok(());
    }
    if matches!(render, RenderMode::None) {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            "fetch --takeover needs a browser render; use --render auto or always",
        ));
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

/// Resolve the request body from `--data` (literal, or `@path` to read a file),
/// rejecting coexistence with `--form`. `--form` fields are wired separately by
/// the caller; this only enforces that they don't coexist with a raw body.
async fn resolve_body(args: &Args) -> Result<Option<Vec<u8>>, Error> {
    if args.data.is_some() && !args.form.is_empty() {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            "--data and --form are mutually exclusive",
        ));
    }
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

/// Parse the `--want` tokens into an artifact set; an empty list means all
/// default artifacts.
fn resolve_want(want: &[String]) -> Result<std::collections::BTreeSet<Artifact>, Error> {
    if want.is_empty() {
        return Ok(Artifact::ALL.iter().copied().collect());
    }
    want.iter().map(|t| parse_artifact(t)).collect()
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
                browser: args.browser.into(),
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

fn parse_artifact(token: &str) -> Result<Artifact, Error> {
    Ok(match token {
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
        other => {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                format!("--want: unknown artifact {other:?}"),
            ));
        }
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

    fn base_args(url: &str) -> Args {
        Args {
            url: url.to_string(),
            endpoint: None,
            token: None,
            browser: BrowserArg::Auto,
            browser_bin: None,
            render: RenderArg::Auto,
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
            network_bodies: NetworkBodiesArg::Off,
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
        prepare_takeover_connection(&mut args, RenderMode::Auto, |token| async move {
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
        prepare_takeover_connection(&mut args, RenderMode::Auto, |token| async move {
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
        let err = prepare_takeover_connection(&mut args, RenderMode::Auto, |_| async {
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

    #[tokio::test]
    async fn takeover_autodiscovery_rejects_render_none() {
        let mut args = base_args("https://contabo.com");
        args.takeover = true;
        let err = prepare_takeover_connection(&mut args, RenderMode::None, |_| async {
            Ok(crate::cli::cmd::container::LocalTakeoverHost {
                endpoint: "ws://127.0.0.1:9222".into(),
                token_secret: None,
            })
        })
        .await
        .err()
        .unwrap();

        assert_eq!(err.error_code, ErrorCode::InvalidArgument);
        assert!(err.detail.contains("browser render"));
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
