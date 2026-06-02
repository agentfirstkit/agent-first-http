//! Feature-agnostic types: error enum, ids, artifact tokens, the protocol
//! envelope writer, redaction list, and duration parsing. Compiles in both
//! sdk-only and host builds; never touches chromiumoxide or axum.

pub mod artifacts;
pub mod envelope;
pub mod error;
pub mod ids;
pub mod path;
pub mod profile_snapshot;
pub mod redact;
pub mod time;

#[cfg(test)]
mod contract_tests;
