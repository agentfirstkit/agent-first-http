//! `afhttp ui takeover` subcommand. Publishes the takeover panel as a UI
//! session and blocks until that session ends.
//!
//! `panel` and `fetch --takeover` hand back a URL and exit; whether anyone ever
//! opened it, and whether they are finished, is left for the agent to guess.
//! This command is the other half: it holds the session open and returns when
//! it ends, which for a takeover is the only ending there is. A VNC canvas
//! carries no submit control, so there is nothing for the person to confirm and
//! no typed result to collect.
//!
//! Two deliveries of that one session, because the person is not always at this
//! machine. A window is the local one: it opens here, and closing it is the
//! ending. `--takeover-no-window` is the remote one: the panel is announced to
//! the cross-process session registry so `afui session list` can see it and
//! `afui session serve` can frame it on a phone, and the ending is the
//! credential running out or this command being stopped. The panel is announced
//! either way — a person at this machine may still want it on their phone.
//!
//! The credential in that panel URL is the host's, not AFUI's. AFUI is told
//! where the panel is and lists it; afhttp minted the credential, afhttp's TTL
//! governs it, and afhttp revokes it on a profile switch or host shutdown.
//! Ending this command withdraws the listing and nothing else.

use std::time::{Duration, Instant};

use agent_first_ui::{UiUpstream, UiWindow, UiWindowConfig};
use serde::Serialize;

use crate::cli::connect::Connection;
use crate::cli::output;
use crate::shared::error::{Error, ErrorCode};

#[derive(Debug)]
pub struct Args {
    pub sub: UiSub,
    pub delivery: Delivery,
}

#[derive(Debug)]
pub enum UiSub {
    Takeover(TakeoverArgs),
}

/// How the panel is put in front of a person.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// An isolated window on this machine. Closing it ends the session.
    Window,
    /// No window: the session is left in the registry for a shell to frame,
    /// and this command waits.
    Listed,
}

#[derive(Debug)]
pub enum TakeoverArgs {
    /// Mint a fresh credential from a running host, then open its panel.
    Mint { connection: Connection },
    /// Open a panel URL an earlier `panel` or `fetch --takeover` already
    /// minted. The credential is in the URL, so no host call is needed.
    Open { takeover_url_secret: String },
}

/// Emitted before the session blocks.
///
/// `session_id` is absent exactly when the panel could not be announced, which
/// only a window delivery survives. Absence is the signal: a window is open, and
/// nothing else can see it.
#[derive(Serialize)]
struct UiReady {
    /// Where the panel is served, credential-free — the secret stays out of
    /// every event this command emits.
    panel_url: String,
    /// No submit control on a VNC canvas: the person watches and acts, and the
    /// session ends without a result.
    session: &'static str,
    /// How this session is being put in front of a person.
    delivery: &'static str,
    /// What `afui session list` calls this panel, so an agent can point a
    /// person at the right one without ever handling the credential.
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    takeover_url_expires_at_rfc3339: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    takeover_url_ttl_s: Option<u64>,
}

/// The terminal event: the session is over, so the person is done with it.
#[derive(Serialize)]
struct UiTakeoverResult {
    panel_url: String,
    session: &'static str,
    delivery: &'static str,
    outcome: &'static str,
    open_s: u64,
}

const SESSION_KIND: &str = "watch";

/// The Provider and UI identifiers this panel is listed under. `takeover` is
/// the `ui_kind` an AFUI frontend override would key on, if a panel afhttp did
/// not write were ever overridable.
const PROVIDER_ID: &str = "afhttp";
const UI_KIND: &str = "takeover";

pub async fn run(args: Args) -> Result<(), Error> {
    match args.sub {
        UiSub::Takeover(takeover) => takeover_run(takeover, args.delivery).await,
    }
}

async fn takeover_run(args: TakeoverArgs, delivery: Delivery) -> Result<(), Error> {
    let panel = resolve_panel(args).await?;
    let panel_url = credential_free_panel_url(&panel.url)?;

    // Announced before the wait starts, for both deliveries: a window is where
    // the person at this machine looks, and the registry is where a person
    // holding a phone looks. Held for exactly as long as this command runs —
    // the listing is withdrawn on the way out of this function, and pruned by
    // whoever reads the registry next if this process dies instead.
    //
    // Failing to announce is fatal for one delivery and not the other, because
    // the deliveries do not depend on it equally: without a window the listing
    // *is* the delivery, and with one the panel is already in front of the
    // person. So a window opens anyway — and says so by carrying no
    // `session_id`, which is the difference an agent can see.
    let announced = UiUpstream::new(PROVIDER_ID, UI_KIND, &panel.url)
        .and_then(|upstream| upstream.with_subject(&panel_url).announce());
    let announced = match (announced, delivery) {
        (Ok(announced), _) => Some(announced),
        (Err(error), Delivery::Listed) => {
            return Err(Error::new(
                ErrorCode::InternalError,
                format!(
                    "could not publish the takeover panel as a UI session: {error}. \
                     Without a window there is nothing else to deliver it with."
                ),
            ));
        }
        (Err(_), Delivery::Window) => None,
    };

    output::emit_progress(
        "ui_takeover",
        &UiReady {
            panel_url: panel_url.clone(),
            session: SESSION_KIND,
            delivery: delivery.as_str(),
            session_id: announced
                .as_ref()
                .map(|announced| announced.metadata().session_id.to_string()),
            takeover_url_expires_at_rfc3339: panel.expires_at_rfc3339,
            takeover_url_ttl_s: panel.ttl_s,
        },
    )?;

    let opened_at = Instant::now();
    let outcome = match delivery {
        Delivery::Window => wait_on_window(&panel.url).await?,
        Delivery::Listed => wait_while_listed(panel.ttl_s).await,
    };
    drop(announced);

    output::emit(
        "ui_takeover",
        &UiTakeoverResult {
            panel_url,
            session: SESSION_KIND,
            delivery: delivery.as_str(),
            outcome,
            open_s: opened_at.elapsed().as_secs(),
        },
    )
}

impl Delivery {
    fn as_str(self) -> &'static str {
        match self {
            Self::Window => "window",
            Self::Listed => "listed",
        }
    }
}

/// Open the panel here and wait for the person to close it.
async fn wait_on_window(url_secret: &str) -> Result<&'static str, Error> {
    // No window is the same failure as no browser: the machine running the
    // agent has nowhere to show a person the panel. That is the case
    // `--takeover-no-window` exists for, so the error says so.
    let mut window = UiWindow::launch(url_secret, &UiWindowConfig::default()).map_err(|error| {
        Error::new(
            ErrorCode::BrowserLaunchFailed,
            format!(
                "could not open a takeover window: {error}. \
                 This delivery needs a display and a Chromium-family browser on this machine; \
                 use `--takeover-no-window` to hand the panel to a phone through \
                 `afui session serve`, or `afhttp panel` to hand the URL to someone else."
            ),
        )
    })?;
    window.wait_closed().await.map_err(|error| {
        Error::new(
            ErrorCode::InternalError,
            format!("waiting on the takeover window: {error}"),
        )
    })?;
    Ok("closed")
}

/// Hold the session open for whoever is framing it, and say how it ended.
///
/// A panel with no window here has no close event to wait on, so the two
/// endings left are the ones that are still real: the credential afhttp minted
/// runs out, or the agent stops waiting. Both bound the session by this
/// process, which is what §4.6 asks — there is no page left hanging either way,
/// because the listing goes when this returns.
///
/// A panel opened from an already-minted URL has no lifetime this command
/// knows, so only the second ending applies.
async fn wait_while_listed(ttl_s: Option<u64>) -> &'static str {
    let stopped = stop_requested();
    match ttl_s {
        Some(ttl_s) => {
            let expiry = tokio::time::sleep(Duration::from_secs(ttl_s));
            tokio::select! {
                () = expiry => "expired",
                () = stopped => "stopped",
            }
        }
        None => {
            stopped.await;
            "stopped"
        }
    }
}

/// Resolves when the agent asks this command to stop.
async fn stop_requested() {
    let interrupt = async {
        let _ignored = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = interrupt => {},
        () = terminate => {},
    }
}

/// A panel URL and, when this run minted it, what the host said about its
/// lifetime. Nothing here is emitted except through `credential_free_panel_url`.
struct Panel {
    url: String,
    expires_at_rfc3339: Option<String>,
    ttl_s: Option<u64>,
}

async fn resolve_panel(args: TakeoverArgs) -> Result<Panel, Error> {
    match args {
        TakeoverArgs::Open {
            takeover_url_secret,
        } => Ok(Panel {
            url: takeover_url_secret,
            expires_at_rfc3339: None,
            ttl_s: None,
        }),
        TakeoverArgs::Mint { connection } => {
            let client = connection.client().await?;
            let handoff = client.takeover_handoff(None, None).await?;
            Ok(Panel {
                url: handoff.takeover_url_secret,
                expires_at_rfc3339: Some(handoff.takeover_url_expires_at_rfc3339),
                ttl_s: Some(handoff.takeover_url_ttl_s),
            })
        }
    }
}

/// The panel without its query, which is where the credential lives. Every
/// event this command emits names the panel this way, so the secret reaches the
/// window and nothing else. Same spelling as `capabilities.takeover.panel_url`,
/// which is credential-free for the same reason.
fn credential_free_panel_url(url: &str) -> Result<String, Error> {
    let mut panel = url::Url::parse(url).map_err(|error| {
        Error::new(
            ErrorCode::InvalidEndpoint,
            format!("--takeover-url-secret: {url:?} is not a URL: {error}"),
        )
    })?;
    panel.set_query(None);
    panel.set_fragment(None);
    Ok(panel.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reported_panel_url_drops_the_credential() {
        let panel = credential_free_panel_url(
            "http://127.0.0.1:9222/takeover/panel?handoff_secret=deadbeef#frag",
        )
        .unwrap();
        assert_eq!(panel, "http://127.0.0.1:9222/takeover/panel");
    }

    #[test]
    fn a_non_url_panel_is_an_argument_error_not_a_launch() {
        let error = credential_free_panel_url("not a url").unwrap_err();
        assert_eq!(error.error_code, ErrorCode::InvalidEndpoint);
    }

    #[test]
    fn neither_emitted_event_carries_the_secret() {
        const SECRET: &str = "0123456789abcdef";
        let url = format!("http://127.0.0.1:9222/takeover/panel?handoff_secret={SECRET}");
        let panel = credential_free_panel_url(&url).unwrap();
        let ready = serde_json::to_string(&UiReady {
            panel_url: panel.clone(),
            session: SESSION_KIND,
            delivery: Delivery::Listed.as_str(),
            session_id: Some("aabbccdd".to_string()),
            takeover_url_expires_at_rfc3339: Some("2026-06-11T00:00:00Z".into()),
            takeover_url_ttl_s: Some(900),
        })
        .unwrap();
        let done = serde_json::to_string(&UiTakeoverResult {
            panel_url: panel,
            session: SESSION_KIND,
            delivery: Delivery::Listed.as_str(),
            outcome: "expired",
            open_s: 12,
        })
        .unwrap();
        assert!(!ready.contains(SECRET), "{ready}");
        assert!(!ready.contains("handoff_secret"), "{ready}");
        assert!(!done.contains(SECRET), "{done}");
        assert!(!done.contains("handoff_secret"), "{done}");
    }

    /// The panel is announced under identifiers AFUI accepts. Getting these
    /// wrong is a runtime error at the one moment a person is waiting.
    #[test]
    fn the_panel_is_announced_under_identifiers_afui_accepts() {
        let announced = UiUpstream::new(
            PROVIDER_ID,
            UI_KIND,
            "http://127.0.0.1:9222/takeover/panel?handoff_secret=deadbeef",
        );
        assert!(announced.is_ok());
    }
}
