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
use agent_first_http::host::listener::{AppState, test_state};

/// The host's own unix-socket listener, not a copy of it: what a request
/// arriving on the socket is told — including that a token-less host answers
/// it, since no web page can reach a unix socket — is decided there.
#[cfg(unix)]
async fn spawn_unix_listener(state: AppState, sock: std::path::PathBuf) {
    let listen = format!("unix:{}", sock.display());
    tokio::spawn(async move {
        let _served = state.serve(&listen).await;
    });
    for _ in 0..100 {
        if sock.exists() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("the unix listener never bound {}", sock.display());
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
    assert_eq!(v["result"]["code"], "health");
    assert_eq!(v["result"]["status"], "starting");
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
    use agent_first_http::host::listener::{ListenAddr, parse_listen};

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
