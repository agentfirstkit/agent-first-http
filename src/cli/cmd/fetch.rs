//! `afhttp fetch` subcommand.

use std::path::PathBuf;
use std::time::Duration;

use crate::cli::connect::{Connection, Resolved};
use crate::cli::output;
use crate::cli::token_source;
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
    pub connection: Connection,
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

async fn run_inner(args: Args) -> Result<(), FetchRunError> {
    let render = args.render;
    let endpoint_was_preconfigured = args.connection.is_explicit();
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
    // A takeover needs a host a person can see, a profile needs one whose
    // browser state persists, and neither is what the inline ephemeral browser
    // is. Both used to be the caller's job to supply; now only naming a
    // *different* host is.
    let host = if args.takeover {
        prepare_host(
            &args.connection,
            true,
            resolved_profile.as_deref(),
            |token| async move {
                crate::cli::cmd::container::discover_default_takeover_host(token.as_deref()).await
            },
        )
        .await?
    } else {
        prepare_host(
            &args.connection,
            false,
            resolved_profile.as_deref(),
            |token| async move {
                crate::cli::cmd::container::discover_default_local_host(token.as_deref()).await
            },
        )
        .await?
    };
    let takeover = args.takeover;
    let takeover_endpoint = host.as_ref().map(|host| host.endpoint.clone());
    let recommended_endpoint =
        takeover_recommended_endpoint(takeover_endpoint.as_deref(), endpoint_was_preconfigured);
    let takeover_token = host.as_ref().and_then(|host| host.token.clone());
    let wait = Wait::parse(&args.wait)?;
    let timeout = Duration::from_millis(args.timeout_ms);
    let network_bodies = args.network_bodies;
    let network_redact = !args.no_network_redact;

    let body_bytes = resolve_body(&args).await?;
    let want = resolve_want(&args.want);
    let mut client = build_client(&args, host.as_ref(), render).await?;
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
        // A form field is as likely to be a password as anything else on this
        // command line.
        let (k, v) = raw.split_once('=').ok_or_else(|| {
            Error::new(
                ErrorCode::InvalidArgument,
                "--form: expected KEY=VALUE, with an equals sign separating them".to_string(),
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
            explicit_jar_needs_a_host_profile(host.is_some())?;
            builder = builder.cookie_jar(jar);
        }
    }

    match builder.send_detailed().await {
        Ok(mut result) => {
            if takeover
                && result.next_action.is_some()
                && let Some(endpoint) = takeover_endpoint.as_deref()
            {
                result.prepare_takeover_retry(recommended_endpoint, explicit_profile.as_deref());
                let tab_id = result.tab_id.as_ref().map(|t| t.as_str().to_string());
                let handoff =
                    mint_handoff(endpoint, takeover_token.as_ref(), tab_id.as_deref()).await;
                match handoff {
                    Ok(handoff) => result.attach_takeover_with_context(
                        handoff.takeover_url_secret,
                        Some(handoff.takeover_url_expires_at_rfc3339),
                        Some(handoff.takeover_url_ttl_s),
                        Some(handoff.takeover_url_scope),
                        recommended_endpoint,
                        explicit_profile.as_deref(),
                    ),
                    Err(error) => retain_after_handoff_failure(&mut result, error),
                }
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

/// The host this fetch talks to, or `None` when it needs none — a plain fetch
/// with no profile and no takeover runs against the inline ephemeral browser,
/// which is the fast path and stays the default.
///
/// `discover` is only called when a host is needed and the caller named none.
/// An explicit `--token-secret` outranks the discovered host's own token: a
/// caller who supplies a credential means it.
///
/// That `--takeover` requires a browser render is not checked here: the
/// registry's takeover shapes pin `--render` to the modes that reach one, so an
/// unusable mix never becomes an invocation.
async fn prepare_host<D, Fut>(
    connection: &Connection,
    takeover: bool,
    profile: Option<&str>,
    discover: D,
) -> Result<Option<Resolved>, Error>
where
    D: FnOnce(Option<String>) -> Fut,
    Fut: std::future::Future<Output = Result<crate::cli::cmd::container::LocalHost, Error>>,
{
    let token = connection
        .token
        .as_ref()
        .map(token_source::read)
        .transpose()?;
    if let Some(endpoint) = connection.endpoint.clone() {
        return Ok(Some(Resolved { endpoint, token }));
    }
    if !takeover && profile.is_none() {
        return Ok(None);
    }
    let discovered = discover(
        token
            .as_ref()
            .map(|token| token.expose_secret().to_string()),
    )
    .await?;
    Ok(Some(Resolved {
        endpoint: discovered.endpoint,
        token: token.or(discovered.token_secret),
    }))
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

async fn mint_handoff(
    endpoint: &str,
    token: Option<&agent_first_data::value_source::SecretString>,
    tab_id: Option<&str>,
) -> Result<crate::sdk::TakeoverHandoffResponse, Error> {
    let mut handoff_client = Client::connect(endpoint)?;
    if let Some(token) = token {
        handoff_client = handoff_client.with_token(token.expose_secret());
    }
    handoff_client.takeover_handoff(None, tab_id).await
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
/// A cookie jar lives inside a host profile, so naming one is only meaningful
/// when there is a host profile to check it against. Without a host — a plain
/// HTTP fetch, or the inline ephemeral browser — nothing could confirm the path
/// is a profile's jar, and accepting it would let any file on disk serve as
/// one. The host-side check that a named jar is the profile's own lives in the
/// SDK's jar resolution.
fn explicit_jar_needs_a_host_profile(has_host: bool) -> Result<(), Error> {
    if has_host {
        return Ok(());
    }
    Err(Error::new(
        ErrorCode::InvalidArgument,
        "--cookie-jar/AFHTTP_COOKIE_JAR names a host profile's cookie jar, and this fetch has \
         no host profile: the jar is profile-internal, never an arbitrary path. Pass \
         --endpoint-url and --profile to use a profile's jar, or drop --cookie-jar",
    ))
}

async fn build_client(
    args: &Args,
    host: Option<&Resolved>,
    render: RenderMode,
) -> Result<Client, Error> {
    match host {
        Some(host) => host.client(),
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

// The two arguments most likely to be carrying a credential are the two whose
// parse errors used to quote them back — `--header` holds `Authorization`, and
// `--cookie` holds a session. `docs/cli.md` already promises that no domain
// error quotes a raw value, because an error event is routinely logged; these
// say what was wrong with the shape and nothing about the contents.
fn parse_header_arg(raw: &str) -> Result<(String, String), Error> {
    let (name, value) = raw.split_once(':').ok_or_else(|| {
        Error::new(
            ErrorCode::InvalidArgument,
            "--header: expected NAME:VALUE, with a colon separating them".to_string(),
        )
    })?;
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            "--header: the header name before the colon must not be empty".to_string(),
        ));
    }
    Ok((name.to_string(), value.trim_start().to_string()))
}

fn parse_cookie_arg(raw: &str) -> Result<FetchCookie, Error> {
    if !raw.contains('=') {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            "--cookie: expected Set-Cookie style NAME=VALUE, with an equals sign separating them"
                .to_string(),
        ));
    }
    let cookie = FetchCookie::parse(raw.to_string())
        .map_err(|_| {
            Error::new(
                ErrorCode::InvalidArgument,
                "--cookie: not a parseable Set-Cookie value".to_string(),
            )
        })?
        .into_owned();
    if cookie.name().trim().is_empty() {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            "--cookie: the cookie name before the equals sign must not be empty".to_string(),
        ));
    }
    Ok(cookie)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_named_cookie_jar_without_a_host_profile_is_refused() {
        let error = explicit_jar_needs_a_host_profile(false).unwrap_err();
        assert_eq!(error.error_code, ErrorCode::InvalidArgument);
        assert!(
            error.detail.contains("profile-internal"),
            "{}",
            error.detail
        );
        assert!(explicit_jar_needs_a_host_profile(true).is_ok());
    }

    /// A resolved token is a `SecretString`, so a test that asserts on one has
    /// to say so — the same friction production code gets.
    fn exposed(token: &Option<agent_first_data::value_source::SecretString>) -> Option<&str> {
        token
            .as_ref()
            .map(agent_first_data::value_source::SecretString::expose_secret)
    }

    fn discovered(token_secret: Option<&str>) -> crate::cli::cmd::container::LocalHost {
        crate::cli::cmd::container::LocalHost {
            endpoint: "ws://127.0.0.1:9222".into(),
            token_secret: token_secret.map(agent_first_data::value_source::SecretString::new),
        }
    }

    /// `docs/cli.md` promises that no domain error quotes a raw value, because
    /// error events are routinely logged and these two arguments are where a
    /// credential lives. Canary values, so a regression is unmistakable.
    #[test]
    fn a_malformed_credential_argument_is_never_quoted_back() {
        const CANARY: &str = "CANARY-SECRET-VALUE";

        let header = parse_header_arg(&format!("Authorization Bearer {CANARY}"))
            .expect_err("a header with no colon is invalid");
        assert!(
            !header.to_string().contains(CANARY),
            "header error leaked its value: {header}"
        );

        let empty_name =
            parse_header_arg(&format!(": {CANARY}")).expect_err("a header with no name is invalid");
        assert!(!empty_name.to_string().contains(CANARY));

        let engine_env = crate::cli::cmd::host::parse_engine_env(&format!("PROXY{CANARY}"))
            .expect_err("an engine env with no equals sign is invalid");
        assert!(
            !engine_env.to_string().contains(CANARY),
            "engine env error leaked its value: {engine_env}"
        );

        let cookie = parse_cookie_arg(&format!("session{CANARY}"))
            .expect_err("a cookie with no equals sign is invalid");
        assert!(
            !cookie.to_string().contains(CANARY),
            "cookie error leaked its value: {cookie}"
        );

        let empty_cookie =
            parse_cookie_arg(&format!("={CANARY}")).expect_err("a cookie with no name is invalid");
        assert!(!empty_cookie.to_string().contains(CANARY));
    }

    /// The unreached case: a plain fetch has no host, which is what keeps the
    /// inline ephemeral browser the default rather than a container.
    #[tokio::test]
    async fn a_plain_fetch_needs_no_host_and_discovers_nothing() {
        let host = prepare_host(&Connection::default(), false, None, |_| async {
            panic!("a plain fetch must not discover a host");
        })
        .await
        .expect("no host is not an error");
        assert!(host.is_none());
    }

    #[tokio::test]
    async fn autodiscovery_fills_missing_endpoint_and_token() {
        let host = prepare_host(
            &Connection::default(),
            true,
            Some("contabo.com"),
            |token| async move {
                assert_eq!(token, None);
                Ok(discovered(Some("secret")))
            },
        )
        .await
        .expect("discovery succeeds")
        .expect("takeover needs a host");

        assert_eq!(host.endpoint, "ws://127.0.0.1:9222");
        assert_eq!(exposed(&host.token), Some("secret"));
    }

    /// A profile switches the host's active browser state, so it needs a host
    /// just as a takeover does — this used to be an error telling the caller to
    /// pass `--endpoint-url`.
    #[tokio::test]
    async fn a_profile_alone_discovers_a_host() {
        let host = prepare_host(&Connection::default(), false, Some("work"), |_| async {
            Ok(discovered(Some("secret")))
        })
        .await
        .expect("discovery succeeds")
        .expect("a profile needs a host");
        assert_eq!(host.endpoint, "ws://127.0.0.1:9222");
    }

    #[tokio::test]
    async fn autodiscovery_preserves_an_explicit_token() {
        let connection = Connection::new(
            None,
            Some(agent_first_data::ValueSource::Literal("argv-token".into())),
        );
        let host = prepare_host(&connection, true, Some("contabo.com"), |token| async move {
            assert_eq!(token.as_deref(), Some("argv-token"));
            Ok(discovered(Some("container-token")))
        })
        .await
        .expect("discovery succeeds")
        .expect("takeover needs a host");

        assert_eq!(host.endpoint, "ws://127.0.0.1:9222");
        assert_eq!(exposed(&host.token), Some("argv-token"));
    }

    #[tokio::test]
    async fn autodiscovery_surfaces_failure() {
        let err = prepare_host(
            &Connection::default(),
            true,
            Some("contabo.com"),
            |_| async {
                Err(Error::new(
                    ErrorCode::InvalidArgument,
                    "default local container `afhttp-host` is not running",
                ))
            },
        )
        .await
        .expect_err("a missing container is an error");

        assert_eq!(err.error_code, ErrorCode::InvalidArgument);
        assert!(err.detail.contains("afhttp-host"));
    }

    /// A named endpoint is used as given, and nothing is discovered behind it.
    #[tokio::test]
    async fn a_named_endpoint_is_never_second_guessed() {
        let connection = Connection::new(Some("ws://box:9222".into()), None);
        let host = prepare_host(&connection, true, Some("contabo.com"), |_| async {
            panic!("an explicit endpoint must not discover");
        })
        .await
        .expect("explicit host resolves")
        .expect("takeover needs a host");
        assert_eq!(host.endpoint, "ws://box:9222");
        assert!(host.token.is_none());
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

/// Handoff failure is independent of capture success. Keep the artifacts and
/// tab available for inspection even when the display is unavailable.
fn retain_after_handoff_failure(result: &mut crate::sdk::fetch::FetchResult, error: Error) {
    result.warnings.push(crate::sdk::fetch::result::Warning {
        artifact: Artifact::RenderedHtml,
        code: error.error_code,
        detail: format!("takeover URL unavailable; tab_id and captured artifacts retained for manual review: {}", error.detail),
    });
}

#[cfg(test)]
mod handoff_failure_tests {
    use super::*;
    #[test]
    fn failed_handoff_retains_tab_artifacts_and_review_action() {
        use crate::sdk::fetch::result::{FetchResult, NextAction, Trace};
        let mut result = FetchResult::new(
            crate::shared::ids::RequestId("request".into()),
            "https://example.test/".into(),
            Trace::default(),
        );
        result.tab_id = Some(TabId::new("retained"));
        result.body_file = Some(PathBuf::from("body.html"));
        result.next_action = Some(NextAction::human_takeover(&result.request_url));
        result.prepare_takeover_retry(None, Some("work"));
        retain_after_handoff_failure(
            &mut result,
            Error::new(ErrorCode::CdpUnavailable, "browser process exited"),
        );
        assert_eq!(result.tab_id.as_ref().unwrap().as_str(), "retained");
        assert!(result.body_file.is_some());
        let next = result.next_action.unwrap();
        assert!(next.takeover_url_secret.is_none());
        assert!(next.recommended_command.contains("--tab retained"));
        assert!(result.warnings[0].detail.contains("browser process exited"));
    }
}
