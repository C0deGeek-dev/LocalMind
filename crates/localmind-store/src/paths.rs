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
/// onto the target); on **any** failure — the write itself, the flush, or
/// the publishing rename — it is best-effort removed rather than left
/// behind (a cleanup failure never shadows the real error; the temp file is
/// inert either way, since no reader ever looks at it). Mirrors the same
/// temp-then-rename shape already used by `sync_engine.rs::write_bundle` for
/// the encrypted sync bundle.
///
/// `fs::rename` replaces an existing destination file atomically on both
/// POSIX (`rename(2)`) and current Windows (`std`'s Windows backend calls
/// `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING`) as long as both paths are
/// on the same volume, which a same-directory temp file always is.
pub fn atomic_write(path: &Path, contents: &[u8]) -> io::Result<()> {
    atomic_write_via(path, |file| file.write_all(contents))
}

/// The actual publish sequence behind [`atomic_write`], parameterized over
/// how bytes reach the temp file so a test can inject a fault partway
/// through the write and observe this exact code path react to it (rather
/// than hand-simulating what a crash leaves behind).
fn atomic_write_via<W>(path: &Path, write: W) -> io::Result<()>
where
    W: FnOnce(&mut fs::File) -> io::Result<()>,
{
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "path has no parent directory")
    })?;
    let (tmp_path, mut file) = create_unique_temp_file(parent, path)?;

    let result = (|| -> io::Result<()> {
        write(&mut file)?;
        // Flush the temp file's own contents to disk before the rename that
        // publishes it, so a crash right after the rename can never expose a
        // file whose bytes are still sitting in a write-back cache.
        file.sync_all()?;
        drop(file); // release the handle before rename, not after
        fs::rename(&tmp_path, path)
    })();

    if result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    result
}

/// Bounded so a persistent collision (or a persistent creation failure)
/// fails loudly instead of looping forever; five attempts is generous for a
/// name space keyed by PID + a monotonic counter + a nanosecond timestamp.
const MAX_TEMP_FILE_ATTEMPTS: u32 = 5;

/// Creates a temp file whose name is guaranteed unique at creation time —
/// `create_new` fails rather than silently truncating a file that happens to
/// already exist at the generated path (another writer's in-flight temp
/// file, or debris this same process failed to clean up on the previous
/// nanosecond), and a fresh name is retried a bounded number of times on
/// that one error kind.
fn create_unique_temp_file(parent: &Path, target: &Path) -> io::Result<(PathBuf, fs::File)> {
    let mut last_collision = None;
    for _ in 0..MAX_TEMP_FILE_ATTEMPTS {
        let candidate = unique_temp_path(parent, target)?;
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((candidate, file)),
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
                last_collision = Some(source);
            }
            Err(source) => return Err(source),
        }
    }
    Err(last_collision.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique temp file name",
        )
    }))
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

/// `pub(crate)` so a sibling module (the orphan reconciliation sweep) can
/// walk every scope directory this resolver would ever write to, instead of
/// hand-maintaining a second copy of this mapping that could silently drift.
pub(crate) fn scope_dir(scope: &MemoryScope) -> &'static str {
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
    use super::{atomic_write, atomic_write_via};
    use std::fs;
    use std::io::{self, Write};

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

    /// Crash-injection: drives the **real** `atomic_write_via` publish
    /// sequence with a writer that writes half a large payload to the temp
    /// file and then fails, reproducing what an interrupted write (crash,
    /// kill -9, full disk) leaves behind, without unsafe code or a child
    /// process (both of which this crate's `unsafe_code = "forbid"` and its
    /// dependency-light posture rule out). Because `path` itself is only
    /// ever touched by the terminal `fs::rename` — reached only after a
    /// fully successful write+flush — an interruption anywhere before that
    /// point structurally cannot expose a truncated or partial file at the
    /// final path; this test proves that structural claim by actually
    /// exercising the failure, not by asserting the design in prose.
    #[test]
    fn an_interrupted_write_never_touches_the_final_path_and_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("memory.md");
        fs::write(&target, b"prior content, written before the interruption").unwrap();

        let large_payload = vec![b'x'; 1_000_000];
        let half = large_payload.len() / 2;
        let result = atomic_write_via(&target, |file| {
            file.write_all(&large_payload[..half])?;
            Err(io::Error::other("simulated interruption"))
        });

        assert!(result.is_err());
        // Observed: the final path is byte-identical to its pre-interruption
        // content — never touched by the interrupted write.
        assert_eq!(
            fs::read(&target).unwrap(),
            b"prior content, written before the interruption"
        );
        // Observed: the dead temp file from the interrupted write is cleaned
        // up, not left to accumulate.
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path() != target)
            .collect();
        assert!(
            leftovers.is_empty(),
            "expected no leftover temp files, found: {leftovers:?}"
        );

        // Expected: a real, uninterrupted atomic_write afterwards still
        // succeeds and fully publishes its own content.
        atomic_write(&target, &large_payload).unwrap();
        assert_eq!(fs::read(&target).unwrap(), large_payload);
    }

    /// A distinct scenario from the interruption above: a temp file left on
    /// disk by an *earlier, already-finished* crashed run must not corrupt
    /// or block a later, unrelated write.
    #[test]
    fn a_stray_temp_file_from_an_earlier_crash_does_not_interfere_with_a_new_write() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("memory.md");
        fs::write(&target, b"prior content").unwrap();
        fs::write(dir.path().join("memory.md.999-0-123.tmp"), b"stale debris").unwrap();

        atomic_write(&target, b"fresh content").unwrap();

        assert_eq!(fs::read(&target).unwrap(), b"fresh content");
    }

    /// The publishing `fs::rename`, not only the write, can fail (a locked
    /// destination, a permissions error, a destination that is itself a
    /// directory). The temp file must not survive that failure either.
    #[test]
    fn a_failed_publish_rename_still_cleans_up_the_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        // A directory can never be the source of a rename onto it via
        // `fs::rename` on either platform, so this deterministically fails
        // the publish step after the temp file has already been fully
        // written.
        let target = dir.path().join("memory.md");
        fs::create_dir_all(&target).unwrap();

        let result = atomic_write(&target, b"content");

        assert!(result.is_err());
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path() != target)
            .collect();
        assert!(
            leftovers.is_empty(),
            "expected the failed rename's temp file to be cleaned up, found: {leftovers:?}"
        );
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
