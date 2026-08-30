//! Write-path sync scoping: a memory that syncs is stamped with the origin
//! machine at write time; a machine-local memory is not. Best-effort stamping
//! never blocks a write. Offline, project-only (hermetic — no global store).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use localmind_core::{
    Confidence, EvidenceKind, EvidenceRef, LessonCategory, MemoryEntry, MemoryEntryId, MemoryScope,
    MemoryStatus, SessionId, SyncDisposition, SyncMeta,
};
use localmind_store::MemoryPersistence;

/// A project-only store (no global store opened, so the test is hermetic) whose
/// sync device label is fixed so the stamped fingerprint is deterministic.
fn project(device_label: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".localmind.toml"),
        format!(
            "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n[sync]\ndevice_label = \"{device_label}\"\n"
        ),
    )
    .unwrap();
    dir
}

fn project_entry(id: &str, disposition: Option<SyncDisposition>) -> MemoryEntry {
    MemoryEntry {
        id: MemoryEntryId::new(id),
        scope: MemoryScope::Project,
        body: "prefer a guard clause".to_string(),
        category: LessonCategory::CodePattern,
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
        sync_meta: SyncMeta {
            disposition,
            origin_env: None,
        },
    }
}

#[test]
fn a_syncing_memory_is_stamped_with_the_origin_machine() {
    let project = project("TestBox");
    let persistence = MemoryPersistence::open_project(project.path()).unwrap();

    // A Project memory syncs by default, so persisting it stamps the origin env.
    let path = persistence
        .persist_memory_entry(&project_entry("m1", None))
        .unwrap();
    let written = std::fs::read_to_string(&path).unwrap();

    assert!(
        written.contains(&format!("origin_os: {}", std::env::consts::OS)),
        "syncing memory must be stamped with the origin OS:\n{written}"
    );
    assert!(written.contains("origin_arch: "));
    assert!(
        written.contains("origin_device: TestBox"),
        "the configured device label must be stamped:\n{written}"
    );
    // The default disposition is not written as an override; it is derived.
    assert!(!written.contains("\nsync: "));

    // Re-parsing recovers the fingerprint and the syncing disposition.
    let parsed = localmind_store::MarkdownMemoryFormat::parse(&written).unwrap();
    assert_eq!(parsed.effective_disposition(), SyncDisposition::Sync);
    let env = parsed.sync_meta.origin_env.expect("origin env stamped");
    assert_eq!(env.device_label, "TestBox");
}

#[test]
fn a_machine_local_memory_is_never_stamped() {
    let project = project("TestBox");
    let persistence = MemoryPersistence::open_project(project.path()).unwrap();

    // An explicit machine-local override means the memory never leaves this
    // device, so no origin machine is recorded.
    let path = persistence
        .persist_memory_entry(&project_entry("m2", Some(SyncDisposition::MachineLocal)))
        .unwrap();
    let written = std::fs::read_to_string(&path).unwrap();

    assert!(
        written.contains("sync: machine_local"),
        "the machine-local override must be written:\n{written}"
    );
    assert!(
        !written.contains("origin_os:"),
        "machine-local memory must not be stamped with an origin machine:\n{written}"
    );

    let parsed = localmind_store::MarkdownMemoryFormat::parse(&written).unwrap();
    assert!(!parsed.syncs());
    assert!(parsed.sync_meta.origin_env.is_none());
}
