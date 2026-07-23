//! Storage boundary for LocalMind.
//!
//! The MVP keeps durable memory Markdown-first and uses SQLite for queue, audit,
//! and index state. This crate owns that persistence behavior and establishes
//! the host → engine dependency direction.

mod bundle;
mod bundle_import;
mod config;
mod context_export;
mod dedup;
mod doc_ingest;
mod eval;
mod extraction;
mod freshness;
mod graph_store;
mod import;
mod language;
mod markdown;
mod memory_persistence;
mod okf;
mod okf_export;
mod okf_import;
mod paths;
mod project_identity;
mod quality;
mod redaction;
mod research;
mod revalidation;
mod review_modes;
mod review_queue;
mod schema;
mod signing;
mod skill_drafts;
mod sync_bundle;
mod sync_engine;

pub use bundle::{
    BundleError, BundleMetadata, BundleScope, ExportOutcome, MemoryBundle, MemoryBundleExporter,
    SecretScanReport, MEMORY_BUNDLE_FORMAT_VERSION,
};
pub use bundle_import::{BundleImportError, BundleImportReport, BundleImporter, ImportTrust};
pub use config::{
    LearningConfig, LocalMindConfig, ProjectConfig, RetrievalConfig, ReviewConfig,
    ReviewModeConfig, StoreConfigError, SyncConfig,
};
pub use context_export::{ContextExport, ContextExportError, ContextExportTarget, ContextExporter};
pub use dedup::{
    canonical, canonical_hash, is_near_duplicate, similarity, token_set, NEAR_DUP_THRESHOLD,
};
pub use doc_ingest::{
    ingest_doc_text, ingest_docs, ingest_docs_into, DocIngestError, DocIngestSummary,
};
pub use eval::{
    default_fixtures, lift, load_fixtures, run_eval, run_eval_lift, run_eval_with, EvalError,
    EvalFixture, EvalLift, EvalReport, EvalReranker, FixtureScore, RetrievalCase,
};
pub use extraction::{
    CloseoutError, CloseoutProcessor, CloseoutReport, DeterministicExtractor, ExtractionInput,
    ExtractionOutput, ModelBackedExtractor, SessionExtractor,
};
pub use freshness::{
    FreshnessFlag, FreshnessReason, FreshnessReport, FreshnessScope, FreshnessThresholds,
};
pub use graph_store::{GraphStore, GraphStoreError, GRAPH_FORMAT_VERSION};
pub use import::{
    ImportError, ImportReport, ImportedSession, TranscriptImportFormat, TranscriptImporter,
};
pub use language::{
    detect_workspace_language, language_for_extension, lesson_language, resolve_memory_language,
};
pub use markdown::{MarkdownMemoryFormat, MarkdownParseError};
pub use memory_persistence::{
    AuditRecord, DocSearchReport, DocSearchResult, DocSearchStatus, EmbedQueryOutcome,
    MemoryPersistence, MemoryPersistenceError, MemoryProvenance, MemoryRecord, MemorySearchResult,
    VectorSearchResult,
};
pub use okf::{OkfFormat, OkfParseError, OKF_VERSION};
pub use okf_export::{OkfExportError, OkfExportReport, OkfExporter};
pub use okf_import::{OkfImportError, OkfImportReport, OkfImporter};
pub use paths::{MemoryPathError, MemoryPathResolver};
pub use project_identity::{ProjectIdentity, ProjectIdentitySource};
pub use quality::{classify_quality, Quality};
pub use redaction::{Redaction, RedactionReport, Redactor};
pub use research::{BatchInsightError, BatchInsightPipeline, BatchInsightReport};
pub use revalidation::{
    is_revalidation_candidate, parse_verdict, RevalidationConfig, RevalidationReport,
    RevalidationVerdict, VerdictSource, VERDICT_PROMPT,
};
pub use review_modes::{ReviewModeError, ReviewModeProcessor, ReviewModeReport};
pub use review_queue::{
    ReviewQueue, ReviewQueueError, ReviewQueueItem, ReviewQueueSummary, REVIEW_DB_FILE_NAME,
};
pub use schema::SchemaError;
pub use signing::{
    author_fingerprint, digest_hex, sign_bundle, sign_detached, verify_detached, verify_signed,
    Device, DeviceCard, KeyStore, RejectReason, SignatureEnvelope, SignedBundle, SigningError,
    TrustClass, VerificationOutcome, SIGNATURE_SCHEMA_VERSION,
};
pub use skill_drafts::{ActiveSkillRecord, SkillDraftError, SkillDraftRecord, SkillDraftStore};
pub use sync_bundle::{
    EncryptedBundle, OpKind, SealedCopy, SignedSyncBundle, SyncBundle, SyncBundleError, SyncCursor,
    SyncOp, ENCRYPTED_BUNDLE_FORMAT_VERSION, SYNC_BUNDLE_FORMAT_VERSION,
};
pub use sync_engine::{SyncEngine, SyncEngineError, SyncRunReport, SyncStatus};

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
        MarkdownMemoryFormat, MemoryPathError, MemoryPathResolver, MemoryPersistence,
        ProjectConfig, ReviewQueue, ReviewQueueError, SessionExtractor, StoreCapabilities,
        StoreConfigError, TranscriptImportFormat, TranscriptImporter,
    };
    use localmind_core::{
        CandidateLesson, Confidence, EvidenceKind, EvidenceRef, LessonCategory, LessonId,
        MemoryEntry, MemoryEntryId, MemoryScope, MemoryStatus, ReviewAction, ReviewDecision,
        ReviewItemId, SessionId, SessionSource, SessionSummary, SuggestedAction, ValidationStatus,
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
            sync_meta: localmind_core::SyncMeta::default(),
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
    fn closeout_extracts_lessons_after_host_speaker_prefixes(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        fs::write(
            temp_dir.path().join(".localmind.toml"),
            "[learning]\nenabled = true\n",
        )?;
        let config = ProjectConfig::discover(temp_dir.path())?;
        let import = TranscriptImporter::import_text(
            &config,
            "user: Lesson: Prefer host-rendered transcript fixtures.\n",
            SessionSource::LocalPilot,
            TranscriptImportFormat::PlainText,
        )?;

        let report = CloseoutProcessor::closeout_project_session(
            temp_dir.path(),
            &import.session_id,
            &DeterministicExtractor,
        )?;

        assert_eq!(report.candidate_count, 1);
        assert_eq!(report.enqueued_count, 1);
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
    fn accepted_review_item_promotes_to_markdown_memory_and_audit(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let item_id = accepted_fixture_item(temp_dir.path(), "Prefer review persistence.")?;
        let persistence = MemoryPersistence::open_project(temp_dir.path())?;

        let entry = persistence.promote_review_item(&item_id)?;
        let records = persistence.audit_records()?;
        let results = persistence.search("review persistence")?;
        let relationships = persistence.relationships_for(&entry.id)?;

        assert!(temp_dir
            .path()
            .join(format!(".localmind/memory/project/{}.md", entry.id))
            .exists());
        assert!(records.iter().any(|record| record.kind == "MemoryPromoted"));
        assert_eq!(results[0].memory_id, entry.id);
        assert!(relationships
            .iter()
            .any(|(kind, target)| kind == "category" && target == "Process"));
        assert!(relationships.iter().any(|(kind, _)| kind == "session"));
        Ok(())
    }

    #[test]
    fn accepted_memory_can_be_listed_and_deleted() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let item_id = accepted_fixture_item(temp_dir.path(), "Prefer removable memory.")?;
        let persistence = MemoryPersistence::open_project(temp_dir.path())?;
        let entry = persistence.promote_review_item(&item_id)?;

        let records = persistence.list_memory()?;
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].memory_id, entry.id);

        assert!(persistence.delete_memory(&entry.id, "test")?);
        assert!(!persistence.delete_memory(&entry.id, "test")?);
        assert!(!temp_dir
            .path()
            .join(format!(".localmind/memory/project/{}.md", entry.id))
            .exists());
        assert!(persistence.search("removable memory")?.is_empty());
        assert!(persistence
            .audit_records()?
            .iter()
            .any(|record| record.kind == "MemoryDeleted"));
        Ok(())
    }

    #[test]
    fn search_tolerates_fts_operator_syntax_in_user_queries(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let item_id = accepted_fixture_item(temp_dir.path(), "Prefer operator-safe search.")?;
        let persistence = MemoryPersistence::open_project(temp_dir.path())?;
        persistence.promote_review_item(&item_id)?;

        // Each of these is FTS5 syntax when unescaped; all must behave as
        // plain text (no error, term-based matching).
        for hostile in [
            "operator-safe AND OR NOT",
            "\"operator-safe",
            "(search",
            "NEAR(search, 2)",
            "search*",
            "-search",
        ] {
            let results = persistence.search(hostile)?;
            assert!(
                results
                    .iter()
                    .all(|result| result.score >= 1 && !result.snippet.is_empty()),
                "hostile query {hostile:?} produced a malformed result"
            );
        }

        assert!(persistence.search("   ")?.is_empty());
        Ok(())
    }

    #[test]
    fn delete_heals_when_the_memory_file_is_already_gone() -> Result<(), Box<dyn std::error::Error>>
    {
        let temp_dir = tempfile::tempdir()?;
        let item_id = accepted_fixture_item(temp_dir.path(), "Prefer healable deletion.")?;
        let persistence = MemoryPersistence::open_project(temp_dir.path())?;
        let entry = persistence.promote_review_item(&item_id)?;

        // Simulate a crash that removed the file but never reached the
        // database statements: the retry must still clean every table.
        fs::remove_file(
            temp_dir
                .path()
                .join(format!(".localmind/memory/project/{}.md", entry.id)),
        )?;

        assert!(persistence.delete_memory(&entry.id, "test")?);
        assert!(persistence.list_memory()?.is_empty());
        assert!(persistence.search("healable deletion")?.is_empty());
        assert!(persistence.relationships_for(&entry.id)?.is_empty());
        assert!(persistence
            .audit_records()?
            .iter()
            .any(|record| record.kind == "MemoryDeleted"));
        Ok(())
    }

    #[test]
    fn audit_metadata_is_valid_json_even_for_hostile_input(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let item_id = accepted_fixture_item(temp_dir.path(), "Prefer parseable audit rows.")?;
        let persistence = MemoryPersistence::open_project(temp_dir.path())?;
        persistence.promote_review_item(&item_id)?;

        let hostile = "quote\" backslash\\ newline\n {\"json\": [1]} end";
        persistence.record_context_export(hostile, "target\"with\\quotes")?;

        for record in persistence.audit_records()? {
            let parsed: serde_json::Value = serde_json::from_str(&record.metadata_json)
                .map_err(|e| format!("unparseable metadata {:?}: {e}", record.metadata_json))?;
            if record.kind == "ContextPackExported" {
                assert_eq!(parsed["query"], hostile);
            }
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn indexed_memory_can_be_deleted_through_a_canonical_root(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let aliases = tempfile::tempdir()?;
        let alias_root = aliases.path().join("project");
        std::os::unix::fs::symlink(temp_dir.path(), &alias_root)?;

        let item_id = accepted_fixture_item(&alias_root, "Prefer alias-safe deletion.")?;
        let alias_persistence = MemoryPersistence::open_project(&alias_root)?;
        let entry = alias_persistence.promote_review_item(&item_id)?;

        let real_persistence = MemoryPersistence::open_project(temp_dir.path())?;
        assert!(real_persistence.delete_memory(&entry.id, "test")?);
        assert!(!temp_dir
            .path()
            .join(format!(".localmind/memory/project/{}.md", entry.id))
            .exists());
        Ok(())
    }

    #[test]
    fn an_excerpt_candidate_cannot_promote_until_edited() -> Result<(), Box<dyn std::error::Error>>
    {
        let temp_dir = tempfile::tempdir()?;
        fs::write(
            temp_dir.path().join(".localmind.toml"),
            "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n",
        )?;
        let queue = ReviewQueue::open_project(temp_dir.path())?;
        let candidate = localmind_core::CandidateLesson::new(
            localmind_core::LessonId::new("excerpt-0001"),
            "Excerpt from web: framework guide.",
            localmind_core::LessonCategory::ToolingNote,
            localmind_core::Confidence::new(0.3)?,
            localmind_core::SuggestedAction::PromoteToMemory,
        )
        .with_evidence_text("raw fetched page text, kept for the reviewer only")
        .requiring_edit_before_promotion();
        queue.enqueue_candidates(&SessionId::new("research-batch"), &[candidate])?;
        let item_id = queue.list()?[0].id.clone();
        queue.decide(ReviewDecision {
            item_id: item_id.clone(),
            action: ReviewAction::Accept,
            reviewer: "test".to_string(),
            decided_at: None,
            note: None,
            replacement_summary: None,
            evidence: Vec::new(),
        })?;
        let persistence = MemoryPersistence::open_project(temp_dir.path())?;

        // Verbatim promotion of a source excerpt is refused with a clear error…
        let refused = persistence.promote_review_item(&item_id);
        assert!(
            matches!(
                refused,
                Err(crate::memory_persistence::MemoryPersistenceError::ReviewItemNeedsEdit { .. })
            ),
            "an unedited excerpt must not become memory: {refused:?}"
        );

        // …while a reviewer-distilled replacement promotes, and only the
        // replacement text lands in the searchable body.
        queue.decide(ReviewDecision {
            item_id: item_id.clone(),
            action: ReviewAction::Edit,
            reviewer: "test".to_string(),
            decided_at: None,
            note: None,
            replacement_summary: Some("Pin framework versions before upgrading.".to_string()),
            evidence: Vec::new(),
        })?;
        let entry = persistence.promote_review_item(&item_id)?;
        assert_eq!(entry.body, "Pin framework versions before upgrading.");
        assert!(!entry.body.contains("raw fetched page text"));
        Ok(())
    }

    #[test]
    fn edited_review_item_promotes_replacement_text() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let item_id = accepted_fixture_item(temp_dir.path(), "Prefer editable memory.")?;
        let queue = ReviewQueue::open_project(temp_dir.path())?;
        queue.decide(ReviewDecision {
            item_id: item_id.clone(),
            action: ReviewAction::Edit,
            reviewer: "test".to_string(),
            decided_at: None,
            note: None,
            replacement_summary: Some("Use edited memory body.".to_string()),
            evidence: Vec::new(),
        })?;
        let persistence = MemoryPersistence::open_project(temp_dir.path())?;

        let entry = persistence.promote_review_item(&item_id)?;

        assert_eq!(entry.body, "Use edited memory body.");
        Ok(())
    }

    #[test]
    fn repeated_promotion_updates_one_search_result() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let item_id = accepted_fixture_item(temp_dir.path(), "Prefer one durable memory row.")?;
        let persistence = MemoryPersistence::open_project(temp_dir.path())?;

        let entry = persistence.promote_review_item(&item_id)?;
        persistence.promote_review_item(&item_id)?;
        let results = persistence.search("durable memory")?;

        assert_eq!(
            results
                .iter()
                .filter(|result| result.memory_id == entry.id)
                .count(),
            1
        );
        Ok(())
    }

    #[test]
    fn search_ranks_keyword_matches_and_filters_misses() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let stronger = accepted_fixture_item(
            temp_dir.path(),
            "Use fixture fixture fixture data for deterministic cargo tests.",
        )?;
        let weaker = accepted_fixture_item(temp_dir.path(), "Use cargo clippy for lint checks.")?;
        let persistence = MemoryPersistence::open_project(temp_dir.path())?;

        let stronger_entry = persistence.promote_review_item(&stronger)?;
        let weaker_entry = persistence.promote_review_item(&weaker)?;
        let ranked = persistence.search("fixture cargo")?;
        let filtered = persistence.search("missing-term")?;

        assert_eq!(ranked[0].memory_id, stronger_entry.id);
        assert!(ranked
            .iter()
            .any(|result| result.memory_id == weaker_entry.id));
        assert!(filtered.is_empty());
        Ok(())
    }

    #[test]
    fn search_snippet_is_centred_on_the_match_not_the_body_head(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        // A memory whose head is boilerplate and whose useful lesson sits far
        // past the old fixed head window.
        let filler = "navigation menu home pricing contact about. ".repeat(20);
        let lesson = format!("{filler}Always pin the quokka registry mirror before publishing.");
        let item = accepted_fixture_item(temp_dir.path(), &lesson)?;
        let persistence = MemoryPersistence::open_project(temp_dir.path())?;
        persistence.promote_review_item(&item)?;

        let results = persistence.search("quokka registry mirror")?;

        assert_eq!(results.len(), 1);
        assert!(
            results[0].snippet.contains("quokka"),
            "snippet {:?} does not contain the matched term",
            results[0].snippet
        );
        Ok(())
    }

    #[test]
    fn stopword_only_queries_match_nothing() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let item = accepted_fixture_item(
            temp_dir.path(),
            "The tests should be run before the release is cut.",
        )?;
        let persistence = MemoryPersistence::open_project(temp_dir.path())?;
        persistence.promote_review_item(&item)?;

        // Every term is a stopword: no signal, no results — previously this
        // matched every English-prose memory in the store.
        assert!(persistence.search("the and of to")?.is_empty());
        // A significant term still reaches the memory even when the query also
        // carries stopwords.
        assert_eq!(persistence.search("the release")?.len(), 1);
        Ok(())
    }

    #[test]
    fn short_terms_match_exactly_instead_of_fanning_out_as_prefixes(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let hub = accepted_fixture_item(
            temp_dir.path(),
            "Publish the wiki through the github mirror job.",
        )?;
        let exact = accepted_fixture_item(temp_dir.path(), "Run git fetch before rebasing.")?;
        let persistence = MemoryPersistence::open_project(temp_dir.path())?;
        persistence.promote_review_item(&hub)?;
        let exact_entry = persistence.promote_review_item(&exact)?;

        // "git" is below the prefix threshold: it matches the token `git`, not
        // every word that merely starts with it (`github`).
        let results = persistence.search("git")?;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].memory_id, exact_entry.id);
        // Longer terms keep prefix recall: `publish` finds `Publish...`, and
        // `rebas` finds `rebasing`.
        assert_eq!(persistence.search("rebas")?.len(), 1);
        Ok(())
    }

    #[test]
    fn one_incidental_term_no_longer_makes_a_memory_eligible(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let unrelated = accepted_fixture_item(
            temp_dir.path(),
            "Tailwind utility classes keep component search stylesheets small.",
        )?;
        let relevant = accepted_fixture_item(
            temp_dir.path(),
            "Memory search retrieval ranks accepted lessons by relevance.",
        )?;
        let persistence = MemoryPersistence::open_project(temp_dir.path())?;
        let unrelated_entry = persistence.promote_review_item(&unrelated)?;
        let relevant_entry = persistence.promote_review_item(&relevant)?;

        // Three significant terms: a body containing only one of them
        // ("search" in the Tailwind memory) is an incidental hit and is
        // dropped; the memory matching several terms survives.
        let results = persistence.search("memory search retrieval")?;

        assert!(results
            .iter()
            .any(|result| result.memory_id == relevant_entry.id));
        assert!(results
            .iter()
            .all(|result| result.memory_id != unrelated_entry.id));
        Ok(())
    }

    #[test]
    fn search_lang_excludes_off_language_lessons_but_keeps_general(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let py = accepted_fixture_item(
            temp_dir.path(),
            "In Python, prefer list comprehensions for mapping data.",
        )?;
        let rs = accepted_fixture_item(
            temp_dir.path(),
            "In Rust, prefer iterators for mapping data.",
        )?;
        let general = accepted_fixture_item(
            temp_dir.path(),
            "Always run the tests before mapping out a release.",
        )?;
        let persistence = MemoryPersistence::open_project(temp_dir.path())?;
        let py_entry = persistence.promote_review_item(&py)?;
        let rs_entry = persistence.promote_review_item(&rs)?;
        let general_entry = persistence.promote_review_item(&general)?;

        // A Rust task: the Python lesson is excluded inside the query; the Rust
        // and the language-agnostic (untagged) lessons stay eligible.
        let rust_hits = persistence.search_lang("mapping", Some("rust"))?;
        let ids: Vec<String> = rust_hits
            .iter()
            .map(|result| result.memory_id.as_str().to_string())
            .collect();
        assert!(
            ids.contains(&rs_entry.id.as_str().to_string()),
            "the same-language lesson must be retrieved"
        );
        assert!(
            ids.contains(&general_entry.id.as_str().to_string()),
            "an untagged general lesson must stay eligible for every task"
        );
        assert!(
            !ids.contains(&py_entry.id.as_str().to_string()),
            "an off-language lesson must not be retrieved"
        );

        // With no language filter, all three match the shared term.
        assert_eq!(persistence.search("mapping")?.len(), 3);
        Ok(())
    }

    #[test]
    fn review_actions_write_audit_records_for_supported_states(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        fs::write(
            temp_dir.path().join(".localmind.toml"),
            "[learning]\nenabled = true\n",
        )?;
        let config = ProjectConfig::discover(temp_dir.path())?;
        let import = TranscriptImporter::import_text(
            &config,
            "Lesson: accept audit.\nLesson: reject audit.\nLesson: edit audit.\nLesson: defer audit.\n",
            SessionSource::GenericTranscript,
            TranscriptImportFormat::PlainText,
        )?;
        CloseoutProcessor::closeout_project_session(
            temp_dir.path(),
            &import.session_id,
            &DeterministicExtractor,
        )?;
        let queue = ReviewQueue::open_project(temp_dir.path())?;
        let persistence = MemoryPersistence::open_project(temp_dir.path())?;
        let items = queue.list()?;
        let decisions = [
            (items[0].id.clone(), ReviewAction::Accept, None),
            (items[1].id.clone(), ReviewAction::Reject, None),
            (
                items[2].id.clone(),
                ReviewAction::Edit,
                Some("edited audit body".to_string()),
            ),
            (items[3].id.clone(), ReviewAction::MarkTemporary, None),
        ];

        for (item_id, action, replacement_summary) in decisions {
            let item = queue.decide(ReviewDecision {
                item_id,
                action,
                reviewer: "test".to_string(),
                decided_at: None,
                note: None,
                replacement_summary,
                evidence: Vec::new(),
            })?;
            persistence.record_review_item_audit(&item)?;
        }

        let records = persistence.audit_records()?;
        let decision_records: Vec<_> = records
            .iter()
            .filter(|record| record.kind == "ReviewDecisionRecorded")
            .collect();
        assert_eq!(decision_records.len(), 4);
        assert!(decision_records
            .iter()
            .any(|record| record.metadata_json.contains(r#""action":"accept""#)));
        assert!(decision_records
            .iter()
            .any(|record| record.metadata_json.contains(r#""action":"reject""#)));
        assert!(decision_records
            .iter()
            .any(|record| record.metadata_json.contains(r#""action":"edit""#)));
        assert!(decision_records
            .iter()
            .any(|record| record.metadata_json.contains(r#""action":"defer""#)));
        Ok(())
    }

    #[test]
    fn unaccepted_review_item_cannot_promote() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        fs::write(
            temp_dir.path().join(".localmind.toml"),
            "[learning]\nenabled = true\n",
        )?;
        let config = ProjectConfig::discover(temp_dir.path())?;
        let import = TranscriptImporter::import_text(
            &config,
            "Lesson: Prefer pending reviews.\n",
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
        let persistence = MemoryPersistence::open_project(temp_dir.path())?;

        let result = persistence.promote_review_item(&item_id);

        assert!(result.is_err());
        Ok(())
    }

    fn accepted_fixture_item(
        project_root: &std::path::Path,
        lesson_text: &str,
    ) -> Result<ReviewItemId, Box<dyn std::error::Error>> {
        // This fixture exercises project-store mechanics (promote / list / delete /
        // audit), so it pins memory to the project scope — global memory is on by
        // default and would route a cross-project category (e.g. `Process`) to the
        // machine-wide store, which these project-path assertions do not cover.
        fs::write(
            project_root.join(".localmind.toml"),
            "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n",
        )?;
        let config = ProjectConfig::discover(project_root)?;
        let import = TranscriptImporter::import_text(
            &config,
            &format!("Lesson: {lesson_text}\n"),
            SessionSource::GenericTranscript,
            TranscriptImportFormat::PlainText,
        )?;
        CloseoutProcessor::closeout_project_session(
            project_root,
            &import.session_id,
            &DeterministicExtractor,
        )?;
        let queue = ReviewQueue::open_project(project_root)?;
        let item_id = queue
            .list()?
            .into_iter()
            .find(|item| {
                item.session_id == import.session_id && item.candidate.summary() == lesson_text
            })
            .ok_or("accepted fixture item missing")?
            .id;
        queue.decide(ReviewDecision {
            item_id: item_id.clone(),
            action: ReviewAction::Accept,
            reviewer: "test".to_string(),
            decided_at: None,
            note: Some("durable".to_string()),
            replacement_summary: None,
            evidence: Vec::new(),
        })?;
        Ok(item_id)
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
