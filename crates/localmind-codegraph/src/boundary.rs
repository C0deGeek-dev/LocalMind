//! The ingest boundary: which files the ingester may read.
//!
//! The host supplies the candidate files through its own permission and
//! redaction boundary; this module is engine-internal defense in depth. It
//! rejects anything outside the workspace root and anything matching the
//! project's `excluded_paths`, and it never enumerates the filesystem itself.

use crate::CodeGraphError;
use std::path::{Path, PathBuf};

/// A file admitted through the boundary, with its repo-relative,
/// forward-slash path (the form stored on graph nodes).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmittedFile {
    pub absolute: PathBuf,
    pub relative: String,
}

/// Why a candidate file was refused. Rejections are reported, not fatal:
/// one out-of-boundary path must not abort an otherwise valid ingest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BoundaryRejection {
    OutsideRoot { path: PathBuf },
    Excluded { path: PathBuf, pattern: String },
    Unreadable { path: PathBuf },
}

pub struct IngestBoundary {
    root: PathBuf,
    excluded: Vec<String>,
}

impl IngestBoundary {
    /// `excluded` carries the project's `excluded_paths` entries; matching is
    /// substring-based over the normalized forward-slash path, consistent with
    /// how the store treats sensitive paths.
    pub fn new(root: impl Into<PathBuf>, excluded: Vec<String>) -> Result<Self, CodeGraphError> {
        let root = root.into();
        let root = root
            .canonicalize()
            .map_err(|source| CodeGraphError::InvalidRoot {
                path: root.clone(),
                source,
            })?;
        Ok(Self { root, excluded })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn admit(&self, candidate: impl AsRef<Path>) -> Result<AdmittedFile, BoundaryRejection> {
        let candidate = candidate.as_ref();
        let absolute = candidate
            .canonicalize()
            .map_err(|_| BoundaryRejection::Unreadable {
                path: candidate.to_path_buf(),
            })?;

        let relative =
            absolute
                .strip_prefix(&self.root)
                .map_err(|_| BoundaryRejection::OutsideRoot {
                    path: candidate.to_path_buf(),
                })?;
        let relative = forward_slashes(relative);

        let normalized_absolute = forward_slashes(&absolute);
        for pattern in &self.excluded {
            if pattern.is_empty() {
                continue;
            }
            let normalized_pattern = pattern.replace('\\', "/");
            if relative.contains(&normalized_pattern)
                || normalized_absolute.contains(&normalized_pattern)
            {
                return Err(BoundaryRejection::Excluded {
                    path: candidate.to_path_buf(),
                    pattern: pattern.clone(),
                });
            }
        }

        Ok(AdmittedFile { absolute, relative })
    }
}

fn forward_slashes(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::{BoundaryRejection, IngestBoundary};
    use std::fs;

    #[test]
    fn admits_files_under_the_root_with_relative_paths() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        fs::create_dir_all(temp_dir.path().join("src"))?;
        let file = temp_dir.path().join("src/lib.rs");
        fs::write(&file, "pub fn hello() {}")?;

        let boundary = IngestBoundary::new(temp_dir.path(), Vec::new())?;
        let admitted = boundary
            .admit(&file)
            .map_err(|rejection| format!("expected admission, got {rejection:?}"))?;

        assert_eq!(admitted.relative, "src/lib.rs");
        Ok(())
    }

    #[test]
    fn rejects_paths_outside_the_root() -> Result<(), Box<dyn std::error::Error>> {
        let workspace = tempfile::tempdir()?;
        let elsewhere = tempfile::tempdir()?;
        let outside = elsewhere.path().join("secret.rs");
        fs::write(&outside, "pub fn hidden() {}")?;

        let boundary = IngestBoundary::new(workspace.path(), Vec::new())?;

        assert!(matches!(
            boundary.admit(&outside),
            Err(BoundaryRejection::OutsideRoot { .. })
        ));
        Ok(())
    }

    #[test]
    fn rejects_excluded_paths() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        fs::create_dir_all(temp_dir.path().join("vendor/private"))?;
        let file = temp_dir.path().join("vendor/private/keys.rs");
        fs::write(&file, "pub const KEY: &str = \"value\";")?;

        let boundary = IngestBoundary::new(temp_dir.path(), vec!["vendor/private".to_string()])?;

        assert!(matches!(
            boundary.admit(&file),
            Err(BoundaryRejection::Excluded { .. })
        ));
        Ok(())
    }

    #[test]
    fn rejects_traversal_escapes_after_normalization() -> Result<(), Box<dyn std::error::Error>> {
        let workspace = tempfile::tempdir()?;
        let elsewhere = tempfile::tempdir()?;
        let outside = elsewhere.path().join("escape.rs");
        fs::write(&outside, "pub fn out() {}")?;
        // A path that starts inside the root but escapes through `..`.
        let sneaky = workspace.path().join("src").join("..").join("..");
        let sneaky = sneaky.join(
            outside
                .strip_prefix(elsewhere.path().parent().unwrap_or(elsewhere.path()))
                .unwrap_or(&outside),
        );

        let boundary = IngestBoundary::new(workspace.path(), Vec::new())?;

        assert!(boundary.admit(&sneaky).is_err());
        Ok(())
    }
}
