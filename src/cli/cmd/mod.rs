//! Per-subcommand implementations. Each builds a request from clap args,
//! calls into the SDK, and emits a response envelope.

pub mod capabilities;
pub mod cdp;
pub mod container;
pub mod fetch;
pub mod health;
pub mod host;
pub mod profile;
pub mod skill;
pub mod tabs;
pub mod takeover;
pub mod ui;
pub mod upload;

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::routing::get;
    use futures::{SinkExt, StreamExt};
    use serde_json::{json, Value};
    use tokio::net::{TcpListener, TcpStream};
    use tokio_tungstenite::tungstenite::Message;

    use super::*;
    use crate::shared::error::ErrorCode;

    fn ensure_rustls_provider() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    }

    async fn spawn_http_host() -> String {
        ensure_rustls_provider();
        let app = axum::Router::new()
            .route(
                "/health",
                get(|| async {
                    axum::Json(json!({
                        "code": "health",
                        "status": "ok",
                        "version": env!("CARGO_PKG_VERSION"),
                        "uptime_s": 1,
                        "tabs_active": 2,
                        "capabilities_url": "/capabilities",
                    }))
                }),
            )
            .route(
                "/capabilities",
                get(|| async {
                    axum::Json(json!({
                        "code": "capabilities",
                        "backend": {"family": "test", "version": "1"},
                        "artifacts": {
                            "body": {"supported": true},
                            "network": {"supported": true, "body_capture": ["xhr"]},
                            "screenshot": {"supported": false}
                        },
                        "wait_modes": ["auto", "load"],
                        "display_takeover": false,
                        "ops_panel": {"supported": false, "screencast": false},
                        "profile": {"persistent": true, "ephemeral": true},
                        "features": {},
                        "limits": {}
                    }))
                }),
            );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}")
    }

    async fn spawn_cdp() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(handle_cdp(stream));
            }
        });
        format!("ws://{addr}")
    }

    async fn handle_cdp(stream: TcpStream) {
        let Ok(ws) = tokio_tungstenite::accept_async(stream).await else {
            return;
        };
        let (mut tx, mut rx) = ws.split();
        while let Some(Ok(Message::Text(text))) = rx.next().await {
            let Ok(value) = serde_json::from_str::<Value>(text.as_str()) else {
                continue;
            };
            let id = value.get("id").and_then(Value::as_i64).unwrap_or(0);
            let method = value.get("method").and_then(Value::as_str).unwrap_or("");
            let params = value.get("params").cloned().unwrap_or(Value::Null);
            let session_id = value
                .get("sessionId")
                .and_then(Value::as_str)
                .unwrap_or("session-1")
                .to_string();
            let (result, delayed_event) = cdp_response(method, &params, &session_id);
            let response = json!({"id": id, "result": result}).to_string();
            if tx.send(Message::Text(response.into())).await.is_err() {
                return;
            }
            if let Some(event) = delayed_event {
                tokio::time::sleep(Duration::from_millis(25)).await;
                let _ = tx.send(Message::Text(event.to_string().into())).await;
            }
        }
    }

    fn cdp_response(method: &str, params: &Value, session_id: &str) -> (Value, Option<Value>) {
        match method {
            "Target.getTargets" => (
                json!({
                    "targetInfos": [{
                        "targetId": "tab-1",
                        "type": "page",
                        "title": "Example",
                        "url": "https://example.test/"
                    }]
                }),
                None,
            ),
            "Target.attachToTarget" => (json!({"sessionId": "session-1"}), None),
            "Target.closeTarget" => (json!({"success": true}), None),
            "Target.detachFromTarget"
            | "Runtime.enable"
            | "DOM.enable"
            | "DOM.setFileInputFiles" => (json!({}), None),
            "DOM.getDocument" => (json!({"root": {"nodeId": 1}}), None),
            "DOM.querySelector" => (json!({"nodeId": 2}), None),
            "DOM.describeNode" => (
                json!({"node": {"nodeName": "INPUT", "attributes": ["type", "file"]}}),
                None,
            ),
            "Runtime.evaluate" => runtime_evaluate_response(params, session_id),
            _ => (json!({"ok": true}), None),
        }
    }

    fn runtime_evaluate_response(params: &Value, session_id: &str) -> (Value, Option<Value>) {
        let expression = params
            .get("expression")
            .and_then(Value::as_str)
            .unwrap_or("");
        if expression == "location.href" {
            return (json!({"result": {"value": "https://example.test/"}}), None);
        }
        if expression.contains("document.title") {
            return (
                json!({"result": {"value": "{\"title\":\"Example\",\"w\":800,\"h\":600,\"dpr\":1}"}}),
                None,
            );
        }
        if expression.contains("AFHTTP_OBSERVATION_SNAPSHOT") {
            return (
                json!({"result": {"value": serde_json::to_string(&json!({
                    "nodes": [{
                        "ref": "obs-1",
                        "frame_id": "main",
                        "role": "button",
                        "name": "Go",
                        "visible": true,
                        "enabled": true,
                        "actions": ["click"]
                    }],
                    "forms": [],
                    "frames": [{"frame_id": "main", "url": "https://example.test/"}],
                    "focused_ref": "obs-1"
                })).unwrap()}}),
                None,
            );
        }
        (
            json!({"result": {"value": 42}}),
            Some(json!({
                "method": "Test.event",
                "sessionId": session_id,
                "params": {"ok": true}
            })),
        )
    }

    #[tokio::test]
    async fn health_and_capabilities_commands_emit_host_payloads() {
        let base = spawn_http_host().await;

        health::run(health::Args {
            endpoint: base.clone(),
            token: Some("token".into()),
        })
        .await
        .unwrap();

        capabilities::run(capabilities::Args {
            endpoint: base,
            token: Some("token".into()),
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn cdp_command_parses_params_waits_and_detaches() {
        let endpoint = spawn_cdp().await;

        cdp::run(cdp::Args {
            method: "Runtime.evaluate".into(),
            endpoint,
            token: Some("a+b&c%20".into()),
            tab: "tab-1".into(),
            params: Some(json!({"expression": "1 + 1"}).to_string()),
            wait: Some("Test.event:1s".into()),
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn cdp_command_validates_json_and_wait_specs() {
        let err = cdp::run(cdp::Args {
            method: "Runtime.evaluate".into(),
            endpoint: "ws://127.0.0.1:1".into(),
            token: None,
            tab: "tab-1".into(),
            params: Some("{".into()),
            wait: None,
        })
        .await
        .err()
        .unwrap();
        assert_eq!(err.error_code, ErrorCode::InvalidArgument);

        let err = cdp::run(cdp::Args {
            method: "Runtime.evaluate".into(),
            endpoint: "ws://127.0.0.1:1".into(),
            token: None,
            tab: "tab-1".into(),
            params: Some("{}".into()),
            wait: Some("missing-timeout".into()),
        })
        .await
        .err()
        .unwrap();
        assert_eq!(err.error_code, ErrorCode::InvalidArgument);
    }

    #[tokio::test]
    async fn tabs_commands_cover_list_and_close() {
        let endpoint = spawn_cdp().await;

        tabs::run(tabs::Args {
            sub: tabs::TabsSub::List(tabs::EndpointArgs {
                endpoint: endpoint.clone(),
                token: Some("token".into()),
            }),
        })
        .await
        .unwrap();

        tabs::run(tabs::Args {
            sub: tabs::TabsSub::Close(tabs::CloseArgs {
                id: "tab-1".into(),
                endpoint: endpoint.clone(),
                token: None,
            }),
        })
        .await
        .unwrap();

        let err = tabs::run(tabs::Args {
            sub: tabs::TabsSub::Close(tabs::CloseArgs {
                id: " ".into(),
                endpoint: "ws://127.0.0.1:1".into(),
                token: None,
            }),
        })
        .await
        .err()
        .unwrap();
        assert_eq!(err.error_code, ErrorCode::InvalidArgument);
    }

    #[tokio::test]
    async fn upload_command_uses_set_file_input_files() {
        let endpoint = spawn_cdp().await;
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("upload.txt");
        tokio::fs::write(&file, b"hello").await.unwrap();

        upload::run(upload::Args {
            endpoint,
            token: Some("token".into()),
            tab: "tab-1".into(),
            selector: "input[type=file]".into(),
            file,
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn profile_command_covers_local_lifecycle_branches() {
        let tmp = tempfile::tempdir().unwrap();
        let profile_dir = tmp.path().join("work");
        std::fs::create_dir_all(&profile_dir).unwrap();
        let meta = crate::sdk::profile::meta::ProfileMeta::new("work");
        std::fs::write(
            profile_dir.join("afhttp-profile.json"),
            serde_json::to_string(&meta).unwrap(),
        )
        .unwrap();
        let downloads_dir = profile_dir.join("downloads");
        std::fs::create_dir_all(&downloads_dir).unwrap();
        std::fs::write(downloads_dir.join("report.csv"), "abc").unwrap();
        let old = std::time::SystemTime::now() - Duration::from_secs(7200);
        filetime::set_file_mtime(&profile_dir, filetime::FileTime::from_system_time(old)).ok();

        profile::run(profile::Args {
            sub: profile::ProfileSub::List(profile::ListArgs {
                profile_root: Some(tmp.path().to_path_buf()),
            }),
        })
        .await
        .unwrap();
        profile::run(profile::Args {
            sub: profile::ProfileSub::Info(profile::InfoArgs {
                name: "work".into(),
                profile_root: Some(tmp.path().to_path_buf()),
            }),
        })
        .await
        .unwrap();
        profile::run(profile::Args {
            sub: profile::ProfileSub::LockStatus(profile::InfoArgs {
                name: "work".into(),
                profile_root: Some(tmp.path().to_path_buf()),
            }),
        })
        .await
        .unwrap();
        profile::run(profile::Args {
            sub: profile::ProfileSub::Cookies(profile::InfoArgs {
                name: "work".into(),
                profile_root: Some(tmp.path().to_path_buf()),
            }),
        })
        .await
        .unwrap();
        profile::run(profile::Args {
            sub: profile::ProfileSub::Downloads(profile::InfoArgs {
                name: "work".into(),
                profile_root: Some(tmp.path().to_path_buf()),
            }),
        })
        .await
        .unwrap();
        profile::run(profile::Args {
            sub: profile::ProfileSub::Prune(profile::PruneArgs {
                older_than: "1h".into(),
                dry_run: true,
                profile_root: Some(tmp.path().to_path_buf()),
            }),
        })
        .await
        .unwrap();
        profile::run(profile::Args {
            sub: profile::ProfileSub::Delete(profile::DeleteArgs {
                name: "work".into(),
                confirm: "work".into(),
                profile_root: Some(tmp.path().to_path_buf()),
            }),
        })
        .await
        .unwrap();
        assert!(!profile_dir.exists());
    }
}
