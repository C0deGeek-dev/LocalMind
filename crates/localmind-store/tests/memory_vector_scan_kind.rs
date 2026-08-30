//! The memory side of the kind-scoped scan contract.
//!
//! `doc_search` already pins that memory vectors cannot crowd doc hits out of
//! its window. The reverse was never pinned, and the reverse is the one that
//! bites in practice: a real store is lopsided toward doc chunks (measured
//! 8,079 doc against 200 memory), so an unfiltered shared top-k hands nearly
//! every slot to docs and a caller that filters for memories *afterwards* is
//! left with almost nothing. That is a degradation, not an emptiness — real
//! prompts still surface one or two memories — which is exactly why it went
//! unnoticed.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

use localmind_core::{
    Confidence, EvidenceKind, EvidenceRef, LessonCategory, MemoryEntry, MemoryEntryId, MemoryScope,
    MemoryStatus, SessionId,
};
use localmind_store::{ingest_doc_text, MemoryPersistence};

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

/// How many nearest vectors a caller scores. Mirrors the window the injection
/// relevance gate uses, which is where this defect was found.
const WINDOW: usize = 8;

fn project(base_url: &str) -> tempfile::TempDir {
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

/// A store shaped like a real one: many doc vectors, few memory vectors, all at
/// the same cosine against `query` so the ranking is decided purely by how the
/// window is shared rather than by relevance.
fn lopsided_store(dir: &std::path::Path, docs: usize, memories: usize) -> MemoryPersistence {
    let persistence = MemoryPersistence::open_project(dir).unwrap();
    let vector = [1.0_f32, 0.0, 0.0, 0.0];
    for index in 0..docs {
        ingest_doc_text(
            &persistence,
            &format!("guide-{index}.md"),
            &format!(
                "# Guide {index}

Launcher notes {index}."
            ),
            true,
        )
        .unwrap();
    }
    for index in 0..memories {
        let entry = memory(&format!("m{index}"), &format!("memory lesson {index}"));
        persistence.persist_memory_entry(&entry).unwrap();
        persistence
            .upsert_memory_embedding(&entry.id, &format!("fp-mem-{index}"), "test-embed", &vector)
            .unwrap();
    }
    persistence
}

#[test]
fn an_unfiltered_window_starves_memory_that_a_kind_scoped_scan_returns() {
    // The defect, reproduced: 40 doc vectors against 4 memory vectors, all at
    // identical cosine. Taking a shared top-8 and filtering for memories
    // afterwards is not the same as scanning memories.
    let base = embeddings_server(&[1.0, 0.0, 0.0, 0.0], 64);
    let dir = project(&base);
    let persistence = lopsided_store(dir.path(), 40, 4);
    let query = [1.0_f32, 0.0, 0.0, 0.0];

    let shared_then_filtered = persistence
        .vector_search(&query, WINDOW)
        .unwrap()
        .into_iter()
        .filter(|row| row.subject_kind == "memory")
        .count();
    let kind_scoped = persistence
        .vector_scan_for_kind(&query, "memory")
        .unwrap()
        .scored
        .len();

    assert_eq!(kind_scoped, 4, "every memory vector is a candidate");
    assert!(
        shared_then_filtered < kind_scoped,
        "the shared window must be shown to starve memory: filtered {shared_then_filtered}, \
         kind-scoped {kind_scoped}"
    );
}

#[test]
fn the_kind_scoped_scan_returns_only_its_kind() {
    let base = embeddings_server(&[1.0, 0.0, 0.0, 0.0], 64);
    let dir = project(&base);
    let persistence = lopsided_store(dir.path(), 10, 3);

    let scan = persistence
        .vector_scan_for_kind(&[1.0_f32, 0.0, 0.0, 0.0], "memory")
        .unwrap();

    assert!(
        scan.scored.iter().all(|row| row.subject_kind == "memory"),
        "a kind-scoped scan never returns another kind"
    );
    assert_eq!(scan.candidates, 3, "candidates counts only this kind");
}

#[test]
fn the_scan_reports_no_candidates_apart_from_no_readable_vectors() {
    // "No memory vectors at all" and "vectors exist but none could be read
    // against this query" are different states, and a caller that cannot tell
    // them apart reports a model mismatch as an empty index.
    let base = embeddings_server(&[1.0, 0.0, 0.0, 0.0], 64);
    let dir = project(&base);
    let persistence = lopsided_store(dir.path(), 5, 0);

    let scan = persistence
        .vector_scan_for_kind(&[1.0_f32, 0.0, 0.0, 0.0], "memory")
        .unwrap();

    assert_eq!(scan.candidates, 0, "no memory rows exist");
    assert!(scan.scored.is_empty());
    assert!(scan.models.is_empty());
}

#[test]
fn a_dimension_mismatched_index_has_candidates_but_no_readable_scores() {
    // The state the memory path previously could not express: rows are present
    // and were embedded under a named model, but none is comparable with this
    // query vector.
    let base = embeddings_server(&[1.0, 0.0, 0.0, 0.0], 64);
    let dir = project(&base);
    let persistence = lopsided_store(dir.path(), 0, 3);

    let scan = persistence
        .vector_scan_for_kind(&[1.0_f32, 0.0], "memory")
        .unwrap();

    assert_eq!(scan.candidates, 3, "the rows are there");
    assert!(
        scan.scored.is_empty(),
        "none of them is readable against a shorter query vector"
    );
    assert_eq!(
        scan.models,
        vec!["test-embed".to_string()],
        "the model they were embedded under is reportable, so the caller can say why"
    );
}
