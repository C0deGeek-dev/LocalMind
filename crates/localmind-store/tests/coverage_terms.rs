//! Coverage counts tokens, not substrings.
//!
//! The term-coverage gate decides which keyword matches survive. Counting a term
//! as present because it appears *inside* a longer word credits words the body
//! never contains — and on a query with no correct answer, that is how an
//! irrelevant body clears the gate.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use localmind_core::{
    Confidence, EvidenceKind, EvidenceRef, LessonCategory, MemoryEntry, MemoryEntryId, MemoryScope,
    MemoryStatus, SessionId,
};
use localmind_store::MemoryPersistence;

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".localmind.toml"),
        "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n",
    )
    .unwrap();
    dir
}

fn entry(id: &str, body: &str) -> MemoryEntry {
    MemoryEntry {
        id: MemoryEntryId::new(id),
        scope: MemoryScope::Project,
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

fn store(dir: &Path, entries: &[(&str, &str)]) -> MemoryPersistence {
    let store = MemoryPersistence::open_project(dir).unwrap();
    for (id, body) in entries {
        store.persist_memory_entry(&entry(id, body)).unwrap();
    }
    store
}

#[test]
fn a_term_inside_a_longer_word_does_not_clear_coverage() {
    // Three significant terms, so the gate is active and a body needs two.
    // `unchanged` contains "changed" and `outcome` contains "come" as substrings,
    // and neither word is in the body. Under the old rule this body cleared the
    // gate on two words it never contained.
    let dir = project();
    let store = store(
        dir.path(),
        &[(
            "infix",
            "the outcome was unchanged after the migration completed",
        )],
    );

    let hits = store.search_lang("changed come pipeline", None).unwrap();
    assert!(
        hits.is_empty(),
        "coverage must not be cleared by substrings: {hits:?}"
    );
}

#[test]
fn a_whole_token_still_clears_coverage() {
    let dir = project();
    let store = store(
        dir.path(),
        &[("tokens", "the pipeline changed and the outcome improved")],
    );

    let hits = store.search_lang("changed outcome pipeline", None).unwrap();
    assert_eq!(hits.len(), 1, "whole-token matches must still count");
}

#[test]
fn a_prefix_match_counts_because_the_query_asked_for_one() {
    // `fts_match_expression` prefix-extends terms at or above the minimum length,
    // so `migrat` matches *migration* in the MATCH itself. Coverage has to agree
    // with the query that produced the row rather than being stricter than it.
    let dir = project();
    let store = store(
        dir.path(),
        &[("prefix", "the migration pipeline completed cleanly")],
    );

    let hits = store.search_lang("migrat pipelin cleanly", None).unwrap();
    assert_eq!(hits.len(), 1, "prefix matches must count: {hits:?}");
}

#[test]
fn a_short_term_is_not_prefix_extended_and_must_match_whole() {
    // Below the prefix threshold the MATCH expression asks for the exact token,
    // so coverage must too — otherwise `me` credits *memory* again by another
    // route.
    let dir = project();
    let store = store(
        dir.path(),
        &[("short", "the memory implementation was replaced")],
    );

    let hits = store.search_lang("me imp pipeline", None).unwrap();
    assert!(
        hits.is_empty(),
        "short terms must match a whole token, not a word start: {hits:?}"
    );
}

#[test]
fn a_hit_reports_whether_it_contains_the_querys_subject() {
    // The rarest term is what a question is *about*; the rest is the shape of the
    // asking. A caller applying a semantic relevance floor needs to tell the two
    // apart, because a floor high enough to drop off-topic noise is also high
    // enough to drop a real answer phrased at a distance.
    let dir = project();
    let store = store(
        dir.path(),
        &[
            (
                "subject",
                "process_dir resolves the Windows verbatim prefix",
            ),
            ("scaffolding", "what you know about a topic and how to ask"),
            // Scaffolding words are common by nature, and that is the whole
            // signal: with one memory each they would tie with the subject, and
            // a tie keeps the earliest term rather than the meaningful one.
            ("common-a", "know what you are doing about the release"),
            ("common-b", "let the reviewer know about the rollback"),
        ],
    );

    let hits = store
        .search_lang("what should I know about process_dir", None)
        .unwrap();

    let subject = hits
        .iter()
        .find(|hit| hit.memory_id.as_str() == "subject")
        .expect("the memory that defines the subject must be retrieved");
    assert!(
        subject.subject_matched,
        "a body containing the query's rarest term answers the question asked"
    );

    // The scaffolding memory may or may not survive the coverage gate — that is
    // not what this pins. What it pins is that if it does, it is not claiming to
    // contain the subject.
    for hit in hits
        .iter()
        .filter(|hit| hit.memory_id.as_str() == "scaffolding")
    {
        assert!(
            !hit.subject_matched,
            "a body matching only the question's shape has not answered it"
        );
    }
}
