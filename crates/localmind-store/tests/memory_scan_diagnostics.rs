//! The memory relevance path reports *why* it returned nothing.
//!
//! It used to collapse every failure into `None`: no endpoint configured, an
//! endpoint that does not answer, nothing embedded yet, and an index embedded
//! under a different model were one indistinguishable state. That is how 266
//! consecutive failed embed calls accrued while `status` reported `ready` and
//! every suite stayed green — the data existed, and no surface could say it.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

use localmind_core::{
    Confidence, EvidenceKind, EvidenceRef, LessonCategory, MemoryEntry, MemoryEntryId, MemoryScope,
    MemoryStatus, SessionId,
};
use localmind_store::{EmbeddingCapability, MemoryPersistence, MemoryScanStatus, StatusSnapshot};

/// A fixture `/v1/embeddings` server answering up to `max_requests` requests
/// with one fixed vector each — the offline stand-in for a live embed model.
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

/// A port nothing is listening on — a configured endpoint that cannot answer.
fn dead_endpoint() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    format!("http://{address}")
}

fn project_with_embeddings(base_url: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".localmind.toml"),
        format!(
            "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n\n[inference]\nembedding_base_url = \"{base_url}\"\nembedding_model = \"test-embed\"\ntimeout_secs = 5\n"
        ),
    )
    .unwrap();
    dir
}

fn project_without_inference() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".localmind.toml"),
        "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n",
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
        sync_meta: localmind_core::SyncMeta::default(),
    }
}

fn seed(persistence: &MemoryPersistence, count: usize, vector: &[f32]) {
    for index in 0..count {
        let entry = memory(&format!("m{index}"), &format!("lesson {index}"));
        persistence.persist_memory_entry(&entry).unwrap();
        persistence
            .upsert_memory_embedding(&entry.id, &format!("fp{index}"), "test-embed", vector)
            .unwrap();
    }
}

#[test]
fn no_configured_endpoint_is_its_own_state() {
    let project = project_without_inference();
    let persistence = MemoryPersistence::open_project(project.path()).unwrap();

    let report = persistence
        .memory_vector_scan_diagnosed("anything")
        .unwrap();

    assert_eq!(report.status, MemoryScanStatus::EmbeddingsNotConfigured);
}

#[test]
fn a_configured_but_dead_endpoint_is_distinguishable_from_an_absent_one() {
    // The state the old path could not express, and the one that actually
    // happened: an endpoint the user configured, that is not answering.
    let project = project_with_embeddings(&dead_endpoint());
    let persistence = MemoryPersistence::open_project(project.path()).unwrap();

    let report = persistence
        .memory_vector_scan_diagnosed("anything")
        .unwrap();

    assert!(
        matches!(
            report.status,
            MemoryScanStatus::EmbeddingEndpointUnavailable { .. }
        ),
        "expected an unavailable endpoint, got {:?}",
        report.status
    );
}

#[test]
fn a_healthy_endpoint_with_nothing_embedded_says_so() {
    let base = embeddings_server(&[1.0, 0.0, 0.0, 0.0], 8);
    let project = project_with_embeddings(&base);
    let persistence = MemoryPersistence::open_project(project.path()).unwrap();

    let report = persistence
        .memory_vector_scan_diagnosed("anything")
        .unwrap();

    assert_eq!(report.status, MemoryScanStatus::NoMemoryVectors);
}

#[test]
fn an_index_embedded_under_another_model_is_a_mismatch_not_an_empty_result() {
    // Stored under a 2-dimension vector, queried with a 4-dimension one: rows
    // exist and none is comparable. Reported as an empty result, this looks
    // exactly like "nothing relevant" — and a model swap silently zeroes recall.
    let base = embeddings_server(&[1.0, 0.0, 0.0, 0.0], 8);
    let project = project_with_embeddings(&base);
    let persistence = MemoryPersistence::open_project(project.path()).unwrap();
    seed(&persistence, 3, &[1.0, 0.0]);

    let report = persistence
        .memory_vector_scan_diagnosed("anything")
        .unwrap();

    match report.status {
        MemoryScanStatus::IndexMismatch { indexed_models } => {
            assert_eq!(indexed_models, vec!["test-embed".to_string()]);
        }
        other => panic!("expected an index mismatch, got {other:?}"),
    }
}

#[test]
fn a_healthy_scan_returns_its_rows() {
    let base = embeddings_server(&[1.0, 0.0, 0.0, 0.0], 8);
    let project = project_with_embeddings(&base);
    let persistence = MemoryPersistence::open_project(project.path()).unwrap();
    seed(&persistence, 3, &[1.0, 0.0, 0.0, 0.0]);

    let report = persistence
        .memory_vector_scan_diagnosed("anything")
        .unwrap();

    assert_eq!(report.status, MemoryScanStatus::Scanned);
    assert_eq!(report.scored.len(), 3);
}

#[test]
fn an_unreachable_endpoint_is_degraded_but_the_store_is_still_ready() {
    // Both halves matter. The endpoint must be reported degraded — that is the
    // whole point — and lexical readiness must NOT be flipped to failure, or a
    // legitimately offline user is told their store is broken.
    let project = project_with_embeddings(&dead_endpoint());

    let snapshot = StatusSnapshot::read(project.path());

    assert!(
        snapshot.ready,
        "a down embedding endpoint never makes the store not-ready: {:?}",
        snapshot.notes
    );
    assert!(matches!(
        snapshot.embedding,
        EmbeddingCapability::Unreachable { .. }
    ));
}

#[test]
fn an_embedding_only_config_is_never_reported_as_a_chat_endpoint() {
    // The config here sets `embedding_base_url` and no chat endpoint. The old
    // status line fell back to the literal "chat endpoint set", inventing a
    // chat endpoint the user had never configured.
    let base = embeddings_server(&[1.0, 0.0, 0.0, 0.0], 8);
    let project = project_with_embeddings(&base);

    let snapshot = StatusSnapshot::read(project.path());

    assert!(matches!(
        snapshot.embedding,
        EmbeddingCapability::Healthy { .. }
    ));
    assert!(
        !snapshot.embedding.summary().contains("chat"),
        "an embedding capability must not describe itself as chat: {}",
        snapshot.embedding.summary()
    );
}

#[test]
fn memory_vector_coverage_is_reportable_and_counts_active_rows() {
    // Without these counts, "most accepted memory carries no vector" is not a
    // state any surface can express — which is precisely how it went unnoticed.
    // The fixture answers exactly two embeds, so the first two memories are
    // vectorised on promotion and the third's embed fails — which is the real
    // shape of the hole: the memory is durable, its vector never arrived.
    let base = embeddings_server(&[1.0, 0.0, 0.0, 0.0], 2);
    let project = project_with_embeddings(&base);
    let persistence = MemoryPersistence::open_project(project.path()).unwrap();
    seed(&persistence, 2, &[1.0, 0.0, 0.0, 0.0]);
    persistence
        .persist_memory_entry(&memory("bare", "unembedded lesson"))
        .unwrap();

    let coverage = persistence.memory_vector_coverage().unwrap();

    assert_eq!(coverage.project_active, 3);
    assert_eq!(coverage.project_vectorized, 2);
    assert_eq!(coverage.holes(), 1, "the gap is expressible");
    assert_eq!(coverage.stale, 0);
}
