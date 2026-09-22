//! Evidence excerpts: bounded observed text on a fact, redacted before it is
//! stored, carried through review, and kept out of accepted memory.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use localmind_core::{
    CandidateLesson, Confidence, EvidenceKind, EvidenceRef, LessonCategory, LessonId, ReviewAction,
    ReviewDecision, ReviewItemId, SessionId, SuggestedAction, EXCERPT_TRUNCATION_MARKER,
    MAX_EXCERPT_CHARS,
};
use localmind_store::{MarkdownMemoryFormat, MemoryPersistence, ReviewQueue};

const SECRET: &str = "sk-proj-abcdefghijklmnopqrstuvwxyz123456";

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".localmind.toml"),
        "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n",
    )
    .unwrap();
    dir
}

fn fact(excerpt: &str) -> EvidenceRef {
    EvidenceRef::identified(
        EvidenceKind::ToolEvent,
        "run_shell call c3 failed",
        "session:2026-09-22-a",
        "localpilot:session/2026-09-22-a#call=c3",
        "sha256:03",
    )
    .with_excerpt(excerpt)
}

fn candidate(evidence: EvidenceRef) -> CandidateLesson {
    CandidateLesson::new(
        LessonId::new("lesson-1"),
        "Export the deploy token before running the release script.",
        LessonCategory::Process,
        Confidence::new(0.8).unwrap(),
        SuggestedAction::PromoteToMemory,
    )
    .with_evidence(evidence)
}

fn stored_json(dir: &tempfile::TempDir) -> String {
    let database = dir.path().join(".localmind").join("localmind.sqlite");
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .query_row("SELECT candidate_json FROM review_items", [], |row| {
            row.get(0)
        })
        .unwrap()
}

#[test]
fn a_secret_in_an_excerpt_never_reaches_the_database() {
    let dir = project();
    let queue = ReviewQueue::open_project(dir.path()).unwrap();
    let leaky = fact(&format!("error: authentication failed for token {SECRET}"));

    queue
        .enqueue_candidates(&SessionId::new("session"), &[candidate(leaky)])
        .unwrap();

    let json = stored_json(&dir);
    assert!(!json.contains(SECRET), "{json}");
    assert!(json.contains("authentication failed"), "the rest survives");

    // The identity a row carries is the identity of what it stores: the
    // redacted form, recomputed from the database, not the caller's copy.
    let stored = queue
        .get(&ReviewItemId::new("lesson-1"))
        .unwrap()
        .unwrap()
        .candidate;
    let evidence = &stored.evidence()[0];
    assert!(
        evidence.identity_is_intact(),
        "redacting an excerpt must not break the fact's id"
    );
}

#[test]
fn redaction_on_write_keeps_the_excerpt_under_its_bound() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".localmind.toml"),
        "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\nexcluded_paths = [\"s.env\"]\n",
    )
    .unwrap();
    let queue = ReviewQueue::open_project(dir.path()).unwrap();
    // The placeholder for a configured path is longer than the path, so an
    // excerpt at the bound grows past it when the store re-redacts — and has
    // to be cut again.
    let at_bound = format!("{} s.env", "x".repeat(MAX_EXCERPT_CHARS - 6));
    assert_eq!(at_bound.chars().count(), MAX_EXCERPT_CHARS);

    queue
        .enqueue_candidates(&SessionId::new("session"), &[candidate(fact(&at_bound))])
        .unwrap();

    let stored = queue
        .get(&ReviewItemId::new("lesson-1"))
        .unwrap()
        .unwrap()
        .candidate;
    let excerpt = stored.evidence()[0].excerpt.clone().unwrap();
    assert!(!excerpt.contains("s.env"));
    assert_eq!(excerpt.chars().count(), MAX_EXCERPT_CHARS);
    assert!(excerpt.ends_with(EXCERPT_TRUNCATION_MARKER));
}

#[test]
fn resubmitting_the_same_leaky_fact_is_a_restatement_not_a_revision() {
    let dir = project();
    let queue = ReviewQueue::open_project(dir.path()).unwrap();
    let leaky = || candidate(fact(&format!("token {SECRET} rejected")));

    queue
        .enqueue_candidates(&SessionId::new("session"), &[leaky()])
        .unwrap();
    let first = stored_json(&dir);
    let inserted = queue
        .enqueue_candidates(&SessionId::new("session"), &[leaky()])
        .unwrap();

    // Identity is taken after redaction on both passes, so the unredacted
    // resubmission matches the stored row instead of reading as new substance.
    assert_eq!(inserted, 0);
    assert_eq!(stored_json(&dir), first, "the row was not rewritten");
    let stored = queue
        .get(&ReviewItemId::new("lesson-1"))
        .unwrap()
        .unwrap()
        .candidate;
    assert!(stored.revises.is_none(), "no revision was recorded");
}

#[test]
fn an_excerpt_is_review_material_and_stays_out_of_accepted_memory() {
    let dir = project();
    let queue = ReviewQueue::open_project(dir.path()).unwrap();
    queue
        .enqueue_candidates(
            &SessionId::new("session"),
            &[candidate(fact("error: DEPLOY_TOKEN is not set"))],
        )
        .unwrap();
    queue
        .decide(ReviewDecision {
            item_id: ReviewItemId::new("lesson-1"),
            action: ReviewAction::Accept,
            reviewer: "reviewer".to_string(),
            decided_at: None,
            note: None,
            replacement_summary: None,
            evidence: Vec::new(),
        })
        .unwrap();

    let store = MemoryPersistence::open_project(dir.path()).unwrap();
    store
        .promote_review_item(&ReviewItemId::new("lesson-1"))
        .unwrap();
    let record = store
        .list_memory()
        .unwrap()
        .into_iter()
        .find(|record| record.memory_id.as_str() == "lesson-1")
        .expect("the lesson was promoted");
    let text = std::fs::read_to_string(&record.path).unwrap();

    // Accepted memory is searchable. Observed output is what the lesson was
    // learned from, not the lesson, so it does not become retrievable text.
    assert!(!text.contains("DEPLOY_TOKEN is not set"), "{text}");
    let parsed = MarkdownMemoryFormat::parse(&text).unwrap();
    assert_eq!(parsed.evidence[0].excerpt, None);
    assert!(
        parsed.evidence[0].identity_is_intact(),
        "and the fact still verifies without it"
    );
}
