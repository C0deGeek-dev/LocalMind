//! Resolve which project store a `--project` value refers to.
//!
//! Both `localmind ui` and `localmind review` take `--project` (default `.`) and
//! open `<project>/.localmind/localmind.sqlite`. With an exact match only,
//! running either from a subdirectory of a project silently opened a *different,
//! empty* store — indistinguishable from "the store is genuinely empty". This
//! resolver walks ancestors for the `.localmind.toml` marker (nearest wins), so
//! a command run anywhere inside a project finds that project's store, and
//! reports when nothing was found instead of opening an empty one by surprise.

use std::path::{Path, PathBuf};

/// The file that marks a directory as a LocalMind project root.
const CONFIG_FILE: &str = ".localmind.toml";

/// Outcome of resolving a `--project` value to a store root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreRoot {
    /// A project root was found (it holds `.localmind.toml`).
    Found {
        /// The resolved project root.
        root: PathBuf,
        /// True when the root is an ancestor of the starting directory — i.e. the
        /// resolver walked up to find it, rather than the given path being a root.
        walked_up: bool,
    },
    /// No `.localmind.toml` at the starting directory or any ancestor.
    NotFound {
        /// The directory the search started from (absolutised for display).
        start: PathBuf,
    },
}

/// Resolve `project` to a store root: the given directory if it holds
/// `.localmind.toml`, else the nearest ancestor that does. Never creates
/// anything and never touches the store. The nearest project shadows a farther
/// one, mirroring how tools resolve a repo root from a working directory.
#[must_use]
pub fn resolve_store_root(project: &Path) -> StoreRoot {
    let start = absolutize(project);
    for dir in start.ancestors() {
        if dir.join(CONFIG_FILE).is_file() {
            return StoreRoot::Found {
                root: dir.to_path_buf(),
                walked_up: dir != start,
            };
        }
    }
    StoreRoot::NotFound { start }
}

/// Absolutise `project` for a clean banner and correct ancestor walk. `.`
/// resolves to the current directory (not a trailing-`.` path); a relative path
/// is joined onto the current directory; an absolute path is used as-is.
fn absolutize(project: &Path) -> PathBuf {
    if project == Path::new(".") {
        return std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    }
    if project.is_absolute() {
        return project.to_path_buf();
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(project))
        .unwrap_or_else(|_| project.to_path_buf())
}

/// Resolve a `--project` value to a usable root for a CLI command, printing an
/// actionable message on stderr and returning `None` when no store exists at or
/// above it (so the caller stops instead of opening/creating an empty store).
/// Announces a walk-up so the operator sees which store was actually opened.
#[must_use]
pub fn resolve_or_report(project: &Path) -> Option<PathBuf> {
    match resolve_store_root(project) {
        StoreRoot::Found { root, walked_up } => {
            if walked_up {
                eprintln!(
                    "localmind: using store at {} (found by walking up)",
                    root.display()
                );
            }
            Some(root)
        }
        StoreRoot::NotFound { start } => {
            eprintln!(
                "localmind: no store found at or above {} \
                 (no ancestor holds .localmind.toml) — run from a project directory \
                 or pass --project <path>",
                start.display()
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn an_ancestor_holding_the_config_is_found_from_a_subdirectory() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join(CONFIG_FILE), "").unwrap();
        let sub = root.join("a").join("b");
        std::fs::create_dir_all(&sub).unwrap();

        match resolve_store_root(&sub) {
            StoreRoot::Found {
                root: found,
                walked_up,
            } => {
                assert_eq!(found, root);
                assert!(walked_up, "the root was found by walking up");
            }
            other => panic!("expected Found, got {other:?}"),
        }
    }

    #[test]
    fn a_root_holding_the_config_is_found_without_walking_up() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join(CONFIG_FILE), "").unwrap();

        match resolve_store_root(root) {
            StoreRoot::Found {
                root: found,
                walked_up,
            } => {
                assert_eq!(found, root);
                assert!(!walked_up, "the given path is itself the root");
            }
            other => panic!("expected Found, got {other:?}"),
        }
    }

    #[test]
    fn the_nearest_root_shadows_a_farther_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let outer = dir.path();
        std::fs::write(outer.join(CONFIG_FILE), "").unwrap();
        let inner = outer.join("inner");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(inner.join(CONFIG_FILE), "").unwrap();
        let deep = inner.join("deep");
        std::fs::create_dir_all(&deep).unwrap();

        match resolve_store_root(&deep) {
            StoreRoot::Found { root: found, .. } => {
                assert_eq!(found, inner, "the nearer root wins over a farther ancestor");
            }
            other => panic!("expected Found, got {other:?}"),
        }
    }

    // Note: a hermetic `NotFound` test is not possible here — the walk goes to the
    // filesystem root, and a dev/CI machine may hold a `.localmind.toml` in a home
    // or drive-root ancestor of the OS temp dir. The `NotFound` branch is the
    // loop's fall-through; `resolve_or_report` handles it. Nearest-wins and
    // walk-up (the load-bearing behaviour) are covered above.
}
