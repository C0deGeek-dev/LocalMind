//! Accepted-memory embeddings: when an embedding endpoint is configured, a
//! promoted/persisted memory is embedded into `vector_index` and is
//! vector-searchable. When the endpoint is down, embedding is best-effort — the
//! memory still persists (the lexical contract is the fallback), it just carries
//! no vector. Both are pinned with a fixture HTTP embedding server (no live
//! model), the offline acceptance bar.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

use localmind_core::{
    Confidence, EvidenceKind, EvidenceRef, LessonCategory, MemoryEntry, MemoryEntryId, MemoryScope,
    MemoryStatus, SessionId,
};
use localmind_store::MemoryPersistence;

/// A fixture `/v1/embeddings` server that answers up to `max_requests` requests
/// with one fixed embedding vector each — the offline stand-in for a live embed
/// model. Returns its base URL.
fn embeddings_server(embedding: &[f32], max_requests: usize) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let values = embedding
        .iter()
        .map(|v| format!("{v}"))
        .collect::<Vec<_>>()
        .join(",");
    let body = format!("{{\"data\":[{{\"embedding\":[{values}]}}]}}");
    thread::spawn(move || {
        for _ in 0..max_requests {
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            loop {
                match stream.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => {
                        request.extend_from_slice(&buffer[..read]);
                        if request_complete(&request) {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    format!("http://{address}")
}

fn request_complete(request: &[u8]) -> bool {
    let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    let headers = String::from_utf8_lossy(&request[..header_end]);
    let mut content_length = 0_usize;
    for line in headers.lines() {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }
    }
    request.len() >= header_end + 4 + content_length
}

/// A project that opts in to a configured embedding endpoint + model. Scope is
/// project-only so the assertions read the project store directly.
fn project_with_embeddings(base_url: &str, model: &str, timeout_secs: u64) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".localmind.toml"),
        format!(
            "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n\n[inference]\nembedding_base_url = \"{base_url}\"\nembedding_model = \"{model}\"\ntimeout_secs = {timeout_secs}\n",
        ),
    )
    .unwrap();
    dir
}

fn memory(id: &str, body: &str) -> MemoryEntry {
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
    }
}

#[test]
fn a_configured_endpoint_embeds_accepted_memory_into_the_vector_index() {
    let embedding = [0.1_f32, 0.2, 0.3, 0.4];
    let base = embeddings_server(&embedding, 1);
    let project = project_with_embeddings(&base, "test-embed", 5);

    let persistence = MemoryPersistence::open_project(project.path()).unwrap();
    persistence
        .persist_memory_entry(&memory("m1", "rewrite the whole file when an edit fails"))
        .unwrap();

    // The vector landed and is queryable: searching with the same vector returns
    // the memory at (near-)perfect cosine, which also proves the stored dimension
    // matches (vector_search skips rows whose dimension differs from the query).
    let hits = persistence.vector_search(&embedding, 5).unwrap();
    assert_eq!(
        hits.len(),
        1,
        "the embedded memory must be vector-searchable"
    );
    assert_eq!(hits[0].subject_id, "m1");
    assert!(
        hits[0].score > 0.99,
        "the same vector must score ~1.0, got {}",
        hits[0].score
    );

    // The on-disk contract is recorded in the audit trail: the vector row carries
    // the model and dimensions, and the inference call is logged as completed.
    let audits = persistence.audit_records().unwrap();
    let vector_audit = audits
        .iter()
        .find(|record| record.kind == "VectorIndexUpdated")
        .expect("a VectorIndexUpdated audit row");
    assert!(vector_audit
        .metadata_json
        .contains("\"model\":\"test-embed\""));
    assert!(vector_audit.metadata_json.contains("\"dimensions\":4"));
    assert!(
        audits
            .iter()
            .any(|record| record.kind == "InferenceCallCompleted"),
        "the embedding call must be audited as completed"
    );
}

#[test]
fn a_down_endpoint_is_best_effort_and_never_fails_promotion() {
    // Port 1 has nothing listening: the embed call fails fast (connection
    // refused), so embedding must fall back rather than fail the persist.
    let project = project_with_embeddings("http://127.0.0.1:1", "test-embed", 2);

    let persistence = MemoryPersistence::open_project(project.path()).unwrap();
    // The promotion/persist MUST succeed even though embedding cannot run.
    persistence
        .persist_memory_entry(&memory("m1", "prefer ripgrep over grep when searching"))
        .expect("a down embedding endpoint must not fail persistence");

    // No vector was stored (the lexical contract is the whole story)...
    let hits = persistence
        .vector_search(&[0.1_f32, 0.2, 0.3, 0.4], 5)
        .unwrap();
    assert!(hits.is_empty(), "a failed embed must store no vector");

    // ...the failure is logged as a skipped, best-effort call, and no vector row
    // was written.
    let audits = persistence.audit_records().unwrap();
    let failure = audits
        .iter()
        .find(|record| record.kind == "InferenceCallFailed")
        .expect("a failed embed must be audited");
    assert!(failure.metadata_json.contains("\"outcome\":\"skipped\""));
    assert!(
        !audits
            .iter()
            .any(|record| record.kind == "VectorIndexUpdated"),
        "no vector row may be written when the embed fails"
    );
}
