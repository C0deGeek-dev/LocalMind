#![allow(clippy::unwrap_used, clippy::expect_used)]

use assert_cmd::Command;
use localmind_core::{
    CandidateLesson, Confidence, EvidenceKind, EvidenceRef, LessonCategory, LessonId, MemoryEntry,
    MemoryEntryId, MemoryScope, MemoryStatus, ReviewItemId, ReviewState, SessionId,
    SuggestedAction,
};
use localmind_store::{MemoryPersistence, ReviewQueue};
use std::fs;

fn seed_memory(id: &str, body: &str) -> MemoryEntry {
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

/// Subject 04 round 2: `DeleteExisting`'s target must be durable — both on
/// the `review_items` row (`delete_existing_target`, schema v14) and in the
/// `ReviewDecisionRecorded` audit row `record_review_item_audit` writes
/// *before* the CLI's separate `delete_memory()` call — so a crash/error
/// between the two still leaves a durable record of which memory the
/// candidate was rejected in favor of removing.
#[test]
fn delete_existing_command_persists_its_target_in_the_row_and_the_audit_log(
) -> Result<(), Box<dyn std::error::Error>> {
    let project = tempfile::tempdir()?;
    fs::write(
        project.path().join(".localmind.toml"),
        "[learning]\nenabled = true\n",
    )?;
    let session = SessionId::new("review-delete-existing-test");
    let target_id = "delete-target";
    let source_id = ReviewItemId::new("delete-source");

    let persistence = MemoryPersistence::open_project(project.path())?;
    persistence.persist_memory_entry(&seed_memory(
        target_id,
        "this tooling note is stale and not worth keeping",
    ))?;

    let queue = ReviewQueue::open_project(project.path())?;
    let source = CandidateLesson::new(
        LessonId::new(source_id.as_str()),
        "do not follow that stale tooling note anymore",
        LessonCategory::Process,
        Confidence::new(0.8)?,
        SuggestedAction::PromoteToMemory,
    );
    queue.enqueue_candidates(&session, &[source])?;
    drop(queue);

    let output = Command::cargo_bin("localmind")?
        .arg("review")
        .arg("delete-existing")
        .arg(source_id.as_str())
        .arg(target_id)
        .arg("--project")
        .arg(project.path())
        .arg("--reviewer")
        .arg("test-reviewer")
        .output()?;

    assert!(output.status.success(), "{:?}", output);
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("Rejected"));
    assert!(stdout.contains(&format!("deleted {target_id}")));

    let queue = ReviewQueue::open_project(project.path())?;
    let decided = queue.get(&source_id)?.ok_or("missing decided item")?;
    assert_eq!(decided.state, ReviewState::Rejected);
    assert_eq!(
        decided
            .delete_existing_target
            .as_ref()
            .map(|id| id.as_str()),
        Some(target_id)
    );

    let audit = persistence
        .audit_records()?
        .into_iter()
        .find(|record| {
            record.kind == "ReviewDecisionRecorded" && record.subject == source_id.as_str()
        })
        .ok_or("missing delete-existing audit record")?;
    assert!(audit
        .metadata_json
        .contains(r#""action":"delete_existing""#));
    assert!(audit
        .metadata_json
        .contains(&format!("\"delete_existing_target\":\"{target_id}\"")));

    // The target was actually removed, and the candidate was never promoted.
    let memory_ids: Vec<String> = persistence
        .list_memory()?
        .iter()
        .map(|record| record.memory_id.as_str().to_string())
        .collect();
    assert!(!memory_ids.contains(&target_id.to_string()));
    assert!(!memory_ids.contains(&source_id.as_str().to_string()));
    Ok(())
}
