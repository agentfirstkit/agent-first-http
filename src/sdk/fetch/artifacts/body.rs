//! Raw HTTP response body artifact (`body.<ext>`).

use std::path::PathBuf;

use crate::sdk::fetch::writer;
use crate::shared::artifacts::{Artifact, ArtifactPaths};
use crate::shared::error::Error;

/// Write `bytes` as the body artifact. `content_type` is consulted to pick
/// the file extension; unknown types fall back to `.bin`.
pub async fn write(
    paths: &ArtifactPaths,
    content_type: Option<&str>,
    bytes: &[u8],
) -> Result<PathBuf, Error> {
    let ext = extension_for(content_type);
    let mut target = paths.file_for(Artifact::Body);
    target.set_extension(ext);
    writer::write_bytes(&target, bytes).await?;
    Ok(target)
}

fn extension_for(content_type: Option<&str>) -> &'static str {
    let Some(ct) = content_type else { return "bin" };
    let primary = ct
        .split(';')
        .next()
        .unwrap_or(ct)
        .trim()
        .to_ascii_lowercase();
    match primary.as_str() {
        "text/html" | "application/xhtml+xml" => "html",
        "application/json" | "application/ld+json" => "json",
        "application/javascript" | "text/javascript" => "js",
        "text/css" => "css",
        "text/plain" => "txt",
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "application/xml" | "text/xml" => "xml",
        "application/pdf" => "pdf",
        "application/octet-stream" => "bin",
        _ => mime_guess::get_mime_extensions_str(&primary)
            .and_then(|exts| exts.first())
            .copied()
            .unwrap_or("bin"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::ids::RequestId;

    #[test]
    fn extension_table_covers_common_mimes() {
        assert_eq!(extension_for(Some("text/html")), "html");
        assert_eq!(extension_for(Some("text/html; charset=utf-8")), "html");
        assert_eq!(extension_for(Some("application/json")), "json");
        assert_eq!(extension_for(Some("image/png")), "png");
        assert_eq!(extension_for(None), "bin");
        assert_eq!(extension_for(Some("application/x-weird-thing")), "bin");
    }

    #[tokio::test]
    async fn writes_body_with_html_extension() {
        let dir = tempfile::tempdir().unwrap();
        let rid = RequestId::new_v4();
        let paths = ArtifactPaths::new(dir.path().to_path_buf(), &rid);
        let p = write(&paths, Some("text/html"), b"<html></html>")
            .await
            .unwrap();
        assert_eq!(p.extension().and_then(|s| s.to_str()), Some("html"));
        let content = tokio::fs::read(&p).await.unwrap();
        assert_eq!(content, b"<html></html>");
    }
}
