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
//! Three deliveries of that one session, because the person is not always at
//! this machine, and the way to them is not always this machine's screen.
//! `--mode` names which, in AFUI's own three words, and an unset flag means
//! `AFUI_DELIVERY` decides — the variable an unattended runner sets once so
//! nothing under it pops a window nobody is there to see.
//!
//! * `window` opens the panel here, and closing it is the ending.
//! * `link` starts AFUI's one-session remote page and hands back that page's
//!   URL. The takeover credential stays behind AFUI's proxy.
//! * `session` announces the panel and nothing more, for `afui session serve`
//!   to frame later.
//!
//! All three go through [`agent_first_ui::UiDeliveryPlan`]. The separately
//! hosted panel uses a short internal lease which this process renews while the
//! AFUI delivery exists and revokes when it ends. That lease is crash cleanup,
//! never the user-visible lifetime of a Link or Session.

use std::time::Instant;

use agent_first_ui::{UiDeliveryMode, UiUpstream};
use serde::Serialize;

use crate::cli::connect::Connection;
use crate::cli::output;
use crate::sdk::takeover::TakeoverUiSession;
use crate::shared::error::{Error, ErrorCode};

#[derive(Debug)]
pub struct Args {
    pub sub: UiSub,
    /// Already resolved against `AFUI_DELIVERY`, because the flag having no
    /// default of its own is what lets that variable be reached at all.
    pub delivery: UiDeliveryMode,
}

#[derive(Debug)]
pub enum UiSub {
    Takeover(TakeoverArgs),
}

#[derive(Debug)]
pub enum TakeoverArgs {
    /// Mint a fresh credential from a running host, then open its panel.
    Mint { connection: Connection },
    /// Open a panel URL an earlier `panel` or `fetch --takeover` already
    /// minted. The credential is in the URL, so no host call is needed.
    Open { takeover_url_secret: String },
}

/// afhttp's own half of the event AFUI has made reachable.
///
/// The delivery half — mode, session identity, the bearer URL when there is
/// one, the attention intervals — is AFUI's and is merged in rather than
/// restated here, so afhttp cannot drift from what every other AFUI-delivered
/// UI reports.
///
/// The upstream takeover credential is not in either half and never reaches
/// output: the URL a `link` publishes is AFUI's own shell URL, which keeps
/// that credential behind its proxy.
#[derive(Serialize)]
struct UiReady {
    /// Where the upstream panel is served, credential-free.
    panel_url: String,
    /// No submit control on a VNC canvas: the person watches and acts, and the
    /// session ends without a result.
    session: &'static str,
}

/// The terminal event: the session is over, so the person is done with it.
#[derive(Serialize)]
struct UiTakeoverResult {
    panel_url: String,
    session: &'static str,
    mode: &'static str,
    outcome: &'static str,
    open_s: u64,
}

const SESSION_KIND: &str = "watch";

/// afhttp's fields and AFUI's delivery facts, as one event.
fn ready_event<T: Serialize>(
    facts: &agent_first_ui::UiDeliveryFacts,
    own: T,
) -> Result<serde_json::Value, Error> {
    let own = serde_json::to_value(own).map_err(|error| {
        Error::new(
            ErrorCode::InternalError,
            format!("build takeover UI readiness event: {error}"),
        )
    })?;
    Ok(agent_first_ui::cli::ready_event_revealing_link(facts, own))
}

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

async fn takeover_run(args: TakeoverArgs, delivery: UiDeliveryMode) -> Result<(), Error> {
    let panel = resolve_panel(args).await?;
    let panel_url = credential_free_panel_url(panel.lease.takeover_url_secret())?;
    // AFUI refuses an https upstream behind `link` or `session` — its remote
    // shell proxies plain HTTP, and a window needs no proxy because the browser
    // talks to the panel directly. Saying so here rather than letting that
    // refusal arrive from `start_upstream` matters because a credential has
    // already been minted by this point: caught here it is given straight back,
    // and caught later it is a live capability nobody asked for.
    if let Err(reason) = window_only_upstream(&panel_url, delivery) {
        let _revoked = panel.lease.revoke().await;
        return Err(reason);
    }
    let upstream = UiUpstream::new(PROVIDER_ID, UI_KIND, panel.lease.takeover_url_secret())
        .map_err(delivery_error)?
        .with_subject(&panel_url);
    // The same offer the `--mode` flag was built from.
    let active = crate::cli::spec::PANEL_DELIVERY
        .resolve(Some(delivery))
        .map_err(delivery_error)?
        .start_upstream(upstream)
        .await
        .map_err(delivery_error)?;

    // The one event whose job is to hand a `link` URL over. AFUI publishes it
    // under a name the emitter will not mask, so this goes out the ordinary
    // way — afhttp used to carry an emitter of its own that redacted the event
    // and then put this one field back.
    let ready = ready_event(
        &active.facts(),
        UiReady {
            panel_url: panel_url.clone(),
            session: SESSION_KIND,
        },
    )?;
    output::emit_progress("ui_takeover", &ready)?;

    let opened_at = Instant::now();
    // AFUI owns every delivery-specific ending. afhttp contributes only the
    // process shutdown signal and the private upstream lease maintenance.
    let ended = {
        let delivery_wait = active.wait();
        let keep_alive = panel.lease.keep_alive();
        tokio::pin!(delivery_wait);
        tokio::pin!(keep_alive);
        tokio::select! {
            result = &mut delivery_wait => result
                .map(|outcome| outcome.ending())
                .map_err(delivery_error),
            () = stop_requested() => Ok("stopped"),
            result = &mut keep_alive => match result {
                Err(error) => Err(error),
                Ok(()) => Err(Error::new(
                    ErrorCode::InternalError,
                    "takeover UI session keep-alive ended unexpectedly",
                )),
            },
        }
    };
    let revoked = panel.lease.revoke().await;
    let outcome = ended?;
    revoked?;

    output::emit(
        "ui_takeover",
        &UiTakeoverResult {
            panel_url,
            session: SESSION_KIND,
            mode: delivery.as_str(),
            outcome,
            open_s: opened_at.elapsed().as_secs(),
        },
    )
}

/// Whether this panel can only be delivered as a local window.
///
/// A takeover host behind TLS is a perfectly good panel; what it is not is
/// something AFUI's remote shell can proxy, because that shell speaks plain
/// HTTP to its upstream. `window` never proxies at all — the browser opens the
/// panel itself — so it is the one delivery that still works.
fn window_only_upstream(panel_url: &str, delivery: UiDeliveryMode) -> Result<(), Error> {
    if delivery == UiDeliveryMode::Window || !panel_url.starts_with("https://") {
        return Ok(());
    }
    Err(Error::new(
        ErrorCode::InvalidEndpoint,
        format!(
            "`{}` delivery cannot proxy the https panel at {panel_url}; use `--mode window`, \
             which opens it directly and needs no proxy",
            delivery.as_str()
        ),
    ))
}

/// One AFUI failure, in afhttp's vocabulary.
///
/// Keyed on AFUI's own classification rather than on its variants, and
/// exhaustive over it. The comment here used to claim that a new AFUI variant
/// "lands in the category it was classified as" — it did not: the arms named
/// individual classifications and everything else fell into `InternalError`,
/// so every classification AFUI added later would have been reported as an
/// afhttp bug. Now there is no wildcard, and adding one stops this compiling
/// until somebody says which afhttp error it is.
fn delivery_error(error: agent_first_ui::Error) -> Error {
    use agent_first_ui::UiErrorKind;

    let error_code = match error.kind() {
        UiErrorKind::WindowUnavailable | UiErrorKind::WindowWaitFailed => {
            ErrorCode::BrowserLaunchFailed
        }
        UiErrorKind::InvalidArgument | UiErrorKind::UpstreamNotProxyable => {
            ErrorCode::InvalidEndpoint
        }
        // The panel is asked for by `--mode`, so a delivery AFUI will not
        // resolve or cannot reach is what the caller passed, not a fault here.
        UiErrorKind::DeliveryModeInvalid
        | UiErrorKind::DeliveryModeNotOffered
        | UiErrorKind::LinkAddressUnavailable => ErrorCode::InvalidEndpoint,
        UiErrorKind::FrontendUnreadable
        | UiErrorKind::FrontendIncompatible
        | UiErrorKind::FrontendUnsafe
        | UiErrorKind::PageRender
        | UiErrorKind::PageIncomplete
        | UiErrorKind::RuntimeMisconfigured
        | UiErrorKind::RuntimeClosed
        | UiErrorKind::RuntimeBusy
        | UiErrorKind::RuntimeMessageTooLarge
        | UiErrorKind::RuntimeBlob
        | UiErrorKind::RuntimePayload
        | UiErrorKind::ConfigUnreadable
        | UiErrorKind::Io => ErrorCode::InternalError,
    };
    Error::new(error_code, error.to_string())
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

struct Panel {
    lease: TakeoverUiSession,
}

async fn resolve_panel(args: TakeoverArgs) -> Result<Panel, Error> {
    match args {
        TakeoverArgs::Open {
            takeover_url_secret,
        } => Ok(Panel {
            lease: TakeoverUiSession::exchange(&takeover_url_secret).await?,
        }),
        TakeoverArgs::Mint { connection } => {
            let client = connection.client().await?;
            Ok(Panel {
                lease: client.takeover_ui_session().await?,
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
            format!("--takeover-url-secret is not a valid URL: {error}"),
        )
    })?;
    panel.set_query(None);
    panel.set_fragment(None);
    Ok(panel.to_string())
}

#[cfg(test)]
mod tests {
    use agent_first_ui::UiAttentionPolicy;

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

    const SECRET: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn ready_event(delivery: UiDeliveryMode, panel: &str) -> serde_json::Value {
        let attention = (delivery == UiDeliveryMode::Link).then(UiAttentionPolicy::default);
        let facts = agent_first_ui::UiDeliveryFacts {
            mode: delivery,
            session_id: "aabbccdd".to_string(),
            link_url_secret: (delivery == UiDeliveryMode::Link)
                .then(|| "http://192.168.1.20:9888/afui-capability/".to_string()),
            idle_timeout_s: attention
                .and_then(|policy| policy.idle_timeout())
                .map(|duration| duration.as_secs()),
            grace_period_s: attention.map(|policy| policy.grace_period().as_secs()),
        };
        super::ready_event(
            &facts,
            UiReady {
                panel_url: panel.to_string(),
                session: SESSION_KIND,
            },
        )
        .unwrap()
    }

    #[test]
    fn no_delivery_event_carries_the_upstream_secret() {
        let url = format!("http://127.0.0.1:9222/takeover/panel?handoff_secret={SECRET}");
        let panel = credential_free_panel_url(&url).unwrap();
        for delivery in [
            UiDeliveryMode::Window,
            UiDeliveryMode::Link,
            UiDeliveryMode::Session,
        ] {
            let ready = serde_json::to_string(&ready_event(delivery, &panel)).unwrap();
            let done = serde_json::to_string(&UiTakeoverResult {
                panel_url: panel.clone(),
                session: SESSION_KIND,
                mode: delivery.as_str(),
                outcome: "expired",
                open_s: 12,
            })
            .unwrap();
            assert!(!ready.contains(SECRET), "{ready}");
            assert!(!ready.contains("handoff_secret"), "{ready}");
            assert!(!done.contains(SECRET), "{done}");
            assert!(!done.contains("handoff_secret"), "{done}");
        }
    }

    /// Link exposes AFUI's outer credential and never the takeover credential
    /// AFUI keeps behind its proxy.
    #[test]
    fn a_link_carries_only_the_afui_url() {
        let value = serde_json::to_value(ready_event(
            UiDeliveryMode::Link,
            "http://127.0.0.1:9222/takeover/panel",
        ))
        .unwrap();
        assert_eq!(
            value[agent_first_ui::cli::LINK_URL_FIELD],
            serde_json::Value::String("http://192.168.1.20:9888/afui-capability/".to_string())
        );
        // Never under the suffixed name, which an emitter would mask.
        assert!(value.get("link_url_secret").is_none(), "{value}");
        assert!(value.get("takeover_url_secret").is_none());
        let policy = UiAttentionPolicy::default();
        assert_eq!(
            value["idle_timeout_s"],
            policy.idle_timeout().unwrap().as_secs()
        );
        assert_eq!(value["grace_period_s"], policy.grace_period().as_secs());
    }

    /// A credential minted a moment ago must not be spent on a delivery that
    /// cannot run — and the refusal has to name the one that can.
    #[test]
    fn an_https_panel_is_refused_before_a_proxying_delivery_starts() {
        for delivery in [UiDeliveryMode::Link, UiDeliveryMode::Session] {
            let refused = window_only_upstream("https://takeover.example/takeover/panel", delivery)
                .expect_err("a proxying delivery cannot reach an https panel");
            assert_eq!(refused.error_code, ErrorCode::InvalidEndpoint);
            assert!(refused.detail.contains("--mode window"), "{refused:?}");
        }
        // A window opens the panel itself, so TLS is not in anybody's way.
        assert!(
            window_only_upstream(
                "https://takeover.example/takeover/panel",
                UiDeliveryMode::Window
            )
            .is_ok()
        );
        // And plain HTTP is what every delivery was always able to reach.
        for delivery in [
            UiDeliveryMode::Window,
            UiDeliveryMode::Link,
            UiDeliveryMode::Session,
        ] {
            assert!(window_only_upstream("http://127.0.0.1:9222/takeover/panel", delivery).is_ok());
        }
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
