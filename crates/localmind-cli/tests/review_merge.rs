use assert_cmd::Command;
use localmind_core::{
    CandidateLesson, Confidence, LessonCategory, LessonId, ReviewAction, ReviewAnnotation,
    ReviewDecision, ReviewItemId, ReviewState, SessionId, SuggestedAction,
};
use localmind_store::{MemoryPersistence, ReviewQueue};
use std::fs;

#[test]
fn merge_command_accepts_the_computed_suggestion_and_persists_its_target(
) -> Result<(), Box<dyn std::error::Error>> {
    let project = tempfile::tempdir()?;
    fs::write(
        project.path().join(".localmind.toml"),
        "[learning]\nenabled = true\n",
    )?;
    let session = SessionId::new("review-merge-test");
    let target_id = ReviewItemId::new("merge-target");
    let source_id = ReviewItemId::new("merge-source");
    let queue = ReviewQueue::open_project(project.path())?;

    let target = CandidateLesson::new(
        LessonId::new(target_id.as_str()),
        "Run the integration suite after exporter changes",
        LessonCategory::TestingStrategy,
        Confidence::new(0.8)?,
        SuggestedAction::PromoteToMemory,
    );
    queue.enqueue_candidates(&session, &[target])?;
    queue.decide(ReviewDecision {
        item_id: target_id.clone(),
        action: ReviewAction::Accept,
        reviewer: "fixture".to_string(),
        decided_at: None,
        note: None,
        replacement_summary: None,
        evidence: Vec::new(),
    })?;

    let mut source = CandidateLesson::new(
        LessonId::new(source_id.as_str()),
        "After exporter changes, run the integration suite",
        LessonCategory::TestingStrategy,
        Confidence::new(0.8)?,
        SuggestedAction::MergeIntoExisting,
    );
    source.review_annotation = Some(ReviewAnnotation {
        score: Confidence::new(0.8)?,
        duplicate_of: Some(target_id.as_str().to_string()),
        conflict: false,
        notes: format!("near-duplicate of {target_id}"),
    });
    queue.enqueue_candidates(&session, &[source])?;
    drop(queue);

    let output = Command::cargo_bin("localmind")?
        .arg("review")
        .arg("merge")
        .arg(source_id.as_str())
        .arg("--project")
        .arg(project.path())
        .arg("--reviewer")
        .arg("test-reviewer")
        .output()?;

    assert!(output.status.success(), "{:?}", output);
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("Merged"));
    assert!(stdout.contains(target_id.as_str()));

    let queue = ReviewQueue::open_project(project.path())?;
    let merged = queue.get(&source_id)?.ok_or("missing merged item")?;
    assert_eq!(merged.state, ReviewState::Merged);
    assert_eq!(merged.reviewer_action.as_deref(), Some("merge"));
    assert_eq!(merged.merge_target.as_ref(), Some(&target_id));

    let persistence = MemoryPersistence::open_project(project.path())?;
    let audit = persistence
        .audit_records()?
        .into_iter()
        .find(|record| {
            record.kind == "ReviewDecisionRecorded" && record.subject == source_id.as_str()
        })
        .ok_or("missing merge audit record")?;
    assert!(audit.metadata_json.contains(r#""action":"merge""#));
    assert!(audit.metadata_json.contains(target_id.as_str()));
    Ok(())
}
