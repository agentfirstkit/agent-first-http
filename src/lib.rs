#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::disallowed_methods,
        clippy::disallowed_macros,
    )
)]

//! `afhttp` — URL acquisition for AI agents.
//!
//! The library exposes the same surface as the CLI, in-process. It speaks
//! CDP/HTTP to an `afhttp host`; it does **not** embed a browser engine.
//! Everything that physically requires Chromium (launch, profile locking,
//! takeover panel) lives behind the `host` feature.
//!
//! Pure SDK consumers depend with `default-features = false, features =
//! ["sdk"]` and connect to an externally started `afhttp host`. The
//! [`Client::inline_ephemeral`] convenience that spawns a private host
//! in-process is only available with the `host` feature.

pub mod sdk;
pub mod shared;

#[cfg(feature = "host")]
pub mod host;

#[cfg(feature = "cli")]
pub mod cli;

pub use sdk::client::Client;
pub use shared::error::{Error, ErrorCode};
