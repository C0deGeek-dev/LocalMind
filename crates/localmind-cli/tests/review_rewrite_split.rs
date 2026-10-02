//! `localmind review edit` and `review split` from the command line: a rewrite
//! or a split makes new items and leaves the original as history.

use assert_cmd::Command;
use localmind_core::{
    CandidateLesson, Confidence, LessonCategory, LessonId, ReviewItemId, ReviewState, SessionId,
    SuggestedAction,
};
use localmind_store::{MemoryPersistence, ReviewQueue};
use std::fs;

const SUMMARY: &str = "Run the migration and restart the workers after a schema change";

fn project() -> Result<tempfile::TempDir, Box<dyn std::error::Error>> {
    let project = tempfile::tempdir()?;
    fs::write(
        project.path().join(".localmind.toml"),
        "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n",
    )?;
    let queue = ReviewQueue::open_project(project.path())?;
    queue.enqueue_candidates(
        &SessionId::new("rewrite-split-test"),
        &[CandidateLesson::new(
            LessonId::new("lesson-a"),
            SUMMARY,
            LessonCategory::Process,
            Confidence::new(0.8)?,
            SuggestedAction::Split,
        )],
    )?;
    Ok(project)
}

fn review(
    project: &std::path::Path,
    args: &[&str],
) -> Result<std::process::Output, Box<dyn std::error::Error>> {
    Ok(Command::cargo_bin("localmind")?
        .arg("review")
        .args(args)
        .arg("--project")
        .arg(project)
        .arg("--reviewer")
        .arg("ada")
        .output()?)
}

#[test]
fn edit_rewrites_into_a_new_item_and_names_both() -> Result<(), Box<dyn std::error::Error>> {
    let project = project()?;

    // Correcting analysis the lesson does not carry is refused, and writes nothing.
    let refused = review(
        project.path(),
        &[
            "edit",
            "lesson-a",
            SUMMARY,
            "--cause",
            "the workers cached the old schema",
        ],
    )?;
    assert!(!refused.status.success());
    assert!(String::from_utf8(refused.stderr)?.contains("carries no hindsight"));

    let output = review(
        project.path(),
        &[
            "edit",
            "lesson-a",
            "Restart the workers after every schema migration",
        ],
    )?;
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout)?;
    assert!(
        stdout.contains("lesson-a -> Merged (rewritten as lesson-a-r1)"),
        "{stdout}"
    );
    assert!(
        stdout.contains("lesson-a-r1 -> Edited (untested"),
        "{stdout}"
    );

    let queue = ReviewQueue::open_project(project.path())?;
    let original = queue
        .get(&ReviewItemId::new("lesson-a"))?
        .ok_or("original")?;
    assert_eq!(original.candidate.summary(), SUMMARY);
    assert_eq!(original.reviewer.as_deref(), Some("ada"));
    let audit = MemoryPersistence::open_project(project.path())?.audit_records()?;
    for subject in ["lesson-a", "lesson-a-r1"] {
        assert!(audit
            .iter()
            .any(|record| record.kind == "ReviewDecisionRecorded" && record.subject == subject));
    }

    // Decided once: a second edit of the original is refused.
    let again = review(
        project.path(),
        &["edit", "lesson-a", "Something else entirely here"],
    )?;
    assert!(!again.status.success());
    assert!(String::from_utf8(again.stderr)?.contains("it is history"));
    Ok(())
}

#[test]
fn split_makes_pending_parts_and_promotes_nothing() -> Result<(), Box<dyn std::error::Error>> {
    let project = project()?;

    let one_part = review(
        project.path(),
        &["split", "lesson-a", "--part", "Run the migration"],
    )?;
    assert!(!one_part.status.success());
    assert!(String::from_utf8(one_part.stderr)?.contains("at least two parts"));

    let output = review(
        project.path(),
        &[
            "split",
            "lesson-a",
            "--part",
            "Run the migration after a schema change",
            "--part",
            "Restart the workers after a migration",
        ],
    )?;
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout)?;
    assert!(
        stdout.contains("lesson-a -> Merged (split into 2 pending items)"),
        "{stdout}"
    );

    let queue = ReviewQueue::open_project(project.path())?;
    let states: Vec<(String, ReviewState)> = queue
        .list()?
        .into_iter()
        .map(|item| (item.id.to_string(), item.state))
        .collect();
    assert!(states.contains(&("lesson-a-s1".to_string(), ReviewState::Pending)));
    assert!(states.contains(&("lesson-a-s2".to_string(), ReviewState::Pending)));
    assert!(MemoryPersistence::open_project(project.path())?
        .list_memory()?
        .is_empty());
    Ok(())
}
