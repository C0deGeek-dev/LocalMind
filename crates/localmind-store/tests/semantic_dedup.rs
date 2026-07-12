//! Semantic dedup of review candidates against accepted memory: a paraphrase
//! that means the same thing as an accepted lesson but shares too few words for
//! the lexical pass (overlap ~0.33, under the 0.6 lexical bar) is caught by
//! vector cosine and flagged `duplicate_of` → routed to review (not
//! auto-accepted), while a genuinely distinct lesson is left alone. With no
//! embedding endpoint the behaviour is exactly the lexical contract (the
//! no-regression fallback). Embeddings are a content-marker fixture server (no
//! live model), the offline acceptance bar.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

use localmind_core::{
    CandidateLesson, Confidence, EvidenceKind, EvidenceRef, LessonCategory, LessonId, MemoryEntry,
    MemoryEntryId, MemoryScope, MemoryStatus, ReviewState, SessionId, SuggestedAction,
};
use localmind_store::{MemoryPersistence, ReviewModeProcessor, ReviewQueue};

/// A fixture `/v1/embeddings` server that keys its reply on request content: an
/// input containing `marker` embeds to `[1, 0]`, anything else to `[0, 1]`. So
/// two texts that both contain the marker are maximally similar (cosine 1.0)
/// even when they share few *words*, and a text without it is orthogonal. This
/// stands in for "two paraphrases mean the same thing" without a live model.
fn marker_embeddings_server(marker: &'static str, max_requests: usize) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    thread::spawn(move || {
        for _ in 0..max_requests {
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            let mut request = Vec::new();
            let mut buffer = [0_u8; 2048];
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
            let text = String::from_utf8_lossy(&request);
            let embedding = if text.contains(marker) {
                "[1,0]"
            } else {
                "[0,1]"
            };
            let body = format!("{{\"data\":[{{\"embedding\":{embedding}}}]}}");
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

fn seed_memory(id: &str, body: &str) -> MemoryEntry {
    seed_memory_scoped(id, body, MemoryScope::Project)
}

fn seed_memory_scoped(id: &str, body: &str, scope: MemoryScope) -> MemoryEntry {
    MemoryEntry {
        id: MemoryEntryId::new(id),
        scope,
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

fn candidate(summary: &str) -> CandidateLesson {
    CandidateLesson::new(
        LessonId::new(format!("lesson-{}", summary.len())),
        summary,
        LessonCategory::Process,
        Confidence::new(0.7).unwrap(),
        SuggestedAction::PromoteToMemory,
    )
    .with_evidence(EvidenceRef::new(EvidenceKind::Transcript, "redacted").redacted())
}

// The accepted lesson and its paraphrase: same meaning, low lexical overlap
// (~0.33), both contain the marker word "edit"; the distinct lesson does not.
const ACCEPTED: &str = "A failed edit should rewrite the whole file.";
const PARAPHRASE: &str = "Replace the entire file when an edit operation fails.";
const DISTINCT: &str = "Run the database migration before starting the server.";

#[test]
fn vector_cosine_collapses_a_paraphrase_the_lexical_pass_misses() {
    // Three embed calls: the seed (ACCEPTED), then the two candidates.
    let base = marker_embeddings_server("edit", 3);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join(".localmind.toml"),
        format!(
            "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n\n[inference]\nembedding_base_url = \"{base}\"\nembedding_model = \"test-embed\"\ntimeout_secs = 5\n\n[review]\nmode = \"automatic\"\ntrusted_threshold = 0.5\nsemantic_dedup = true\n",
        ),
    )
    .unwrap();

    let persistence = MemoryPersistence::open_project(root).unwrap();
    persistence
        .persist_memory_entry(&seed_memory("mem-accepted", ACCEPTED))
        .unwrap();

    let queue = ReviewQueue::open_project(root).unwrap();
    queue
        .enqueue_candidates(
            &SessionId::new("session"),
            &[candidate(PARAPHRASE), candidate(DISTINCT)],
        )
        .unwrap();

    ReviewModeProcessor::apply_project(root).unwrap();

    let items = queue.list().unwrap();
    let find = |summary: &str| {
        items
            .iter()
            .find(|item| item.candidate.summary() == summary)
            .unwrap_or_else(|| panic!("missing item {summary}"))
    };

    // The paraphrase is below the lexical bar but a vector duplicate: flagged
    // against the accepted memory and held for review (not auto-accepted).
    let paraphrase = find(PARAPHRASE);
    assert_ne!(
        paraphrase.state,
        ReviewState::Accepted,
        "a semantic duplicate must not auto-accept"
    );
    assert_eq!(
        paraphrase
            .candidate
            .review_annotation
            .as_ref()
            .unwrap()
            .duplicate_of
            .as_deref(),
        Some("mem-accepted"),
        "the paraphrase must be flagged a duplicate of the accepted memory"
    );

    // The distinct lesson is not a vector duplicate: it auto-accepts as before.
    let distinct = find(DISTINCT);
    assert_eq!(
        distinct.state,
        ReviewState::Accepted,
        "a genuinely distinct lesson must not be merged"
    );
}

#[test]
fn vector_cosine_collapses_a_global_paraphrase_the_project_scan_missed() {
    // The semantic rung must reach the machine-wide global store: the accepted
    // lesson is **global**-scoped, so its vector lives in the global
    // `vector_index`. A project-only vector scan would query an empty project index,
    // the semantic rung would find nothing, and automatic mode would auto-accept the
    // paraphrase. With `vector_search` scanning project + global, the global
    // paraphrase is flagged `duplicate_of` and routed to review, never merged.
    let base = marker_embeddings_server("edit", 3);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let global = tempfile::tempdir().unwrap();
    let global_root = global.path().join("memory");
    std::fs::write(
        root.join(".localmind.toml"),
        format!(
            "[learning]\nenabled = true\nallowed_scopes = [\"project\", \"global_user\"]\nglobal_memory_root = '{}'\n\n[inference]\nembedding_base_url = \"{base}\"\nembedding_model = \"test-embed\"\ntimeout_secs = 5\n\n[review]\nmode = \"automatic\"\ntrusted_threshold = 0.5\nsemantic_dedup = true\n",
            global_root.display(),
        ),
    )
    .unwrap();

    let persistence = MemoryPersistence::open_project(root).unwrap();
    // The accepted memory is global-scoped: it persists into the machine-wide
    // store and its vector lands in the global `vector_index`.
    persistence
        .persist_memory_entry(&seed_memory_scoped(
            "mem-accepted",
            ACCEPTED,
            MemoryScope::GlobalUser,
        ))
        .unwrap();

    let queue = ReviewQueue::open_project(root).unwrap();
    queue
        .enqueue_candidates(
            &SessionId::new("session"),
            &[candidate(PARAPHRASE), candidate(DISTINCT)],
        )
        .unwrap();

    ReviewModeProcessor::apply_project(root).unwrap();

    let items = queue.list().unwrap();
    let find = |summary: &str| {
        items
            .iter()
            .find(|item| item.candidate.summary() == summary)
            .unwrap_or_else(|| panic!("missing item {summary}"))
    };

    let paraphrase = find(PARAPHRASE);
    assert_ne!(
        paraphrase.state,
        ReviewState::Accepted,
        "a semantic duplicate of a GLOBAL memory must not auto-accept"
    );
    assert_eq!(
        paraphrase
            .candidate
            .review_annotation
            .as_ref()
            .unwrap()
            .duplicate_of
            .as_deref(),
        Some("mem-accepted"),
        "the paraphrase must be flagged a duplicate of the global accepted memory"
    );

    let distinct = find(DISTINCT);
    assert_eq!(
        distinct.state,
        ReviewState::Accepted,
        "a genuinely distinct lesson must not be merged"
    );
}

/// A fixture `/v1/embeddings` server returning a fixed **2-D unit vector** keyed
/// on a marker token in the request. Because each vector is a unit vector of the
/// form `[c, sqrt(1 - c^2)]`, its cosine against the anchor `[1, 0]` is exactly
/// `c` — so the route-to-review band edges are hit precisely:
/// `conftoken` → 0.90 (confident, ≥ 0.86), `bandtoken` → 0.84 (borderline, in
/// `[0.83, 0.86)`), `disttoken` → 0.80 (below the band → distinct).
fn tiered_embeddings_server(max_requests: usize) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    thread::spawn(move || {
        for _ in 0..max_requests {
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            let mut request = Vec::new();
            let mut buffer = [0_u8; 2048];
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
            let text = String::from_utf8_lossy(&request);
            let embedding = if text.contains("anchortoken") {
                "[1.0,0.0]"
            } else if text.contains("conftoken") {
                "[0.9,0.4358899]"
            } else if text.contains("bandtoken") {
                "[0.84,0.5425865]"
            } else if text.contains("disttoken") {
                "[0.8,0.6]"
            } else {
                "[0.0,1.0]"
            };
            let body = format!("{{\"data\":[{{\"embedding\":{embedding}}}]}}");
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

fn tiered_candidate(id: &str, summary: &str) -> CandidateLesson {
    CandidateLesson::new(
        LessonId::new(id),
        summary,
        LessonCategory::Process,
        Confidence::new(0.7).unwrap(),
        SuggestedAction::PromoteToMemory,
    )
    .with_evidence(EvidenceRef::new(EvidenceKind::Transcript, "redacted").redacted())
}

#[test]
fn a_borderline_paraphrase_routes_to_review_while_a_distinct_one_does_not() {
    // The route-to-review band: a confident match (cosine ≥ 0.86) and a borderline
    // match (in [0.83, 0.86)) both flag `duplicate_of` and are held for review
    // (never auto-merged); a genuinely-distinct lesson (cosine < 0.83) auto-accepts.
    // Four embed calls: the seed body, then the three candidate summaries.
    let base = tiered_embeddings_server(4);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join(".localmind.toml"),
        format!(
            "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n\n[inference]\nembedding_base_url = \"{base}\"\nembedding_model = \"test-embed\"\ntimeout_secs = 5\n\n[review]\nmode = \"automatic\"\ntrusted_threshold = 0.5\nsemantic_dedup = true\n",
        ),
    )
    .unwrap();

    let persistence = MemoryPersistence::open_project(root).unwrap();
    persistence
        .persist_memory_entry(&seed_memory(
            "mem-accepted",
            "use anchortoken when scanning directories",
        ))
        .unwrap();

    // Distinct marker tokens keep lexical overlap with the seed at 0 (well under
    // the 0.6 bar), so each candidate reaches the vector rung; the embedder's
    // marker decides its cosine tier.
    let confident = "prefer conftoken for recursive lookups";
    let borderline = "choose bandtoken to traverse nested folders";
    let distinct = "compile disttoken with debug symbols enabled";
    let queue = ReviewQueue::open_project(root).unwrap();
    queue
        .enqueue_candidates(
            &SessionId::new("session"),
            &[
                tiered_candidate("c-conf", confident),
                tiered_candidate("c-band", borderline),
                tiered_candidate("c-dist", distinct),
            ],
        )
        .unwrap();

    ReviewModeProcessor::apply_project(root).unwrap();

    let items = queue.list().unwrap();
    let find = |summary: &str| {
        items
            .iter()
            .find(|item| item.candidate.summary() == summary)
            .unwrap_or_else(|| panic!("missing item {summary}"))
    };
    let annotation = |summary: &str| {
        find(summary)
            .candidate
            .review_annotation
            .clone()
            .unwrap_or_else(|| panic!("missing annotation for {summary}"))
    };

    // Confident (0.90): flagged, held for review, with the confident note.
    let conf = find(confident);
    assert_ne!(
        conf.state,
        ReviewState::Accepted,
        "a confident semantic duplicate must not auto-accept"
    );
    assert_eq!(
        annotation(confident).duplicate_of.as_deref(),
        Some("mem-accepted")
    );
    assert_eq!(
        annotation(confident).notes,
        "Similar accepted memory found; human review recommended."
    );

    // Borderline (0.84): in the band — flagged, held for review, borderline note.
    let band = find(borderline);
    assert_ne!(
        band.state,
        ReviewState::Accepted,
        "a borderline paraphrase in the review band must route to review, not auto-accept"
    );
    assert_eq!(
        annotation(borderline).duplicate_of.as_deref(),
        Some("mem-accepted"),
        "a borderline match must still flag the duplicate it is near"
    );
    assert_eq!(
        annotation(borderline).notes,
        "Borderline semantic match (review band); human review recommended.",
        "a borderline match must be surfaced to the human as borderline"
    );

    // Distinct (0.80): below the band — not flagged, auto-accepts (no false merge).
    let dist = find(distinct);
    assert_eq!(
        dist.state,
        ReviewState::Accepted,
        "a genuinely-distinct lesson below the band must not be flagged"
    );
    assert!(
        annotation(distinct).duplicate_of.is_none(),
        "a sub-band cosine must not flag a duplicate"
    );
}

#[test]
fn without_an_endpoint_dedup_is_exactly_the_lexical_contract() {
    // No `[inference]` block => semantic dedup inactive => the paraphrase's ~0.33
    // lexical overlap is under the 0.6 bar, so it is NOT a duplicate and
    // auto-accepts, exactly as before this feature existed (no regression).
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join(".localmind.toml"),
        "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n\n[review]\nmode = \"automatic\"\ntrusted_threshold = 0.5\nsemantic_dedup = true\n",
    )
    .unwrap();

    let persistence = MemoryPersistence::open_project(root).unwrap();
    persistence
        .persist_memory_entry(&seed_memory("mem-accepted", ACCEPTED))
        .unwrap();

    let queue = ReviewQueue::open_project(root).unwrap();
    queue
        .enqueue_candidates(&SessionId::new("session"), &[candidate(PARAPHRASE)])
        .unwrap();

    ReviewModeProcessor::apply_project(root).unwrap();

    let paraphrase = queue
        .list()
        .unwrap()
        .into_iter()
        .find(|item| item.candidate.summary() == PARAPHRASE)
        .unwrap();
    assert_eq!(
        paraphrase.state,
        ReviewState::Accepted,
        "with no embeddings the lexical-only path must auto-accept the paraphrase"
    );
    assert!(
        paraphrase
            .candidate
            .review_annotation
            .as_ref()
            .unwrap()
            .duplicate_of
            .is_none(),
        "lexical overlap under the bar must not flag a duplicate"
    );
}
