//! Download navigation handling: detect when a navigation turns into a file
//! download, wait for the file to settle on disk, and build the download
//! `FetchResult`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tokio::sync::broadcast;

use crate::sdk::cdp::ws_client::{CdpEvent, Connection};
use crate::sdk::fetch::FetchBuilder;
use crate::sdk::fetch::artifacts::{console as console_artifact, network as network_artifact};
use crate::sdk::fetch::deadline::FetchDeadline;
use crate::sdk::fetch::pipeline::sensitive_capture;
use crate::sdk::fetch::result::{FetchResult, RenderDecision};
use crate::shared::artifacts::Artifact;
use crate::shared::error::{Error, ErrorCode};
use crate::shared::ids::{RequestId, TabId};
use crate::shared::time::duration_ms;

use super::{cdp_send_no_session, finish_failed_target, finish_target};

pub(super) struct DownloadNavigation {
    pub(super) builder: FetchBuilder,
    pub(super) conn: std::sync::Arc<Connection>,
    pub(super) network_collector: crate::sdk::fetch::artifacts::collectors::NetworkCollector,
    pub(super) console_collector:
        Option<crate::sdk::fetch::artifacts::collectors::ConsoleCollector>,
    pub(super) download_dir: Option<PathBuf>,
    pub(super) download_start: Option<DownloadStart>,
    pub(super) downloads_before: BTreeSet<PathBuf>,
    pub(super) request_id: RequestId,
    pub(super) paths: crate::shared::artifacts::ArtifactPaths,
    pub(super) start: Instant,
    pub(super) nav_duration: Duration,
    pub(super) escalation_reason: Option<String>,
    pub(super) target_id: String,
    pub(super) session_id: String,
    pub(super) main_frame_id: Option<String>,
    pub(super) created_target: bool,
    pub(super) deadline: FetchDeadline,
}

pub(super) async fn finish_download_navigation(
    ctx: DownloadNavigation,
) -> Result<FetchResult, Error> {
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
        main_frame_id,
        created_target,
        deadline,
    } = ctx;
    let Some(download_dir) = download_dir else {
        finish_failed_target(
            &conn,
            &target_id,
            &session_id,
            created_target && !builder.keep_tab_open,
            &deadline,
        )
        .await;
        return Err(Error::new(
            ErrorCode::ArtifactCaptureFailed,
            "navigation became a download, but the host profile download directory was unavailable",
        ));
    };
    // The download directory is the profile's, shared by every fetch on it:
    // only a file named for this tab's download is this fetch's.
    let expected_name = download_start
        .as_ref()
        .and_then(|start| start.suggested_filename.as_deref());
    let downloaded = match wait_for_download_with_deadline(
        &download_dir,
        &downloads_before,
        expected_name,
        &deadline,
    )
    .await
    {
        Ok(file) => file,
        Err(e) => {
            finish_failed_target(
                &conn,
                &target_id,
                &session_id,
                created_target && !builder.keep_tab_open,
                &deadline,
            )
            .await;
            return Err(e);
        }
    };
    let main_entry = network_collector
        .wait_for_main_status(
            Duration::from_millis(builder.readiness.observe_main_wait_ms),
            main_frame_id.as_deref(),
        )
        .await;
    deadline.update_trace(|trace| {
        trace.render_decision = RenderDecision::Browser;
        trace.render_mode = builder.render.as_trace();
        trace.render_used = true;
        trace.escalation_reason = escalation_reason.clone();
        trace.main_request_observed = main_entry.is_some();
        trace.navigation_duration_ms = Some(duration_ms(nav_duration));
        trace.wait_mode = Some(builder.wait.mode_name().to_string());
        trace.wait_satisfied_by = Some("download".into());
        trace.capture_reason = Some("download".into());
        trace.cookie_jar_file = builder.cookie_jar.path.clone();
        trace.cookie_jar_warning = builder.cookie_jar.warning.clone();
        trace.sensitive_capture = sensitive_capture(&builder);
    });
    let mut result = FetchResult::new(request_id, builder.url.clone(), deadline.snapshot());
    result.status = main_entry.as_ref().and_then(|e| e.status).unwrap_or(0);
    result.tab_id = Some(TabId::new(target_id.clone()));
    result.download_file = Some(downloaded.path.clone());
    result.download_bytes = Some(downloaded.bytes);
    result.download_filename = Some(downloaded.filename);
    result.download_url = download_start
        .as_ref()
        .and_then(|start| start.url.clone())
        .or_else(|| Some(builder.url.clone()));
    result.download_state = Some("completed".to_string());
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
    if finish_target(&conn, &target_id, &session_id, created_target).await {
        result.tab_id = None;
    } else if created_target {
        result.warnings.push(crate::sdk::fetch::result::Warning {
            artifact: Artifact::RenderedHtml,
            code: ErrorCode::CdpError,
            detail: "completed download retained its tab because safe cleanup failed".into(),
        });
        finish_target(&conn, &target_id, &session_id, false).await;
    }
    deadline.update_trace(|trace| {
        trace.duration_ms = duration_ms(start.elapsed());
    });
    result.trace = deadline.complete_trace();
    Ok(result)
}

pub(super) async fn configure_download_capture(
    client: &crate::sdk::Client,
    conn: &Connection,
    deadline: &FetchDeadline,
) -> Option<PathBuf> {
    let remaining = deadline.remaining("configure_download_capture").ok()?;
    let info = tokio::time::timeout(remaining, client.profile_info())
        .await
        .ok()?
        .ok()?;
    let profile_dir = info.path.as_ref()?;
    let download_dir = profile_dir.join("downloads");
    tokio::fs::create_dir_all(&download_dir).await.ok()?;
    cdp_send_no_session(
        conn,
        "Browser.setDownloadBehavior",
        &serde_json::json!({
            "behavior": "allow",
            "downloadPath": download_dir.display().to_string(),
            "eventsEnabled": true,
        }),
        "configure_download_capture",
        deadline,
    )
    .await
    .ok()?;
    Some(download_dir)
}

#[derive(Debug, Clone)]
pub(super) struct DownloadStart {
    url: Option<String>,
    suggested_filename: Option<String>,
}

/// What identifies this fetch's tab in download events. `Page.*` events
/// carry the session; `Browser.downloadWillBegin` carries only the frame,
/// which for a navigation download is the tab's root frame.
pub(super) struct DownloadFrames<'a> {
    pub(super) session_id: &'a str,
    pub(super) target_id: &'a str,
    pub(super) main_frame_id: Option<&'a str>,
}

impl DownloadFrames<'_> {
    fn owns(&self, event: &CdpEvent) -> bool {
        match event.method.as_str() {
            "Page.downloadWillBegin" => event.session_id.as_deref() == Some(self.session_id),
            "Browser.downloadWillBegin" => event
                .params
                .get("frameId")
                .and_then(|v| v.as_str())
                .is_some_and(|frame| frame == self.target_id || Some(frame) == self.main_frame_id),
            _ => false,
        }
    }
}

pub(super) async fn wait_for_download_start(
    rx: &mut broadcast::Receiver<CdpEvent>,
    timeout: Duration,
    frames: &DownloadFrames<'_>,
) -> Option<DownloadStart> {
    if let Some(start) = poll_download_start(rx, frames) {
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
                if let Some(start) = download_start_from_event(&event, frames) {
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
    frames: &DownloadFrames<'_>,
) -> Option<DownloadStart> {
    loop {
        match rx.try_recv() {
            Ok(event) => {
                if let Some(start) = download_start_from_event(&event, frames) {
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

fn download_start_from_event(
    event: &CdpEvent,
    frames: &DownloadFrames<'_>,
) -> Option<DownloadStart> {
    if !frames.owns(event) {
        return None;
    }
    let field = |name: &str| {
        event
            .params
            .get(name)
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    Some(DownloadStart {
        url: field("url"),
        suggested_filename: field("suggestedFilename"),
    })
}

pub(super) fn navigate_has_download_flag(navigate: &serde_json::Value) -> bool {
    navigate
        .get("isDownload")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

pub(super) fn navigation_became_download(
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

pub(super) async fn snapshot_download_dir(dir: &Path) -> std::io::Result<BTreeSet<PathBuf>> {
    let mut out = BTreeSet::new();
    let mut read_dir = tokio::fs::read_dir(dir).await?;
    while let Some(entry) = read_dir.next_entry().await? {
        out.insert(entry.path());
    }
    Ok(out)
}

async fn wait_for_download_with_deadline(
    dir: &Path,
    before: &BTreeSet<PathBuf>,
    expected_name: Option<&str>,
    deadline: &FetchDeadline,
) -> Result<CompletedDownload, Error> {
    deadline
        .run_result("capture_download", ErrorCode::NavigationTimeout, async {
            wait_for_download_file(
                dir,
                before,
                expected_name,
                deadline.remaining("capture_download")?,
            )
            .await
        })
        .await
}

async fn wait_for_download_file(
    dir: &Path,
    before: &BTreeSet<PathBuf>,
    expected_name: Option<&str>,
    timeout: Duration,
) -> Result<CompletedDownload, Error> {
    let deadline = Instant::now() + timeout;
    loop {
        if Instant::now() >= deadline {
            return Err(Error::new(
                ErrorCode::NavigationTimeout,
                format!("download did not complete in {}", dir.display()),
            ));
        }
        if let Some(path) = newest_completed_download(dir, before, expected_name).await {
            let size1 = tokio::fs::metadata(&path).await.map(|m| m.len()).ok();
            tokio::time::sleep(Duration::from_millis(100)).await;
            let size2 = tokio::fs::metadata(&path).await.map(|m| m.len()).ok();
            if let (Some(a), Some(b)) = (size1, size2)
                && a == b
            {
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
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The newest new, finished file in `dir`. With `expected_name` (the
/// download's suggested filename) only that name, or the browser's
/// de-duplicated `name (N).ext` form of it, qualifies.
async fn newest_completed_download(
    dir: &Path,
    before: &BTreeSet<PathBuf>,
    expected_name: Option<&str>,
) -> Option<PathBuf> {
    let mut read_dir = tokio::fs::read_dir(dir).await.ok()?;
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    while let Ok(Some(entry)) = read_dir.next_entry().await {
        let path = entry.path();
        if before.contains(&path) || path.extension().and_then(|e| e.to_str()) == Some("crdownload")
        {
            continue;
        }
        if let Some(expected) = expected_name
            && !path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|name| is_download_named(name, expected))
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

/// `name` is `expected`, or `expected` with a ` (N)` the browser inserted
/// before the extension because the name was taken.
fn is_download_named(name: &str, expected: &str) -> bool {
    if name == expected {
        return true;
    }
    let (stem, ext) = match expected.rfind('.') {
        Some(dot) if dot > 0 => expected.split_at(dot),
        _ => (expected, ""),
    };
    name.strip_circumfix(stem, ext)
        .and_then(|middle| middle.strip_circumfix(" (", ')'))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unfinished_download_uses_remaining_fetch_budget_and_timeout_trace() {
        use crate::sdk::fetch::result::{TraceRenderMode, TraceStageStatus};
        let dir = tempfile::tempdir().unwrap();
        // A partial download never counts as a completed artifact.
        std::fs::write(dir.path().join("pending.crdownload"), "partial").unwrap();
        let deadline = FetchDeadline::new(Duration::from_millis(200), TraceRenderMode::Always);
        tokio::time::sleep(Duration::from_millis(150)).await;
        let started = Instant::now();
        let error = wait_for_download_with_deadline(dir.path(), &BTreeSet::new(), None, &deadline)
            .await
            .unwrap_err();
        assert_eq!(error.error_code, ErrorCode::NavigationTimeout);
        assert!(started.elapsed() < Duration::from_millis(150));
        let trace = deadline.snapshot();
        assert!(
            trace
                .stages
                .iter()
                .any(|stage| stage.name == "capture_download"
                    && stage.status == TraceStageStatus::Timeout)
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
                suggested_filename: None,
            }),
        ));
    }

    #[test]
    fn download_events_from_other_tabs_are_ignored() {
        let frames = DownloadFrames {
            session_id: "mine",
            target_id: "TARGET",
            main_frame_id: Some("ROOT"),
        };
        let event = |method: &str, session: Option<&str>, frame: &str| CdpEvent {
            method: method.into(),
            session_id: session.map(str::to_string),
            params: serde_json::json!({
                "frameId": frame, "guid": "g1", "url": "https://example.test/f.bin",
                "suggestedFilename": "f.bin",
            }),
        };
        for (ev, owned) in [
            (event("Browser.downloadWillBegin", None, "TARGET"), true),
            (event("Browser.downloadWillBegin", None, "ROOT"), true),
            (event("Browser.downloadWillBegin", None, "OTHER"), false),
            (event("Page.downloadWillBegin", Some("mine"), "x"), true),
            (
                event("Page.downloadWillBegin", Some("other"), "TARGET"),
                false,
            ),
        ] {
            let start = download_start_from_event(&ev, &frames);
            assert_eq!(start.is_some(), owned, "{ev:?}");
            if let Some(start) = start {
                assert_eq!(start.suggested_filename.as_deref(), Some("f.bin"));
            }
        }
    }

    #[tokio::test]
    async fn only_the_expected_download_name_is_taken_from_the_shared_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("report.pdf"), "old").unwrap();
        let before = snapshot_download_dir(dir.path()).await.unwrap();
        std::fs::write(dir.path().join("report (1).pdf"), "mine").unwrap();
        // Another fetch's download, written later, is newer but not ours.
        std::fs::write(dir.path().join("other.bin"), "theirs").unwrap();
        let found = newest_completed_download(dir.path(), &before, Some("report.pdf"))
            .await
            .unwrap();
        assert_eq!(found, dir.path().join("report (1).pdf"));
        for (name, expected, matches) in [
            ("a.tar.gz", "a.tar.gz", true),
            ("a.tar (2).gz", "a.tar.gz", true),
            ("README (3)", "README", true),
            ("report (x).pdf", "report.pdf", false),
            ("report ().pdf", "report.pdf", false),
            ("report.pdf.exe", "report.pdf", false),
        ] {
            assert_eq!(is_download_named(name, expected), matches, "{name}");
        }
    }
}
