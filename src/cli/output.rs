//! Output glue. The single seam through which CLI subcommands write their
//! one line of JSON to stdout.

use serde::Serialize;
use std::io::Write;

use crate::shared::error::Error;

/// Emit a payload to stdout as one line of JSON. Wraps
/// `shared::envelope::emit` and is the single seam through which CLI
/// subcommands produce output.
pub fn emit<T: Serialize>(code: &str, payload: &T) -> Result<(), Error> {
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    crate::shared::envelope::emit(&mut handle, code, payload)?;
    handle.flush().ok();
    Ok(())
}

/// Emit a payload to stdout without AFDATA redaction. This is only for
/// explicit reveal flags; normal command output must use [`emit`].
pub fn emit_unredacted<T: Serialize>(code: &str, payload: &T) -> Result<(), Error> {
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    crate::shared::envelope::emit_unredacted(&mut handle, code, payload)?;
    handle.flush().ok();
    Ok(())
}
