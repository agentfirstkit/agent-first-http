//! Retry/backoff: confirms FetchBuilder.retry + backoff actually re-runs
//! the pipeline when an error carries `retryable: true`, sleeps the
//! configured amount between attempts, and short-circuits for
//! non-retryable failures.

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

use std::time::{Duration, Instant};

use agent_first_http::sdk::fetch::RenderMode;
use agent_first_http::sdk::Client;
use agent_first_http::shared::artifacts::Artifact;
use agent_first_http::shared::error::ErrorCode;

/// `127.0.0.1:1` is a well-known no-listener port; reqwest fails the
/// connect immediately so the test stays fast and deterministic.
const UNREACHABLE_URL: &str = "http://127.0.0.1:1/will-fail";

#[tokio::test]
async fn retry_zero_runs_a_single_attempt() {
    let tmpdir = tempfile::tempdir().expect("tempdir");
    let client = Client::connect("ws://localhost:9999").expect("client");

    let start = Instant::now();
    let err = client
        .fetch(UNREACHABLE_URL)
        .render(RenderMode::None)
        .want([Artifact::Body])
        .out_dir(tmpdir.path().to_path_buf())
        .send()
        .await
        .err()
        .expect("fetch should fail");
    let elapsed = start.elapsed();

    // Single connect attempt — should fail fast (< 1s on local loopback).
    assert!(
        elapsed < Duration::from_millis(1500),
        "single-attempt fetch took {elapsed:?}, retry not configured but loop kicked in?",
    );
    // Connect failures map to retryable error codes even though we don't
    // retry in this test.
    assert!(matches!(
        err.error_code,
        ErrorCode::HostUnreachable | ErrorCode::TargetUnreachable
    ));
}

#[tokio::test]
async fn retry_with_fixed_backoff_inserts_delays_between_attempts() {
    let tmpdir = tempfile::tempdir().expect("tempdir");
    let client = Client::connect("ws://localhost:9999").expect("client");

    let start = Instant::now();
    let err = client
        .fetch(UNREACHABLE_URL)
        .render(RenderMode::None)
        .retry(2)
        .backoff_ms(150)
        .want([Artifact::Body])
        .out_dir(tmpdir.path().to_path_buf())
        .send()
        .await
        .err()
        .expect("fetch should still fail after retries");
    let elapsed = start.elapsed();

    // With retry=2 the fetch tries 3 times; between attempts the pipeline
    // sleeps for 150ms (fixed). The two sleeps total ≥ 300ms. Allow some
    // slack for the actual connect attempts.
    assert!(
        elapsed >= Duration::from_millis(300),
        "fetch with retry=2 backoff=150ms took only {elapsed:?}; the loop is not actually retrying",
    );
    assert!(matches!(
        err.error_code,
        ErrorCode::HostUnreachable | ErrorCode::TargetUnreachable
    ));
    // Whichever code we got must still be retryable so the loop knew to
    // re-attempt.
    assert!(err.retryable);
}

#[tokio::test]
async fn retry_does_not_fire_for_non_retryable_errors() {
    // An invalid argument is non-retryable; the retry loop must not
    // re-run the pipeline against the same bad input.
    let tmpdir = tempfile::tempdir().expect("tempdir");
    let client = Client::connect("ws://localhost:9999").expect("client");

    let start = Instant::now();
    let err = client
        .fetch(UNREACHABLE_URL)
        .render(RenderMode::None)
        .retry(3)
        .backoff_ms(500)
        // Conflicting header + flag → InvalidArgument from PreparedRequestOptions.
        .header("User-Agent", "from-header/1")
        .user_agent("from-method/1")
        .want([Artifact::Body])
        .out_dir(tmpdir.path().to_path_buf())
        .send()
        .await
        .err()
        .expect("should error before any retry");
    let elapsed = start.elapsed();

    assert_eq!(err.error_code, ErrorCode::InvalidArgument);
    // With retry=3 and backoff=500ms, a retry loop would add 1500ms+.
    // We expect the failure to be near-instant — well under one backoff
    // interval.
    assert!(
        elapsed < Duration::from_millis(400),
        "non-retryable error took {elapsed:?}; the retry loop fired anyway",
    );
}
