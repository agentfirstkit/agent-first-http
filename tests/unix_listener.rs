//! N7 integration test: unix-socket listener round-trip on `cfg(unix)`.
//! On Windows the file is empty (cfg gate skips both tests).

#![cfg(feature = "host")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::disallowed_methods,
    clippy::disallowed_macros,
    clippy::err_expect,
    clippy::print_stdout,
    clippy::useless_conversion
)]

mod support;

use agent_first_http::host::bootstrap::HealthPublic;
use agent_first_http::host::listener::{router_for_tests, test_state, AppState};

#[cfg(unix)]
async fn spawn_unix_listener(state: AppState, sock: std::path::PathBuf) {
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use hyper_util::server::conn::auto;
    use tokio::net::UnixListener;
    use tower::Service;

    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock).expect("bind");
    let app = router_for_tests(state);
    let mut make_service = app.into_make_service();
    tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(p) => p,
                Err(_) => break,
            };
            let tower_service = match futures::future::poll_fn(|cx| {
                <_ as Service<axum::http::Request<axum::body::Body>>>::poll_ready(
                    &mut make_service,
                    cx,
                )
            })
            .await
            {
                Ok(()) => make_service.call(()).await.expect("svc"),
                Err(_) => break,
            };
            tokio::spawn(async move {
                let io = TokioIo::new(stream);
                let svc = hyper::service::service_fn(
                    move |req: axum::http::Request<hyper::body::Incoming>| {
                        let mut tower_service = tower_service.clone();
                        async move {
                            let (parts, body) = req.into_parts();
                            let req =
                                axum::http::Request::from_parts(parts, axum::body::Body::new(body));
                            <_ as Service<axum::http::Request<axum::body::Body>>>::call(
                                &mut tower_service,
                                req,
                            )
                            .await
                        }
                    },
                );
                let _ = auto::Builder::new(TokioExecutor::new())
                    .serve_connection(io, svc)
                    .await;
            });
        }
    });
}

#[cfg(unix)]
#[tokio::test]
async fn unix_listener_serves_health_over_uds() {
    use std::io::ErrorKind;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;

    let tmp = tempfile::tempdir().expect("tmp");
    let sock = tmp.path().join("afhttp.sock");
    let state = test_state(None, HealthPublic::Off);
    spawn_unix_listener(state, sock.clone()).await;
    // Tiny wait so the listener task is ready to accept.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Plain HTTP/1.1 GET /health over the unix socket.
    let mut stream = UnixStream::connect(&sock).await.expect("connect");
    let req = "GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
    stream.write_all(req.as_bytes()).await.expect("write");

    let mut buf = Vec::new();
    loop {
        let mut tmp = [0u8; 4096];
        match stream.read(&mut tmp).await {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => break,
            Err(_) => break,
        }
    }

    let text = String::from_utf8_lossy(&buf);
    assert!(text.starts_with("HTTP/1.1 200"), "wire response: {text}");
    let body_start = text.find("\r\n\r\n").expect("header/body split") + 4;
    let body = &text[body_start..];
    let v: serde_json::Value = serde_json::from_str(body).expect("body json");
    assert_eq!(v["code"], "health");
    assert_eq!(v["status"], "starting");
}

#[cfg(unix)]
#[tokio::test]
async fn sdk_client_serves_health_over_uds() {
    let tmp = tempfile::tempdir().expect("tmp");
    let sock = tmp.path().join("afhttp-sdk.sock");
    let state = test_state(None, HealthPublic::Off);
    spawn_unix_listener(state, sock.clone()).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let client = agent_first_http::sdk::Client::connect(&format!("unix:{}", sock.display()))
        .expect("client");
    let health = client.health().await.expect("health over uds");
    assert_eq!(health.code, "health");
    assert_eq!(health.status, "starting");
}

#[test]
fn parse_listen_accepts_tcp_and_rejects_invalid_unix() {
    use agent_first_http::host::listener::{parse_listen, ListenAddr};

    let r = parse_listen("tcp:127.0.0.1:0").expect("tcp parses");
    match r {
        ListenAddr::Tcp(_) => {}
        #[cfg(unix)]
        _ => panic!("expected Tcp"),
    }

    let err = parse_listen("garbage").err().expect("garbage rejected");
    assert_eq!(
        err.error_code,
        agent_first_http::shared::error::ErrorCode::InvalidArgument,
    );

    #[cfg(unix)]
    {
        let r = parse_listen("unix:/tmp/afhttp.sock").expect("unix parses on cfg(unix)");
        match r {
            ListenAddr::Unix(p) => assert_eq!(p, std::path::PathBuf::from("/tmp/afhttp.sock")),
            _ => panic!("expected Unix"),
        }

        let err = parse_listen("unix:")
            .err()
            .expect("empty unix path rejected");
        assert_eq!(
            err.error_code,
            agent_first_http::shared::error::ErrorCode::InvalidArgument,
        );
    }
}
