//! Feature-agnostic types: error enum, ids, artifact tokens, the protocol
//! event reader, redaction lists, and duration parsing. Compiles in both
//! sdk-only and host builds; never touches chromiumoxide or axum. The AFDATA
//! emitter is the one exception: only the host and CLI write events.

#[cfg(feature = "host")]
pub mod afdata;
pub mod artifacts;
pub mod error;
pub mod events;
pub mod ids;
pub mod path;
pub mod profile_snapshot;
pub mod redact;
pub mod time;

#[cfg(all(test, feature = "host"))]
mod contract_tests;
