//! Output glue. The single seam through which CLI subcommands write their
//! one line of JSON to stdout.

use crate::shared::error::Error;
use serde::Serialize;

/// Emit a payload to stdout as one line of JSON. Wraps
/// `shared::afdata::emit` and is the single seam through which CLI
/// subcommands produce output.
pub fn emit<T: Serialize>(code: &str, payload: &T) -> Result<(), Error> {
    crate::shared::afdata::emit_process(code, payload)
}

/// Emit a payload to stdout without AFDATA redaction. This is only for
/// explicit reveal flags; normal command output must use [`emit`].
pub fn emit_unredacted<T: Serialize>(code: &str, payload: &T) -> Result<(), Error> {
    crate::shared::afdata::emit_process_unredacted(code, payload)
}

/// Emit an otherwise-redacted result while revealing only the short-lived
/// takeover capability requested by an explicit panel/takeover operation.
pub fn emit_revealing_takeover<T: Serialize>(code: &str, payload: &T) -> Result<(), Error> {
    crate::shared::afdata::emit_process_revealing_takeover(code, payload)
}

/// Emit one non-terminal progress event. Only commands that block on a person
/// have anything to report before their result.
pub fn emit_progress<T: Serialize>(code: &str, payload: &T) -> Result<(), Error> {
    crate::shared::afdata::emit_process_progress(code, payload)
}
