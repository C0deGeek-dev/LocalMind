//! Retrieval regression pin: a **global**-scoped memory must receive a
//! vector-score contribution in `hybrid_memory_search`. Before the fix
//! `vector_search` scanned the project index only, so a global memory surfaced by
//! the (already global-aware) keyword search never got a vector boost — its
//! `vector_score` stayed `0`. With `vector_search` now scanning project + global,
//! the global memory's stored vector is reachable and contributes to the hybrid
//! score. A no-embeddings run (no query vector) stays byte-identical to
//! keyword-only.
//!
//! Embeddings are a content-marker fixture server (no live model), the offline
//! acceptance bar — mirroring the store crate's `semantic_dedup` fixture.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

use localmind_core::{
    Confidence, EnvFingerprint, EvidenceKind, EvidenceRef, LessonCategory, MemoryEntry,
    MemoryEntryId, MemoryScope, MemoryStatus, SessionId,
};
use localmind_search::{hybrid_memory_search, HybridMemorySearchOptions, SemanticQuery};
use localmind_store::MemoryPersistence;

/// A fixture `/v1/embeddings` server keying its reply on request content: input
/// containing `marker` embeds to `[1, 0]`, anything else to `[0, 1]`. So a memory
/// body containing the marker is maximally similar (cosine 1.0) to a `[1, 0]`
/// query vector. Stands in for a live model.
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

fn global_entry(id: &str, body: &str) -> MemoryEntry {
    MemoryEntry {
        id: MemoryEntryId::new(id),
        scope: MemoryScope::GlobalUser,
        body: body.to_string(),
        category: LessonCategory::DebuggingRecipe,
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
fn a_global_memory_gets_a_vector_score_in_hybrid_search() {
    // One embed call: the seed memory's body at persist time. The query vector is
    // supplied directly, so it needs no embed.
    let base = marker_embeddings_server("ripgrep", 1);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let global = tempfile::tempdir().unwrap();
    let global_root = global.path().join("memory");
    std::fs::write(
        root.join(".localmind.toml"),
        format!(
            "[learning]\nenabled = true\nallowed_scopes = [\"project\", \"global_user\"]\nglobal_memory_root = '{}'\n\n[inference]\nembedding_base_url = \"{base}\"\nembedding_model = \"test-embed\"\ntimeout_secs = 5\n",
            global_root.display(),
        ),
    )
    .unwrap();

    let persistence = MemoryPersistence::open_project(root).unwrap();
    // The memory is global-scoped: it persists into the machine-wide store and its
    // vector lands in the global `vector_index`.
    persistence
        .persist_memory_entry(&global_entry("g1", "use ripgrep for fast code search"))
        .unwrap();

    // A query vector aligned with the seed's marker embedding (`[1, 0]`).
    let query_vector = [1.0_f32, 0.0];
    let mut options = HybridMemorySearchOptions::new(10);
    options.semantic = SemanticQuery::Vector(&query_vector);
    let results = hybrid_memory_search(&persistence, "ripgrep", options).unwrap();
    let g1 = results
        .iter()
        .find(|result| result.memory.memory_id.as_str() == "g1")
        .expect("the global memory must surface via the (global-aware) keyword search");
    assert!(
        g1.vector_score > 0.0,
        "a global memory must receive a vector-score contribution (was 0 before the fix): {g1:?}"
    );

    // No query vector ⇒ keyword-only: the global memory still surfaces, contributes
    // no vector score, and its combined score is exactly the keyword score
    // (byte-identical to the pre-embeddings contract).
    let mut options = HybridMemorySearchOptions::new(10);
    options.semantic = SemanticQuery::Disabled;
    let keyword_only = hybrid_memory_search(&persistence, "ripgrep", options).unwrap();
    let g1_keyword = keyword_only
        .iter()
        .find(|result| result.memory.memory_id.as_str() == "g1")
        .expect("keyword-only retrieval must still find the global memory");
    assert_eq!(
        g1_keyword.vector_score, 0.0,
        "the keyword-only path must contribute no vector score"
    );
    assert_eq!(
        g1_keyword.combined_score, g1_keyword.keyword_score,
        "with no vector, the combined score is exactly the keyword score"
    );
}

#[test]
fn hybrid_search_applies_the_configured_foreign_machine_penalty() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join(".localmind.toml"),
        "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n\n[sync]\ndevice_label = \"PC\"\nforeign_env_weight = 0.5\n",
    )
    .unwrap();
    let persistence = MemoryPersistence::open_project(root).unwrap();

    let mut local = global_entry("local", "postgres index migration recipe");
    local.scope = MemoryScope::Project;
    local.sync_meta.origin_env = Some(EnvFingerprint::capture("PC"));
    let mut foreign = global_entry("foreign", "postgres index migration recipe");
    foreign.scope = MemoryScope::Project;
    foreign.sync_meta.origin_env = Some(EnvFingerprint::capture("Laptop"));
    persistence.persist_memory_entry(&foreign).unwrap();
    persistence.persist_memory_entry(&local).unwrap();

    let mut options = HybridMemorySearchOptions::new(10);
    options.semantic = SemanticQuery::Disabled;
    let results = hybrid_memory_search(&persistence, "postgres index", options).unwrap();
    let local = results
        .iter()
        .find(|result| result.memory.memory_id.as_str() == "local")
        .unwrap();
    let foreign = results
        .iter()
        .find(|result| result.memory.memory_id.as_str() == "foreign")
        .unwrap();

    assert_eq!(local.combined_score, local.keyword_score);
    assert_eq!(foreign.combined_score, foreign.keyword_score * 0.5);
    assert_eq!(results[0].memory.memory_id.as_str(), "local");

    // Re-read the same project through ProjectConfig with the documented
    // disabling value. Equal base scores return to their deterministic
    // unweighted order, proving the config key controls the final ranking.
    std::fs::write(
        root.join(".localmind.toml"),
        "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n\n[sync]\ndevice_label = \"PC\"\nforeign_env_weight = 1.0\n",
    )
    .unwrap();
    let unweighted_persistence = MemoryPersistence::open_project(root).unwrap();
    let mut options = HybridMemorySearchOptions::new(10);
    options.semantic = SemanticQuery::Disabled;
    let unweighted =
        hybrid_memory_search(&unweighted_persistence, "postgres index", options).unwrap();
    assert_eq!(
        unweighted[0].memory.memory_id.as_str(),
        "foreign",
        "foreign_env_weight = 1.0 must restore the unweighted order"
    );
    assert_eq!(unweighted[0].combined_score, unweighted[1].combined_score);
}
