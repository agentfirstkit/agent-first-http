//! Browser-backed fetch path: CDP navigation, artifact capture, wait modes,
//! and network-body capture.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use cookie::SameSite;
use serde::Serialize;
use serde_json::Value;
use url::Url;

use crate::sdk::cdp::{session, ws_client::Connection};
use crate::sdk::endpoint::Endpoint;
use crate::sdk::fetch::artifacts::{
    body as body_artifact,
    collectors::{ConsoleCollector, NetworkCollector},
    console as console_artifact, content as content_artifact, network as network_artifact,
    observation as observation_artifact, rendered_html as rendered_html_artifact,
    screenshot as screenshot_artifact, text as text_artifact,
};
use crate::sdk::fetch::deadline::FetchDeadline;
use crate::sdk::fetch::page_classification;
use crate::sdk::fetch::result::{FetchResult, RenderDecision, Warning};
use crate::sdk::fetch::wait::Wait;
use crate::sdk::fetch::writer;
use crate::sdk::fetch::FetchBuilder;
use crate::shared::artifacts::{Artifact, ArtifactPaths};
use crate::shared::error::{Error, ErrorCode};
use crate::shared::ids::{RequestId, TabId};
use crate::shared::time::duration_ms;

use super::cookie_jar_resolve::sync_browser_cookies_to_jar;
use super::request_opts::{
    cookie_is_expired, effective_cookie_secure, parse_cookie_url, prepare_cookie,
    validate_cookie_scope, PreparedCookie, PreparedRequestOptions,
};
use super::NetworkBodies;

mod capture;
mod download;
mod navigate;
mod network_bodies;
mod wait;

use capture::{
    capture_content, capture_inner_text, capture_location, capture_outer_html,
    capture_page_snapshot, capture_response_body, capture_screenshot,
    navigation_status_from_performance, PageSnapshot,
};
use download::{
    configure_download_capture, finish_download_navigation, navigate_has_download_flag,
    navigation_became_download, snapshot_download_dir, wait_for_download_start, DownloadNavigation,
};
use navigate::navigate_with_method;
use network_bodies::{capture_network_bodies, NetworkBodyCapture};
use wait::{wait_for_condition, WaitContext, WaitTuning};

pub(super) fn reject_http_only_evaluate(
    request_options: &PreparedRequestOptions,
) -> Result<(), Error> {
    if request_options.has_evaluate_after_wait() {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            ".evaluate_after_wait(...) requires a browser-backed fetch; use --render always or let --render auto escalate for another reason",
        ));
    }
    Ok(())
}

pub(super) async fn browser_path(
    mut builder: FetchBuilder,
    mut request_options: PreparedRequestOptions,
    request_id: RequestId,
    paths: ArtifactPaths,
    start: Instant,
    escalation_reason: Option<String>,
    deadline: &FetchDeadline,
) -> Result<FetchResult, Error> {
    deadline.update_trace(|trace| {
        trace.render_decision = RenderDecision::Browser;
        trace.render_mode = builder.render.as_trace();
        trace.render_used = true;
        trace.escalation_reason = escalation_reason.clone();
        trace.cookie_jar_file = builder.cookie_jar.path.clone();
        trace.cookie_jar_warning = builder.cookie_jar.warning.clone();
        trace.sensitive_capture = super::sensitive_capture(&builder);
    });
    deadline.set_stage("resolve_inline_host");
    if builder.client.has_inline_host()
        && !builder.client.inline_host_started().await
        && builder.cookie_jar.path.is_none()
        && !builder.cookie_jar.disabled
    {
        deadline
            .run_result(
                "resolve_inline_host",
                ErrorCode::NavigationTimeout,
                builder.client.effective_endpoint(),
            )
            .await?;
        deadline
            .run_result(
                "resolve_cookie_jar",
                ErrorCode::NavigationTimeout,
                super::cookie_jar_resolve::resolve_cookie_jar_path(&mut builder),
            )
            .await?;
        deadline.update_trace(|trace| {
            trace.cookie_jar_file = builder.cookie_jar.path.clone();
            trace.cookie_jar_warning = builder.cookie_jar.warning.clone();
        });
    }
    deadline
        .run_result(
            "prepare_request",
            ErrorCode::NavigationTimeout,
            writer::ensure_dir(&paths.root),
        )
        .await?;
    let endpoint = builder.client.endpoint();
    if !endpoint_is_remote(endpoint) {
        return Err(Error::new(
            ErrorCode::RenderUnavailable,
            "browser fetch requires --endpoint-url pointing at an afhttp host",
        ));
    }

    if let Some(jar_path) = &builder.cookie_jar.path {
        if let Ok(url) = Url::parse(&builder.url) {
            if let Ok(jar) = crate::sdk::profile::cookie_jar::CookieJar::load(jar_path) {
                let mut prepared = Vec::new();
                for c in jar.applicable_cookies(&url) {
                    if let Ok(p) = prepare_cookie(&c, "cookie_jar") {
                        prepared.push(p);
                    }
                }
                request_options.merge_jar_cookies(prepared);
            }
        }
    }

    let conn = deadline
        .run_result(
            "connect_cdp",
            ErrorCode::NavigationTimeout,
            builder.client.cdp_connection(),
        )
        .await?;

    let (target_id, session_id, close_target_after_fetch) = if let Some(tab) = builder.tab.as_ref()
    {
        let target_id = tab.as_str().to_string();
        let session_id = deadline
            .run_result(
                "attach_target",
                ErrorCode::NavigationTimeout,
                session::attach_to_target(&conn, &target_id),
            )
            .await?;
        (target_id, session_id, false)
    } else {
        let (target_id, session_id) = deadline
            .run_result(
                "open_target",
                ErrorCode::NavigationTimeout,
                session::open_blank_target(&conn),
            )
            .await?;
        // Keep the new target open when the caller asked (human takeover);
        // otherwise close it once the fetch finishes.
        (target_id, session_id, !builder.keep_tab_open)
    };

    deadline
        .run_result("browser_enable", ErrorCode::NavigationTimeout, async {
            for method in [
                "Page.enable",
                "Runtime.enable",
                "Network.enable",
                "DOM.enable",
            ] {
                let _ = cdp_send(
                    &conn,
                    &session_id,
                    method,
                    &serde_json::json!({}),
                    "browser_enable",
                    deadline,
                )
                .await?;
            }
            Ok(())
        })
        .await?;

    if let Err(e) = deadline
        .run_result(
            "prepare_request",
            ErrorCode::NavigationTimeout,
            apply_browser_request_options(
                &conn,
                &session_id,
                &builder.url,
                &request_options,
                deadline,
            ),
        )
        .await
    {
        finish_target(&conn, &target_id, &session_id, close_target_after_fetch).await;
        return Err(e);
    }

    let network_collector = NetworkCollector::start(
        &conn,
        builder.network.redact,
        builder.network.capture_ws,
        builder.network.capture_sse,
    );
    let console_collector = if builder.want.contains(&Artifact::Console) {
        Some(ConsoleCollector::start(&conn))
    } else {
        None
    };
    let download_dir = configure_download_capture(&builder.client, &conn, deadline).await;
    let mut download_events = download_dir.as_ref().map(|_| conn.subscribe());
    let downloads_before = if let Some(dir) = download_dir.as_ref() {
        snapshot_download_dir(dir).await.unwrap_or_default()
    } else {
        BTreeSet::new()
    };
    let initial_url = capture_location(&conn, &session_id, deadline)
        .await
        .ok()
        .flatten();

    let nav_start = Instant::now();
    let navigate = deadline
        .run_result(
            "navigate",
            ErrorCode::NavigationTimeout,
            navigate_with_method(
                &conn,
                &session_id,
                &builder.url,
                &request_options.method,
                &request_options.body_payload,
                builder.timeout,
                deadline,
            ),
        )
        .await;
    let navigate = match navigate {
        Ok(v) => v,
        Err(e) => {
            finish_target(&conn, &target_id, &session_id, close_target_after_fetch).await;
            return Err(e);
        }
    };
    let download_start = if let Some(rx) = download_events.as_mut() {
        // ERR_ABORTED only gives the CDP download event time to arrive; it
        // is not itself proof of a download.
        let wait_for_download_event = navigate_has_download_flag(&navigate)
            || navigate.get("errorText").and_then(|v| v.as_str()) == Some("net::ERR_ABORTED");
        let wait = if wait_for_download_event {
            Duration::from_millis(1_000)
        } else {
            Duration::ZERO
        };
        wait_for_download_start(rx, wait, &session_id).await
    } else {
        None
    };
    if navigation_became_download(&navigate, download_start.as_ref()) {
        return finish_download_navigation(DownloadNavigation {
            builder,
            conn,
            network_collector,
            console_collector,
            download_dir,
            download_start,
            downloads_before,
            request_id,
            paths,
            start,
            nav_duration: nav_start.elapsed(),
            escalation_reason,
            target_id,
            session_id,
            close_target_after_fetch,
            deadline: deadline.clone(),
        })
        .await;
    }
    if let Some(err) = navigate.get("errorText").and_then(|v| v.as_str()) {
        if !err.is_empty() {
            let code = classify_navigate_error(err);
            finish_target(&conn, &target_id, &session_id, close_target_after_fetch).await;
            return Err(Error::new(code, format!("Page.navigate errorText: {err}")));
        }
    }

    if let Err(e) = deadline
        .run_result(
            "wait_navigation_commit",
            ErrorCode::NavigationTimeout,
            wait_for_navigation_commit(
                &conn,
                &session_id,
                &builder.url,
                initial_url.as_deref(),
                deadline,
            ),
        )
        .await
    {
        finish_target(&conn, &target_id, &session_id, close_target_after_fetch).await;
        return Err(e);
    }

    deadline.update_trace(|trace| {
        trace.navigation_duration_ms = Some(duration_ms(nav_start.elapsed()));
        trace.wait_mode = Some(builder.wait.mode_name().to_string());
    });
    let wait_outcome = match deadline
        .run_result(
            "wait_readiness",
            ErrorCode::NavigationTimeout,
            wait_for_condition(
                WaitContext {
                    conn: &conn,
                    session_id: &session_id,
                    collector: &network_collector,
                    deadline,
                },
                &builder.wait,
                WaitTuning {
                    timeout: builder.timeout,
                    readiness_idle_ms: builder.readiness.idle_ms,
                    readiness_stable_ms: builder.readiness.stable_ms,
                },
            ),
        )
        .await
    {
        Ok(outcome) => outcome,
        Err(e) => {
            finish_target(&conn, &target_id, &session_id, close_target_after_fetch).await;
            if e.error_code == ErrorCode::CdpTimeout {
                return Err(Error::new(ErrorCode::NavigationTimeout, e.detail));
            }
            return Err(e);
        }
    };
    let nav_duration = nav_start.elapsed();

    if request_options.has_evaluate_after_wait() {
        if let Err(e) = deadline
            .run_result(
                "evaluate_after_wait",
                ErrorCode::NavigationTimeout,
                evaluate_after_wait(&conn, &session_id, &request_options, deadline),
            )
            .await
        {
            finish_target(&conn, &target_id, &session_id, close_target_after_fetch).await;
            return Err(e);
        }
    }

    let mut warnings: Vec<Warning> = Vec::new();
    let mut result = FetchResult::new(request_id.clone(), builder.url.clone(), deadline.snapshot());
    result.tab_id = Some(TabId::new(target_id.clone()));

    let main_entry = network_collector
        .wait_for_main_status(Duration::from_millis(
            builder.readiness.observe_main_wait_ms,
        ))
        .await;
    result.trace.main_request_observed = main_entry.is_some();
    deadline.update_trace(|trace| {
        trace.main_request_observed = main_entry.is_some();
        trace.navigation_duration_ms = Some(duration_ms(nav_duration));
        trace.wait_mode = Some(wait_outcome.wait_mode.clone());
        trace.wait_satisfied_by = wait_outcome.wait_satisfied_by.clone();
        trace.network_quiet = wait_outcome.network_quiet;
        trace.dom_stable = wait_outcome.dom_stable;
        trace.text_stable = wait_outcome.text_stable;
        trace.capture_reason = Some(wait_outcome.capture_reason.clone());
        trace.cookie_jar_file = builder.cookie_jar.path.clone();
        trace.cookie_jar_warning = builder.cookie_jar.warning.clone();
        trace.sensitive_capture = super::sensitive_capture(&builder);
    });

    if wait_outcome.readiness_timed_out {
        warnings.push(Warning {
            artifact: Artifact::Network,
            code: ErrorCode::ReadinessTimeout,
            detail: format!(
                "--wait auto captured before all readiness signals settled: network_quiet={:?}, dom_stable={:?}, text_stable={:?}",
                wait_outcome.network_quiet, wait_outcome.dom_stable, wait_outcome.text_stable
            ),
        });
    }

    let page_snapshot = capture_page_snapshot(&conn, &session_id, deadline)
        .await
        .ok();
    if let Some(snapshot) = page_snapshot.as_ref() {
        if !snapshot.url.is_empty() {
            result.final_url = snapshot.url.clone();
        }
    }
    if let Err(e) = validate_navigation_capture(
        &builder.url,
        initial_url.as_deref(),
        &result.final_url,
        main_entry.as_ref(),
        page_snapshot.as_ref(),
    ) {
        finish_target(&conn, &target_id, &session_id, close_target_after_fetch).await;
        return Err(e);
    }
    if let Some(snapshot) = page_snapshot.as_ref() {
        if let Some(classification) = page_classification::classify(
            Some(&snapshot.html),
            Some(&snapshot.text),
            Some(&snapshot.title),
        ) {
            result.set_page_kind(classification.kind);
            warnings.push(Warning {
                artifact: Artifact::RenderedHtml,
                code: classification.code,
                detail: classification.detail,
            });
        }
    }
    if let Some(main) = main_entry.as_ref() {
        if let Some(status) = main.status {
            result.status = status;
        } else {
            warnings.push(Warning {
                artifact: Artifact::Network,
                code: ErrorCode::ArtifactCaptureFailed,
                detail: format!("main request {} had no response status", main.request_id),
            });
        }
    } else if should_warn_main_not_observed(&builder.url, &result.final_url, page_snapshot.as_ref())
    {
        warnings.push(Warning {
            artifact: Artifact::Network,
            code: ErrorCode::ArtifactCaptureFailed,
            detail: "main document network request was not observed".into(),
        });
    }
    if result.status == 0 {
        if let Some(status) = navigation_status_from_performance(&conn, &session_id, deadline).await
        {
            result.status = status;
        }
    }

    if builder.want.contains(&Artifact::RenderedHtml) {
        match deadline
            .run_result(
                "capture_rendered_html",
                ErrorCode::ArtifactCaptureTimeout,
                capture_outer_html(&conn, &session_id, deadline),
            )
            .await
        {
            Ok(html) => {
                push_size_warning(&mut warnings, Artifact::RenderedHtml, html.trim().len(), 1);
                if let Ok(path) = rendered_html_artifact::write(&paths, &html).await {
                    result.set_artifact_file(Artifact::RenderedHtml, path);
                }
            }
            Err(e) => warnings.push(Warning {
                artifact: Artifact::RenderedHtml,
                code: e.error_code,
                detail: e.detail,
            }),
        }
    }

    if builder.want.contains(&Artifact::Text) {
        match deadline
            .run_result(
                "capture_text",
                ErrorCode::ArtifactCaptureTimeout,
                capture_inner_text(&conn, &session_id, deadline),
            )
            .await
        {
            Ok(text) => {
                push_size_warning(
                    &mut warnings,
                    Artifact::Text,
                    text.trim().len(),
                    builder.readiness.min_text_bytes as usize,
                );
                if let Ok(path) = text_artifact::write(&paths, &text).await {
                    result.set_artifact_file(Artifact::Text, path);
                }
            }
            Err(e) => warnings.push(Warning {
                artifact: Artifact::Text,
                code: e.error_code,
                detail: e.detail,
            }),
        }
    }

    if builder.want.contains(&Artifact::Content) || builder.want.contains(&Artifact::ContentJson) {
        match deadline
            .run_result(
                "capture_content",
                ErrorCode::ArtifactCaptureTimeout,
                capture_content(&conn, &session_id, deadline),
            )
            .await
        {
            Ok(content) => {
                if builder.want.contains(&Artifact::Content) {
                    push_size_warning(
                        &mut warnings,
                        Artifact::Content,
                        content.markdown.trim().len(),
                        builder.readiness.min_text_bytes as usize,
                    );
                    match content_artifact::write_markdown(&paths, &content.markdown).await {
                        Ok(path) => result.set_artifact_file(Artifact::Content, path),
                        Err(e) => warnings.push(Warning {
                            artifact: Artifact::Content,
                            code: e.error_code,
                            detail: e.detail,
                        }),
                    }
                }
                if builder.want.contains(&Artifact::ContentJson) {
                    match content_artifact::write_json(&paths, &content.json).await {
                        Ok(path) => result.set_artifact_file(Artifact::ContentJson, path),
                        Err(e) => warnings.push(Warning {
                            artifact: Artifact::ContentJson,
                            code: e.error_code,
                            detail: e.detail,
                        }),
                    }
                }
            }
            Err(e) => {
                if builder.want.contains(&Artifact::Content) {
                    warnings.push(Warning {
                        artifact: Artifact::Content,
                        code: e.error_code,
                        detail: e.detail.clone(),
                    });
                }
                if builder.want.contains(&Artifact::ContentJson) {
                    warnings.push(Warning {
                        artifact: Artifact::ContentJson,
                        code: e.error_code,
                        detail: e.detail,
                    });
                }
            }
        }
    }

    if builder.want.contains(&Artifact::Screenshot) {
        match deadline
            .run_result(
                "capture_screenshot",
                ErrorCode::ArtifactCaptureTimeout,
                capture_screenshot(&conn, &session_id, deadline),
            )
            .await
        {
            Ok(png) => {
                if let Ok(path) = screenshot_artifact::write(&paths, &png).await {
                    result.set_artifact_file(Artifact::Screenshot, path);
                }
            }
            Err(e) => warnings.push(Warning {
                artifact: Artifact::Screenshot,
                code: e.error_code,
                detail: e.detail,
            }),
        }
    }

    if builder.want.contains(&Artifact::Body) {
        if let Some(main) = main_entry.as_ref() {
            match deadline
                .run_result(
                    "capture_body",
                    ErrorCode::ArtifactCaptureTimeout,
                    capture_response_body(&conn, &session_id, &main.request_id, deadline),
                )
                .await
            {
                Ok(bytes) => {
                    match body_artifact::write(&paths, main.mime_type.as_deref(), &bytes).await {
                        Ok(path) => result.set_artifact_file(Artifact::Body, path),
                        Err(e) => warnings.push(Warning {
                            artifact: Artifact::Body,
                            code: e.error_code,
                            detail: e.detail,
                        }),
                    }
                }
                Err(e) => warnings.push(Warning {
                    artifact: Artifact::Body,
                    code: if e.error_code == ErrorCode::CdpTimeout {
                        ErrorCode::ArtifactCaptureTimeout
                    } else {
                        e.error_code
                    },
                    detail: e.detail,
                }),
            }
        } else {
            warnings.push(Warning {
                artifact: Artifact::Body,
                code: ErrorCode::ArtifactCaptureFailed,
                detail: "cannot write body_file: main document request was not observed".into(),
            });
        }
    }

    if builder.want.contains(&Artifact::Body) && builder.want.contains(&Artifact::Network) {
        if let (Some(main), Some(path)) = (
            main_entry.as_ref(),
            result.artifact_file(Artifact::Body).cloned(),
        ) {
            network_collector
                .set_body_file(&main.request_id, path)
                .await;
        }
    }

    if builder.want.contains(&Artifact::Observation) {
        match deadline
            .run_result(
                "capture_observation",
                ErrorCode::ArtifactCaptureTimeout,
                observation_artifact::capture(&conn, &session_id, &result.final_url),
            )
            .await
        {
            Ok(obs) => {
                if obs.nodes.is_empty() {
                    warnings.push(Warning {
                        artifact: Artifact::Observation,
                        code: ErrorCode::ObservationEmpty,
                        detail: "observation contained zero projected nodes".into(),
                    });
                }
                if let Ok(path) = observation_artifact::write(&paths, &obs).await {
                    result.set_artifact_file(Artifact::Observation, path);
                }
            }
            Err(e) => warnings.push(Warning {
                artifact: Artifact::Observation,
                code: e.error_code,
                detail: e.detail,
            }),
        }
    }

    if builder.want.contains(&Artifact::Storage) {
        match deadline
            .run_result(
                "capture_storage",
                ErrorCode::ArtifactCaptureTimeout,
                crate::sdk::fetch::artifacts::storage::capture(
                    &conn,
                    &session_id,
                    &result.final_url,
                ),
            )
            .await
        {
            Ok(snap) => {
                if let Ok(path) = crate::sdk::fetch::artifacts::storage::write(&paths, &snap).await
                {
                    result.set_artifact_file(Artifact::Storage, path);
                }
            }
            Err(e) => warnings.push(Warning {
                artifact: Artifact::Storage,
                code: e.error_code,
                detail: e.detail,
            }),
        }
    }

    if let Some(collector) = console_collector {
        match deadline
            .run_result(
                "capture_console",
                ErrorCode::ArtifactCaptureTimeout,
                async {
                    let log = collector.finish().await;
                    console_artifact::write(&paths, &log).await
                },
            )
            .await
        {
            Ok(path) => result.set_artifact_file(Artifact::Console, path),
            Err(e) => warnings.push(Warning {
                artifact: Artifact::Console,
                code: e.error_code,
                detail: e.detail,
            }),
        }
    }

    // Write WS frame / SSE event JSONL files.
    if builder.network.capture_ws {
        write_frames_jsonl(
            &paths,
            network_collector.take_ws_frames().await,
            &mut warnings,
        )
        .await;
    }
    if builder.network.capture_sse {
        write_frames_jsonl(
            &paths,
            network_collector.take_sse_events().await,
            &mut warnings,
        )
        .await;
    }

    let effective_network_bodies = if matches!(builder.network.bodies, NetworkBodies::Off)
        && matches!(builder.wait, Wait::Auto)
    {
        NetworkBodies::Xhr
    } else {
        builder.network.bodies
    };
    if !matches!(effective_network_bodies, NetworkBodies::Off) {
        match deadline
            .run_result(
                "capture_network_bodies",
                ErrorCode::ArtifactCaptureTimeout,
                async {
                    capture_network_bodies(
                        NetworkBodyCapture {
                            conn: &conn,
                            session_id: &session_id,
                            collector: &network_collector,
                            paths: &paths,
                            mode: effective_network_bodies,
                            max_bytes: builder.network.body_max_bytes,
                            deadline,
                        },
                        &mut warnings,
                    )
                    .await;
                    Ok(())
                },
            )
            .await
        {
            Ok(()) => {}
            Err(e) => warnings.push(Warning {
                artifact: Artifact::Network,
                code: e.error_code,
                detail: e.detail,
            }),
        }
    }
    if builder.want.contains(&Artifact::Network) {
        match deadline
            .run_result(
                "capture_network",
                ErrorCode::ArtifactCaptureTimeout,
                async {
                    let log = network_collector.finish().await;
                    Ok(log)
                },
            )
            .await
        {
            Ok(log) => {
                push_network_readiness_warnings(&log.summary, &mut warnings);
                match network_artifact::write(&paths, &log).await {
                    Ok(path) => result.set_artifact_file(Artifact::Network, path),
                    Err(e) => warnings.push(Warning {
                        artifact: Artifact::Network,
                        code: e.error_code,
                        detail: e.detail,
                    }),
                }
            }
            Err(e) => warnings.push(Warning {
                artifact: Artifact::Network,
                code: e.error_code,
                detail: e.detail,
            }),
        }
    } else {
        let _ = network_collector.finish().await;
    }

    if let Some(jar_path) = &builder.cookie_jar.path {
        if let Err(e) = deadline
            .run_result(
                "sync_cookie_jar",
                ErrorCode::NavigationTimeout,
                sync_browser_cookies_to_jar(&conn, &session_id, &result.final_url, jar_path),
            )
            .await
        {
            warnings.push(Warning {
                artifact: Artifact::Network,
                code: e.error_code,
                detail: format!("cookie jar sync: {}", e.detail),
            });
        }
    }

    if close_target_after_fetch {
        let _ = session::close_target(&conn, &target_id).await;
    } else {
        let _ = session::detach_from_target(&conn, &session_id).await;
    }

    result.warnings = warnings;
    deadline.update_trace(|trace| {
        trace.duration_ms = duration_ms(start.elapsed());
    });
    result.trace = deadline.complete_trace();
    Ok(result)
}

async fn apply_browser_request_options(
    conn: &Connection,
    session_id: &str,
    url: &str,
    request_options: &PreparedRequestOptions,
    deadline: &FetchDeadline,
) -> Result<(), Error> {
    let extra_headers = request_options.cdp_extra_headers();
    if !extra_headers.is_empty() {
        cdp_send(
            conn,
            session_id,
            "Network.setExtraHTTPHeaders",
            &serde_json::json!({ "headers": extra_headers }),
            "apply_request_options",
            deadline,
        )
        .await?;
    }

    if let Some((user_agent, _)) = &request_options.user_agent {
        cdp_send(
            conn,
            session_id,
            "Network.setUserAgentOverride",
            &serde_json::json!({ "userAgent": user_agent }),
            "apply_request_options",
            deadline,
        )
        .await?;
    }

    let cookies = request_options.browser_cookies();
    if !cookies.is_empty() {
        let parsed_url = parse_cookie_url(url)?;
        let mut params = Vec::new();
        for cookie in cookies {
            validate_cookie_scope(cookie, &parsed_url, ".cookie(...)")?;
            if effective_cookie_secure(cookie) && parsed_url.scheme() != "https" {
                continue;
            }
            if cookie_is_expired(cookie) {
                continue;
            }
            params.push(cdp_cookie_param(cookie, &parsed_url));
        }
        if params.is_empty() {
            return Ok(());
        }
        cdp_send(
            conn,
            session_id,
            "Network.setCookies",
            &serde_json::json!({ "cookies": params }),
            "apply_request_options",
            deadline,
        )
        .await?;
    }

    Ok(())
}

fn cdp_cookie_param(cookie: &PreparedCookie, url: &Url) -> Value {
    let mut param = serde_json::Map::new();
    param.insert("name".to_string(), serde_json::json!(&cookie.name));
    param.insert("value".to_string(), serde_json::json!(&cookie.value));
    param.insert("url".to_string(), serde_json::json!(url.as_str()));
    if let Some(domain) = &cookie.domain {
        param.insert("domain".to_string(), serde_json::json!(domain));
    }
    if let Some(path) = &cookie.path {
        param.insert("path".to_string(), serde_json::json!(path));
    }
    if cookie.secure.is_some() || effective_cookie_secure(cookie) {
        param.insert(
            "secure".to_string(),
            serde_json::json!(effective_cookie_secure(cookie)),
        );
    }
    if let Some(http_only) = cookie.http_only {
        param.insert("httpOnly".to_string(), serde_json::json!(http_only));
    }
    if let Some(same_site) = cookie.same_site {
        param.insert(
            "sameSite".to_string(),
            serde_json::json!(cdp_same_site(same_site)),
        );
    }
    if let Some(expires) = cookie.expires_epoch_s {
        param.insert("expires".to_string(), serde_json::json!(expires));
    }
    if cookie.partitioned == Some(true) {
        param.insert(
            "partitionKey".to_string(),
            serde_json::json!({
                "topLevelSite": cookie_top_level_site(url),
                "hasCrossSiteAncestor": false,
            }),
        );
    }
    Value::Object(param)
}

fn cdp_same_site(same_site: SameSite) -> &'static str {
    match same_site {
        SameSite::Strict => "Strict",
        SameSite::Lax => "Lax",
        SameSite::None => "None",
    }
}

fn cookie_top_level_site(url: &Url) -> String {
    let host = url.host_str().unwrap_or_default();
    format!("{}://{}", url.scheme(), host)
}

async fn evaluate_after_wait(
    conn: &Connection,
    session_id: &str,
    request_options: &PreparedRequestOptions,
    deadline: &FetchDeadline,
) -> Result<(), Error> {
    for (idx, js) in request_options.evaluate_after_wait.iter().enumerate() {
        let result = cdp_send(
            conn,
            session_id,
            "Runtime.evaluate",
            &serde_json::json!({
                "expression": js,
                "awaitPromise": true,
                "returnByValue": false,
            }),
            "evaluate_after_wait",
            deadline,
        )
        .await
        .map_err(|e| {
            Error::new(
                e.error_code,
                format!("evaluate_after_wait[{idx}]: {}", e.detail),
            )
            .with_retryable(e.retryable)
        })?;
        if let Some(exception) = result.get("exceptionDetails") {
            let detail = runtime_exception_detail(exception);
            return Err(Error::new(
                ErrorCode::CdpError,
                format!("evaluate_after_wait[{idx}]: {detail}"),
            ));
        }
    }
    Ok(())
}

fn runtime_exception_detail(exception: &Value) -> String {
    exception
        .get("exception")
        .and_then(|e| e.get("description").or_else(|| e.get("value")))
        .and_then(|v| v.as_str())
        .or_else(|| exception.get("text").and_then(|v| v.as_str()))
        .unwrap_or("Runtime.evaluate returned exceptionDetails")
        .to_string()
}

pub(super) async fn finish_target(
    conn: &Connection,
    target_id: &str,
    session_id: &str,
    close_target_after_fetch: bool,
) {
    if close_target_after_fetch {
        let _ = session::close_target(conn, target_id).await;
    } else {
        let _ = session::detach_from_target(conn, session_id).await;
    }
}

async fn wait_for_navigation_commit(
    conn: &Connection,
    session_id: &str,
    request_url: &str,
    initial_url: Option<&str>,
    deadline: &FetchDeadline,
) -> Result<(), Error> {
    if request_url == "about:blank" || initial_url.is_none() {
        return Ok(());
    }
    let initial_url = initial_url.unwrap_or_default();
    if urls_same_ignoring_fragment(request_url, initial_url) {
        return Ok(());
    }
    let timeout = deadline.bounded_remaining("wait_navigation_commit", Duration::from_secs(3))?;
    let wait_deadline = Instant::now() + timeout;
    while Instant::now() < wait_deadline {
        if let Ok(Some(current)) = capture_location(conn, session_id, deadline).await {
            if !urls_same_ignoring_fragment(&current, initial_url) && current != "about:blank" {
                return Ok(());
            }
        }
        sleep_until_or_deadline(wait_deadline, Duration::from_millis(50)).await;
    }
    Ok(())
}

fn validate_navigation_capture(
    request_url: &str,
    initial_url: Option<&str>,
    final_url: &str,
    main_entry: Option<&network_artifact::NetworkEntry>,
    snapshot: Option<&PageSnapshot>,
) -> Result<(), Error> {
    if request_url == "about:blank" {
        return Ok(());
    }
    let final_url = final_url.trim();
    if final_url.is_empty() || final_url == "about:blank" {
        return Err(Error::new(
            ErrorCode::NavigationStale,
            format!(
                "navigation did not commit: requested {request_url}, page stayed at {final_url:?}"
            ),
        ));
    }
    if let Some(initial) = initial_url {
        let stayed_on_initial = urls_same_ignoring_fragment(final_url, initial)
            && !urls_same_ignoring_fragment(request_url, initial);
        let conflicts_with_main = main_entry
            .map(|entry| {
                !entry.url.is_empty() && !urls_same_ignoring_fragment(final_url, &entry.url)
            })
            .unwrap_or(false);
        if stayed_on_initial && (main_entry.is_none() || conflicts_with_main) {
            return Err(Error::new(
                ErrorCode::NavigationStale,
                format!(
                    "navigation captured a stale page: requested {request_url}, initial {initial}, final {final_url}"
                ),
            ));
        }
    }
    if main_entry.is_none()
        && snapshot.is_some_and(|s| !s.has_dom_content() && s.ready_state == "complete")
    {
        return Err(Error::new(
            ErrorCode::ArtifactCaptureFailed,
            format!(
                "navigation to {request_url} did not produce a main document network request and the captured DOM was empty"
            ),
        ));
    }
    Ok(())
}

fn should_warn_main_not_observed(
    request_url: &str,
    final_url: &str,
    snapshot: Option<&PageSnapshot>,
) -> bool {
    if request_url == "about:blank" || final_url == "about:blank" {
        return true;
    }
    !snapshot.is_some_and(PageSnapshot::has_dom_content)
}

fn urls_same_ignoring_fragment(a: &str, b: &str) -> bool {
    fn normalized(raw: &str) -> String {
        match Url::parse(raw) {
            Ok(mut url) => {
                url.set_fragment(None);
                url.to_string()
            }
            Err(_) => raw.to_string(),
        }
    }
    normalized(a) == normalized(b)
}

pub(super) async fn cdp_send<P: Serialize>(
    conn: &Connection,
    session_id: &str,
    method: &str,
    params: &P,
    stage: &'static str,
    deadline: &FetchDeadline,
) -> Result<Value, Error> {
    let remaining = deadline.remaining(stage)?;
    conn.send_timeout(method, params, Some(session_id), remaining)
        .await
}

pub(super) async fn cdp_send_no_session<P: Serialize>(
    conn: &Connection,
    method: &str,
    params: &P,
    stage: &'static str,
    deadline: &FetchDeadline,
) -> Result<Value, Error> {
    let remaining = deadline.remaining(stage)?;
    conn.send_timeout(method, params, None, remaining).await
}

fn classify_navigate_error(error_text: &str) -> ErrorCode {
    if error_text.contains("NAME_NOT_RESOLVED") || error_text.contains("ICANN_NAME_COLLISION") {
        ErrorCode::DnsResolutionFailed
    } else if error_text.contains("CERT_") || error_text.contains("SSL_") {
        ErrorCode::TlsError
    } else if error_text.contains("CONNECTION_") || error_text.contains("ADDRESS_UNREACHABLE") {
        ErrorCode::TargetUnreachable
    } else {
        ErrorCode::HostUnreachable
    }
}

fn endpoint_is_remote(endpoint: &Endpoint) -> bool {
    match endpoint {
        Endpoint::Ws { .. } | Endpoint::Http { .. } => true,
        #[cfg(unix)]
        Endpoint::Unix { .. } => true,
    }
}

fn push_size_warning(
    warnings: &mut Vec<Warning>,
    artifact: Artifact,
    trimmed_bytes: usize,
    tiny_threshold: usize,
) {
    if trimmed_bytes == 0 {
        warnings.push(Warning {
            artifact,
            code: ErrorCode::ArtifactEmpty,
            detail: format!("{artifact} artifact was empty after trimming whitespace"),
        });
    } else if trimmed_bytes < tiny_threshold {
        warnings.push(Warning {
            artifact,
            code: ErrorCode::ArtifactTiny,
            detail: format!(
                "{artifact} artifact was {trimmed_bytes} bytes; threshold is {tiny_threshold}"
            ),
        });
    }
}

fn push_network_readiness_warnings(
    summary: &network_artifact::NetworkSummary,
    warnings: &mut Vec<Warning>,
) {
    if summary.inflight_total_at_capture > 0 {
        warnings.push(Warning {
            artifact: Artifact::Network,
            code: ErrorCode::NetworkNotIdle,
            detail: format!(
                "{} request(s) were still pending/responded at capture",
                summary.inflight_total_at_capture
            ),
        });
    }
    let pending_xhr = ["XHR", "Fetch", "EventSource"].iter().any(|kind| {
        summary
            .pending_by_resource_type
            .get(*kind)
            .copied()
            .unwrap_or(0)
            > 0
    });
    if pending_xhr {
        warnings.push(Warning {
            artifact: Artifact::Network,
            code: ErrorCode::PendingXhrAtCapture,
            detail: format!(
                "pending XHR/fetch/EventSource at capture: {:?}",
                summary.pending_by_resource_type
            ),
        });
    }
}

pub(super) async fn sleep_until_or_deadline(deadline: Instant, duration: Duration) {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return;
    }
    tokio::time::sleep(duration.min(remaining)).await;
}

/// Write per-connection frame/event data as JSONL under `network-bodies/`.
async fn write_frames_jsonl(
    paths: &ArtifactPaths,
    map: std::collections::HashMap<String, Vec<Value>>,
    warnings: &mut Vec<Warning>,
) {
    for (request_id, frames) in map {
        if frames.is_empty() {
            continue;
        }
        let dir = paths.network_bodies_dir();
        if let Err(e) = crate::sdk::fetch::writer::ensure_dir(&dir).await {
            warnings.push(Warning {
                artifact: Artifact::Network,
                code: e.error_code,
                detail: format!("frames jsonl mkdir: {}", e.detail),
            });
            continue;
        }
        let target = dir.join(format!("{request_id}.frames.jsonl"));
        let mut lines = String::new();
        for frame in &frames {
            if let Ok(s) = serde_json::to_string(frame) {
                lines.push_str(&s);
                lines.push('\n');
            }
        }
        if let Err(e) = crate::sdk::fetch::writer::write_bytes(&target, lines.as_bytes()).await {
            warnings.push(Warning {
                artifact: Artifact::Network,
                code: e.error_code,
                detail: format!("frames jsonl write {}: {}", target.display(), e.detail),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_navigate_error_buckets_chromium_symbols() {
        assert_eq!(
            classify_navigate_error("net::ERR_NAME_NOT_RESOLVED"),
            ErrorCode::DnsResolutionFailed
        );
        assert_eq!(
            classify_navigate_error("net::ERR_ICANN_NAME_COLLISION"),
            ErrorCode::DnsResolutionFailed
        );
        assert_eq!(
            classify_navigate_error("net::ERR_CERT_AUTHORITY_INVALID"),
            ErrorCode::TlsError
        );
        assert_eq!(
            classify_navigate_error("net::ERR_SSL_PROTOCOL_ERROR"),
            ErrorCode::TlsError
        );
        assert_eq!(
            classify_navigate_error("net::ERR_CONNECTION_REFUSED"),
            ErrorCode::TargetUnreachable
        );
        assert_eq!(
            classify_navigate_error("net::ERR_CONNECTION_TIMED_OUT"),
            ErrorCode::TargetUnreachable
        );
        assert_eq!(
            classify_navigate_error("net::ERR_ADDRESS_UNREACHABLE"),
            ErrorCode::TargetUnreachable
        );
        assert_eq!(
            classify_navigate_error("net::ERR_UNEXPECTED"),
            ErrorCode::HostUnreachable
        );
    }

    #[test]
    fn navigation_capture_rejects_stale_previous_page() {
        let err = validate_navigation_capture(
            "https://example.test/next",
            Some("https://example.test/previous"),
            "https://example.test/previous",
            None,
            Some(&PageSnapshot {
                url: "https://example.test/previous".into(),
                title: "Previous".into(),
                ready_state: "complete".into(),
                text: "previous page".into(),
                html: "<html><body>previous page</body></html>".into(),
            }),
        )
        .unwrap_err();
        assert_eq!(err.error_code, ErrorCode::NavigationStale);
    }

    #[test]
    fn navigation_capture_allows_requested_about_blank_without_main_request() {
        validate_navigation_capture(
            "about:blank",
            Some("about:blank"),
            "about:blank",
            None,
            Some(&PageSnapshot {
                url: "about:blank".into(),
                title: String::new(),
                ready_state: "complete".into(),
                text: String::new(),
                html: "<html><head></head><body></body></html>".into(),
            }),
        )
        .unwrap();
    }
}
