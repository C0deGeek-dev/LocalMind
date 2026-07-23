//! Semantic doc search reports *why* it returned nothing: an empty doc index,
//! embeddings not configured, an unreachable endpoint, unvectored passages, a
//! model/dimension-mismatched index, and a genuine no-match are all distinct
//! states. Also pins that doc vectors are ranked kind-filtered — memory
//! vectors can never crowd doc hits out of the window — and that hits below
//! the relevance floor are not presented merely to fill the limit.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

use localmind_core::{
    Confidence, EvidenceKind, EvidenceRef, LessonCategory, MemoryEntry, MemoryEntryId, MemoryScope,
    MemoryStatus, SessionId,
};
use localmind_store::{ingest_doc_text, DocSearchStatus, MemoryPersistence};

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

fn project_with_embeddings(base_url: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), base_url);
    dir
}

fn write_config(root: &std::path::Path, base_url: &str) {
    std::fs::write(
        root.join(".localmind.toml"),
        format!(
            "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n\n[inference]\nembedding_base_url = \"{base_url}\"\nembedding_model = \"test-embed\"\ntimeout_secs = 5\n",
        ),
    )
    .unwrap();
}

fn project_without_embeddings() -> tempfile::TempDir {
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

#[test]
fn an_empty_doc_index_reads_as_no_doc_chunks() {
    let project = project_without_embeddings();
    let persistence = MemoryPersistence::open_project(project.path()).unwrap();

    let report = persistence.doc_search_diagnosed("anything", 5).unwrap();

    assert_eq!(report.status, DocSearchStatus::NoDocChunks);
    assert!(report.results.is_empty());
}

#[test]
fn unconfigured_embeddings_read_as_not_configured_not_no_match() {
    let project = project_without_embeddings();
    let persistence = MemoryPersistence::open_project(project.path()).unwrap();
    ingest_doc_text(
        &persistence,
        "guide.md",
        "# Guide\n\nUse the launcher.",
        false,
    )
    .unwrap();

    let report = persistence.doc_search_diagnosed("launcher", 5).unwrap();

    assert_eq!(report.status, DocSearchStatus::EmbeddingsNotConfigured);
}

#[test]
fn an_unreachable_endpoint_reads_as_endpoint_unavailable() {
    // Bind-then-drop a listener so the port is closed at query time.
    let dead = {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    };
    let project = project_with_embeddings(&dead);
    let persistence = MemoryPersistence::open_project(project.path()).unwrap();
    ingest_doc_text(
        &persistence,
        "guide.md",
        "# Guide\n\nUse the launcher.",
        false,
    )
    .unwrap();

    let report = persistence.doc_search_diagnosed("launcher", 5).unwrap();

    assert!(
        matches!(
            report.status,
            DocSearchStatus::EmbeddingEndpointUnavailable { .. }
        ),
        "got {:?}",
        report.status
    );
}

#[test]
fn vectorless_passages_read_as_no_doc_vectors() {
    let base = embeddings_server(&[1.0, 0.0, 0.0, 0.0], 1);
    let project = project_with_embeddings(&base);
    let persistence = MemoryPersistence::open_project(project.path()).unwrap();
    // Ingested with embedding suppressed: passages exist, vectors do not.
    ingest_doc_text(
        &persistence,
        "guide.md",
        "# Guide\n\nUse the launcher.",
        false,
    )
    .unwrap();

    let report = persistence.doc_search_diagnosed("launcher", 5).unwrap();

    assert_eq!(report.status, DocSearchStatus::NoDocVectors);
}

#[test]
fn a_dimension_mismatched_index_reads_as_index_mismatch() {
    // Index embedded at 4 dimensions...
    let four_dim = embeddings_server(&[1.0, 0.0, 0.0, 0.0], 2);
    let project = project_with_embeddings(&four_dim);
    let persistence = MemoryPersistence::open_project(project.path()).unwrap();
    let (_, embedded) = ingest_doc_text(
        &persistence,
        "guide.md",
        "# Guide\n\nUse the launcher.",
        true,
    )
    .unwrap();
    assert!(embedded > 0, "fixture must embed the doc chunk");
    drop(persistence);

    // ...then the active embedding model produces 8 dimensions.
    let eight_dim = embeddings_server(&[1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0], 1);
    write_config(project.path(), &eight_dim);
    let persistence = MemoryPersistence::open_project(project.path()).unwrap();

    let report = persistence.doc_search_diagnosed("launcher", 5).unwrap();

    match report.status {
        DocSearchStatus::IndexMismatch {
            indexed_models,
            query_dimensions,
        } => {
            assert_eq!(query_dimensions, 8);
            assert!(!indexed_models.is_empty());
        }
        other => panic!("expected IndexMismatch, got {other:?}"),
    }
}

#[test]
fn memory_vectors_cannot_crowd_doc_hits_out_of_the_window() {
    let base = embeddings_server(&[0.5, 0.5, 0.0, 0.0], 16);
    let project = project_with_embeddings(&base);
    let persistence = MemoryPersistence::open_project(project.path()).unwrap();
    ingest_doc_text(
        &persistence,
        "guide.md",
        "# Guide\n\nUse the launcher.",
        true,
    )
    .unwrap();
    // Eight memory vectors at the same cosine as the doc vector: a shared
    // top-k over both kinds would fill any small window with memory rows.
    for index in 0..8 {
        persistence
            .persist_memory_entry(&memory(
                &format!("m{index}"),
                &format!("memory lesson {index}"),
            ))
            .unwrap();
    }

    let report = persistence.doc_search_diagnosed("launcher", 1).unwrap();

    assert_eq!(report.status, DocSearchStatus::Searched);
    assert_eq!(
        report.results.len(),
        1,
        "the doc hit must survive a limit-1 search regardless of memory vectors"
    );
    assert_eq!(report.results[0].path, "guide.md");
}

#[test]
fn hits_below_the_relevance_floor_are_not_padded_into_results() {
    // Index embedded orthogonal to what the query will embed to: cosine ~0,
    // far below the default floor — a nearest neighbour, but not a relevant one.
    let index_server = embeddings_server(&[1.0, 0.0, 0.0, 0.0], 2);
    let project = project_with_embeddings(&index_server);
    let persistence = MemoryPersistence::open_project(project.path()).unwrap();
    ingest_doc_text(
        &persistence,
        "guide.md",
        "# Guide\n\nUse the launcher.",
        true,
    )
    .unwrap();
    drop(persistence);

    let query_server = embeddings_server(&[0.0, 1.0, 0.0, 0.0], 1);
    write_config(project.path(), &query_server);
    let persistence = MemoryPersistence::open_project(project.path()).unwrap();

    let report = persistence
        .doc_search_diagnosed("unrelated topic", 5)
        .unwrap();

    assert_eq!(report.status, DocSearchStatus::Searched);
    assert!(
        report.results.is_empty(),
        "an orthogonal nearest neighbour must not be presented as relevant"
    );
}
