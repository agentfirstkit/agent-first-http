//! `storage` artifact: localStorage, sessionStorage, and IndexedDB database
//! names captured via `Runtime.evaluate`. Default off — sensitive data risk.
//! Capped at 256 KiB of JSON; values truncated if the cap is exceeded.

use std::path::PathBuf;

use serde::Serialize;

use crate::sdk::cdp::ws_client::Connection;
use crate::sdk::fetch::writer;
use crate::shared::artifacts::ArtifactPaths;
use crate::shared::error::{Error, ErrorCode};

/// Hard cap on the serialized JSON size. Values are truncated if exceeded.
const SIZE_CAP_BYTES: usize = 256 * 1024;

#[derive(Serialize)]
pub struct StorageSnapshot {
    pub schema_version: u32,
    pub url: String,
    pub local_storage: serde_json::Value,
    pub session_storage: serde_json::Value,
    /// IndexedDB database names only (no full dump — size and privacy risk).
    pub indexed_db_names: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated: Option<TruncationNote>,
}

#[derive(Serialize)]
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
    // localStorage
    let ls = eval_json(
        conn,
        session_id,
        "(function(){ var o={}; for(var i=0;i<localStorage.length;i++){ \
         var k=localStorage.key(i); o[k]=localStorage.getItem(k); } \
         return JSON.stringify(o); })()",
    )
    .await
    .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()));

    // sessionStorage
    let ss = eval_json(
        conn,
        session_id,
        "(function(){ var o={}; for(var i=0;i<sessionStorage.length;i++){ \
         var k=sessionStorage.key(i); o[k]=sessionStorage.getItem(k); } \
         return JSON.stringify(o); })()",
    )
    .await
    .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()));

    // IndexedDB names
    let idb_names = eval_str_array(
        conn,
        session_id,
        "indexedDB.databases().then(dbs => JSON.stringify(dbs.map(d => d.name || '')))",
    )
    .await
    .unwrap_or_default();

    let mut snapshot = StorageSnapshot {
        schema_version: 1,
        url: url.to_string(),
        local_storage: ls,
        session_storage: ss,
        indexed_db_names: idb_names,
        truncated: None,
    };

    // Check size cap.
    if let Ok(json) = serde_json::to_string(&snapshot) {
        if json.len() > SIZE_CAP_BYTES {
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
    let s = r["result"]["value"].as_str().unwrap_or("{}");
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
    let s = r["result"]["value"].as_str().unwrap_or("[]");
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
