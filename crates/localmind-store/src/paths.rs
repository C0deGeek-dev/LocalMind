use crate::{MarkdownMemoryFormat, ProjectConfig};
use localmind_core::{MemoryEntry, MemoryEntryId, MemoryScope};
use std::fs;
use std::path::{Component, Path, PathBuf};
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
        fs::write(&path, MarkdownMemoryFormat::serialize(entry)).map_err(|source| {
            MemoryPathError::WriteMemory {
                path: path.clone(),
                source,
            }
        })?;
        Ok(path)
    }
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
