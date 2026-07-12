//! Path-independent project identity.
//!
//! `D:\repos\X` on one machine and `C:\repos\X` on another are the *same*
//! project, so a project store must be keyed by something stable across
//! machines — never a filesystem path. The key is resolved in priority order:
//! an explicit `[sync] project_key`, else the normalized git `origin` remote,
//! else (weakly) the project directory name.

use crate::ProjectConfig;
use std::fs;
use std::path::Path;

/// Where a resolved [`ProjectIdentity`] key came from. The source is surfaced so
/// a user can see whether their two machines will actually agree (an explicit
/// key or a shared git remote agree; a directory-name fallback is only as stable
/// as the folder name and can collide).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectIdentitySource {
    /// Pinned via `[sync] project_key`.
    Explicit,
    /// Derived from the git `origin` remote URL.
    GitRemote,
    /// Fallback: the project directory's name. Weak — two unrelated repos with
    /// the same folder name collide, and renaming the folder changes identity.
    DirectoryName,
}

impl ProjectIdentitySource {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ProjectIdentitySource::Explicit => "explicit",
            ProjectIdentitySource::GitRemote => "git_remote",
            ProjectIdentitySource::DirectoryName => "directory_name",
        }
    }

    /// Whether this source is stable enough to agree across machines without the
    /// user checking it. A directory-name fallback is not.
    #[must_use]
    pub fn is_stable(self) -> bool {
        !matches!(self, ProjectIdentitySource::DirectoryName)
    }
}

/// A stable, path-independent identity for a project store.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectIdentity {
    pub key: String,
    pub source: ProjectIdentitySource,
}

impl ProjectIdentity {
    /// Resolve the identity for a project. Total — always yields a key, falling
    /// back to the directory name when nothing more stable is available.
    #[must_use]
    pub fn resolve(config: &ProjectConfig) -> Self {
        if let Some(key) = config.sync_project_key() {
            let key = key.trim();
            if !key.is_empty() {
                return Self {
                    key: key.to_string(),
                    source: ProjectIdentitySource::Explicit,
                };
            }
        }
        if let Some(remote) = read_git_origin(&config.project_root) {
            let normalized = normalize_remote(&remote);
            if !normalized.is_empty() {
                return Self {
                    key: normalized,
                    source: ProjectIdentitySource::GitRemote,
                };
            }
        }
        Self {
            key: directory_key(&config.project_root),
            source: ProjectIdentitySource::DirectoryName,
        }
    }
}

/// Read the `origin` remote URL from `<root>/.git/config`. Returns `None` when
/// there is no `.git` directory, when `.git` is a *file* (a worktree/submodule
/// gitdir redirect — deliberately not followed here), or when no `origin` url is
/// present. Best-effort: any read/parse failure yields `None`, never an error.
fn read_git_origin(root: &Path) -> Option<String> {
    let git_dir = root.join(".git");
    if !git_dir.is_dir() {
        return None;
    }
    let content = fs::read_to_string(git_dir.join("config")).ok()?;
    let mut in_origin = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            // A new section header; we care only about `[remote "origin"]`.
            in_origin = section_is_origin(trimmed);
            continue;
        }
        if in_origin {
            if let Some(value) = trimmed.strip_prefix("url") {
                if let Some((_, url)) = value.split_once('=') {
                    let url = url.trim();
                    if !url.is_empty() {
                        return Some(url.to_string());
                    }
                }
            }
        }
    }
    None
}

/// Whether a git-config section header names the `origin` remote. Accepts both
/// `[remote "origin"]` and the (rare) `[remote.origin]` spellings.
fn section_is_origin(header: &str) -> bool {
    let inner = header.trim_start_matches('[').trim_end_matches(']').trim();
    inner == "remote \"origin\"" || inner == "remote.origin"
}

/// Normalize a remote URL to a stable `host/path` key so that the same repo
/// cloned over HTTPS on one machine and SSH on another resolves identically:
/// scheme, `user@`, a trailing `.git`, and surrounding slashes are stripped, the
/// scp-style `host:path` colon becomes `/`, and the result is lowercased.
fn normalize_remote(url: &str) -> String {
    let mut rest = url.trim();
    for scheme in [
        "git+ssh://",
        "ssh://",
        "git://",
        "https://",
        "http://",
        "ftps://",
        "ftp://",
    ] {
        if let Some(stripped) = rest.strip_prefix(scheme) {
            rest = stripped;
            break;
        }
    }
    // Drop any `user@` userinfo (e.g. the `git@` of an scp-style or ssh URL).
    if let Some((_, after)) = rest.split_once('@') {
        rest = after;
    }
    // scp-style `host:org/repo` → `host/org/repo` (only the first colon, which
    // separates host from path; later colons would be path content).
    let mut normalized = rest.replacen(':', "/", 1);
    normalized = normalized.trim_matches('/').to_string();
    if let Some(stripped) = normalized.strip_suffix(".git") {
        normalized = stripped.to_string();
    }
    normalized.trim_matches('/').to_lowercase()
}

/// The directory-name fallback key, tagged `dir:` so it can never be mistaken
/// for a git-remote key.
fn directory_key(root: &Path) -> String {
    let name = root
        .file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    format!("dir:{name}")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::{normalize_remote, ProjectIdentity, ProjectIdentitySource};
    use crate::ProjectConfig;
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    fn write_project(dir: &Path, extra: &str) {
        fs::write(
            dir.join(".localmind.toml"),
            format!("[learning]\nenabled = true\n{extra}"),
        )
        .unwrap();
    }

    fn init_git_origin(dir: &Path, url: &str) {
        let git = dir.join(".git");
        fs::create_dir_all(&git).unwrap();
        fs::write(
            git.join("config"),
            format!("[core]\n\tbare = false\n[remote \"origin\"]\n\turl = {url}\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n"),
        )
        .unwrap();
    }

    #[test]
    fn https_and_ssh_remotes_normalize_to_the_same_key() {
        assert_eq!(
            normalize_remote("https://github.com/C0deGeek-dev/LocalMind.git"),
            "github.com/c0degeek-dev/localmind"
        );
        assert_eq!(
            normalize_remote("git@github.com:C0deGeek-dev/LocalMind.git"),
            "github.com/c0degeek-dev/localmind"
        );
        assert_eq!(
            normalize_remote("ssh://git@github.com/C0deGeek-dev/LocalMind"),
            "github.com/c0degeek-dev/localmind"
        );
    }

    #[test]
    fn explicit_key_wins_over_everything() {
        let dir = TempDir::new().unwrap();
        write_project(dir.path(), "[sync]\nproject_key = \"my-pinned-key\"\n");
        init_git_origin(dir.path(), "https://github.com/org/repo.git");
        let config = ProjectConfig::discover(dir.path()).unwrap();
        let identity = ProjectIdentity::resolve(&config);
        assert_eq!(identity.key, "my-pinned-key");
        assert_eq!(identity.source, ProjectIdentitySource::Explicit);
        assert!(identity.source.is_stable());
    }

    #[test]
    fn same_repo_different_path_resolves_to_the_same_key() {
        // Two checkout paths, same origin remote → identical, path-independent key.
        let pc = TempDir::new().unwrap();
        let laptop = TempDir::new().unwrap();
        write_project(pc.path(), "");
        write_project(laptop.path(), "");
        init_git_origin(pc.path(), "https://github.com/org/repo.git");
        init_git_origin(laptop.path(), "git@github.com:org/repo.git");
        let pc_id = ProjectIdentity::resolve(&ProjectConfig::discover(pc.path()).unwrap());
        let laptop_id = ProjectIdentity::resolve(&ProjectConfig::discover(laptop.path()).unwrap());
        assert_eq!(pc_id.key, laptop_id.key);
        assert_eq!(pc_id.source, ProjectIdentitySource::GitRemote);
    }

    #[test]
    fn no_git_falls_back_to_directory_name_and_flags_it_unstable() {
        let dir = TempDir::new().unwrap();
        write_project(dir.path(), "");
        let identity = ProjectIdentity::resolve(&ProjectConfig::discover(dir.path()).unwrap());
        assert_eq!(identity.source, ProjectIdentitySource::DirectoryName);
        assert!(identity.key.starts_with("dir:"));
        assert!(!identity.source.is_stable());
    }
}
