//! `afhttp fetch` subcommand.

use std::path::PathBuf;
use std::time::Duration;

use clap::Args as ClapArgs;
use clap::ValueEnum;

use crate::cli::output;
use crate::host::bootstrap::BrowserChoice;
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

#[derive(ValueEnum, Debug, Clone, Copy, Default)]
pub enum NetworkRedactArg {
    #[default]
    On,
    Off,
}

impl std::fmt::Display for NetworkRedactArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::On => "on",
            Self::Off => "off",
        })
    }
}

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// URL to fetch.
    pub url: String,
    /// CDP endpoint of a running host. Omit to spawn an inline ephemeral host
    /// for this one fetch.
    #[arg(long = "endpoint-url", help_heading = "Connection")]
    pub endpoint: Option<String>,
    /// Bearer token, if the host was started with `--token-secret`.
    #[arg(long = "token-secret", help_heading = "Connection")]
    pub token: Option<String>,
    /// Browser backend for the inline host: auto, chromium, chrome,
    /// chrome_shell, fingerprint-chromium, edge, brave, lightpanda, camoufox.
    /// Ignored when --endpoint-url is set (the host owns its browser).
    #[arg(long, default_value = "auto", help_heading = "Connection")]
    pub browser: String,
    /// Browser binary path for the inline host, for when auto-discovery can't
    /// find one. Ignored when --endpoint-url is set.
    #[arg(long = "browser-bin", value_name = "PATH", help_heading = "Connection")]
    pub browser_bin: Option<PathBuf>,
    /// Render strategy: none (HTTP fast path, no browser), auto (HTTP first,
    /// escalate to the browser on failure), or always (browser only).
    #[arg(long, default_value = "auto", help_heading = "Rendering")]
    pub render: String,
    /// Tab target to use. "new" allocates a temporary target and closes it
    /// after fetch; an id reuses that target and leaves it open.
    #[arg(
        long,
        default_value = "new",
        value_name = "new|<id>",
        help_heading = "Connection"
    )]
    pub tab: String,
    /// Readiness signal before capture on the browser path:
    /// auto | load | idle | selector:<css> | selector-visible:<css> | ms:<n>.
    #[arg(long, default_value = "auto", help_heading = "Rendering")]
    pub wait: String,
    /// Add a request header (repeatable). Format: `Name: value`.
    #[arg(long = "header", value_name = "K:V", help_heading = "Request")]
    pub headers: Vec<String>,
    /// Add a request cookie (repeatable). Format: `name=value`.
    #[arg(long = "cookie", value_name = "name=value", help_heading = "Request")]
    pub cookies: Vec<String>,
    /// Override the User-Agent header for this fetch.
    #[arg(long, help_heading = "Request")]
    pub user_agent: Option<String>,
    /// JavaScript to evaluate after the wait condition resolves (repeatable).
    /// Runs in page context before artifacts are captured.
    #[arg(long, value_name = "js", help_heading = "Rendering")]
    pub evaluate_after_wait: Vec<String>,
    /// Artifacts to capture, comma-separated. Omit for all of: body,
    /// rendered_html, text, screenshot, network, console, observation
    /// (storage is opt-in only).
    #[arg(long, value_delimiter = ',', help_heading = "Rendering")]
    pub want: Vec<String>,
    /// HTTP method. Common values: POST, PUT, PATCH, DELETE.
    #[arg(long, default_value = "GET", help_heading = "Request")]
    pub method: String,
    /// Request body as a string. Prefix with `@` to read from a file path
    /// (e.g. `--data @payload.json`). Mutually exclusive with `--form`.
    #[arg(long, help_heading = "Request")]
    pub data: Option<String>,
    /// Request body from a file path. Mutually exclusive with `--form`.
    #[arg(long, help_heading = "Request")]
    pub data_file: Option<PathBuf>,
    /// Add a form field (repeatable). Sends body as
    /// `application/x-www-form-urlencoded`. Mutually exclusive with `--data`.
    /// Format: `key=value`.
    #[arg(long = "form", value_name = "key=value", help_heading = "Request")]
    pub form: Vec<String>,
    /// Capture response bodies for network requests: off, xhr (XHR/fetch
    /// only), or all.
    #[arg(long, default_value_t = NetworkBodiesArg::Off, help_heading = "Network capture")]
    pub network_bodies: NetworkBodiesArg,
    /// Per-body cap for captured network bodies, in bytes.
    #[arg(long, default_value_t = DEFAULT_NETWORK_BODY_MAX_BYTES, help_heading = "Network capture")]
    pub network_body_max_bytes: u64,
    /// Network quiet window used by --wait auto, in milliseconds.
    #[arg(long, default_value_t = 800, help_heading = "Rendering")]
    pub readiness_idle_ms: u64,
    /// DOM/text unchanged window used by --wait auto, in milliseconds.
    #[arg(long, default_value_t = 500, help_heading = "Rendering")]
    pub readiness_stable_ms: u64,
    /// Low visible-text byte threshold for --wait auto quality warnings only.
    #[arg(long, default_value_t = 32, help_heading = "Rendering")]
    pub readiness_min_text_bytes: u64,
    /// Redact sensitive values in network.json: on or off. On by default;
    /// off writes raw Authorization/Cookie headers and token-bearing query
    /// params to the artifact — only disable for trusted local debugging.
    #[arg(long, default_value_t = NetworkRedactArg::On, help_heading = "Network capture")]
    pub network_redact: NetworkRedactArg,
    /// Directory to write artifacts into. Defaults to an `afhttp-out`
    /// subdirectory of the working directory.
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
    #[arg(long, default_value_t = 500, help_heading = "Rendering")]
    pub observe_main_wait_ms: u64,
    /// Upper bound on the HTTP-path response body, in bytes. Default
    /// 1 GiB (`1073741824`). `0` disables the cap entirely. When the
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
    /// Overall fetch timeout, in milliseconds.
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

async fn run_inner(args: Args) -> Result<(), FetchRunError> {
    let render = RenderMode::parse(&args.render)?;
    let wait = Wait::parse(&args.wait)?;
    let timeout = Duration::from_millis(args.timeout_ms);
    let network_bodies = NetworkBodies::from(args.network_bodies);
    let network_redact = matches!(args.network_redact, NetworkRedactArg::On);

    let body_bytes = resolve_body(&args).await?;
    let want = resolve_want(&args.want)?;
    let client = build_client(&args, render).await?;

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
        Ok(result) => Ok(output::emit("fetch", &result)?),
        Err(err) => {
            output::emit("error", &err)?;
            Err(FetchRunError::Emitted(err.into_error()))
        }
    }
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

/// Resolve the request body from `--data` / `--data-file`, rejecting the
/// mutually exclusive combinations. `--form` fields are wired separately by
/// the caller; this only enforces that they don't coexist with a raw body.
async fn resolve_body(args: &Args) -> Result<Option<Vec<u8>>, Error> {
    if args.data.is_some() && args.data_file.is_some() {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            "--data and --data-file are mutually exclusive",
        ));
    }
    if (args.data.is_some() || args.data_file.is_some()) && !args.form.is_empty() {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            "--data/--data-file and --form are mutually exclusive",
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
    if let Some(path) = &args.data_file {
        return Ok(Some(tokio::fs::read(path).await.map_err(|e| {
            Error::new(
                ErrorCode::IoError,
                format!("--data-file {}: {e}", path.display()),
            )
        })?));
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
            let browser = args
                .browser
                .parse::<BrowserChoice>()
                .map_err(|e| Error::new(ErrorCode::InvalidArgument, format!("--browser: {e}")))?;
            let cfg = InlineConfig {
                browser,
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
