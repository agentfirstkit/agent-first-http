//! Artifact tokens (`architecture.md §8`) + on-disk path resolution.
//!
//! Seven default tokens identify the artifacts a fetch can produce, plus
//! `Storage` (default-off, sensitive-data risk). Each maps to a fixed
//! filename; the response JSON references them as absolute paths under
//! `--out/<request_id>/`.

use serde::{Deserialize, Serialize};

use crate::shared::ids::RequestId;

/// Artifact kinds. `Body` is the only one HTTP-only fetches produce;
/// the others all require a browser. `Storage` is default-off due to
/// sensitive-data risk — agents must request it explicitly via `--want`.
/// Per-artifact warnings (§8 last ¶) are emitted when a backend lacks
/// the capability rather than failing the fetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Artifact {
    Body,
    RenderedHtml,
    Text,
    Screenshot,
    Network,
    Console,
    Observation,
    /// localStorage + sessionStorage + IndexedDB names. Default off.
    Storage,
}

impl Artifact {
    /// The seven default artifacts captured on every browser fetch.
    /// `Storage` is intentionally excluded — sensitive data risk means
    /// agents must opt in with `--want storage`.
    pub const ALL: [Self; 7] = [
        Self::Body,
        Self::RenderedHtml,
        Self::Text,
        Self::Screenshot,
        Self::Network,
        Self::Console,
        Self::Observation,
    ];

    /// Default filename portion (extension chosen from content-type for
    /// `Body`; fixed for everything else). `body.<ext>` is filled in at
    /// write time when the response headers are known.
    #[must_use]
    pub const fn filename_template(self) -> &'static str {
        match self {
            Self::Body => "body",
            Self::RenderedHtml => "rendered.html",
            Self::Text => "text.txt",
            Self::Screenshot => "page.png",
            Self::Network => "network.json",
            Self::Console => "console.json",
            Self::Observation => "observation.json",
            Self::Storage => "storage.json",
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Body => "body",
            Self::RenderedHtml => "rendered_html",
            Self::Text => "text",
            Self::Screenshot => "screenshot",
            Self::Network => "network",
            Self::Console => "console",
            Self::Observation => "observation",
            Self::Storage => "storage",
        }
    }
}

impl std::fmt::Display for Artifact {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Concrete on-disk locations for a single fetch. Built from `--out` (or
/// the default `./afhttp-out/`) plus the request id.
#[derive(Debug, Clone)]
pub struct ArtifactPaths {
    pub root: std::path::PathBuf,
}

impl ArtifactPaths {
    /// Compose `<base>/<request_id>/`. Does not create the directory; the
    /// fetch writer creates it just before producing the first file so a
    /// fetch that fails before any artifact is captured leaves no
    /// half-empty dirs behind.
    #[must_use]
    pub fn new(base: impl Into<std::path::PathBuf>, request_id: &RequestId) -> Self {
        let base = crate::shared::path::absolute_lexical(base.into());
        Self {
            root: base.join(request_id.as_str()),
        }
    }

    /// Path for a given artifact. `Body` returns `body` with no extension;
    /// callers append the content-type-derived suffix.
    #[must_use]
    pub fn file_for(&self, artifact: Artifact) -> std::path::PathBuf {
        self.root.join(artifact.filename_template())
    }

    /// `<root>/network-bodies/<request_id>.<ext>` per `§8`. The CDP
    /// `request_id` (not the fetch request id) is what's interpolated by
    /// callers; this helper just hands back the directory.
    #[must_use]
    pub fn network_bodies_dir(&self) -> std::path::PathBuf {
        self.root.join("network-bodies")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_seven_tokens_present() {
        assert_eq!(Artifact::ALL.len(), 7);
        // Storage is intentionally absent from ALL (default-off).
        assert!(!Artifact::ALL.contains(&Artifact::Storage));
    }

    #[test]
    fn filename_templates_match_spec_table() {
        let table = [
            (Artifact::Body, "body"),
            (Artifact::RenderedHtml, "rendered.html"),
            (Artifact::Text, "text.txt"),
            (Artifact::Screenshot, "page.png"),
            (Artifact::Network, "network.json"),
            (Artifact::Console, "console.json"),
            (Artifact::Observation, "observation.json"),
            (Artifact::Storage, "storage.json"),
        ];
        for (a, f) in table {
            assert_eq!(a.filename_template(), f);
        }
    }

    #[test]
    fn artifact_paths_compose_under_request_id() {
        let rid = RequestId("abc-123".into());
        let paths = ArtifactPaths::new("/tmp/out", &rid);
        assert_eq!(
            paths.file_for(Artifact::Observation),
            std::path::PathBuf::from("/tmp/out/abc-123/observation.json"),
        );
        assert_eq!(
            paths.network_bodies_dir(),
            std::path::PathBuf::from("/tmp/out/abc-123/network-bodies"),
        );
    }

    #[test]
    fn artifact_paths_absolutize_relative_out_dir() {
        let rid = RequestId("abc-123".into());
        let paths = ArtifactPaths::new("relative-out", &rid);
        assert!(
            paths.root.is_absolute(),
            "artifact root must be absolute: {}",
            paths.root.display()
        );
    }

    #[test]
    fn artifacts_serialize_to_snake_case() {
        let all_including_storage: &[Artifact] = &[
            Artifact::Body,
            Artifact::RenderedHtml,
            Artifact::Text,
            Artifact::Screenshot,
            Artifact::Network,
            Artifact::Console,
            Artifact::Observation,
            Artifact::Storage,
        ];
        for a in all_including_storage {
            let s = serde_json::to_string(a).unwrap_or_default();
            assert_eq!(s, format!("\"{}\"", a.as_str()));
        }
    }
}
