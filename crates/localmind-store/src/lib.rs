//! Storage boundary for LocalMind.
//!
//! The MVP keeps durable memory Markdown-first and uses SQLite for queue, audit,
//! and index state. This crate will own that persistence behavior; subject 01
//! only establishes the dependency direction.

mod config;
mod extraction;
mod import;
mod markdown;
mod paths;
mod redaction;
mod review_queue;

pub use config::{LearningConfig, LocalMindConfig, ProjectConfig, StoreConfigError};
pub use extraction::{
    CloseoutError, CloseoutProcessor, CloseoutReport, DeterministicExtractor, ExtractionInput,
    ExtractionOutput, SessionExtractor,
};
pub use import::{
    ImportError, ImportReport, ImportedSession, TranscriptImportFormat, TranscriptImporter,
};
pub use markdown::MarkdownMemoryFormat;
pub use paths::{MemoryPathError, MemoryPathResolver};
pub use redaction::{Redaction, RedactionReport, Redactor};
pub use review_queue::{ReviewQueue, ReviewQueueError, ReviewQueueItem, ReviewQueueSummary};

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
        CloseoutProcessor, DeterministicExtractor, ExtractionInput, ExtractionOutput,
        MarkdownMemoryFormat, MemoryPathError, MemoryPathResolver, ProjectConfig, ReviewQueue,
        ReviewQueueError, SessionExtractor, StoreCapabilities, StoreConfigError,
        TranscriptImportFormat, TranscriptImporter,
    };
    use localmind_core::{
        CandidateLesson, Confidence, EvidenceKind, EvidenceRef, LessonCategory, LessonId,
        MemoryEntry, MemoryEntryId, MemoryScope, MemoryStatus, ReviewAction, ReviewDecision,
        SessionId, SessionSource, SessionSummary, SuggestedAction, ValidationStatus,
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

    #[test]
    fn import_refuses_disabled_projects() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let transcript_path = temp_dir.path().join("transcript.txt");
        fs::write(&transcript_path, "fixed a bug")?;
        fs::write(
            temp_dir.path().join(".localmind.toml"),
            "[learning]\nenabled = false\n",
        )?;

        let error = TranscriptImporter::import_file(
            temp_dir.path(),
            &transcript_path,
            SessionSource::GenericTranscript,
            TranscriptImportFormat::PlainText,
        );

        assert!(error.is_err());
        Ok(())
    }

    #[test]
    fn import_writes_only_redacted_transcript() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let transcript_path = temp_dir.path().join("transcript.txt");
        fs::write(
            &transcript_path,
            "token = sk-proj-abcdefghijklmnopqrstuvwxyz123456\npassword=super-secret\n",
        )?;
        fs::write(
            temp_dir.path().join(".localmind.toml"),
            "[learning]\nenabled = true\nexcluded_paths = [\"C:/Users/David/secrets\"]\n",
        )?;

        let report = TranscriptImporter::import_file(
            temp_dir.path(),
            &transcript_path,
            SessionSource::OpenAiCodex,
            TranscriptImportFormat::PlainText,
        )?;
        let transcript = fs::read_to_string(&report.redacted_transcript_path)?;
        let metadata = fs::read_to_string(&report.metadata_path)?;

        assert!(!transcript.contains("sk-proj-abcdefghijklmnopqrstuvwxyz123456"));
        assert!(!transcript.contains("super-secret"));
        assert!(transcript.contains("[REDACTED:openai_api_key]"));
        assert!(transcript.contains("[REDACTED:password_assignment]"));
        assert!(metadata.contains("OpenAiCodex"));
        Ok(())
    }

    #[test]
    fn import_redacts_configured_sensitive_paths() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let transcript_path = temp_dir.path().join("transcript.txt");
        fs::write(
            &transcript_path,
            "read C:/Users/David/secrets/config.json during debugging",
        )?;
        fs::write(
            temp_dir.path().join(".localmind.toml"),
            "[learning]\nenabled = true\nexcluded_paths = [\"C:/Users/David/secrets/config.json\"]\n",
        )?;

        let report = TranscriptImporter::import_file(
            temp_dir.path(),
            &transcript_path,
            SessionSource::ClaudeCode,
            TranscriptImportFormat::PlainText,
        )?;
        let transcript = fs::read_to_string(&report.redacted_transcript_path)?;

        assert!(!transcript.contains("C:/Users/David/secrets/config.json"));
        assert!(transcript.contains("[REDACTED:sensitive_path]"));
        Ok(())
    }

    #[test]
    fn closeout_persists_summary_and_candidate_lessons() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        fs::write(
            temp_dir.path().join(".localmind.toml"),
            "[learning]\nenabled = true\n",
        )?;
        let config = ProjectConfig::discover(temp_dir.path())?;
        let import = TranscriptImporter::import_text(
            &config,
            "Fixed failing tests.\nLesson: Prefer deterministic closeout tests.\nTODO: Create a skill for review queues.\n",
            SessionSource::GenericTranscript,
            TranscriptImportFormat::PlainText,
        )?;

        let report = CloseoutProcessor::closeout_project_session(
            temp_dir.path(),
            &import.session_id,
            &DeterministicExtractor,
        )?;
        let summary_json = fs::read_to_string(&report.summary_path)?;
        let candidates_json = fs::read_to_string(&report.candidates_path)?;

        assert!(summary_json.contains("Session session-"));
        assert!(summary_json.contains("Fixed failing tests."));
        assert!(candidates_json.contains("Prefer deterministic closeout tests."));
        assert!(candidates_json.contains("CreateSkillDraft"));
        assert_eq!(report.enqueued_count, 2);
        Ok(())
    }

    #[test]
    fn closeout_deduplicates_candidates() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        fs::write(
            temp_dir.path().join(".localmind.toml"),
            "[learning]\nenabled = true\n",
        )?;
        let config = ProjectConfig::discover(temp_dir.path())?;
        let import = TranscriptImporter::import_text(
            &config,
            "Lesson: Prefer fixtures.\nLesson: Prefer fixtures.\n",
            SessionSource::GenericTranscript,
            TranscriptImportFormat::PlainText,
        )?;

        let report = CloseoutProcessor::closeout_project_session(
            temp_dir.path(),
            &import.session_id,
            &DeterministicExtractor,
        )?;

        assert_eq!(report.candidate_count, 1);
        Ok(())
    }

    #[test]
    fn review_queue_migrates_and_starts_empty() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        fs::write(
            temp_dir.path().join(".localmind.toml"),
            "[learning]\nenabled = true\n",
        )?;

        let queue = ReviewQueue::open_project(temp_dir.path())?;
        let summary = queue.summary()?;

        assert_eq!(summary.pending, 0);
        assert!(temp_dir.path().join(".localmind/localmind.sqlite").exists());
        Ok(())
    }

    #[test]
    fn closeout_enqueue_is_idempotent() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        fs::write(
            temp_dir.path().join(".localmind.toml"),
            "[learning]\nenabled = true\n",
        )?;
        let config = ProjectConfig::discover(temp_dir.path())?;
        let import = TranscriptImporter::import_text(
            &config,
            "Lesson: Prefer queue fixtures.\n",
            SessionSource::GenericTranscript,
            TranscriptImportFormat::PlainText,
        )?;

        let first = CloseoutProcessor::closeout_project_session(
            temp_dir.path(),
            &import.session_id,
            &DeterministicExtractor,
        )?;
        let second = CloseoutProcessor::closeout_project_session(
            temp_dir.path(),
            &import.session_id,
            &DeterministicExtractor,
        )?;

        assert_eq!(first.enqueued_count, 1);
        assert_eq!(second.enqueued_count, 0);
        Ok(())
    }

    #[test]
    fn review_decisions_are_idempotent_and_record_details() -> Result<(), Box<dyn std::error::Error>>
    {
        let temp_dir = tempfile::tempdir()?;
        fs::write(
            temp_dir.path().join(".localmind.toml"),
            "[learning]\nenabled = true\n",
        )?;
        let config = ProjectConfig::discover(temp_dir.path())?;
        let import = TranscriptImporter::import_text(
            &config,
            "Lesson: Prefer review decisions.\n",
            SessionSource::GenericTranscript,
            TranscriptImportFormat::PlainText,
        )?;
        CloseoutProcessor::closeout_project_session(
            temp_dir.path(),
            &import.session_id,
            &DeterministicExtractor,
        )?;
        let queue = ReviewQueue::open_project(temp_dir.path())?;
        let item_id = queue.list()?[0].id.clone();
        let decision = ReviewDecision {
            item_id: item_id.clone(),
            action: ReviewAction::Accept,
            reviewer: "test".to_string(),
            decided_at: None,
            note: Some("looks right".to_string()),
            replacement_summary: None,
            evidence: Vec::new(),
        };

        let first = queue.decide(decision.clone())?;
        let second = queue.decide(decision)?;

        assert_eq!(first.state, localmind_core::ReviewState::Accepted);
        assert_eq!(second.state, localmind_core::ReviewState::Accepted);
        assert_eq!(second.note.as_deref(), Some("looks right"));
        Ok(())
    }

    #[test]
    fn edit_decision_requires_replacement_text() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        fs::write(
            temp_dir.path().join(".localmind.toml"),
            "[learning]\nenabled = true\n",
        )?;
        let config = ProjectConfig::discover(temp_dir.path())?;
        let import = TranscriptImporter::import_text(
            &config,
            "Lesson: Prefer edit validation.\n",
            SessionSource::GenericTranscript,
            TranscriptImportFormat::PlainText,
        )?;
        CloseoutProcessor::closeout_project_session(
            temp_dir.path(),
            &import.session_id,
            &DeterministicExtractor,
        )?;
        let queue = ReviewQueue::open_project(temp_dir.path())?;
        let item_id = queue.list()?[0].id.clone();

        let result = queue.decide(ReviewDecision {
            item_id,
            action: ReviewAction::Edit,
            reviewer: "test".to_string(),
            decided_at: None,
            note: None,
            replacement_summary: Some(" ".to_string()),
            evidence: Vec::new(),
        });

        assert!(matches!(result, Err(ReviewQueueError::InvalidEdit { .. })));
        Ok(())
    }

    #[test]
    fn review_queue_refuses_disabled_projects() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        fs::write(
            temp_dir.path().join(".localmind.toml"),
            "[learning]\nenabled = false\n",
        )?;

        let result = ReviewQueue::open_project(temp_dir.path());

        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn closeout_rejects_missing_import_artifacts() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        fs::write(
            temp_dir.path().join(".localmind.toml"),
            "[learning]\nenabled = true\n",
        )?;

        let result = CloseoutProcessor::closeout_project_session(
            temp_dir.path(),
            &SessionId::new("session-missing"),
            &DeterministicExtractor,
        );

        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn closeout_rejects_low_confidence_extractor_output() -> Result<(), Box<dyn std::error::Error>>
    {
        let temp_dir = tempfile::tempdir()?;
        fs::write(
            temp_dir.path().join(".localmind.toml"),
            "[learning]\nenabled = true\n",
        )?;
        let config = ProjectConfig::discover(temp_dir.path())?;
        let import = TranscriptImporter::import_text(
            &config,
            "Lesson: Prefer fixtures.\n",
            SessionSource::GenericTranscript,
            TranscriptImportFormat::PlainText,
        )?;

        let result = CloseoutProcessor::closeout_project_session(
            temp_dir.path(),
            &import.session_id,
            &LowConfidenceExtractor,
        );

        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn closeout_rejects_malformed_extractor_output() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        fs::write(
            temp_dir.path().join(".localmind.toml"),
            "[learning]\nenabled = true\n",
        )?;
        let config = ProjectConfig::discover(temp_dir.path())?;
        let import = TranscriptImporter::import_text(
            &config,
            "Lesson: Prefer fixtures.\n",
            SessionSource::GenericTranscript,
            TranscriptImportFormat::PlainText,
        )?;

        let result = CloseoutProcessor::closeout_project_session(
            temp_dir.path(),
            &import.session_id,
            &MalformedExtractor,
        );

        assert!(result.is_err());
        Ok(())
    }

    struct LowConfidenceExtractor;

    impl SessionExtractor for LowConfidenceExtractor {
        fn extract(
            &self,
            input: ExtractionInput,
        ) -> Result<ExtractionOutput, super::CloseoutError> {
            let summary = SessionSummary::new(input.session_id.clone(), "title", "body");
            let lesson = CandidateLesson::new(
                LessonId::new("lesson-low-confidence"),
                "Too uncertain to promote.",
                LessonCategory::Process,
                Confidence::new(0.1)?,
                SuggestedAction::PromoteToMemory,
            );

            Ok(ExtractionOutput {
                summary,
                candidates: vec![lesson],
            })
        }
    }

    struct MalformedExtractor;

    impl SessionExtractor for MalformedExtractor {
        fn extract(
            &self,
            input: ExtractionInput,
        ) -> Result<ExtractionOutput, super::CloseoutError> {
            let summary = SessionSummary::new(input.session_id.clone(), "title", "body");
            let mut lesson = CandidateLesson::new(
                LessonId::new("lesson-malformed"),
                "Malformed candidate.",
                LessonCategory::Process,
                Confidence::new(0.8)?,
                SuggestedAction::PromoteToMemory,
            );
            lesson.validation_status = ValidationStatus::Malformed;

            Ok(ExtractionOutput {
                summary,
                candidates: vec![lesson],
            })
        }
    }
}
