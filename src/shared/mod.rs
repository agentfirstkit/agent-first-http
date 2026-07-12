//! Feature-agnostic types: error enum, ids, artifact tokens, the protocol
//! AFDATA adapter, redaction list, and duration parsing. Compiles in both
//! sdk-only and host builds; never touches chromiumoxide or axum.

pub mod afdata;
pub mod artifacts;
pub mod error;
pub mod ids;
pub mod path;
pub mod profile_snapshot;
pub mod redact;
pub mod time;

#[cfg(test)]
mod contract_tests;
