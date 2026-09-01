#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::thread;

use localmind_core::{
    CandidateLesson, Confidence, EvidenceKind, EvidenceRef, LessonCategory, LessonId, MemoryEntry,
    MemoryEntryId, MemoryScope, MemoryStatus, ReviewAction, ReviewDecision, ReviewItemId,
    SessionId, SessionSource, SuggestedAction, SyncMeta,
};
use localmind_store::{
    BatchInsightPipeline, CloseoutProcessor, DeterministicExtractor, ImportReport,
    MemoryPersistence, ProjectConfig, ReviewQueue, TranscriptImportFormat, TranscriptImporter,
};

fn project(config: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".localmind.toml"), config).unwrap();
    dir
}

fn import_session(raw_text: &str) -> (tempfile::TempDir, ImportReport) {
    let dir = project("[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n");
    let config = ProjectConfig::discover(dir.path()).unwrap();
    let report = TranscriptImporter::import_text(
        &config,
        raw_text,
        SessionSource::GenericTranscript,
        TranscriptImportFormat::PlainText,
    )
    .unwrap();
    (dir, report)
}

fn audit_rows(root: &Path, kind: &str) -> Vec<localmind_store::AuditRecord> {
    MemoryPersistence::open_project(root)
        .unwrap()
        .audit_records()
        .unwrap()
        .into_iter()
        .filter(|row| row.kind == kind)
        .collect()
}

fn closeout_fixture() -> (tempfile::TempDir, ImportReport) {
    let (dir, import) =
        import_session("Lesson: Record every generated candidate in the audit log.\n");
    CloseoutProcessor::closeout_project_session(
        dir.path(),
        &import.session_id,
        &DeterministicExtractor,
    )
    .unwrap();
    (dir, import)
}

#[test]
fn importing_a_session_emits_session_imported() {
    let (dir, report) = import_session("A harmless imported session.\n");
    let rows = audit_rows(dir.path(), "SessionImported");

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].subject, report.session_id.as_str());
    assert!(rows[0].metadata_json.contains("GenericTranscript"));
}

#[test]
fn sanitising_an_imported_transcript_emits_transcript_redacted() {
    let secret = "sk-proj-abcdefghijklmnopqrstuvwxyz123456";
    let (dir, report) = import_session(&format!("token = {secret}\n"));
    let rows = audit_rows(dir.path(), "TranscriptRedacted");

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].subject, report.session_id.as_str());
    assert!(rows[0].metadata_json.contains(r#""redaction_count":1"#));
    assert!(!rows[0].metadata_json.contains(secret));
}

#[test]
fn closing_out_a_session_emits_summary_created() {
    let (dir, import) = closeout_fixture();
    let rows = audit_rows(dir.path(), "SummaryCreated");

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].subject, import.session_id.as_str());
    assert!(rows[0].metadata_json.contains(r#""candidate_count":1"#));
}

#[test]
fn closing_out_a_session_emits_candidate_lesson_created() {
    let (dir, import) = closeout_fixture();
    let rows = audit_rows(dir.path(), "CandidateLessonCreated");

    assert_eq!(rows.len(), 1);
    assert!(rows[0].subject.starts_with("lesson-"));
    assert!(rows[0].metadata_json.contains(import.session_id.as_str()));
    assert!(!rows[0]
        .metadata_json
        .contains("Record every generated candidate"));
}

#[test]
fn a_distillation_batch_emits_distillation_created() {
    let dir = run_batch(false);
    let rows = audit_rows(dir.path(), "DistillationCreated");

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].subject, "distillation-0000");
    assert!(rows[0].metadata_json.contains("distillation-batch"));
    assert!(!rows[0]
        .metadata_json
        .contains("Audit every generated batch insight"));
}

#[test]
fn a_research_batch_emits_research_insight_created() {
    let dir = run_batch(true);
    let rows = audit_rows(dir.path(), "ResearchInsightCreated");

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].subject, "research-0000");
    assert!(rows[0].metadata_json.contains("research-batch"));
    assert!(!rows[0]
        .metadata_json
        .contains("Audit every generated batch insight"));
}

fn seed_memory(id: &str, body: &str) -> MemoryEntry {
    MemoryEntry {
        id: MemoryEntryId::new(id),
        scope: MemoryScope::Project,
        body: body.to_string(),
        category: LessonCategory::ProjectConvention,
        confidence: Confidence::new(0.9).unwrap(),
        source_session: Some(SessionId::new("seed")),
        evidence: vec![EvidenceRef::new(EvidenceKind::Transcript, "redacted").redacted()],
        tags: vec!["accepted".to_string()],
        related_files: Vec::new(),
        related_entities: Vec::new(),
        created_at: None,
        updated_at: None,
        supersedes: Vec::new(),
        contradicts: Vec::new(),
        status: MemoryStatus::Active,
        sync_meta: SyncMeta::default(),
    }
}

fn supersede_candidate(id: &str, summary: &str) -> CandidateLesson {
    CandidateLesson::new(
        LessonId::new(id),
        summary,
        LessonCategory::ProjectConvention,
        Confidence::new(0.8).unwrap(),
        SuggestedAction::SupersedeExisting,
    )
    .with_evidence(EvidenceRef::new(EvidenceKind::Transcript, "redacted").redacted())
}

/// Retires `target_id` (already-persisted) with a new memory whose summary
/// is `replacement_summary`, via the same candidate → decide → promote path
/// a reviewer uses. Returns the project dir.
fn supersede_via_review(
    target_id: &str,
    target_body: &str,
    replacement_id: &str,
    replacement_summary: &str,
) -> tempfile::TempDir {
    let dir = project("[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n");
    let persistence = MemoryPersistence::open_project(dir.path()).unwrap();
    persistence
        .persist_memory_entry(&seed_memory(target_id, target_body))
        .unwrap();

    let queue = ReviewQueue::open_project(dir.path()).unwrap();
    queue
        .enqueue_candidates(
            &SessionId::new("s-supersede"),
            &[supersede_candidate(replacement_id, replacement_summary)],
        )
        .unwrap();
    queue
        .decide(ReviewDecision {
            item_id: ReviewItemId::new(replacement_id),
            action: ReviewAction::Supersede(MemoryEntryId::new(target_id)),
            reviewer: "tester".to_string(),
            decided_at: None,
            note: None,
            replacement_summary: None,
            evidence: Vec::new(),
        })
        .unwrap();
    persistence
        .promote_review_item(&ReviewItemId::new(replacement_id))
        .unwrap();
    dir
}

#[test]
fn superseding_a_memory_captures_its_prior_body_in_the_audit_row() {
    let dir = supersede_via_review(
        "m1",
        "use tabs for indentation in this project",
        "m2",
        "do not use tabs for indentation; use spaces instead",
    );
    let rows = audit_rows(dir.path(), "MemorySuperseded");

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].subject, "m1");
    // Existing keys survive unchanged alongside the new one.
    assert!(rows[0].metadata_json.contains(r#""superseded_by":"m2""#));
    assert!(rows[0]
        .metadata_json
        .contains(r#""before_body":"use tabs for indentation in this project""#));
}

#[test]
fn an_oversized_superseded_body_is_truncated_in_the_audit_row() {
    let long_body = "x".repeat(20_000);
    let dir = supersede_via_review("m1", &long_body, "m2", "a short replacement");
    let rows = audit_rows(dir.path(), "MemorySuperseded");

    assert_eq!(rows.len(), 1);
    let metadata = &rows[0].metadata_json;
    let expected_truncated = format!("{}…", "x".repeat(16_384));
    assert!(
        metadata.contains(&format!(r#""before_body":"{expected_truncated}""#)),
        "expected before_body truncated to exactly 16,384 chars plus an ellipsis marker"
    );
    assert_eq!(
        metadata.matches('x').count(),
        16_384,
        "no more than the capped character count of the original body may survive"
    );
}

fn run_batch(research: bool) -> tempfile::TempDir {
    let base_url = one_chat_response();
    let dir = project(&format!(
        "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n\n[inference]\nchat_base_url = \"{base_url}\"\nchat_model = \"fixture-chat\"\ntimeout_secs = 5\n"
    ));
    let persistence = MemoryPersistence::open_project(dir.path()).unwrap();
    persistence
        .persist_memory_entry(&MemoryEntry {
            id: MemoryEntryId::new("seed"),
            scope: MemoryScope::Project,
            body: "Audit lifecycle operations after they create durable state.".to_string(),
            category: LessonCategory::Process,
            confidence: Confidence::new(0.9).unwrap(),
            source_session: None,
            evidence: Vec::new(),
            tags: Vec::new(),
            related_files: Vec::new(),
            related_entities: Vec::new(),
            created_at: None,
            updated_at: None,
            supersedes: Vec::new(),
            contradicts: Vec::new(),
            status: MemoryStatus::Active,
            sync_meta: SyncMeta::default(),
        })
        .unwrap();

    let report = if research {
        BatchInsightPipeline::research(dir.path(), "audit lifecycle").unwrap()
    } else {
        BatchInsightPipeline::distill(dir.path()).unwrap()
    };
    assert_eq!(report.enqueued, 1);
    dir
}

fn one_chat_response() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 2048];
        loop {
            let read = stream.read(&mut buffer).unwrap();
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
            if request_complete(&request) {
                break;
            }
        }
        let insight = r#"{"insights":[{"summary":"Audit every generated batch insight after it becomes durable.","category":"process","confidence":0.8}]}"#;
        let body = serde_json::json!({
            "choices": [{ "message": { "content": insight } }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
        })
        .to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(response.as_bytes()).unwrap();
    });
    format!("http://{address}")
}

fn request_complete(request: &[u8]) -> bool {
    let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    let headers = String::from_utf8_lossy(&request[..header_end]);
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    request.len() >= header_end + 4 + content_length
}
