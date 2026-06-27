//! End-to-end: export accepted memory on "machine A", move the signed pack to a
//! fresh "machine B", import → verify → review → promote, and confirm the lessons
//! are retrievable, scope-correct, and carry origin provenance.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use localmind_core::{
    Confidence, EvidenceKind, EvidenceRef, LessonCategory, MemoryEntry, MemoryEntryId, MemoryScope,
    MemoryStatus, ReviewAction, ReviewDecision, ReviewItemId,
};
use localmind_store::{
    sign_bundle, BundleImporter, BundleScope, ImportTrust, KeyStore, MemoryBundleExporter,
    MemoryPersistence, ReviewQueue, SignedBundle,
};

/// A hermetic project with its global store rooted inside its own dir.
fn machine(dir: &tempfile::TempDir) -> std::path::PathBuf {
    let root = dir.path().to_path_buf();
    let global = root.join("global-store").join("memory");
    std::fs::write(
        root.join(".localmind.toml"),
        format!(
            "[learning]\nenabled = true\nglobal_memory_root = {:?}\n",
            global.to_string_lossy()
        ),
    )
    .unwrap();
    root
}

fn entry(id: &str, scope: MemoryScope, category: LessonCategory, body: &str) -> MemoryEntry {
    MemoryEntry {
        id: MemoryEntryId::new(id),
        scope,
        body: body.to_string(),
        category,
        confidence: Confidence::new(0.85).unwrap(),
        source_session: None,
        evidence: vec![EvidenceRef::new(EvidenceKind::ManualNote, "origin note")],
        tags: vec!["accepted".to_string()],
        related_files: vec!["src/lib.rs".to_string()],
        related_entities: Vec::new(),
        created_at: None,
        updated_at: None,
        supersedes: Vec::new(),
        contradicts: Vec::new(),
        status: MemoryStatus::Active,
    }
}

#[test]
fn signed_bundle_round_trips_from_machine_a_to_machine_b() {
    // --- Machine A: accept memory, export + sign a bundle to a file. ---
    let dir_a = tempfile::tempdir().unwrap();
    let machine_a = machine(&dir_a);
    let store_a = MemoryPersistence::open_project(&machine_a).unwrap();
    store_a
        .persist_memory_entry(&entry(
            "lesson-global",
            MemoryScope::GlobalUser,
            LessonCategory::DebuggingRecipe,
            "when a borrow error blocks an await, clone before the await",
        ))
        .unwrap();
    store_a
        .persist_memory_entry(&entry(
            "lesson-project",
            MemoryScope::Project,
            LessonCategory::ProjectConvention,
            "this repo pins workspace dependencies with exact versions",
        ))
        .unwrap();

    let key_a = KeyStore::open(&machine_a)
        .unwrap()
        .load_or_generate()
        .unwrap();
    let author_a = localmind_store::author_fingerprint(&key_a.verifying_key().to_bytes());
    let outcome = MemoryBundleExporter::open_project(&machine_a)
        .unwrap()
        .export(BundleScope::Both, &author_a)
        .unwrap();
    assert_eq!(outcome.bundle.entries.len(), 2);
    let signed = sign_bundle(&outcome.bundle, &key_a).unwrap();

    // Move the pack: serialize to a file, read it on the other machine.
    let pack_path = dir_a.path().join("knowledge.bundle.json");
    std::fs::write(&pack_path, signed.to_pretty_json().unwrap()).unwrap();

    // --- Machine B: a fresh, independent store + keypair. ---
    let dir_b = tempfile::tempdir().unwrap();
    let machine_b = machine(&dir_b);
    let received = SignedBundle::from_json(&std::fs::read_to_string(&pack_path).unwrap()).unwrap();

    // First import: B does not know A's key → Untrusted, but still review-gated.
    let importer = BundleImporter::new(&machine_b);
    let report = importer.import(&received, true).unwrap();
    assert_eq!(report.trust, ImportTrust::Untrusted);
    assert_eq!(report.added, 2);

    // Nothing is in active memory yet — import never promotes.
    let store_b = MemoryPersistence::open_project(&machine_b).unwrap();
    assert!(store_b.list_memory().unwrap().is_empty());

    // Review: accept + promote each imported candidate.
    let queue_b = ReviewQueue::open_project(&machine_b).unwrap();
    for id in ["lesson-global", "lesson-project"] {
        queue_b
            .decide(ReviewDecision {
                item_id: ReviewItemId::new(id),
                action: ReviewAction::Accept,
                reviewer: "machine-b-operator".to_string(),
                decided_at: None,
                note: None,
                replacement_summary: None,
                evidence: Vec::new(),
            })
            .unwrap();
        store_b.promote_review_item(&ReviewItemId::new(id)).unwrap();
    }

    // The lessons are retrievable on machine B.
    let hits = store_b.search("borrow error").unwrap();
    assert!(
        hits.iter()
            .any(|h| h.snippet.contains("clone before the await")),
        "the imported global lesson is retrievable on machine B"
    );

    // Scope-correct: the global lesson is in the global store, the project lesson
    // in the project store.
    let scopes: std::collections::BTreeMap<String, String> = store_b
        .list_memory()
        .unwrap()
        .into_iter()
        .map(|record| (record.memory_id.to_string(), record.scope))
        .collect();
    assert_eq!(
        scopes.get("lesson-global").map(String::as_str),
        Some("GlobalUser")
    );
    assert_eq!(
        scopes.get("lesson-project").map(String::as_str),
        Some("Project")
    );

    // Origin provenance survived: the promoted (project) memory's source session
    // carries the originating author's fingerprint from the import, and the
    // promotion is audited on machine B.
    let provenance = store_b
        .provenance(&MemoryEntryId::new("lesson-project"))
        .unwrap()
        .expect("the imported lesson has provenance");
    assert_eq!(
        provenance.source_session.as_deref(),
        Some(format!("import-{author_a}").as_str()),
        "the imported memory records its origin author in provenance"
    );
    assert!(
        store_b
            .audit_records()
            .unwrap()
            .iter()
            .any(|a| a.subject == "lesson-project"),
        "promotion of the imported lesson is audited on machine B"
    );

    // Trust upgrade: once B adds A's key to its trust list, the same pack verifies
    // as Trusted.
    KeyStore::open(&machine_b)
        .unwrap()
        .add_trusted(&key_a.verifying_key().to_bytes(), "machine-a")
        .unwrap();
    let trusted_report = BundleImporter::new(&machine_b)
        .import(&received, false)
        .unwrap();
    assert_eq!(trusted_report.trust, ImportTrust::Trusted);
}
