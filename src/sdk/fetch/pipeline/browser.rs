//! Browser-backed fetch path: CDP navigation, artifact capture, wait modes,
//! and network-body capture.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cookie::SameSite;
use serde_json::Value;
use url::Url;

use tokio::sync::broadcast;

use crate::sdk::cdp::{
    session,
    ws_client::{CdpEvent, Connection},
};
use crate::sdk::endpoint::Endpoint;
use crate::sdk::fetch::artifacts::{
    body as body_artifact,
    collectors::{ConsoleCollector, NetworkCollector},
    console as console_artifact, network as network_artifact,
    network_bodies as network_bodies_artifact, observation as observation_artifact,
    rendered_html as rendered_html_artifact, screenshot as screenshot_artifact,
    text as text_artifact,
};
use crate::sdk::fetch::result::{FetchResult, RenderDecision, Trace, Warning};
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
    validate_cookie_scope, BodyPayload, PreparedCookie, PreparedRequestOptions,
};
use super::NetworkBodies;

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
) -> Result<FetchResult, Error> {
    if builder.client.has_inline_host()
        && !builder.client.inline_host_started().await
        && builder.cookie_jar.is_none()
        && !builder.cookie_jar_disabled
    {
        let _ = builder.client.effective_endpoint().await?;
        super::cookie_jar_resolve::resolve_cookie_jar_path(&mut builder).await?;
    }
    writer::ensure_dir(&paths.root).await?;
    let endpoint = builder.client.endpoint();
    if !endpoint_is_remote(endpoint) {
        return Err(Error::new(
            ErrorCode::RenderUnavailable,
            "browser fetch requires --endpoint-url pointing at an afhttp host",
        ));
    }

    if let Some(jar_path) = &builder.cookie_jar {
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

    let conn = builder.client.cdp_connection().await?;

    let (target_id, session_id, close_target_after_fetch) = if let Some(tab) = builder.tab.as_ref()
    {
        let target_id = tab.as_str().to_string();
        let session_id = session::attach_to_target(&conn, &target_id).await?;
        (target_id, session_id, false)
    } else {
        let (target_id, session_id) = session::open_blank_target(&conn).await?;
        (target_id, session_id, true)
    };

    for method in [
        "Page.enable",
        "Runtime.enable",
        "Network.enable",
        "DOM.enable",
    ] {
        let _ = conn
            .send(method, &serde_json::json!({}), Some(&session_id))
            .await?;
    }

    if let Err(e) =
        apply_browser_request_options(&conn, &session_id, &builder.url, &request_options).await
    {
        finish_target(&conn, &target_id, &session_id, close_target_after_fetch).await;
        return Err(e);
    }

    let network_collector = NetworkCollector::start(
        &conn,
        builder.network_redact,
        builder.capture_ws,
        builder.capture_sse,
    );
    let console_collector = if builder.want.contains(&Artifact::Console) {
        Some(ConsoleCollector::start(&conn))
    } else {
        None
    };
    let download_dir = configure_download_capture(&builder.client, &conn).await;
    let mut download_events = download_dir.as_ref().map(|_| conn.subscribe());
    let downloads_before = if let Some(dir) = download_dir.as_ref() {
        snapshot_download_dir(dir).await.unwrap_or_default()
    } else {
        BTreeSet::new()
    };

    let nav_start = Instant::now();
    let navigate = navigate_with_method(
        &conn,
        &session_id,
        &builder.url,
        &request_options.method,
        &request_options.body_payload,
        builder.timeout,
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

    if let Err(e) = wait_for_condition(&conn, &session_id, &builder.wait, builder.timeout).await {
        finish_target(&conn, &target_id, &session_id, close_target_after_fetch).await;
        if e.error_code == ErrorCode::CdpTimeout {
            return Err(Error::new(ErrorCode::NavigationTimeout, e.detail));
        }
        return Err(e);
    }
    let nav_duration = nav_start.elapsed();

    if let Err(e) = evaluate_after_wait(&conn, &session_id, &request_options).await {
        finish_target(&conn, &target_id, &session_id, close_target_after_fetch).await;
        return Err(e);
    }

    let mut warnings: Vec<Warning> = Vec::new();
    let mut result = FetchResult {
        request_id: request_id.clone(),
        url: builder.url.clone(),
        final_url: builder.url.clone(),
        status: 0,
        tab_id: Some(TabId::new(target_id.clone())),
        trace: Trace {
            render_decision: RenderDecision::Browser,
            render_mode: builder.render.as_trace(),
            render_used: true,
            escalation_reason,
            main_request_observed: false,
            duration_ms: 0,
            navigation_duration_ms: Some(duration_ms(nav_duration)),
            cookie_jar_file: builder.cookie_jar.clone(),
            cookie_jar_warning: builder.cookie_jar_warning.clone(),
            sensitive_capture: super::sensitive_capture(&builder),
        },
        warnings: Vec::new(),
        body_file: None,
        rendered_html_file: None,
        text_file: None,
        screenshot_file: None,
        network_file: None,
        console_file: None,
        observation_file: None,
        storage_file: None,
        download_file: None,
        download_bytes: None,
        download_filename: None,
        download_url: None,
        download_state: None,
    };

    let main_entry = network_collector
        .wait_for_main_status(Duration::from_millis(builder.observe_main_wait_ms))
        .await;
    result.trace.main_request_observed = main_entry.is_some();

    if let Ok(doc) = conn
        .send(
            "Runtime.evaluate",
            &serde_json::json!({
                "expression": "JSON.stringify({url: location.href})",
                "returnByValue": true,
            }),
            Some(&session_id),
        )
        .await
    {
        if let Some(s) = doc["result"]["value"].as_str() {
            if let Ok(v) = serde_json::from_str::<Value>(s) {
                if let Some(u) = v.get("url").and_then(|x| x.as_str()) {
                    result.final_url = u.to_string();
                }
            }
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
    } else {
        warnings.push(Warning {
            artifact: Artifact::Network,
            code: ErrorCode::ArtifactCaptureFailed,
            detail: "main document network request was not observed".into(),
        });
    }
    if result.status == 0 {
        if let Some(status) = navigation_status_from_performance(&conn, &session_id).await {
            result.status = status;
        }
    }

    if builder.want.contains(&Artifact::RenderedHtml) {
        match capture_outer_html(&conn, &session_id).await {
            Ok(html) => {
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
        match capture_inner_text(&conn, &session_id).await {
            Ok(text) => {
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

    if builder.want.contains(&Artifact::Screenshot) {
        match capture_screenshot(&conn, &session_id).await {
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
            match capture_response_body(&conn, &session_id, &main.request_id).await {
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
                    code: e.error_code,
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
        match observation_artifact::capture(&conn, &session_id, &result.final_url).await {
            Ok(obs) => {
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
        match crate::sdk::fetch::artifacts::storage::capture(&conn, &session_id, &result.final_url)
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
        let log = collector.finish().await;
        if let Ok(path) = console_artifact::write(&paths, &log).await {
            result.set_artifact_file(Artifact::Console, path);
        }
    }

    // Write WS frame / SSE event JSONL files.
    if builder.capture_ws {
        write_frames_jsonl(
            &paths,
            network_collector.take_ws_frames().await,
            &mut warnings,
        )
        .await;
    }
    if builder.capture_sse {
        write_frames_jsonl(
            &paths,
            network_collector.take_sse_events().await,
            &mut warnings,
        )
        .await;
    }

    if !matches!(builder.network_bodies, NetworkBodies::Off) {
        capture_network_bodies(
            &conn,
            &session_id,
            &network_collector,
            &paths,
            builder.network_bodies,
            builder.network_body_max_bytes,
            &mut warnings,
        )
        .await;
    }
    if builder.want.contains(&Artifact::Network) {
        let log = network_collector.finish().await;
        if let Ok(path) = network_artifact::write(&paths, &log).await {
            result.set_artifact_file(Artifact::Network, path);
        }
    } else {
        let _ = network_collector.finish().await;
    }

    if let Some(jar_path) = &builder.cookie_jar {
        if let Err(e) =
            sync_browser_cookies_to_jar(&conn, &session_id, &result.final_url, jar_path).await
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
    result.trace.duration_ms = duration_ms(start.elapsed());
    Ok(result)
}

struct DownloadNavigation {
    builder: FetchBuilder,
    conn: std::sync::Arc<Connection>,
    network_collector: NetworkCollector,
    console_collector: Option<ConsoleCollector>,
    download_dir: Option<PathBuf>,
    download_start: Option<DownloadStart>,
    downloads_before: BTreeSet<PathBuf>,
    request_id: RequestId,
    paths: ArtifactPaths,
    start: Instant,
    nav_duration: Duration,
    escalation_reason: Option<String>,
    target_id: String,
    session_id: String,
    close_target_after_fetch: bool,
}

async fn finish_download_navigation(ctx: DownloadNavigation) -> Result<FetchResult, Error> {
    let DownloadNavigation {
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
        nav_duration,
        escalation_reason,
        target_id,
        session_id,
        close_target_after_fetch,
    } = ctx;
    let Some(download_dir) = download_dir else {
        finish_target(&conn, &target_id, &session_id, close_target_after_fetch).await;
        return Err(Error::new(
            ErrorCode::ArtifactCaptureFailed,
            "navigation became a download, but the host profile download directory was unavailable",
        ));
    };
    let downloaded =
        match wait_for_download_file(&download_dir, &downloads_before, builder.timeout).await {
            Ok(file) => file,
            Err(e) => {
                finish_target(&conn, &target_id, &session_id, close_target_after_fetch).await;
                return Err(e);
            }
        };
    let main_entry = network_collector
        .wait_for_main_status(Duration::from_millis(builder.observe_main_wait_ms))
        .await;
    let mut result = FetchResult {
        request_id,
        url: builder.url.clone(),
        final_url: builder.url.clone(),
        status: main_entry.as_ref().and_then(|e| e.status).unwrap_or(0),
        tab_id: Some(TabId::new(target_id.clone())),
        trace: Trace {
            render_decision: RenderDecision::Browser,
            render_mode: builder.render.as_trace(),
            render_used: true,
            escalation_reason,
            main_request_observed: main_entry.is_some(),
            duration_ms: 0,
            navigation_duration_ms: Some(duration_ms(nav_duration)),
            cookie_jar_file: builder.cookie_jar.clone(),
            cookie_jar_warning: builder.cookie_jar_warning.clone(),
            sensitive_capture: super::sensitive_capture(&builder),
        },
        warnings: Vec::new(),
        body_file: None,
        rendered_html_file: None,
        text_file: None,
        screenshot_file: None,
        network_file: None,
        console_file: None,
        observation_file: None,
        storage_file: None,
        download_file: Some(downloaded.path.clone()),
        download_bytes: Some(downloaded.bytes),
        download_filename: Some(downloaded.filename),
        download_url: download_start
            .as_ref()
            .and_then(|start| start.url.clone())
            .or_else(|| Some(builder.url.clone())),
        download_state: Some("completed".to_string()),
    };
    if let Some(collector) = console_collector {
        let log = collector.finish().await;
        if let Ok(path) = console_artifact::write(&paths, &log).await {
            result.set_artifact_file(Artifact::Console, path);
        }
    }
    if builder.want.contains(&Artifact::Network) {
        let log = network_collector.finish().await;
        if let Ok(path) = network_artifact::write(&paths, &log).await {
            result.set_artifact_file(Artifact::Network, path);
        }
    } else {
        let _ = network_collector.finish().await;
    }
    finish_target(&conn, &target_id, &session_id, close_target_after_fetch).await;
    result.trace.duration_ms = duration_ms(start.elapsed());
    Ok(result)
}

async fn configure_download_capture(
    client: &crate::sdk::Client,
    conn: &Connection,
) -> Option<PathBuf> {
    let info = client.profile_info().await.ok()?;
    let profile_dir = info.path.as_ref()?;
    let download_dir = profile_dir.join("downloads");
    tokio::fs::create_dir_all(&download_dir).await.ok()?;
    conn.send(
        "Browser.setDownloadBehavior",
        &serde_json::json!({
            "behavior": "allow",
            "downloadPath": download_dir.display().to_string(),
            "eventsEnabled": true,
        }),
        None,
    )
    .await
    .ok()?;
    Some(download_dir)
}

#[derive(Debug, Clone)]
struct DownloadStart {
    url: Option<String>,
}

async fn wait_for_download_start(
    rx: &mut broadcast::Receiver<CdpEvent>,
    timeout: Duration,
    session_id: &str,
) -> Option<DownloadStart> {
    if let Some(start) = poll_download_start(rx, session_id) {
        return Some(start);
    }
    if timeout.is_zero() {
        return None;
    }
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(event)) => {
                if let Some(start) = download_start_from_event(&event, session_id) {
                    return Some(start);
                }
            }
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(broadcast::error::RecvError::Closed)) | Err(_) => return None,
        }
    }
}

fn poll_download_start(
    rx: &mut broadcast::Receiver<CdpEvent>,
    session_id: &str,
) -> Option<DownloadStart> {
    loop {
        match rx.try_recv() {
            Ok(event) => {
                if let Some(start) = download_start_from_event(&event, session_id) {
                    return Some(start);
                }
            }
            Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
            Err(broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed) => {
                return None;
            }
        }
    }
}

fn download_start_from_event(event: &CdpEvent, session_id: &str) -> Option<DownloadStart> {
    match event.method.as_str() {
        "Browser.downloadWillBegin" => {}
        "Page.downloadWillBegin" if event.session_id.as_deref() == Some(session_id) => {}
        "Page.downloadWillBegin" => return None,
        _ => return None,
    }
    Some(DownloadStart {
        url: event
            .params
            .get("url")
            .and_then(|v| v.as_str())
            .map(str::to_string),
    })
}

fn navigate_has_download_flag(navigate: &serde_json::Value) -> bool {
    navigate
        .get("isDownload")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

fn navigation_became_download(
    navigate: &serde_json::Value,
    download_start: Option<&DownloadStart>,
) -> bool {
    download_start.is_some() || navigate_has_download_flag(navigate)
}

#[derive(Debug)]
struct CompletedDownload {
    path: PathBuf,
    bytes: u64,
    filename: String,
}

async fn snapshot_download_dir(dir: &Path) -> std::io::Result<BTreeSet<PathBuf>> {
    let mut out = BTreeSet::new();
    let mut read_dir = tokio::fs::read_dir(dir).await?;
    while let Some(entry) = read_dir.next_entry().await? {
        out.insert(entry.path());
    }
    Ok(out)
}

async fn wait_for_download_file(
    dir: &Path,
    before: &BTreeSet<PathBuf>,
    timeout: Duration,
) -> Result<CompletedDownload, Error> {
    let deadline = Instant::now() + timeout;
    loop {
        if Instant::now() >= deadline {
            return Err(Error::new(
                ErrorCode::ArtifactCaptureFailed,
                format!("download did not complete in {}", dir.display()),
            ));
        }
        if let Some(path) = newest_completed_download(dir, before).await {
            let size1 = tokio::fs::metadata(&path).await.map(|m| m.len()).ok();
            tokio::time::sleep(Duration::from_millis(100)).await;
            let size2 = tokio::fs::metadata(&path).await.map(|m| m.len()).ok();
            if let (Some(a), Some(b)) = (size1, size2) {
                if a == b {
                    let filename = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("download")
                        .to_string();
                    return Ok(CompletedDownload {
                        path,
                        bytes: b,
                        filename,
                    });
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn newest_completed_download(dir: &Path, before: &BTreeSet<PathBuf>) -> Option<PathBuf> {
    let mut read_dir = tokio::fs::read_dir(dir).await.ok()?;
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    while let Ok(Some(entry)) = read_dir.next_entry().await {
        let path = entry.path();
        if before.contains(&path) || path.extension().and_then(|e| e.to_str()) == Some("crdownload")
        {
            continue;
        }
        let Ok(meta) = entry.metadata().await else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let modified = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        if newest
            .as_ref()
            .map(|(current, _)| modified > *current)
            .unwrap_or(true)
        {
            newest = Some((modified, path));
        }
    }
    newest.map(|(_, path)| path)
}

async fn apply_browser_request_options(
    conn: &Connection,
    session_id: &str,
    url: &str,
    request_options: &PreparedRequestOptions,
) -> Result<(), Error> {
    let extra_headers = request_options.cdp_extra_headers();
    if !extra_headers.is_empty() {
        conn.send(
            "Network.setExtraHTTPHeaders",
            &serde_json::json!({ "headers": extra_headers }),
            Some(session_id),
        )
        .await?;
    }

    if let Some((user_agent, _)) = &request_options.user_agent {
        conn.send(
            "Network.setUserAgentOverride",
            &serde_json::json!({ "userAgent": user_agent }),
            Some(session_id),
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
        conn.send(
            "Network.setCookies",
            &serde_json::json!({ "cookies": params }),
            Some(session_id),
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
    if let Some(expires) = cookie.expires_unix {
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
) -> Result<(), Error> {
    for (idx, js) in request_options.evaluate_after_wait.iter().enumerate() {
        let result = conn
            .send(
                "Runtime.evaluate",
                &serde_json::json!({
                    "expression": js,
                    "awaitPromise": true,
                    "returnByValue": false,
                }),
                Some(session_id),
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

async fn finish_target(
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

async fn capture_response_body(
    conn: &Connection,
    session_id: &str,
    request_id: &str,
) -> Result<Vec<u8>, Error> {
    let resp = conn
        .send(
            "Network.getResponseBody",
            &serde_json::json!({"requestId": request_id}),
            Some(session_id),
        )
        .await?;
    decode_response_body(request_id, &resp)
}

fn decode_response_body(request_id: &str, resp: &serde_json::Value) -> Result<Vec<u8>, Error> {
    let body_str = resp.get("body").and_then(|v| v.as_str()).unwrap_or("");
    let base64_encoded = resp
        .get("base64Encoded")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if base64_encoded {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .decode(body_str)
            .map_err(|e| {
                Error::new(
                    ErrorCode::ArtifactCaptureFailed,
                    format!("base64 decode for {request_id}: {e}"),
                )
            })
    } else {
        Ok(body_str.as_bytes().to_vec())
    }
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

async fn capture_network_bodies(
    conn: &Connection,
    session_id: &str,
    collector: &NetworkCollector,
    paths: &ArtifactPaths,
    mode: NetworkBodies,
    max_bytes: u64,
    warnings: &mut Vec<Warning>,
) {
    let finished = collector.take_finished().await;
    for request_id in finished {
        let Some(entry) = collector.entry(&request_id).await else {
            continue;
        };
        if !network_bodies_eligible(mode, &entry.resource_type) {
            continue;
        }
        let resp = match conn
            .send(
                "Network.getResponseBody",
                &serde_json::json!({"requestId": request_id}),
                Some(session_id),
            )
            .await
        {
            Ok(v) => v,
            Err(e) => {
                warnings.push(Warning {
                    artifact: Artifact::Network,
                    code: ErrorCode::ArtifactCaptureFailed,
                    detail: format!("getResponseBody({request_id}): {}", e.detail),
                });
                continue;
            }
        };
        let bytes = match decode_response_body(&request_id, &resp) {
            Ok(bytes) => bytes,
            Err(e) => {
                warnings.push(Warning {
                    artifact: Artifact::Network,
                    code: e.error_code,
                    detail: e.detail,
                });
                continue;
            }
        };
        let truncated = bytes.len() as u64 > max_bytes;
        let max_len = usize::try_from(max_bytes).unwrap_or(usize::MAX);
        let final_bytes: &[u8] = if truncated {
            &bytes[..max_len.min(bytes.len())]
        } else {
            &bytes
        };
        let ext = ext_for_mime(entry.mime_type.as_deref());
        match network_bodies_artifact::write(paths, &request_id, ext, final_bytes).await {
            Ok(path) => {
                collector.set_body_file(&request_id, path).await;
                if truncated {
                    warnings.push(Warning {
                        artifact: Artifact::Network,
                        code: ErrorCode::NetworkBodyTruncated,
                        detail: format!("body for {request_id} truncated to {max_bytes} bytes"),
                    });
                }
                if entry
                    .mime_type
                    .as_deref()
                    .is_some_and(|m| m.contains("json"))
                    && serde_json::from_slice::<serde_json::Value>(final_bytes).is_ok()
                {
                    collector
                        .set_hint(&request_id, "json_valid", serde_json::Value::Bool(true))
                        .await;
                }
            }
            Err(e) => warnings.push(Warning {
                artifact: Artifact::Network,
                code: e.error_code,
                detail: e.detail,
            }),
        }
    }
}

fn network_bodies_eligible(mode: NetworkBodies, resource_type: &str) -> bool {
    match mode {
        NetworkBodies::Off => false,
        NetworkBodies::All => true,
        NetworkBodies::Xhr => matches!(resource_type, "XHR" | "Fetch" | "EventSource"),
    }
}

fn ext_for_mime(mime: Option<&str>) -> &'static str {
    match mime.unwrap_or("") {
        m if m.starts_with("application/json") => "json",
        m if m.starts_with("text/html") => "html",
        m if m.starts_with("text/css") => "css",
        m if m.starts_with("application/javascript") || m.starts_with("text/javascript") => "js",
        m if m.starts_with("text/plain") => "txt",
        m if m.starts_with("image/png") => "png",
        m if m.starts_with("image/jpeg") => "jpg",
        m if m.starts_with("image/webp") => "webp",
        m if m.starts_with("image/svg") => "svg",
        m if m.starts_with("text/event-stream") => "txt",
        _ => "bin",
    }
}

async fn wait_for_condition(
    conn: &Connection,
    session_id: &str,
    wait: &Wait,
    timeout: Duration,
) -> Result<(), Error> {
    let sid = session_id.to_string();
    match wait {
        Wait::Load => {
            if let Ok(r) = conn
                .send(
                    "Runtime.evaluate",
                    &serde_json::json!({
                        "expression": "document.readyState",
                        "returnByValue": true,
                    }),
                    Some(session_id),
                )
                .await
            {
                if r["result"]["value"].as_str() == Some("complete") {
                    return Ok(());
                }
            }
            let event_wait = async {
                conn.wait_event(timeout, |ev| {
                    ev.method == "Page.loadEventFired" && ev.session_id.as_deref() == Some(&sid)
                })
                .await
                .map(|_| ())
            };
            let poll = async {
                let deadline = Instant::now() + timeout;
                while Instant::now() < deadline {
                    if let Ok(r) = conn
                        .send(
                            "Runtime.evaluate",
                            &serde_json::json!({
                                "expression": "document.readyState",
                                "returnByValue": true,
                            }),
                            Some(session_id),
                        )
                        .await
                    {
                        if r["result"]["value"].as_str() == Some("complete") {
                            return Ok::<(), Error>(());
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(Error::new(
                    ErrorCode::CdpTimeout,
                    "Wait::Load: readyState never became complete",
                ))
            };
            tokio::select! {
                r = event_wait => r,
                r = poll => r,
            }
        }
        Wait::Idle => {
            let _ = conn
                .send(
                    "Page.setLifecycleEventsEnabled",
                    &serde_json::json!({"enabled": true}),
                    Some(session_id),
                )
                .await;
            conn.wait_event(timeout, |ev| {
                ev.method == "Page.lifecycleEvent"
                    && ev.params["name"].as_str() == Some("networkIdle")
                    && ev.session_id.as_deref() == Some(&sid)
            })
            .await?;
            Ok(())
        }
        Wait::Selector(sel) => {
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                let expr = format!(
                    "!!document.querySelector({})",
                    serde_json::to_string(sel).unwrap_or_else(|_| "null".into())
                );
                let r = conn
                    .send(
                        "Runtime.evaluate",
                        &serde_json::json!({
                            "expression": expr,
                            "returnByValue": true,
                        }),
                        Some(session_id),
                    )
                    .await?;
                if r["result"]["value"].as_bool() == Some(true) {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(Error::new(
                ErrorCode::WaitSelectorUnmatched,
                format!("selector {sel:?} did not appear before --timeout"),
            ))
        }
        Wait::SelectorVisible(sel) => {
            let deadline = Instant::now() + timeout;
            let json_sel = serde_json::to_string(sel).unwrap_or_else(|_| "null".into());
            let expr = format!(
                "(function(){{\
                   const el = document.querySelector({json_sel});\
                   if (!el) return false;\
                   const r = el.getBoundingClientRect();\
                   if (r.width === 0 || r.height === 0) return false;\
                   const style = window.getComputedStyle(el);\
                   if (style.visibility === 'hidden') return false;\
                   if (style.display === 'none') return false;\
                   if (style.opacity === '0') return false;\
                   if (style.position !== 'fixed' && el.offsetParent === null) return false;\
                   return true;\
                 }})()"
            );
            while Instant::now() < deadline {
                let r = conn
                    .send(
                        "Runtime.evaluate",
                        &serde_json::json!({
                            "expression": expr,
                            "returnByValue": true,
                        }),
                        Some(session_id),
                    )
                    .await?;
                if r["result"]["value"].as_bool() == Some(true) {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(Error::new(
                ErrorCode::WaitSelectorUnmatched,
                format!("selector-visible {sel:?} did not appear visible before --timeout"),
            ))
        }
        Wait::Ms(n) => {
            tokio::time::sleep(Duration::from_millis(*n)).await;
            Ok(())
        }
    }
}

async fn navigation_status_from_performance(conn: &Connection, session_id: &str) -> Option<u16> {
    let r = conn
        .send(
            "Runtime.evaluate",
            &serde_json::json!({
                "expression": "(() => { const nav = performance.getEntriesByType('navigation')[0]; return nav && Number.isFinite(nav.responseStatus) ? nav.responseStatus : 0; })()",
                "returnByValue": true,
            }),
            Some(session_id),
        )
        .await
        .ok()?;
    let status = r["result"]["value"].as_u64()?;
    if (100..=599).contains(&status) {
        Some(status as u16)
    } else {
        None
    }
}

async fn capture_outer_html(conn: &Connection, session_id: &str) -> Result<String, Error> {
    let dom_outer = async {
        let doc = conn
            .send(
                "DOM.getDocument",
                &serde_json::json!({"depth": -1, "pierce": true}),
                Some(session_id),
            )
            .await?;
        let node_id = doc["root"]["nodeId"].as_i64().ok_or_else(|| {
            Error::new(ErrorCode::CdpError, "DOM.getDocument: missing root nodeId")
        })?;
        let outer = conn
            .send(
                "DOM.getOuterHTML",
                &serde_json::json!({"nodeId": node_id}),
                Some(session_id),
            )
            .await?;
        outer["outerHTML"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| Error::new(ErrorCode::CdpError, "DOM.getOuterHTML: missing outerHTML"))
    }
    .await;

    match dom_outer {
        Ok(html) if !html.is_empty() => Ok(html),
        _ => capture_outer_html_via_runtime(conn, session_id).await,
    }
}

async fn capture_outer_html_via_runtime(
    conn: &Connection,
    session_id: &str,
) -> Result<String, Error> {
    let r = conn
        .send(
            "Runtime.evaluate",
            &serde_json::json!({
                "expression": "document.documentElement ? document.documentElement.outerHTML : ''",
                "returnByValue": true,
            }),
            Some(session_id),
        )
        .await?;
    Ok(r["result"]["value"].as_str().unwrap_or("").to_string())
}

async fn capture_inner_text(conn: &Connection, session_id: &str) -> Result<String, Error> {
    let r = conn
        .send(
            "Runtime.evaluate",
            &serde_json::json!({
                "expression": "document.body ? document.body.innerText : ''",
                "returnByValue": true,
            }),
            Some(session_id),
        )
        .await?;
    Ok(r["result"]["value"].as_str().unwrap_or("").to_string())
}

/// Navigate to `url` using `method`. For GET (the default) this is a plain
/// `Page.navigate`. For other methods, `Fetch.enable` is used to intercept
/// the first Document request and reissue it with the correct method and body.
async fn navigate_with_method(
    conn: &Connection,
    session_id: &str,
    url: &str,
    method: &str,
    body_payload: &BodyPayload,
    timeout: Duration,
) -> Result<serde_json::Value, Error> {
    let is_get = method.eq_ignore_ascii_case("GET");
    let has_body = !matches!(body_payload, BodyPayload::None);

    if is_get && !has_body {
        // Standard GET navigation — no interception needed.
        let mut navigate_params = serde_json::json!({"url": url});
        if let Ok(frame_tree) = conn
            .send(
                "Page.getFrameTree",
                &serde_json::json!({}),
                Some(session_id),
            )
            .await
        {
            if let Some(frame_id) = frame_tree
                .pointer("/frameTree/frame/id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                if let Some(obj) = navigate_params.as_object_mut() {
                    obj.insert("frameId".into(), serde_json::json!(frame_id));
                }
            }
        }
        return conn
            .send("Page.navigate", &navigate_params, Some(session_id))
            .await;
    }

    // Non-GET or body present: intercept the navigation request and override
    // its method/body via Fetch.enable before the network layer sends it.
    conn.send(
        "Fetch.enable",
        &serde_json::json!({
            "patterns": [{"resourceType": "Document", "requestStage": "Request"}]
        }),
        Some(session_id),
    )
    .await?;

    let sid = session_id.to_string();
    let method_str = method.to_string();
    let post_data_b64: Option<String> = match body_payload {
        BodyPayload::None => None,
        BodyPayload::Bytes(b) => {
            use base64::Engine;
            Some(base64::engine::general_purpose::STANDARD.encode(b))
        }
        BodyPayload::Form(fields) => {
            let encoded = fields
                .iter()
                .map(|(k, v)| format!("{}={}", urlencoding_simple(k), urlencoding_simple(v)))
                .collect::<Vec<_>>()
                .join("&");
            use base64::Engine;
            Some(base64::engine::general_purpose::STANDARD.encode(encoded.as_bytes()))
        }
    };

    // Register the intercept listener BEFORE sending Page.navigate so we
    // don't miss the Fetch.requestPaused event.
    let intercept = {
        let sid2 = sid.clone();
        async move {
            let ev = conn
                .wait_event(timeout, |ev| {
                    ev.method == "Fetch.requestPaused"
                        && ev.session_id.as_deref() == Some(&sid2)
                        && ev.params.get("resourceType").and_then(|v| v.as_str())
                            == Some("Document")
                })
                .await?;
            let request_id = ev.params["requestId"].as_str().unwrap_or("").to_string();
            let mut params = serde_json::json!({
                "requestId": request_id,
                "method": method_str,
            });
            if let Some(b64) = post_data_b64 {
                params["postData"] = serde_json::json!(b64);
            }
            if matches!(body_payload, BodyPayload::Form(_)) {
                // Inject Content-Type for form-encoded bodies.
                params["headers"] = serde_json::json!([
                    {"name": "Content-Type", "value": "application/x-www-form-urlencoded"}
                ]);
            }
            conn.send("Fetch.continueRequest", &params, Some(&sid2))
                .await?;
            conn.send("Fetch.disable", &serde_json::json!({}), Some(&sid2))
                .await
        }
    };

    let nav_params = serde_json::json!({"url": url});
    let navigate = conn.send("Page.navigate", &nav_params, Some(&sid));

    let (nav_result, intercept_result) = tokio::join!(navigate, intercept);
    intercept_result?;
    nav_result
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

fn urlencoding_simple(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            other => {
                out.push('%');
                out.push_str(&format!("{other:02X}"));
            }
        }
    }
    out
}

async fn capture_screenshot(conn: &Connection, session_id: &str) -> Result<Vec<u8>, Error> {
    let r = conn
        .send(
            "Page.captureScreenshot",
            &serde_json::json!({"format": "png", "captureBeyondViewport": false}),
            Some(session_id),
        )
        .await?;
    let b64 = r["data"]
        .as_str()
        .ok_or_else(|| Error::new(ErrorCode::CdpError, "captureScreenshot: missing data"))?;
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| {
            Error::new(
                ErrorCode::ArtifactCaptureFailed,
                format!("screenshot base64 decode: {e}"),
            )
        })
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
    fn err_aborted_is_not_a_download_without_cdp_download_event() {
        let navigate = serde_json::json!({"errorText": "net::ERR_ABORTED"});
        assert!(!navigation_became_download(&navigate, None));
        assert!(navigation_became_download(
            &navigate,
            Some(&DownloadStart {
                url: Some("https://example.test/file.bin".into()),
            }),
        ));
    }
}
