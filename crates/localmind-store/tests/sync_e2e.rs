//! Comprehensive offline validation of the whole sync loop (D008 bar): two store
//! roots + a shared folder simulate PC↔laptop. Covers convergence in both
//! directions, machine-local exclusion, ciphertext-only transport, revocation,
//! and tolerance of a corrupt bundle — the properties a real two-machine run
//! would check.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use localmind_core::{
    Confidence, LessonCategory, MemoryEntry, MemoryEntryId, MemoryScope, MemoryStatus,
    SyncDisposition, SyncMeta,
};
use localmind_store::{KeyStore, MemoryPersistence, ReviewQueue, SyncEngine};

fn project(label: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".localmind.toml"),
        format!("[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n[sync]\ndevice_label = \"{label}\"\n"),
    )
    .unwrap();
    dir
}

fn entry(id: &str, body: &str, disposition: Option<SyncDisposition>) -> MemoryEntry {
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
        sync_meta: SyncMeta {
            disposition,
            origin_env: None,
        },
    }
}

fn enroll_each_other(a: &Path, b: &Path) {
    let (sa, sb) = (KeyStore::open(a).unwrap(), KeyStore::open(b).unwrap());
    let (ca, cb) = (
        sa.own_device_card("A").unwrap(),
        sb.own_device_card("B").unwrap(),
    );
    sa.enroll_device(&cb, &cb.fingerprint()).unwrap();
    sb.enroll_device(&ca, &ca.fingerprint()).unwrap();
}

fn seed(root: &Path, entry: &MemoryEntry) {
    MemoryPersistence::open_project(root)
        .unwrap()
        .persist_memory_entry(entry)
        .unwrap();
}

fn pending_count(root: &Path) -> usize {
    ReviewQueue::open_project(root)
        .unwrap()
        .list()
        .unwrap()
        .len()
}

#[test]
fn machine_local_memory_never_leaves_the_device_but_syncable_memory_converges() {
    std::env::set_var("LOCALMIND_GLOBAL_ROOT", "@project");
    let a = project("A");
    let b = project("B");
    let folder = tempfile::tempdir().unwrap();
    enroll_each_other(a.path(), b.path());

    // A has a portable lesson and a machine-local one; B has its own portable one.
    seed(a.path(), &entry("a-portable", "use ripgrep", None));
    seed(
        a.path(),
        &entry(
            "a-local",
            "run with -ngl 0 on this box",
            Some(SyncDisposition::MachineLocal),
        ),
    );
    seed(b.path(), &entry("b-portable", "prefer guard clauses", None));

    // Only the portable memory is exported from A (machine-local is excluded).
    let a_report = SyncEngine::open(a.path()).run(folder.path()).unwrap();
    assert_eq!(
        a_report.exported_ops, 1,
        "machine-local memory is not exported"
    );

    // Bidirectional convergence: B receives A's portable lesson, A receives B's.
    let b_report = SyncEngine::open(b.path()).run(folder.path()).unwrap();
    assert_eq!(b_report.imported_candidates, 1);
    let a_second = SyncEngine::open(a.path()).run(folder.path()).unwrap();
    assert_eq!(a_second.imported_candidates, 1);

    // B never sees A's machine-local lesson.
    let b_items = ReviewQueue::open_project(b.path()).unwrap().list().unwrap();
    assert!(b_items
        .iter()
        .all(|item| !item.candidate.summary().contains("-ngl 0")));

    // The folder holds only ciphertext — neither body appears in plaintext.
    for file in std::fs::read_dir(folder.path()).unwrap().flatten() {
        let text = std::fs::read_to_string(file.path()).unwrap();
        assert!(!text.contains("ripgrep"));
        assert!(!text.contains("guard clauses"));
        assert!(!text.contains("-ngl 0"));
    }
}

#[test]
fn a_revoked_device_is_dropped_as_a_recipient_and_as_a_trusted_signer() {
    std::env::set_var("LOCALMIND_GLOBAL_ROOT", "@project");
    let a = project("A");
    let b = project("B");
    let folder = tempfile::tempdir().unwrap();
    enroll_each_other(a.path(), b.path());

    seed(a.path(), &entry("a1", "a lesson from A", None));
    seed(b.path(), &entry("b1", "a lesson from B", None));
    // Both publish; A imports B's bundle fine while enrolled.
    SyncEngine::open(a.path()).run(folder.path()).unwrap();
    let before = SyncEngine::open(b.path()).run(folder.path()).unwrap();
    assert_eq!(before.rejected_unknown_signer, 0);
    let a_before = SyncEngine::open(a.path()).run(folder.path()).unwrap();
    assert_eq!(a_before.peers_scanned, 1);

    // A revokes B (its only enrolled device).
    let card_b = KeyStore::open(b.path())
        .unwrap()
        .own_device_card("B")
        .unwrap();
    assert!(KeyStore::open(a.path())
        .unwrap()
        .revoke_device(&card_b.fingerprint())
        .unwrap());

    // A can no longer encrypt to anyone (fail-closed: nothing exported) and B's
    // still-present bundle is now an untrusted signer to A.
    let a_after = SyncEngine::open(a.path()).run(folder.path()).unwrap();
    assert!(
        !a_after.exported,
        "no enrolled recipient ⇒ nothing exported"
    );
    assert_eq!(
        a_after.rejected_unknown_signer, 1,
        "B's signature is no longer trusted"
    );
    assert_eq!(a_after.imported_candidates, 0);
}

#[test]
fn a_corrupt_or_foreign_folder_file_is_skipped_not_fatal() {
    std::env::set_var("LOCALMIND_GLOBAL_ROOT", "@project");
    let a = project("A");
    let b = project("B");
    let folder = tempfile::tempdir().unwrap();
    enroll_each_other(a.path(), b.path());

    seed(a.path(), &entry("a1", "a good lesson", None));
    SyncEngine::open(a.path()).run(folder.path()).unwrap();

    // A truncated/garbage bundle and a valid-JSON-but-wrong-shape one.
    std::fs::write(folder.path().join("garbage.sync"), "{ not valid json").unwrap();
    std::fs::write(
        folder.path().join("wrongshape.sync"),
        "{\"hello\":\"world\"}",
    )
    .unwrap();

    // B still imports A's real bundle and skips the two bad files without error.
    let b_report = SyncEngine::open(b.path()).run(folder.path()).unwrap();
    assert_eq!(b_report.imported_candidates, 1);
    assert!(b_report.skipped_files >= 2, "the two bad files are skipped");
    assert_eq!(pending_count(b.path()), 1);
}
