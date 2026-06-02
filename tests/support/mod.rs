//! Shared helpers used by every integration test under `tests/`.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::disallowed_methods,
    clippy::disallowed_macros,
    clippy::err_expect,
    clippy::print_stdout,
    clippy::useless_conversion
)]

pub mod env;
pub mod fixture_server;

use std::sync::OnceLock;

/// Install the aws-lc-rs rustls provider exactly once per test process.
/// Both `Client::connect` and bare `reqwest::Client::new()` need this when
/// reqwest is compiled with `rustls-tls-no-provider`. Call from the top of
/// any test that creates a bare reqwest client.
pub fn ensure_rustls_provider() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}
