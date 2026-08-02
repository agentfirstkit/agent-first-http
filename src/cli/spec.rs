//! The closed-world `cli-spec-v1` registry for `afhttp`.
//!
//! One registry is the single source for argv parsing, typed invocation values,
//! which argument mixes are legal, output contracts, `--help`, and `docs/cli.md`
//! (rendered by `--docs`). Anything the parser can decide is expressed here as a
//! shape rather than re-checked by a handler.

use agent_first_data::{
    ArgSpec, BuiltCliSpec, CliSpec, CliSpecError, Combination, CommandSpec, OutputSpec,
    build_afdata_cli,
};

use crate::cli::cmd::container::{DEFAULT_CONTAINER_NAME, DEFAULT_CONTAINER_PORT};
use crate::sdk::fetch::DEFAULT_NETWORK_BODY_MAX_BYTES;

/// Browser backends the inline client and the host can launch.
pub const BROWSERS: [&str; 9] = [
    "auto",
    "chromium",
    "chrome",
    "chrome-headless-shell",
    "fingerprint-chromium",
    "edge",
    "brave",
    "lightpanda",
    "camoufox",
];

/// Artifact tokens `fetch --want` accepts, in `shared::artifacts::Artifact`
/// order. A `fetch` unit test proves every one of them maps to an artifact.
pub const ARTIFACTS: [&str; 10] = [
    "body",
    "rendered_html",
    "text",
    "content",
    "content_json",
    "screenshot",
    "network",
    "console",
    "observation",
    "storage",
];

/// Optional image components `container install --with` accepts. A `container`
/// unit test proves every one of them maps to a Dockerfile build arg.
pub const CONTAINER_COMPONENTS: [&str; 7] = [
    "chrome-headless-shell",
    "lightpanda",
    "fingerprint-chromium",
    "camoufox",
    "chrome",
    "brave",
    "kasmvnc",
];

/// Container runtimes `--runtime` accepts. Omitted means auto-detection.
pub const CONTAINER_RUNTIMES: [&str; 3] = ["docker", "podman", "apple"];

const RENDER_MODES: [&str; 3] = ["none", "auto", "always"];
/// Render modes that reach a real browser. Human takeover has nothing to take
/// over without one, so the takeover shapes are fixed to these.
const TAKEOVER_RENDER_MODES: [&str; 2] = ["auto", "always"];
const NETWORK_BODIES: [&str; 3] = ["off", "xhr", "all"];
const DISPLAY_MODES: [&str; 2] = ["headless", "headful"];
const HEALTH_PUBLIC: [&str; 2] = ["off", "minimal"];
const TAKEOVER_PROVIDERS: [&str; 2] = ["off", "kasmvnc"];

/// The named agents, and the fan-out value that is not one of them.
const AGENTS: [&str; 4] = ["codex", "claude-code", "opencode", "hermes"];
const EVERY_AGENT: &str = "all";

/// Command results. Every `afhttp` command answers in AFDATA protocol JSON —
/// there is no human rendering of a fetch — so the format set is exactly that.
fn protocol_output() -> OutputSpec {
    OutputSpec::protocol_finite(["json"], ["split", "stdout", "stderr"], "json", "split")
        .file_sinks(["stdout", "stderr"])
}

/// `afhttp host` announces `host_ready` and then keeps serving, so a later
/// failure is a second event on the same run rather than a new invocation.
/// That is a stream, and a stream is one ordered channel by default.
fn stream_output() -> OutputSpec {
    OutputSpec::protocol_stream(["json"], ["split", "stdout", "stderr"], "json", "stdout")
        .file_sinks(["stdout", "stderr"])
}

/// Bytes the container runtime wrote, forwarded unchanged.
fn raw_output() -> OutputSpec {
    OutputSpec::raw().file_sinks(["stdout", "stderr"])
}

/// `--help`, `--version` and `--docs`. Unlike a command result these are read
/// by people as often as by agents, so they keep the full format set.
fn lifecycle_output() -> OutputSpec {
    OutputSpec::protocol_finite(
        ["json", "yaml", "plain"],
        ["split", "stdout", "stderr"],
        "json",
        "split",
    )
    .file_sinks(["stdout", "stderr"])
}

/// Build the whole `afhttp` CLI registry.
pub fn cli_spec() -> Result<BuiltCliSpec, CliSpecError> {
    let mut spec = CliSpec::new("afhttp", env!("CARGO_PKG_VERSION"))
        .about(env!("CARGO_PKG_DESCRIPTION"))
        .display_name(env!("DISPLAY_NAME"))
        .lifecycle_output(lifecycle_output())
        .command(CommandSpec::root())
        .command(fetch_command())
        .command(host_command())
        .command(upload_command())
        .command(cdp_command())
        .command(panel_command())
        .command(endpoint_query_command(
            "health",
            "health",
            "Query a host's /health endpoint.",
        ))
        .command(endpoint_query_command(
            "capabilities",
            "capabilities",
            "Query a host's /capabilities endpoint.",
        ))
        .command(
            CommandSpec::new(["profile"])
                .about("Inspect and delete the on-disk browser profiles this machine owns."),
        )
        .command(profile_list_command())
        .command(profile_info_command(
            "info",
            "profile_info",
            "Show one profile's size, last use, and lock state.",
        ))
        .command(profile_info_command(
            "lock-status",
            "profile_lock_status",
            "Report whether a profile is currently locked by a running host.",
        ))
        .command(profile_info_command(
            "downloads",
            "profile_downloads",
            "List files captured in a profile's browser download directory.",
        ))
        .command(profile_info_command(
            "cookies",
            "profile_cookies",
            "Show the non-expired cookies in a profile's jar, with values redacted.",
        ))
        .command(profile_delete_command())
        .command(profile_prune_command())
        .command(
            CommandSpec::new(["tabs"]).about("List and close the CDP targets attached to a host."),
        )
        .command(tabs_list_command())
        .command(tabs_close_command())
        .command(CommandSpec::new(["skill"]).about(
            "Manage the Agent-First HTTP skill for Codex, Claude Code, opencode, and Hermes.",
        ))
        .command(skill_command(
            "status",
            "Show whether the Agent-First HTTP skill is installed, valid, and up to date.",
            false,
        ))
        .command(skill_command(
            "install",
            "Install or refresh the Agent-First HTTP skill.",
            true,
        ))
        .command(skill_command(
            "uninstall",
            "Remove an afhttp-managed Agent-First HTTP skill.",
            true,
        ))
        .command(CommandSpec::new(["container"]).about(
            "Build and run a long-lived host container from the embedded recipe (Docker, Podman, or Apple).",
        ))
        .command(container_install_command())
        .command(container_uninstall_command())
        .command(container_status_command())
        .command(container_logs_command());
    // Absent from a source tarball with no reachable .git, and the version
    // payload omits it rather than reporting the literal "unknown".
    if let Some(build) = Some(env!("GIT_SHA")).filter(|sha| *sha != "unknown") {
        spec = spec.build_id(build);
    }
    build_afdata_cli(spec)
}

// ── shared arguments ────────────────────────────────────────────────────────

fn endpoint_arg() -> ArgSpec {
    ArgSpec::option("--endpoint-url", "URL")
        .about("CDP endpoint of a running host; falls back to AFHTTP_ENDPOINT_URL")
}

fn token_arg() -> ArgSpec {
    ArgSpec::option("--token-secret", "TOKEN")
        .about("Bearer token for a protected host; falls back to AFHTTP_TOKEN_SECRET")
}

fn profile_root_arg() -> ArgSpec {
    ArgSpec::option("--profile-root", "DIR")
        .about("Profiles root directory; defaults to $XDG_DATA_HOME/afhttp/profiles")
}

fn backend_arg() -> ArgSpec {
    ArgSpec::option("--backend", "BACKEND")
        .about("Browser backend scope; required when one profile name exists under several")
}

fn runtime_arg() -> ArgSpec {
    ArgSpec::option_enum("--runtime", CONTAINER_RUNTIMES)
        .value_name("RUNTIME")
        .about("Container runtime; auto-detected when omitted, then AFHTTP_CONTAINER_RUNTIME")
}

fn container_name_arg() -> ArgSpec {
    ArgSpec::option("--name", "NAME")
        .default(DEFAULT_CONTAINER_NAME)
        .about("Container name")
}

fn reveal_token_arg() -> ArgSpec {
    ArgSpec::flag("--reveal-token-secret")
        .about("Include the long-lived host token in the result instead of redacting it")
}

// ── fetch ───────────────────────────────────────────────────────────────────

fn fetch_command() -> CommandSpec {
    CommandSpec::new(["fetch"])
        .about("Fetch a URL over HTTP or a real browser and write the requested artifacts.")
        .arg(ArgSpec::positional("url", 0, "URL").about("URL to fetch"))
        .arg(endpoint_arg())
        .arg(token_arg())
        .arg(
            ArgSpec::option_enum("--browser", BROWSERS)
                .value_name("BACKEND")
                .default("auto")
                .about("Browser backend for the inline host; ignored when --endpoint-url is set"),
        )
        .arg(
            ArgSpec::option("--browser-bin", "PATH")
                .about("Browser binary for the inline host when auto-discovery cannot find one"),
        )
        .arg(
            ArgSpec::option_enum("--render", RENDER_MODES)
                .value_name("MODE")
                .default("auto")
                .about(
                    "Render strategy: none is the HTTP fast path, auto escalates on failure, \
                     always uses the browser",
                ),
        )
        .arg(
            ArgSpec::option("--tab", "new|<id>")
                .default("new")
                .about("new allocates a temporary target; a CDP target id reuses and keeps it"),
        )
        .arg(
            ArgSpec::flag("--takeover")
                .about("Escalate captcha, login, or 2FA walls to human takeover"),
        )
        .arg(ArgSpec::option("--profile", "NAME").about(
            "Host profile to switch to for this fetch; defaults to the URL's registrable \
             domain under --takeover",
        ))
        .arg(ArgSpec::option("--wait", "SPEC").default("auto").about(
            "Readiness signal: auto | load | idle | selector:<css> | \
                     selector-visible:<css> | ms:<n>",
        ))
        .arg(
            ArgSpec::option("--header", "NAME:VALUE")
                .repeatable()
                .about("Request header, as Name:value"),
        )
        .arg(
            ArgSpec::option("--cookie", "NAME=VALUE")
                .repeatable()
                .about("Request cookie, in Set-Cookie syntax"),
        )
        .arg(
            ArgSpec::option("--user-agent", "UA")
                .about("Override the User-Agent header for this fetch"),
        )
        .arg(
            ArgSpec::option("--evaluate-after-wait", "JS")
                .repeatable()
                .about("JavaScript evaluated in page context after readiness"),
        )
        .arg(
            ArgSpec::option_enum("--want", ARTIFACTS)
                .value_name("ARTIFACT")
                .repeatable()
                .about(
                    "Artifact to capture; defaults to body on the HTTP path and the browser \
                     set when rendering. storage is sensitive and opt-in",
                ),
        )
        .arg(
            ArgSpec::option("--method", "METHOD")
                .default("GET")
                .about("HTTP method"),
        )
        .arg(
            ArgSpec::option("--data", "STRING|@FILE")
                .about("Request body; @ reads the rest as a file path"),
        )
        .arg(
            ArgSpec::option("--form", "NAME=VALUE")
                .repeatable()
                .about("URL-encoded form field; sends application/x-www-form-urlencoded"),
        )
        .arg(
            ArgSpec::option_enum("--network-bodies", NETWORK_BODIES)
                .value_name("MODE")
                .default("off")
                .about("Capture network response bodies; xhr captures XHR/fetch only"),
        )
        .arg(
            ArgSpec::option_i64("--network-body-max-bytes", "BYTES")
                .default_i64(DEFAULT_NETWORK_BODY_MAX_BYTES as i64)
                .about("Per-body byte cap for captured network sub-requests"),
        )
        .arg(
            ArgSpec::option_i64("--readiness-idle-ms", "MS")
                .default_i64(800)
                .about("Network quiet window used by --wait auto"),
        )
        .arg(
            ArgSpec::option_i64("--readiness-stable-ms", "MS")
                .default_i64(500)
                .about("DOM/text unchanged window used by --wait auto"),
        )
        .arg(
            ArgSpec::option_i64("--readiness-min-text-bytes", "BYTES")
                .default_i64(32)
                .about("Low visible-text threshold that only warns about --wait auto quality"),
        )
        .arg(ArgSpec::flag("--no-network-redact").about(
            "Write raw Authorization/Cookie headers and token query parameters to network.json",
        ))
        .arg(
            ArgSpec::option("--out", "DIR")
                .about("Directory for artifacts; defaults to afhttp-out under the temp directory"),
        )
        .arg(ArgSpec::option("--cookie-jar", "PATH").about(
            "Override the profile cookie-jar path; must match the host profile. Honors \
             AFHTTP_COOKIE_JAR",
        ))
        .arg(
            ArgSpec::flag("--no-cookie-jar")
                .about("Replay no cookies and merge no Set-Cookie back; wins over --cookie-jar"),
        )
        .arg(
            ArgSpec::option_i64("--observe-main-wait-ms", "MS")
                .default_i64(500)
                .about("Maximum wait for the browser's main-document event"),
        )
        .arg(
            ArgSpec::option_i64("--max-response-bytes", "BYTES")
                .default_i64(1_073_741_824)
                .about("Maximum main HTTP-path response body size; 0 disables the cap"),
        )
        .arg(
            ArgSpec::option_i64("--retry", "N")
                .default_i64(0)
                .about("Retry attempts after the first, for retryable errors only"),
        )
        .arg(
            ArgSpec::option_i64("--backoff-ms", "MS")
                .default_i64(250)
                .about("Fixed delay between retries"),
        )
        .arg(
            ArgSpec::option("--proxy-url", "URL").about(
                "Upstream proxy for the HTTP fast path; ambient HTTP_PROXY is never honored",
            ),
        )
        .arg(
            ArgSpec::option("--ca-cert", "PATH")
                .about("PEM file with extra root CAs for the HTTP path"),
        )
        .arg(
            ArgSpec::flag("--tls-insecure")
                .about("Disable TLS certificate verification on the HTTP path"),
        )
        .arg(
            ArgSpec::option_i64("--timeout-ms", "MS")
                .default_i64(30_000)
                .about("Overall fetch timeout for both the HTTP and browser paths"),
        )
        .arg(
            ArgSpec::flag("--capture-ws")
                .about("Capture WebSocket frame payloads; treat the artifact as sensitive"),
        )
        .arg(
            ArgSpec::flag("--capture-sse")
                .about("Capture SSE event payloads; treat the artifact as sensitive"),
        )
        .combination(fetch_shape(
            "fetch",
            "Fetch with no request body",
            false,
            None,
        ))
        .combination(fetch_shape(
            "fetch-data",
            "Fetch with a raw request body from --data",
            false,
            Some("data"),
        ))
        .combination(fetch_shape(
            "fetch-form",
            "Fetch with URL-encoded fields from --form",
            false,
            Some("form"),
        ))
        .combination(fetch_shape(
            "fetch-takeover",
            "Hand a wall to a human, with no request body",
            true,
            None,
        ))
        .combination(fetch_shape(
            "fetch-takeover-data",
            "Hand a wall to a human, with a raw request body from --data",
            true,
            Some("data"),
        ))
        .combination(fetch_shape(
            "fetch-takeover-form",
            "Hand a wall to a human, with URL-encoded fields from --form",
            true,
            Some("form"),
        ))
}

/// Everything a fetch accepts regardless of body kind or takeover.
fn fetch_common() -> Vec<&'static str> {
    vec![
        "endpoint_url",
        "token_secret",
        "browser",
        "browser_bin",
        "tab",
        "profile",
        "wait",
        "header",
        "cookie",
        "user_agent",
        "evaluate_after_wait",
        "want",
        "method",
        "network_bodies",
        "network_body_max_bytes",
        "readiness_idle_ms",
        "readiness_stable_ms",
        "readiness_min_text_bytes",
        "no_network_redact",
        "out",
        "cookie_jar",
        "no_cookie_jar",
        "observe_main_wait_ms",
        "max_response_bytes",
        "retry",
        "backoff_ms",
        "proxy_url",
        "ca_cert",
        "tls_insecure",
        "timeout_ms",
        "capture_ws",
        "capture_sse",
    ]
}

/// One fetch shape.
///
/// Two exclusions the handler used to raise at runtime are shapes here: a
/// request body is either `--data` or `--form` and never both, and `--takeover`
/// only exists where a browser does, so the takeover shapes pin `--render` to
/// the modes that reach one.
fn fetch_shape(id: &str, about: &str, takeover: bool, body: Option<&str>) -> Combination {
    let mut optional = fetch_common();
    let mut combination = Combination::new(id)
        .action("fetch")
        .about(about)
        .required(["url"]);
    if takeover {
        combination = combination
            .required(["takeover"])
            .fixed_one_of("render", TAKEOVER_RENDER_MODES);
    } else {
        optional.push("render");
    }
    if let Some(body) = body {
        combination = combination.required([body]);
    }
    combination.optional(optional).output(protocol_output())
}

// ── host ────────────────────────────────────────────────────────────────────

fn host_command() -> CommandSpec {
    let shared = [
        "profile",
        "browser",
        "browser_bin",
        "token_secret",
        "no_health",
        "health_public",
        "engine_env",
        "browser_arg",
        "proxy_url",
        "recent_requests_cap",
    ];
    let mut headless = shared.to_vec();
    headless.push("display");
    let mut takeover = shared.to_vec();
    takeover.push("takeover_quality_percent");

    CommandSpec::new(["host"])
        .about("Run the browser host: launch a backend browser and serve /cdp for clients.")
        .arg(
            ArgSpec::option("--listen", "tcp:HOST:PORT|unix:PATH")
                .about("Listener address; a non-loopback TCP address also requires --token-secret"),
        )
        .arg(
            ArgSpec::option("--profile", "NAME")
                .default("-")
                .about("Initial profile name, or - for an ephemeral profile"),
        )
        .arg(
            ArgSpec::option_enum("--display", DISPLAY_MODES)
                .value_name("MODE")
                .about("Browser display mode"),
        )
        .arg(
            ArgSpec::option_enum("--takeover-provider", TAKEOVER_PROVIDERS)
                .value_name("PROVIDER")
                .default("off")
                .about("Real-display takeover provider served at /takeover/panel"),
        )
        .arg(
            ArgSpec::option_i64("--takeover-quality-percent", "PERCENT")
                .default_i64(100)
                .about("Takeover image quality from 0 to 100; lower trades clarity for bandwidth"),
        )
        .arg(
            ArgSpec::option_enum("--browser", BROWSERS)
                .value_name("BACKEND")
                .default("auto")
                .about("Browser backend"),
        )
        .arg(ArgSpec::option("--browser-bin", "PATH").about("Override the browser binary path"))
        .arg(token_arg())
        .arg(
            ArgSpec::flag("--no-health")
                .about("Stop serving /health and /capabilities, which are on by default"),
        )
        .arg(
            ArgSpec::option_enum("--health-public", HEALTH_PUBLIC)
                .value_name("MODE")
                .default("off")
                .about("Serve /health unauthenticated with a minimal payload"),
        )
        .arg(
            ArgSpec::option("--engine-env", "NAME=VALUE")
                .repeatable()
                .about("Environment variable for the browser subprocess; all others are scrubbed"),
        )
        .arg(
            ArgSpec::option("--browser-arg", "FLAG")
                .repeatable()
                .about("Raw backend flag appended after the host's defaults"),
        )
        .arg(
            ArgSpec::option("--proxy-url", "URL")
                .about("Upstream browser proxy; ambient HTTP_PROXY is never inherited"),
        )
        .arg(
            ArgSpec::option_i64("--recent-requests-cap", "N")
                .default_i64(0)
                .about("Enable /recent-requests with a bounded ring of N entries; 0 is off"),
        )
        .combination(
            Combination::new("host")
                .action("host")
                .about("Serve no takeover surface; --display picks headless or headful")
                .fixed("takeover_provider", "off")
                .required(["listen"])
                .optional(headless)
                .output(stream_output()),
        )
        .combination(
            Combination::new("host-takeover")
                .action("host")
                .about("Serve a KasmVNC real-display takeover, which is always headful")
                .fixed("takeover_provider", "kasmvnc")
                .required(["listen"])
                .optional(takeover)
                .output(stream_output()),
        )
}

// ── single-endpoint commands ────────────────────────────────────────────────

fn endpoint_query_command(name: &str, action: &str, about: &str) -> CommandSpec {
    CommandSpec::new([name])
        .about(about)
        .arg(endpoint_arg())
        .arg(token_arg())
        .combination(
            Combination::new(name)
                .action(action)
                .optional(["endpoint_url", "token_secret"])
                .output(protocol_output()),
        )
}

fn panel_command() -> CommandSpec {
    CommandSpec::new(["panel"])
        .about("Mint a short-lived takeover URL for a running takeover-ready host.")
        .arg(endpoint_arg())
        .arg(token_arg())
        .combination(
            Combination::new("panel")
                .action("panel")
                .optional(["endpoint_url", "token_secret"])
                .output(protocol_output()),
        )
}

fn upload_command() -> CommandSpec {
    CommandSpec::new(["upload"])
        .about("Upload a local file into a browser tab's file input via DOM.setFileInputFiles.")
        .arg(endpoint_arg())
        .arg(token_arg())
        .arg(ArgSpec::option("--tab", "TARGET_ID").about("CDP target id to operate in"))
        .arg(
            ArgSpec::option("--selector", "CSS")
                .about("CSS selector for the <input type=file> element"),
        )
        .arg(ArgSpec::option("--file", "PATH").about("Local file to upload"))
        .combination(
            Combination::new("upload")
                .action("upload")
                .required(["tab", "selector", "file"])
                .optional(["endpoint_url", "token_secret"])
                .output(protocol_output()),
        )
}

fn cdp_command() -> CommandSpec {
    CommandSpec::new(["cdp"])
        .about("Send one raw CDP method to a tab and return its result.")
        .arg(ArgSpec::positional("method", 0, "METHOD").about("CDP method name"))
        .arg(endpoint_arg())
        .arg(token_arg())
        .arg(ArgSpec::option("--tab", "TARGET_ID").about("CDP target id to drive"))
        .arg(
            ArgSpec::option("--params", "JSON|@-")
                .about("Method parameters as a JSON literal, or @- to read them from stdin"),
        )
        .arg(
            ArgSpec::option("--wait-event", "EVENT:TIMEOUT")
                .about("Wait for a CDP event before exiting, for example Page.loadEventFired:5s"),
        )
        .combination(
            Combination::new("cdp")
                .action("cdp")
                .required(["method", "tab"])
                .optional(["endpoint_url", "token_secret", "params", "wait_event"])
                .output(protocol_output()),
        )
}

// ── profile ─────────────────────────────────────────────────────────────────

fn profile_list_command() -> CommandSpec {
    CommandSpec::new(["profile", "list"])
        .about("List the on-disk profiles under the profiles root.")
        .arg(profile_root_arg())
        .combination(
            Combination::new("profile-list")
                .action("profile_list")
                .optional(["profile_root"])
                .output(protocol_output()),
        )
}

fn profile_info_command(name: &str, action: &str, about: &str) -> CommandSpec {
    CommandSpec::new(["profile", name])
        .about(about)
        .arg(ArgSpec::positional("name", 0, "NAME").about("Profile name"))
        .arg(backend_arg())
        .arg(profile_root_arg())
        .combination(
            Combination::new(format!("profile-{name}"))
                .action(action)
                .required(["name"])
                .optional(["backend", "profile_root"])
                .output(protocol_output()),
        )
}

fn profile_delete_command() -> CommandSpec {
    CommandSpec::new(["profile", "delete"])
        .about("Delete a profile and all of its on-disk state.")
        .arg(ArgSpec::positional("name", 0, "NAME").about("Profile name to delete"))
        .arg(backend_arg())
        .arg(
            ArgSpec::option("--confirm", "NAME")
                .about("Confirmation guard; must equal the profile name"),
        )
        .arg(profile_root_arg())
        .combination(
            Combination::new("profile-delete")
                .action("profile_delete")
                .required(["name", "confirm"])
                .optional(["backend", "profile_root"])
                .output(protocol_output()),
        )
}

fn profile_prune_command() -> CommandSpec {
    CommandSpec::new(["profile", "prune"])
        .about("Delete profiles whose last use is older than a cutoff.")
        .arg(
            ArgSpec::option("--older-than", "DURATION")
                .about("Age cutoff such as 30d or 12h; profiles last used before it are removed"),
        )
        .arg(
            ArgSpec::flag("--dry-run")
                .about("Report what would be deleted without deleting anything"),
        )
        .arg(profile_root_arg())
        .combination(
            Combination::new("profile-prune")
                .action("profile_prune")
                .required(["older_than"])
                .optional(["dry_run", "profile_root"])
                .output(protocol_output()),
        )
}

// ── tabs ────────────────────────────────────────────────────────────────────

fn tabs_list_command() -> CommandSpec {
    CommandSpec::new(["tabs", "list"])
        .about("List the CDP targets currently attached to a host.")
        .arg(endpoint_arg())
        .arg(token_arg())
        .combination(
            Combination::new("tabs-list")
                .action("tabs_list")
                .optional(["endpoint_url", "token_secret"])
                .output(protocol_output()),
        )
}

fn tabs_close_command() -> CommandSpec {
    CommandSpec::new(["tabs", "close"])
        .about("Close one CDP target by its target id.")
        .arg(ArgSpec::option("--tab", "TARGET_ID").about("CDP target id to close"))
        .arg(endpoint_arg())
        .arg(token_arg())
        .combination(
            Combination::new("tabs-close")
                .action("tabs_close")
                .required(["tab"])
                .optional(["endpoint_url", "token_secret"])
                .output(protocol_output()),
        )
}

// ── skill ───────────────────────────────────────────────────────────────────

/// One skill verb, as two shapes.
///
/// `--skills-dir` names a single directory, so it is meaningless when the verb
/// fans out across every agent. Registering that as two shapes rather than one
/// shape plus a runtime check means the illegal mix is rejected by the parser,
/// and both legal mixes are visible in one `--help`.
fn skill_command(verb: &str, about: &str, force: bool) -> CommandSpec {
    let mut command = CommandSpec::new(["skill", verb])
        .about(about)
        .arg(
            ArgSpec::option_enum("--agent", std::iter::once(EVERY_AGENT).chain(AGENTS))
                .value_name("AGENT")
                .default(EVERY_AGENT)
                .about("Agent to manage"),
        )
        .arg(
            ArgSpec::option_enum("--scope", ["personal", "workspace"])
                .value_name("SCOPE")
                .default("personal")
                .about("Skill scope"),
        )
        .arg(ArgSpec::option("--skills-dir", "DIR").about("Directory that contains skill folders"));

    let mut every: Vec<&str> = vec!["scope"];
    let mut named: Vec<&str> = vec!["scope", "skills_dir"];
    if force {
        command = command.arg(
            ArgSpec::flag("--force")
                .about("Overwrite or remove an Agent-First HTTP skill this tool did not manage"),
        );
        every.push("force");
        named.push("force");
    }

    command
        .combination(
            Combination::new(format!("skill-{verb}-every-agent"))
                .action(format!("skill_{verb}"))
                .about("Target every agent that supports the scope")
                .fixed("agent", EVERY_AGENT)
                .optional(every)
                .output(protocol_output()),
        )
        .combination(
            Combination::new(format!("skill-{verb}-one-agent"))
                .action(format!("skill_{verb}"))
                .about("Target one named agent; only this shape accepts --skills-dir")
                .fixed_one_of("agent", AGENTS)
                .optional(named)
                .output(protocol_output()),
        )
}

// ── container ───────────────────────────────────────────────────────────────

fn container_install_command() -> CommandSpec {
    let shared = [
        "runtime",
        "name",
        "port",
        "profile",
        "shm_size",
        "takeover_provider",
        "with",
        "reveal_token_secret",
        "host_args",
    ];
    let mut release = shared.to_vec();
    release.push("rebuild");
    let mut from_source = shared.to_vec();
    from_source.push("context");

    CommandSpec::new(["container", "install"])
        .about("Build the host image if missing, run the container, and print the client command.")
        .arg(runtime_arg())
        .arg(container_name_arg())
        .arg(
            ArgSpec::option_i64("--port", "PORT")
                .default_i64(i64::from(DEFAULT_CONTAINER_PORT))
                .about("Host CDP port, published on 127.0.0.1"),
        )
        .arg(
            ArgSpec::option("--profile", "NAME")
                .about("Initial profile inside the container; defaults to - for ephemeral"),
        )
        .arg(
            ArgSpec::option("--shm-size", "SIZE")
                .about("Chromium /dev/shm size; defaults to 1g, or 2g when takeover is on"),
        )
        .arg(
            ArgSpec::option_enum("--takeover-provider", TAKEOVER_PROVIDERS)
                .value_name("PROVIDER")
                .default("kasmvnc")
                .about("Takeover provider for the built host; off builds a lean headless host"),
        )
        .arg(
            ArgSpec::option_enum("--with", CONTAINER_COMPONENTS)
                .value_name("COMPONENT")
                .repeatable()
                .about("Extra image component to build"),
        )
        .arg(ArgSpec::flag("--rebuild").about("Rebuild the image even if it already exists"))
        .arg(
            ArgSpec::flag("--from-source")
                .about("Compile the image from a source checkout instead of a prebuilt release"),
        )
        .arg(ArgSpec::option("--context", "DIR").about(
            "Source checkout for --from-source; defaults to the current directory, then this \
             binary's own checkout",
        ))
        .arg(reveal_token_arg())
        .arg(
            ArgSpec::positional("host_args", 0, "HOST_ARG")
                .repeatable()
                .about("Arguments forwarded to `afhttp host` inside the container, after --"),
        )
        .combination(
            Combination::new("container-install")
                .action("container_install")
                .about("Build from the release pinned to this binary's version")
                .optional(release)
                .output(protocol_output()),
        )
        .combination(
            Combination::new("container-install-from-source")
                .action("container_install")
                .about("Compile from a source checkout, which always rebuilds")
                .required(["from_source"])
                .optional(from_source)
                .output(protocol_output()),
        )
}

fn container_uninstall_command() -> CommandSpec {
    CommandSpec::new(["container", "uninstall"])
        .about("Stop and remove the container.")
        .arg(runtime_arg())
        .arg(container_name_arg())
        .arg(ArgSpec::flag("--purge").about("Also remove the built image and the cached context"))
        .combination(
            Combination::new("container-uninstall")
                .action("container_uninstall")
                .optional(["runtime", "name", "purge"])
                .output(protocol_output()),
        )
}

fn container_status_command() -> CommandSpec {
    CommandSpec::new(["container", "status"])
        .about("Report whether the host is running, with its endpoint and client command.")
        .arg(runtime_arg())
        .arg(container_name_arg())
        .arg(
            ArgSpec::option_i64("--port", "PORT")
                .default_i64(i64::from(DEFAULT_CONTAINER_PORT))
                .about("Published host port, used to format the endpoint and client command"),
        )
        .arg(reveal_token_arg())
        .combination(
            Combination::new("container-status")
                .action("container_status")
                .optional(["runtime", "name", "port", "reveal_token_secret"])
                .output(protocol_output()),
        )
}

/// `container logs` in its two lifecycles.
///
/// The summary is one protocol event about a captured file. Following the
/// runtime's own log stream is neither finite nor protocol-shaped, so it is a
/// separate shape with a raw output contract — which is also why `--follow`
/// without `--raw` is rejected by the parser instead of by the handler.
fn container_logs_command() -> CommandSpec {
    CommandSpec::new(["container", "logs"])
        .about("Capture the container logs, or stream them raw.")
        .arg(runtime_arg())
        .arg(container_name_arg())
        .arg(ArgSpec::flag("--follow").about("Keep following the log output"))
        .arg(
            ArgSpec::flag("--raw")
                .about("Stream raw runtime logs instead of returning a JSON summary"),
        )
        .combination(
            Combination::new("container-logs")
                .action("container_logs")
                .about("Capture the logs to a file and return a JSON summary of the tail")
                .optional(["runtime", "name"])
                .output(protocol_output()),
        )
        .combination(
            Combination::new("container-logs-raw")
                .action("container_logs")
                .about("Forward the runtime's own log bytes; the only shape that can follow")
                .required(["raw"])
                .optional(["runtime", "name", "follow"])
                .output(raw_output()),
        )
}

#[cfg(test)]
mod tests {
    use agent_first_data::{CliErrorRule, CliOutcome};

    use super::*;

    fn built() -> BuiltCliSpec {
        match cli_spec() {
            Ok(cli) => cli,
            Err(error) => panic!("registry must build: {error}"),
        }
    }

    #[test]
    fn registry_builds_and_every_shape_is_reachable() {
        let cli = built();
        // Each generated argv must resolve back to the shape it came from, so
        // an overlapping or unreachable combination fails here rather than at a
        // caller's first invocation.
        let synthetics = cli.synthetic_invocations();
        assert!(!synthetics.is_empty(), "the registry generated no fixtures");
        for synthetic in synthetics {
            let argv = synthetic.argv.clone();
            match cli.resolve_from(argv.clone()) {
                Ok(CliOutcome::Run(invocation)) => assert_eq!(
                    invocation.combination_id(),
                    synthetic.combination_id,
                    "{argv:?} resolved to the wrong shape"
                ),
                Ok(_) => panic!("{argv:?} did not resolve to a run"),
                Err(error) => panic!("{argv:?} failed to resolve: {}", error.message),
            }
        }
    }

    #[test]
    fn takeover_rejects_the_render_mode_that_has_no_browser() {
        let cli = built();
        let error = match cli.resolve_from([
            "afhttp",
            "fetch",
            "https://example.com",
            "--takeover",
            "--render",
            "none",
        ]) {
            Err(error) => error,
            Ok(_) => panic!("--takeover --render none must be rejected"),
        };
        assert_eq!(error.rule, CliErrorRule::UnregisteredCombination);
    }

    #[test]
    fn a_request_body_is_data_or_form_but_never_both() {
        let cli = built();
        let error = match cli.resolve_from([
            "afhttp",
            "fetch",
            "https://example.com",
            "--data",
            "x",
            "--form",
            "a=b",
        ]) {
            Err(error) => error,
            Ok(_) => panic!("--data with --form must be rejected"),
        };
        assert_eq!(error.rule, CliErrorRule::UnregisteredCombination);
    }

    #[test]
    fn following_logs_needs_the_raw_shape() {
        let cli = built();
        let error = match cli.resolve_from(["afhttp", "container", "logs", "--follow"]) {
            Err(error) => error,
            Ok(_) => panic!("--follow without --raw must be rejected"),
        };
        assert_eq!(error.rule, CliErrorRule::UnregisteredCombination);
    }

    #[test]
    fn a_takeover_host_cannot_be_asked_for_a_headless_display() {
        let cli = built();
        let error = match cli.resolve_from([
            "afhttp",
            "host",
            "--listen",
            "tcp:127.0.0.1:9222",
            "--takeover-provider",
            "kasmvnc",
            "--display",
            "headless",
        ]) {
            Err(error) => error,
            Ok(_) => panic!("a headless takeover host must be rejected"),
        };
        assert_eq!(error.rule, CliErrorRule::UnregisteredCombination);
    }

    #[test]
    fn skills_dir_requires_one_named_agent() {
        let cli = built();
        let error =
            match cli.resolve_from(["afhttp", "skill", "install", "--skills-dir", "/tmp/skills"]) {
                Err(error) => error,
                Ok(_) => panic!("--skills-dir without an explicit --agent must be rejected"),
            };
        assert_eq!(error.rule, CliErrorRule::UnregisteredCombination);
    }

    /// The whole registry as one stable text surface: every command path, every
    /// argument with its spelling, closed value set and default, and every
    /// shape with the action it runs.
    fn surface(cli: &BuiltCliSpec) -> String {
        let mut out = String::new();
        for command in &cli.spec().commands {
            let path = if command.command_path.is_empty() {
                "afhttp".to_string()
            } else {
                command.command_path.join(" ")
            };
            out.push_str(&format!("command {path}\n"));
            for argument in &command.arguments {
                let syntax = match &argument.syntax {
                    agent_first_data::ArgSyntax::Long { name } => name.clone(),
                    agent_first_data::ArgSyntax::Positional { index } => {
                        format!("<positional {index}>")
                    }
                };
                out.push_str(&format!("  arg {} {syntax}", argument.argument_id));
                if !argument.enum_values.is_empty() {
                    out.push_str(&format!(" <{}>", argument.enum_values.join("|")));
                }
                if let Some(default) = &argument.default {
                    out.push_str(&format!(" ={}", plain(default)));
                }
                if argument.repeatable {
                    out.push_str(" ...");
                }
                out.push('\n');
            }
            for combination in &command.combinations {
                out.push_str(&format!(
                    "  shape {} -> {}\n",
                    combination.combination_id, combination.action_id
                ));
            }
        }
        out
    }

    fn plain(value: &agent_first_data::CliValue) -> String {
        value
            .as_str()
            .map(str::to_string)
            .or_else(|| value.as_i64().map(|value| value.to_string()))
            .unwrap_or_default()
    }

    #[test]
    fn command_flag_snapshot_matches() {
        assert_eq!(
            surface(&built()),
            include_str!("../../tests/golden/cli-command-flags.txt")
        );
    }

    #[test]
    fn cli_contract_has_no_legacy_aliases() {
        let surface = surface(&built());
        for forbidden in [
            "command download\n",
            "command takeover\n",
            "command hard-site\n",
            "--profile-name",
            concat!("profile", "_name"),
            concat!("?", "profile="),
            "arg timeout --timeout\n",
            "arg health --health\n",
            "arg network_redact --network-redact\n",
            "arg takeover_quality --takeover-quality\n",
            "chrome_shell",
            "fingerprint_chromium",
            "legacy",
        ] {
            assert!(
                !surface.contains(forbidden),
                "CLI contract retained forbidden legacy surface {forbidden:?}: {surface}"
            );
        }
    }
}
