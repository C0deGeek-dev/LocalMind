use crate::{MemoryPersistence, SkillDraftError, SkillDraftStore, StoreConfigError};
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextExportTarget {
    Generic,
    ClaudeCode,
    OpenAiCodex,
    LocalPilot,
}

impl ContextExportTarget {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Generic => "generic",
            Self::ClaudeCode => "claude-code",
            Self::OpenAiCodex => "open-ai-codex",
            Self::LocalPilot => "localpilot",
        }
    }

    #[must_use]
    fn heading(self) -> &'static str {
        match self {
            Self::Generic => "Generic agent context",
            Self::ClaudeCode => "Claude Code context",
            Self::OpenAiCodex => "OpenAI Codex context",
            Self::LocalPilot => "LocalPilot built-in context",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextExport {
    pub target: ContextExportTarget,
    pub body_markdown: String,
}

pub struct ContextExporter {
    project_root: PathBuf,
}

impl ContextExporter {
    pub fn open_project(project_root: impl AsRef<Path>) -> Result<Self, ContextExportError> {
        let config =
            crate::ProjectConfig::discover(project_root).map_err(ContextExportError::Config)?;
        Ok(Self {
            project_root: config.project_root,
        })
    }

    pub fn export(
        &self,
        query: &str,
        target: ContextExportTarget,
    ) -> Result<ContextExport, ContextExportError> {
        let persistence = MemoryPersistence::open_project(&self.project_root)?;
        let skills = SkillDraftStore::open_project(&self.project_root)?;
        let memories = persistence.search(query)?;
        let drafts = skills.list()?;
        let mut body = String::new();

        body.push_str(&format!("# {}\n\n", target.heading()));
        body.push_str(&format!("Query: `{}`\n\n", query));
        body.push_str("## Accepted Memory\n\n");
        if memories.is_empty() {
            body.push_str("- No accepted memory matched this query.\n\n");
        } else {
            for result in memories.iter().take(6) {
                body.push_str(&format!(
                    "- `{}` score={} path=`{}`\n  {}\n",
                    result.memory_id,
                    result.score,
                    result.path.display(),
                    result.snippet
                ));
            }
            body.push('\n');
        }

        body.push_str("## Suggested Skills\n\n");
        if drafts.is_empty() {
            body.push_str("- No disabled skill drafts are available.\n\n");
        } else {
            for record in drafts.iter().take(6) {
                body.push_str(&format!(
                    "- `{}` `{}`: {}\n",
                    record.draft.id, record.draft.name, record.draft.description
                ));
            }
            body.push('\n');
        }

        if matches!(target, ContextExportTarget::LocalPilot) {
            body.push_str("## Host Integration\n\n");
            body.push_str(
                "- Render this as bundled LocalPilot learning context; do not require a separate LocalMind install.\n\n",
            );
        }

        persistence.record_context_export(query, target.label())?;
        Ok(ContextExport {
            target,
            body_markdown: body,
        })
    }
}

#[derive(Debug, Error)]
pub enum ContextExportError {
    #[error(transparent)]
    Config(#[from] StoreConfigError),
    #[error(transparent)]
    MemoryPersistence(#[from] crate::MemoryPersistenceError),
    #[error(transparent)]
    SkillDraft(#[from] SkillDraftError),
}
