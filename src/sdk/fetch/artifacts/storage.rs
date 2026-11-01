//! `storage` artifact: localStorage, sessionStorage, and IndexedDB database
//! names captured via `Runtime.evaluate`. Default off — sensitive data risk.
//! Capped at 256 KiB of JSON; past the cap every value is dropped and a
//! `truncated` note says so.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::sdk::cdp::ws_client::Connection;
use crate::sdk::fetch::writer;
use crate::shared::artifacts::ArtifactPaths;
use crate::shared::error::{Error, ErrorCode};

/// Hard cap on the serialized JSON size. Past it the values are dropped.
const SIZE_CAP_BYTES: usize = 256 * 1024;
pub const STORAGE_SCHEMA_VERSION: u32 = 2;

#[derive(Serialize, Deserialize)]
pub struct StorageSnapshot {
    pub schema_version: u32,
    #[serde(alias = "url")]
    pub page_url: String,
    pub local_storage: serde_json::Value,
    pub session_storage: serde_json::Value,
    /// IndexedDB database names only (no full dump — size and privacy risk).
    pub indexed_db_names: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated: Option<TruncationNote>,
}

#[derive(Serialize, Deserialize)]
pub struct TruncationNote {
    pub reason: String,
    pub cap_bytes: usize,
}

/// Capture localStorage, sessionStorage, and IndexedDB names via CDP.
pub async fn capture(
    conn: &Connection,
    session_id: &str,
    url: &str,
) -> Result<StorageSnapshot, Error> {
    // A storage area the page cannot read (an opaque origin, a sandboxed
    // frame, storage blocked by policy) throws. That is a failed capture, not
    // an empty store.
    let ls = eval_json(
        conn,
        session_id,
        "localStorage",
        "(function(){ var o={}; for(var i=0;i<localStorage.length;i++){ \
         var k=localStorage.key(i); o[k]=localStorage.getItem(k); } \
         return JSON.stringify(o); })()",
    )
    .await?;

    let ss = eval_json(
        conn,
        session_id,
        "sessionStorage",
        "(function(){ var o={}; for(var i=0;i<sessionStorage.length;i++){ \
         var k=sessionStorage.key(i); o[k]=sessionStorage.getItem(k); } \
         return JSON.stringify(o); })()",
    )
    .await?;

    // IndexedDB names are best-effort: `indexedDB.databases()` is missing on
    // some engines, and a missing list says nothing about the stores above.
    let idb_names = eval_str_array(
        conn,
        session_id,
        "indexedDB.databases().then(dbs => JSON.stringify(dbs.map(d => d.name || '')))",
    )
    .await
    .unwrap_or_default();

    let mut snapshot = StorageSnapshot {
        schema_version: STORAGE_SCHEMA_VERSION,
        page_url: url.to_string(),
        local_storage: ls,
        session_storage: ss,
        indexed_db_names: idb_names,
        truncated: None,
    };

    // Check size cap.
    if let Ok(json) = serde_json::to_string(&snapshot)
        && json.len() > SIZE_CAP_BYTES
    {
        snapshot.local_storage = serde_json::Value::Null;
        snapshot.session_storage = serde_json::Value::Null;
        snapshot.indexed_db_names = Vec::new();
        snapshot.truncated = Some(TruncationNote {
            reason: format!(
                "serialized size {} bytes exceeded cap {}; values omitted",
                json.len(),
                SIZE_CAP_BYTES
            ),
            cap_bytes: SIZE_CAP_BYTES,
        });
    }

    Ok(snapshot)
}

pub async fn write(paths: &ArtifactPaths, snapshot: &StorageSnapshot) -> Result<PathBuf, Error> {
    let target = paths.file_for(crate::shared::artifacts::Artifact::Storage);
    writer::ensure_dir(&paths.root).await?;
    let json = serde_json::to_vec_pretty(snapshot)
        .map_err(|e| Error::new(ErrorCode::InternalError, format!("storage: serialize: {e}")))?;
    writer::write_bytes(&target, &json).await?;
    Ok(target)
}

async fn eval_json(
    conn: &Connection,
    session_id: &str,
    area: &str,
    expr: &str,
) -> Result<serde_json::Value, Error> {
    let r = conn
        .send(
            "Runtime.evaluate",
            &serde_json::json!({
                "expression": expr,
                "returnByValue": true,
                "awaitPromise": true,
            }),
            Some(session_id),
        )
        .await?;
    let s = evaluated_string(&r, area)?;
    serde_json::from_str(s).map_err(|e| {
        Error::new(
            ErrorCode::ArtifactCaptureFailed,
            format!("storage: parse json: {e}"),
        )
    })
}

async fn eval_str_array(
    conn: &Connection,
    session_id: &str,
    expr: &str,
) -> Result<Vec<String>, Error> {
    let r = conn
        .send(
            "Runtime.evaluate",
            &serde_json::json!({
                "expression": expr,
                "returnByValue": true,
                "awaitPromise": true,
            }),
            Some(session_id),
        )
        .await?;
    let s = evaluated_string(&r, "indexedDB")?;
    serde_json::from_str::<Vec<serde_json::Value>>(s)
        .map(|arr| {
            arr.into_iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .map_err(|e| {
            Error::new(
                ErrorCode::ArtifactCaptureFailed,
                format!("storage: parse idb names: {e}"),
            )
        })
}

/// The string an evaluation returned, or a capture failure when the page
/// threw (or returned something else) instead.
fn evaluated_string<'a>(reply: &'a serde_json::Value, area: &str) -> Result<&'a str, Error> {
    if let Some(details) = reply.get("exceptionDetails") {
        let message = details
            .pointer("/exception/description")
            .or_else(|| details.get("text"))
            .and_then(|v| v.as_str())
            .unwrap_or("exception");
        return Err(Error::new(
            ErrorCode::ArtifactCaptureFailed,
            format!("storage: reading {area} threw: {message}"),
        ));
    }
    reply["result"]["value"].as_str().ok_or_else(|| {
        Error::new(
            ErrorCode::ArtifactCaptureFailed,
            format!("storage: reading {area} returned no string"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_throwing_storage_read_is_a_capture_failure_not_an_empty_store() {
        let threw = json!({
            "result": {"type": "object", "subtype": "error"},
            "exceptionDetails": {"text": "Uncaught", "exception": {
                "description": "SecurityError: Failed to read the 'localStorage' property"
            }},
        });
        let error = evaluated_string(&threw, "localStorage").unwrap_err();
        assert_eq!(error.error_code, ErrorCode::ArtifactCaptureFailed);
        assert_eq!(
            error.detail,
            "storage: reading localStorage threw: SecurityError: Failed to read the 'localStorage' property"
        );
        let undefined = json!({"result": {"type": "undefined"}});
        assert_eq!(
            evaluated_string(&undefined, "sessionStorage")
                .unwrap_err()
                .error_code,
            ErrorCode::ArtifactCaptureFailed
        );
        let ok = json!({"result": {"type": "string", "value": "{}"}});
        assert_eq!(evaluated_string(&ok, "localStorage").unwrap(), "{}");
    }
}
