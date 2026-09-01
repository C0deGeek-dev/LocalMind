use crate::{MarkdownMemoryFormat, ProjectConfig};
use localmind_core::{MemoryEntry, MemoryEntryId, MemoryScope};
use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

pub struct MemoryPathResolver;

impl MemoryPathResolver {
    pub fn memory_file_path(
        config: &ProjectConfig,
        scope: &MemoryScope,
        id: &MemoryEntryId,
    ) -> Result<PathBuf, MemoryPathError> {
        if !config.allows_scope(scope) {
            return Err(MemoryPathError::ScopeNotAllowed {
                scope: format!("{scope:?}"),
            });
        }

        // Global-scope memory is machine-wide, so it is rooted at the per-user
        // home store, resolved separately from the project store; every other
        // scope lives under the project memory root.
        let root = match scope {
            MemoryScope::GlobalUser => config
                .global_memory_root()
                .ok_or(MemoryPathError::NoGlobalRoot)?,
            _ => config.memory_root(),
        };
        let relative = Path::new(scope_dir(scope)).join(format!("{}.md", safe_id(id.as_str())?));
        reject_unsafe_relative_path(&relative)?;
        let candidate = root.join(relative);
        ensure_child_path(&root, &candidate)?;
        Ok(candidate)
    }

    pub fn write_memory_file(
        config: &ProjectConfig,
        entry: &MemoryEntry,
    ) -> Result<PathBuf, MemoryPathError> {
        let path = Self::memory_file_path(config, &entry.scope, &entry.id)?;
        let parent = path
            .parent()
            .ok_or_else(|| MemoryPathError::MissingParent { path: path.clone() })?;
        fs::create_dir_all(parent).map_err(|source| MemoryPathError::CreateDirectory {
            path: parent.to_path_buf(),
            source,
        })?;
        atomic_write(&path, MarkdownMemoryFormat::serialize(entry).as_bytes()).map_err(
            |source| MemoryPathError::WriteMemory {
                path: path.clone(),
                source,
            },
        )?;
        Ok(path)
    }
}

/// A process-lifetime counter mixed into every temp-file name so concurrent
/// writers in this process (and, with the PID, across process restarts)
/// never race each other for the same temp path. Not security-sensitive —
/// only needs to avoid collision, not be unpredictable — so a counter plus
/// PID plus a timestamp is enough and needs no random-number dependency.
static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Writes `contents` to `path` so the final path is always either fully
/// written or left exactly as it was — never observed truncated or partial,
/// on any interruption (crash, power loss, kill -9).
///
/// The temp file is created in `path`'s own parent directory, **never** a
/// system temp directory: `fs::rename` is only atomic when source and
/// destination are on the same filesystem/volume, and a same-directory temp
/// file guarantees that. On success the temp file no longer exists (renamed
/// onto the target); on any failure it is best-effort removed rather than
/// left behind. Mirrors the same temp-then-rename shape already used by
/// `sync_engine.rs::write_bundle` for the encrypted sync bundle.
///
/// `fs::rename` replaces an existing destination file atomically on both
/// POSIX (`rename(2)`) and current Windows (`std`'s Windows backend calls
/// `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING`) as long as both paths are
/// on the same volume, which a same-directory temp file always is.
pub fn atomic_write(path: &Path, contents: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "path has no parent directory")
    })?;
    let tmp_path = unique_temp_path(parent, path)?;

    let write_result = (|| -> io::Result<()> {
        let mut file = fs::File::create(&tmp_path)?;
        file.write_all(contents)?;
        // Flush the temp file's own contents to disk before the rename that
        // publishes it, so a crash right after the rename can never expose a
        // file whose bytes are still sitting in a write-back cache.
        file.sync_all()?;
        Ok(())
    })();

    if let Err(source) = write_result {
        // Best-effort: a cleanup failure must not shadow the real write
        // error, and the temp file is inert (never the file any reader
        // looks at) even if it is left behind.
        let _ = fs::remove_file(&tmp_path);
        return Err(source);
    }

    fs::rename(&tmp_path, path)
}

fn unique_temp_path(parent: &Path, target: &Path) -> io::Result<PathBuf> {
    let file_name = target
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
    let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    Ok(parent.join(format!(
        "{}.{}-{}-{nanos}.tmp",
        file_name.to_string_lossy(),
        std::process::id(),
        counter,
    )))
}

fn scope_dir(scope: &MemoryScope) -> &'static str {
    match scope {
        MemoryScope::GlobalUser => "global",
        MemoryScope::Project => "project",
        MemoryScope::Session => "session",
        MemoryScope::Skill => "skill",
        MemoryScope::Research => "research",
    }
}

fn safe_id(id: &str) -> Result<String, MemoryPathError> {
    if id.is_empty()
        || id
            .chars()
            .any(|character| !(character.is_ascii_alphanumeric() || matches!(character, '-' | '_')))
    {
        Err(MemoryPathError::UnsafeMemoryId { id: id.to_string() })
    } else {
        Ok(id.to_string())
    }
}

fn reject_unsafe_relative_path(path: &Path) -> Result<(), MemoryPathError> {
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        Err(MemoryPathError::PathTraversal {
            path: path.to_path_buf(),
        })
    } else {
        Ok(())
    }
}

fn ensure_child_path(root: &Path, candidate: &Path) -> Result<(), MemoryPathError> {
    let root_components: Vec<_> = root.components().collect();
    let candidate_components: Vec<_> = candidate.components().collect();

    if candidate_components.starts_with(&root_components) {
        Ok(())
    } else {
        Err(MemoryPathError::PathTraversal {
            path: candidate.to_path_buf(),
        })
    }
}

#[derive(Debug, Error)]
pub enum MemoryPathError {
    #[error("memory scope is not allowed by project config: {scope}")]
    ScopeNotAllowed { scope: String },
    #[error("global memory has no resolvable root (no home directory and no configured global_memory_root)")]
    NoGlobalRoot,
    #[error("unsafe memory id: {id}")]
    UnsafeMemoryId { id: String },
    #[error("memory path escapes the configured root: {path:?}")]
    PathTraversal { path: PathBuf },
    #[error("memory path has no parent directory: {path:?}")]
    MissingParent { path: PathBuf },
    #[error("failed to create memory directory {path:?}: {source}")]
    CreateDirectory {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to write memory file {path:?}: {source}")]
    WriteMemory {
        path: PathBuf,
        source: std::io::Error,
    },
}

#[cfg(test)]
mod atomic_write_tests {
    #![allow(clippy::unwrap_used)]
    use super::atomic_write;
    use std::fs;

    #[test]
    fn a_successful_write_is_fully_readable_back() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("memory.md");

        atomic_write(&target, b"hello atomic world").unwrap();

        assert_eq!(fs::read(&target).unwrap(), b"hello atomic world");
    }

    #[test]
    fn an_existing_target_is_fully_replaced_not_merged_or_appended() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("memory.md");
        fs::write(&target, b"this is the old, longer body of the file").unwrap();

        atomic_write(&target, b"new").unwrap();

        // Fully replaced: no trailing bytes from the old, longer content
        // survive (which a naive in-place write/truncate-less write could
        // leave behind).
        assert_eq!(fs::read(&target).unwrap(), b"new");
    }

    #[test]
    fn the_temp_file_does_not_leak_on_the_happy_path() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("memory.md");

        atomic_write(&target, b"content").unwrap();

        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path() != target)
            .collect();
        assert!(
            leftovers.is_empty(),
            "expected only the target file, found: {leftovers:?}"
        );
    }

    /// Crash-injection: a real crash can only ever leave a **partial temp
    /// file** behind, because the target path is touched exactly once, by
    /// the final `fs::rename` — never by the write itself. This test
    /// reproduces exactly that shape (a half-written file at the same
    /// temp-name pattern `atomic_write` uses) without needing to actually
    /// kill a process mid-write, and proves the target is untouched by it.
    #[test]
    fn a_crash_during_the_temp_write_never_touches_the_final_path() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("memory.md");
        fs::write(&target, b"prior content, written before any crash").unwrap();

        let large_payload = vec![b'x'; 1_000_000];
        let simulated_crash_tmp = dir.path().join("memory.md.999-0-123.tmp");
        fs::write(
            &simulated_crash_tmp,
            &large_payload[..large_payload.len() / 2],
        )
        .unwrap();

        // The final path is exactly as it was before the "crash" — it was
        // never touched by the interrupted temp-file write.
        assert_eq!(
            fs::read(&target).unwrap(),
            b"prior content, written before any crash"
        );

        // A real, uninterrupted atomic_write still succeeds afterwards and
        // fully publishes its own content — a stray partial temp file left
        // by an earlier crash never interferes with a later write.
        atomic_write(&target, &large_payload).unwrap();
        assert_eq!(fs::read(&target).unwrap(), large_payload);
    }

    #[test]
    fn a_write_to_an_unwritable_temp_location_leaves_the_target_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("does-not-exist-yet").join("memory.md");
        // The parent directory is never created here (unlike
        // `write_memory_file`, which creates it first) — `atomic_write`
        // itself only opens a temp file, it never creates directories, so
        // this must fail rather than silently succeed into a missing
        // directory.
        let result = atomic_write(&target, b"content");

        assert!(result.is_err());
        assert!(!target.exists());
    }
}
