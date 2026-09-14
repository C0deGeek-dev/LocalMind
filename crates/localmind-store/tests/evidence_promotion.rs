//! Evidence identity through promotion into accepted memory.
//!
//! Before this, the memory Markdown dropped `content_hash` and `metadata`, so a
//! promoted fact kept its id and lost the means to verify it. The guarantee is
//! now local and stated as such: identity survives promotion on this machine,
//! and does not claim to survive a redacting export.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use localmind_core::{
    CandidateLesson, Confidence, EvidenceKind, EvidenceRef, LessonCategory, LessonId, MemoryEntry,
    MemoryEntryId, MemoryScope, MemoryStatus, ReviewAction, ReviewDecision, ReviewItemId,
    SessionId, SuggestedAction, SyncMeta, EVIDENCE_SOURCE_KEY,
};
use localmind_store::{
    BundleScope, MarkdownMemoryFormat, MemoryBundleExporter, MemoryPersistence, ReviewQueue,
};

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".localmind.toml"),
        "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n",
    )
    .unwrap();
    dir
}

fn identified() -> EvidenceRef {
    EvidenceRef::identified(
        EvidenceKind::TestOutput,
        "migration output",
        "session:2026-09-14-a",
        "repo@aaa#migrate.log",
        "sha256:01",
    )
}

fn entry(evidence: Vec<EvidenceRef>) -> MemoryEntry {
    MemoryEntry {
        id: MemoryEntryId::new("mem-1"),
        scope: MemoryScope::Project,
        body: "Run the database migration before starting the server.".to_string(),
        category: LessonCategory::Process,
        confidence: Confidence::new(0.8).unwrap(),
        source_session: Some(SessionId::new("session")),
        evidence,
        tags: vec!["accepted".to_string()],
        related_files: Vec::new(),
        related_entities: Vec::new(),
        created_at: None,
        updated_at: None,
        supersedes: Vec::new(),
        contradicts: Vec::new(),
        status: MemoryStatus::Active,
        sync_meta: SyncMeta::default(),
    }
}

#[test]
fn a_fact_promoted_through_review_can_still_be_verified() {
    let dir = project();
    let queue = ReviewQueue::open_project(dir.path()).unwrap();
    let candidate = CandidateLesson::new(
        LessonId::new("lesson-1"),
        "Run the database migration before starting the server.",
        LessonCategory::Process,
        Confidence::new(0.8).unwrap(),
        SuggestedAction::PromoteToMemory,
    )
    .with_evidence(identified());
    queue
        .enqueue_candidates(&SessionId::new("session"), &[candidate])
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

    // Read the fact back from the Markdown file itself — the canonical source
    // of truth for accepted memory — not from an in-memory copy.
    let record = store
        .list_memory()
        .unwrap()
        .into_iter()
        .find(|record| record.memory_id.as_str() == "lesson-1")
        .expect("the lesson was promoted");
    let parsed =
        MarkdownMemoryFormat::parse(&std::fs::read_to_string(&record.path).unwrap()).unwrap();

    let fact = &parsed.evidence[0];
    assert!(fact.has_canonical_id());
    assert!(
        fact.identity_is_intact(),
        "a promoted fact keeps the inputs its id was derived from"
    );
    assert_eq!(fact.content_hash.as_deref(), Some("sha256:01"));
    assert_eq!(fact.source(), Some("session:2026-09-14-a"));
}

#[test]
fn a_memory_file_written_without_the_new_fields_parses_as_it_always_did() {
    let legacy = entry(vec![EvidenceRef::new(
        EvidenceKind::Transcript,
        "redacted transcript",
    )
    .redacted()]);
    let text = MarkdownMemoryFormat::serialize(&legacy);
    assert!(
        !text.contains("content_hash"),
        "nothing new is written for a legacy fact"
    );
    assert!(!text.contains("source:"));

    let parsed = MarkdownMemoryFormat::parse(&text).unwrap();
    assert_eq!(parsed.evidence, legacy.evidence);
    assert!(!parsed.evidence[0].identity_is_intact());
}

#[test]
fn only_the_two_identity_inputs_leave_metadata() {
    let mut fact = identified();
    fact.metadata
        .insert("operator_note".to_string(), "do not export me".to_string());
    let text = MarkdownMemoryFormat::serialize(&entry(vec![fact]));

    assert!(text.contains("content_hash:"));
    assert!(text.contains("source:"));
    assert!(
        !text.contains("operator_note") && !text.contains("do not export me"),
        "metadata is an open map; anything beyond source would reach a bundle unredacted"
    );

    let parsed = MarkdownMemoryFormat::parse(&text).unwrap();
    assert_eq!(
        parsed.evidence[0].metadata.keys().collect::<Vec<_>>(),
        vec![EVIDENCE_SOURCE_KEY]
    );
    assert!(parsed.evidence[0].identity_is_intact());
}

#[test]
fn a_bundle_redacts_the_source_and_does_not_pretend_identity_survives() {
    let dir = project();
    let store = MemoryPersistence::open_project(dir.path()).unwrap();
    let leaky = EvidenceRef::identified(
        EvidenceKind::Command,
        "deploy output",
        "session with token sk-proj-abcdefghijklmnopqrstuvwxyz123456",
        "repo@aaa#deploy.log",
        "sha256:02",
    );
    store.persist_memory_entry(&entry(vec![leaky])).unwrap();

    let outcome = MemoryBundleExporter::open_project(dir.path())
        .unwrap()
        .export(BundleScope::Project, "author")
        .unwrap();
    let exported = &outcome.bundle.entries[0].evidence[0];
    let source = exported.source().unwrap_or_default();

    assert!(outcome.scan.found_secrets());
    assert!(
        !source.contains("sk-proj"),
        "the source is redacted on export"
    );
    assert!(
        !exported.identity_is_intact(),
        "redacting an identity input means the receiving machine cannot re-verify, by design"
    );
}
