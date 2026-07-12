//! End-to-end sync-folder exchange between two store roots (two "machines") over
//! a shared folder. Offline (D008): real keys, real sealed bundles, real review
//! queue. Proves export→import routes to review, conflicts route to review under
//! a fresh id (no last-writer-wins), unknown signers are rejected fail-closed,
//! and re-runs are idempotent.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use localmind_core::{
    Confidence, LessonCategory, MemoryEntry, MemoryEntryId, MemoryScope, MemoryStatus, SyncMeta,
};
use localmind_store::{KeyStore, MemoryPersistence, ReviewQueue, SyncEngine};

/// A project-only store (hermetic: no home global store) with a fixed device
/// label. `LOCALMIND_GLOBAL_ROOT=@project` roots each project's keys under itself
/// so the two projects have distinct device identities.
fn project(label: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".localmind.toml"),
        format!("[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n[sync]\ndevice_label = \"{label}\"\n"),
    )
    .unwrap();
    dir
}

fn entry(id: &str, body: &str) -> MemoryEntry {
    MemoryEntry {
        id: MemoryEntryId::new(id),
        scope: MemoryScope::Project,
        body: body.to_string(),
        category: LessonCategory::CodePattern,
        confidence: Confidence::new(0.9).unwrap(),
        source_session: None,
        evidence: Vec::new(),
        tags: Vec::new(),
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

/// Mutually enroll two stores as each other's devices (each trusts the other's
/// signature and can encrypt to it).
fn enroll_each_other(a: &Path, b: &Path) {
    let store_a = KeyStore::open(a).unwrap();
    let store_b = KeyStore::open(b).unwrap();
    let card_a = store_a.own_device_card("A").unwrap();
    let card_b = store_b.own_device_card("B").unwrap();
    store_a
        .enroll_device(&card_b, &card_b.fingerprint())
        .unwrap();
    store_b
        .enroll_device(&card_a, &card_a.fingerprint())
        .unwrap();
}

fn seed(root: &Path, entry: &MemoryEntry) {
    MemoryPersistence::open_project(root)
        .unwrap()
        .persist_memory_entry(entry)
        .unwrap();
}

fn pending(root: &Path) -> Vec<localmind_store::ReviewQueueItem> {
    ReviewQueue::open_project(root).unwrap().list().unwrap()
}

#[test]
fn a_memory_written_on_one_device_arrives_as_a_review_candidate_on_the_other() {
    std::env::set_var("LOCALMIND_GLOBAL_ROOT", "@project");
    let a = project("A");
    let b = project("B");
    let folder = tempfile::tempdir().unwrap();
    enroll_each_other(a.path(), b.path());

    seed(a.path(), &entry("m1", "prefer ripgrep over grep"));

    // A exports; the folder now holds A's encrypted bundle only.
    let a_report = SyncEngine::open(a.path()).run(folder.path()).unwrap();
    assert!(a_report.exported);
    assert_eq!(a_report.exported_ops, 1);

    // The folder artefact is ciphertext — the memory body is not in it.
    let files: Vec<_> = std::fs::read_dir(folder.path())
        .unwrap()
        .flatten()
        .map(|e| std::fs::read_to_string(e.path()).unwrap())
        .collect();
    assert!(files.iter().all(|text| !text.contains("prefer ripgrep")));

    // B imports A's memory as a review candidate (never straight to memory).
    let b_report = SyncEngine::open(b.path()).run(folder.path()).unwrap();
    assert_eq!(b_report.peers_scanned, 1);
    assert_eq!(b_report.imported_candidates, 1);
    let items = pending(b.path());
    assert_eq!(items.len(), 1);
    assert!(items[0].candidate.summary().contains("ripgrep"));

    // Idempotent: B re-running imports nothing new.
    let again = SyncEngine::open(b.path()).run(folder.path()).unwrap();
    assert_eq!(again.imported_candidates, 0);
    assert_eq!(pending(b.path()).len(), 1);
}

#[test]
fn a_diverging_edit_routes_to_review_as_a_conflict_and_never_overwrites() {
    std::env::set_var("LOCALMIND_GLOBAL_ROOT", "@project");
    let a = project("A");
    let b = project("B");
    let folder = tempfile::tempdir().unwrap();
    enroll_each_other(a.path(), b.path());

    // The same memory id diverges: A and B each have their own body for `m1`.
    seed(a.path(), &entry("m1", "A's version of the lesson"));
    seed(b.path(), &entry("m1", "B's version of the lesson"));

    SyncEngine::open(a.path()).run(folder.path()).unwrap();
    let b_report = SyncEngine::open(b.path()).run(folder.path()).unwrap();

    // The divergence is a conflict routed to review, not applied.
    assert_eq!(b_report.conflicts, 1);
    assert_eq!(b_report.imported_candidates, 1);
    let items = pending(b.path());
    assert_eq!(items.len(), 1);
    // The candidate has a conflict id, so promotion can never overwrite B's m1.
    assert!(items[0]
        .candidate
        .id
        .as_str()
        .starts_with("sync-conflict-m1-"));

    // B's local memory is untouched (no last-writer-wins).
    let persistence = MemoryPersistence::open_project(b.path()).unwrap();
    let provenance = persistence
        .provenance(&MemoryEntryId::new("m1"))
        .unwrap()
        .expect("B's original memory still present");
    assert_eq!(provenance.memory_id.as_str(), "m1");
}

#[test]
fn a_bundle_from_an_unenrolled_signer_is_rejected_fail_closed() {
    std::env::set_var("LOCALMIND_GLOBAL_ROOT", "@project");
    let a = project("A");
    let b = project("B");
    let folder = tempfile::tempdir().unwrap();

    // A one-directional enrollment: A encrypts to B, but B does NOT enroll A, so
    // B does not trust A's signature.
    let store_a = KeyStore::open(a.path()).unwrap();
    let store_b = KeyStore::open(b.path()).unwrap();
    let card_b = store_b.own_device_card("B").unwrap();
    store_a
        .enroll_device(&card_b, &card_b.fingerprint())
        .unwrap();

    seed(a.path(), &entry("m1", "unenrolled sender lesson"));
    SyncEngine::open(a.path()).run(folder.path()).unwrap();

    // B can decrypt (it is a recipient) but rejects the unknown signer.
    let b_report = SyncEngine::open(b.path()).run(folder.path()).unwrap();
    assert_eq!(b_report.rejected_unknown_signer, 1);
    assert_eq!(b_report.imported_candidates, 0);
    assert!(pending(b.path()).is_empty());
}
