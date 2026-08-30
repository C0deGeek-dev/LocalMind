//! The repair path for vectors that were never written, or written under a
//! model that is no longer current.
//!
//! Embedding is best-effort and one-shot: a failed embed is audited and never
//! retried, so before this existed the first outage was permanent. The sweep
//! set is defined by a **query** — absent, or content changed since embedding,
//! or embedded under another model — which is what makes the job idempotent and
//! resumable without any persisted progress state.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;

use localmind_core::{
    Confidence, EvidenceKind, EvidenceRef, LessonCategory, MemoryEntry, MemoryEntryId, MemoryScope,
    MemoryStatus, SessionId,
};
use localmind_store::MemoryPersistence;

/// A fixture `/v1/embeddings` server that answers indefinitely and counts how
/// many embeds it served — the counter is how idempotence is observed.
fn counting_server(embedding: &[f32]) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let values = embedding
        .iter()
        .map(|v| format!("{v}"))
        .collect::<Vec<_>>()
        .join(",");
    let body = format!("{{\"data\":[{{\"embedding\":[{values}]}}]}}");
    let served = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&served);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
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
            counter.fetch_add(1, Ordering::SeqCst);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    (format!("http://{address}"), served)
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

fn dead_endpoint() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    format!("http://{address}")
}

fn project(base_url: &str, model: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".localmind.toml"),
        format!(
            "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n\n[inference]\nembedding_base_url = \"{base_url}\"\nembedding_model = \"{model}\"\ntimeout_secs = 5\n"
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

#[test]
fn a_row_with_no_vector_is_in_the_sweep_set_and_the_sweep_fills_it() {
    // The outage case: memories persisted while the endpoint was down.
    let dead = project(&dead_endpoint(), "test-embed");
    {
        let persistence = MemoryPersistence::open_project(dead.path()).unwrap();
        for index in 0..3 {
            persistence
                .persist_memory_entry(&memory(&format!("m{index}"), &format!("lesson {index}")))
                .unwrap();
        }
        let coverage = persistence.memory_vector_coverage().unwrap();
        assert_eq!(coverage.holes(), 3, "a down endpoint leaves every row bare");
    }

    // The endpoint comes back: rewrite the config to a live one, same store.
    let (base, _served) = counting_server(&[1.0, 0.0, 0.0, 0.0]);
    std::fs::write(
        dead.path().join(".localmind.toml"),
        format!(
            "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n\n[inference]\nembedding_base_url = \"{base}\"\nembedding_model = \"test-embed\"\ntimeout_secs = 5\n"
        ),
    )
    .unwrap();
    let persistence = MemoryPersistence::open_project(dead.path()).unwrap();

    let plan = persistence.backfill_plan().unwrap();
    assert_eq!(plan.memories.absent, 3);
    assert_eq!(plan.total(), 3);

    let report = persistence.backfill_run(false).unwrap();

    assert_eq!(report.embedded, 3);
    assert_eq!(report.failed, 0);
    assert_eq!(
        persistence.memory_vector_coverage().unwrap().holes(),
        0,
        "the holes are gone"
    );
}

#[test]
fn a_completed_sweep_is_idempotent() {
    // The property that makes the sweep safe to re-run after an interruption:
    // the set is a query, so a finished sweep has nothing left to do.
    let (base, served) = counting_server(&[1.0, 0.0, 0.0, 0.0]);
    let dir = project(&base, "test-embed");
    let persistence = MemoryPersistence::open_project(dir.path()).unwrap();
    for index in 0..3 {
        persistence
            .persist_memory_entry(&memory(&format!("m{index}"), &format!("lesson {index}")))
            .unwrap();
    }

    let first = persistence.backfill_run(false).unwrap();
    let after_first = served.load(Ordering::SeqCst);
    let second = persistence.backfill_run(false).unwrap();

    assert!(second.planned.is_empty(), "nothing left in the sweep set");
    assert_eq!(second.embedded, 0);
    assert_eq!(
        served.load(Ordering::SeqCst),
        after_first,
        "a no-op sweep must not spend a single embed call (first run: {})",
        first.embedded
    );
}

#[test]
fn an_edited_body_re_enters_the_sweep_set() {
    // The fingerprint arm: the vector exists but no longer describes the text.
    let (base, _served) = counting_server(&[1.0, 0.0, 0.0, 0.0]);
    let dir = project(&base, "test-embed");
    let persistence = MemoryPersistence::open_project(dir.path()).unwrap();
    persistence
        .persist_memory_entry(&memory("m0", "the original body"))
        .unwrap();
    persistence.backfill_run(false).unwrap();
    assert!(persistence.backfill_plan().unwrap().is_empty());

    persistence
        .persist_memory_entry(&memory("m0", "a materially different body"))
        .unwrap();
    // Persisting re-embeds, so force the stale state the sweep exists to catch.
    persistence
        .upsert_memory_embedding(
            &MemoryEntryId::new("m0"),
            "a-fingerprint-from-the-old-body",
            "test-embed",
            &[1.0, 0.0, 0.0, 0.0],
        )
        .unwrap();

    let plan = persistence.backfill_plan().unwrap();

    assert_eq!(plan.memories.stale_fingerprint, 1, "{plan:?}");
    assert_eq!(plan.memories.absent, 0);
}

#[test]
fn a_model_change_re_enters_the_sweep_set_and_is_actually_repaired() {
    // The arm the old write path could not fix at all: the body is unchanged, so
    // a fingerprint-only guard short-circuits and the row keeps a vector from
    // the wrong model while the write reports success.
    let (base, _served) = counting_server(&[1.0, 0.0, 0.0, 0.0]);
    let dir = project(&base, "old-model");
    let persistence = MemoryPersistence::open_project(dir.path()).unwrap();
    persistence
        .persist_memory_entry(&memory("m0", "a lesson"))
        .unwrap();
    persistence.backfill_run(false).unwrap();
    assert!(persistence.backfill_plan().unwrap().is_empty());
    drop(persistence);

    // Same store, same bodies, different configured model.
    std::fs::write(
        dir.path().join(".localmind.toml"),
        format!(
            "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n\n[inference]\nembedding_base_url = \"{base}\"\nembedding_model = \"new-model\"\ntimeout_secs = 5\n"
        ),
    )
    .unwrap();
    let persistence = MemoryPersistence::open_project(dir.path()).unwrap();

    let plan = persistence.backfill_plan().unwrap();
    assert_eq!(plan.memories.model_mismatch, 1, "{plan:?}");
    assert_eq!(plan.memories.absent, 0);

    let report = persistence.backfill_run(false).unwrap();

    assert_eq!(report.embedded, 1);
    assert!(
        persistence.backfill_plan().unwrap().is_empty(),
        "the model mismatch is actually repaired, not skipped while reporting success"
    );
}

#[test]
fn a_refused_row_is_counted_and_does_not_abandon_the_rest() {
    // A sweep is the repair path; stopping at the first failure would make it
    // useless on exactly the flaky endpoint that created the holes.
    let dead = project(&dead_endpoint(), "test-embed");
    let persistence = MemoryPersistence::open_project(dead.path()).unwrap();
    for index in 0..3 {
        persistence
            .persist_memory_entry(&memory(&format!("m{index}"), &format!("lesson {index}")))
            .unwrap();
    }

    let report = persistence.backfill_run(false).unwrap();

    assert_eq!(report.failed, 3, "every row was attempted");
    assert_eq!(report.embedded, 0);
    assert_eq!(
        report.planned.memories.absent, 3,
        "the plan still reports the real size of the repair"
    );
}

#[test]
fn orphaned_vectors_are_reported_and_pruned_only_on_request() {
    // D006: a vector with no active memory behind it is not re-embedded, and
    // whether it is removed is the operator's call, not a silent side effect.
    // The live global store has exactly one of these; deleting a memory leaves
    // its vector behind the same way.
    let (base, _served) = counting_server(&[1.0, 0.0, 0.0, 0.0]);
    let dir = project(&base, "test-embed");
    let persistence = MemoryPersistence::open_project(dir.path()).unwrap();
    persistence
        .upsert_memory_embedding(
            &MemoryEntryId::new("ghost"),
            "fp",
            "test-embed",
            &[1.0, 0.0, 0.0, 0.0],
        )
        .unwrap();

    let plan = persistence.backfill_plan().unwrap();
    assert_eq!(plan.stale_memory_vectors, 1, "reported: {plan:?}");
    assert!(
        plan.is_empty(),
        "an orphan is never re-embedded — there is no body to embed"
    );

    let kept = persistence.backfill_run(false).unwrap();
    assert_eq!(kept.pruned, 0, "not pruned without asking");
    assert_eq!(
        persistence.backfill_plan().unwrap().stale_memory_vectors,
        1,
        "still there after a sweep that was not asked to prune"
    );

    let pruned = persistence.backfill_run(true).unwrap();

    assert_eq!(pruned.pruned, 1);
    assert_eq!(persistence.backfill_plan().unwrap().stale_memory_vectors, 0);
}

#[test]
fn a_repaired_row_leaves_an_audit_trail() {
    // The audit log was the only witness to the outage this sweep repairs. A
    // repair that writes no audit row is invisible to the same place a reviewer
    // would look to confirm it happened.
    let (base, _served) = counting_server(&[1.0, 0.0, 0.0, 0.0]);
    let dir = project(&base, "test-embed");
    let persistence = MemoryPersistence::open_project(dir.path()).unwrap();
    persistence
        .persist_memory_entry(&memory("m0", "a lesson"))
        .unwrap();
    // Force it back into the sweep set under a fingerprint that no longer matches.
    persistence
        .upsert_memory_embedding(
            &MemoryEntryId::new("m0"),
            "a-stale-fingerprint",
            "test-embed",
            &[1.0, 0.0, 0.0, 0.0],
        )
        .unwrap();
    let before = persistence
        .audit_records()
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == "InferenceCallCompleted")
        .count();

    let report = persistence.backfill_run(false).unwrap();

    assert_eq!(report.embedded, 1);
    let after = persistence
        .audit_records()
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == "InferenceCallCompleted")
        .count();
    assert!(
        after > before,
        "the repair must be visible in the audit trail (before {before}, after {after})"
    );
}
