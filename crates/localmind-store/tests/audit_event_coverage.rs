#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::thread;

use localmind_core::{
    Confidence, LessonCategory, MemoryEntry, MemoryEntryId, MemoryScope, MemoryStatus,
    SessionSource, SyncMeta,
};
use localmind_store::{
    BatchInsightPipeline, CloseoutProcessor, DeterministicExtractor, ImportReport,
    MemoryPersistence, ProjectConfig, TranscriptImportFormat, TranscriptImporter,
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
