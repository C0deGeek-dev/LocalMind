//! Storage boundary for LocalMind.
//!
//! The MVP keeps durable memory Markdown-first and uses SQLite for queue, audit,
//! and index state. This crate will own that persistence behavior; subject 01
//! only establishes the dependency direction.

mod config;
mod markdown;
mod paths;

pub use config::{LearningConfig, LocalMindConfig, ProjectConfig, StoreConfigError};
pub use markdown::MarkdownMemoryFormat;
pub use paths::{MemoryPathError, MemoryPathResolver};

use localmind_core::{LearningAuditEvent, MemoryEntry, ReviewItem};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StoreCapabilities {
    pub markdown_memory: bool,
    pub review_queue: bool,
    pub audit_log: bool,
    pub search_index: bool,
}

impl StoreCapabilities {
    #[must_use]
    pub fn mvp() -> Self {
        Self {
            markdown_memory: true,
            review_queue: true,
            audit_log: true,
            search_index: true,
        }
    }
}

pub type StoreRecordSet = (MemoryEntry, ReviewItem, LearningAuditEvent);

#[cfg(test)]
mod tests {
    use super::{
        MarkdownMemoryFormat, MemoryPathError, MemoryPathResolver, ProjectConfig,
        StoreCapabilities, StoreConfigError,
    };
    use localmind_core::{
        Confidence, EvidenceKind, EvidenceRef, LessonCategory, MemoryEntry, MemoryEntryId,
        MemoryScope, MemoryStatus, SessionId,
    };
    use std::fs;

    #[test]
    fn mvp_store_shape_keeps_memory_and_audit_separate() {
        let capabilities = StoreCapabilities::mvp();

        assert!(capabilities.markdown_memory);
        assert!(capabilities.audit_log);
    }

    #[test]
    fn missing_config_refuses_learning_writes() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;

        let error = ProjectConfig::discover(temp_dir.path());

        assert!(matches!(error, Err(StoreConfigError::MissingConfig { .. })));
        Ok(())
    }

    #[test]
    fn disabled_project_refuses_learning_writes() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        fs::write(
            temp_dir.path().join(".localmind.toml"),
            "[learning]\nenabled = false\n",
        )?;

        let error = ProjectConfig::discover(temp_dir.path());

        assert!(matches!(
            error,
            Err(StoreConfigError::LearningDisabled { .. })
        ));
        Ok(())
    }

    #[test]
    fn valid_config_enables_project_scope() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        fs::write(
            temp_dir.path().join(".localmind.toml"),
            "[learning]\nenabled = true\nmemory_root = \".localmind/memory\"\nallowed_scopes = [\"project\"]\n",
        )?;

        let config = ProjectConfig::discover(temp_dir.path())?;

        assert!(config.allows_scope(&MemoryScope::Project));
        assert!(!config.allows_scope(&MemoryScope::GlobalUser));
        assert_eq!(
            config.memory_root(),
            temp_dir.path().join(".localmind/memory")
        );
        Ok(())
    }

    #[test]
    fn malformed_config_has_clear_error() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        fs::write(temp_dir.path().join(".localmind.toml"), "[learning\n")?;

        let error = ProjectConfig::discover(temp_dir.path());

        assert!(matches!(
            error,
            Err(StoreConfigError::MalformedConfig { .. })
        ));
        Ok(())
    }

    #[test]
    fn unsafe_memory_root_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        fs::write(
            temp_dir.path().join(".localmind.toml"),
            "[learning]\nenabled = true\nmemory_root = \"../memory\"\n",
        )?;

        let error = ProjectConfig::discover(temp_dir.path());

        assert!(matches!(
            error,
            Err(StoreConfigError::UnsafeMemoryRoot { .. })
        ));
        Ok(())
    }

    #[test]
    fn unsafe_memory_id_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        fs::write(
            temp_dir.path().join(".localmind.toml"),
            "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n",
        )?;
        let config = ProjectConfig::discover(temp_dir.path())?;

        let error = MemoryPathResolver::memory_file_path(
            &config,
            &MemoryScope::Project,
            &MemoryEntryId::new("../escape"),
        );

        assert!(matches!(error, Err(MemoryPathError::UnsafeMemoryId { .. })));
        Ok(())
    }

    #[test]
    fn markdown_memory_serializes_provenance_fields() -> Result<(), Box<dyn std::error::Error>> {
        let entry = sample_memory_entry()?;

        let markdown = MarkdownMemoryFormat::serialize(&entry);

        assert!(markdown.contains("scope: Project"));
        assert!(markdown.contains("category: ProjectConvention"));
        assert!(markdown.contains("confidence: 0.900"));
        assert!(markdown.contains("source_session: session-1"));
        assert!(markdown.contains("related_files:"));
        assert!(markdown.contains("src/lib.rs"));
        assert!(markdown.contains("related_entities:"));
        assert!(markdown.contains("ReviewQueue"));
        assert!(markdown.contains("evidence:"));
        assert!(markdown.contains("Prefer project-scoped reviewed memory."));
        Ok(())
    }

    #[test]
    fn memory_file_creation_stays_inside_memory_root() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        fs::write(
            temp_dir.path().join(".localmind.toml"),
            "[learning]\nenabled = true\nmemory_root = \".localmind/memory\"\nallowed_scopes = [\"project\"]\n",
        )?;
        let config = ProjectConfig::discover(temp_dir.path())?;
        let entry = sample_memory_entry()?;

        let path = MemoryPathResolver::write_memory_file(&config, &entry)?;
        let content = fs::read_to_string(&path)?;

        assert_eq!(
            path,
            temp_dir
                .path()
                .join(".localmind/memory/project/memory-1.md")
        );
        assert!(content.contains("id: memory-1"));
        Ok(())
    }

    fn sample_memory_entry() -> Result<MemoryEntry, Box<dyn std::error::Error>> {
        let evidence = EvidenceRef::new(EvidenceKind::Transcript, "session transcript").redacted();

        Ok(MemoryEntry {
            id: MemoryEntryId::new("memory-1"),
            scope: MemoryScope::Project,
            body: "Prefer project-scoped reviewed memory.".to_string(),
            category: LessonCategory::ProjectConvention,
            confidence: Confidence::new(0.9)?,
            source_session: Some(SessionId::new("session-1")),
            evidence: vec![evidence],
            tags: vec!["reviewed".to_string()],
            related_files: vec!["src/lib.rs".to_string()],
            related_entities: vec!["ReviewQueue".to_string()],
            created_at: None,
            updated_at: None,
            supersedes: Vec::new(),
            contradicts: Vec::new(),
            status: MemoryStatus::Active,
        })
    }
}
