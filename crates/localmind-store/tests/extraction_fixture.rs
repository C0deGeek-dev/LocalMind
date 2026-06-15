//! Acceptance bar for the deterministic extractor (see vision.md
//! "Extraction acceptance bar"): a realistic session fixture must produce at
//! least 3 reviewable candidates, every candidate must survive validation and
//! land in the review queue, and the heuristic families must each prove
//! themselves on the fixture rather than in isolation.

use localmind_core::{LessonCategory, SessionSource, SuggestedAction};
use localmind_store::{
    CloseoutProcessor, DeterministicExtractor, ProjectConfig, ReviewQueue, TranscriptImportFormat,
    TranscriptImporter,
};
use std::fs;

const FIXTURE: &str = include_str!("fixtures/coding-session.txt");

#[test]
fn fixture_session_produces_reviewable_candidates() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    fs::write(
        temp_dir.path().join(".localmind.toml"),
        "[learning]\nenabled = true\n",
    )?;
    let config = ProjectConfig::discover(temp_dir.path())?;
    let import = TranscriptImporter::import_text(
        &config,
        FIXTURE,
        SessionSource::GenericTranscript,
        TranscriptImportFormat::PlainText,
    )?;

    let report = CloseoutProcessor::closeout_project_session(
        temp_dir.path(),
        &import.session_id,
        &DeterministicExtractor,
    )?;

    // The acceptance bar: at least 3 candidates, all valid and enqueued.
    assert!(
        report.candidate_count >= 3,
        "expected at least 3 candidates, got {}",
        report.candidate_count
    );
    assert_eq!(report.candidate_count, report.enqueued_count);

    let queue = ReviewQueue::open_project(temp_dir.path())?;
    let items = queue.list()?;
    assert_eq!(items.len(), report.enqueued_count);

    // Each heuristic family contributes on this fixture.
    let categories: Vec<LessonCategory> = items
        .iter()
        .map(|item| item.candidate.category.clone())
        .collect();
    assert!(
        categories.contains(&LessonCategory::Process),
        "explicit Lesson: marker missing from {categories:?}"
    );
    assert!(
        categories.contains(&LessonCategory::DebuggingRecipe),
        "failure-to-resolution pair missing from {categories:?}"
    );
    assert!(
        categories.contains(&LessonCategory::CandidateSkill),
        "repeated-command workflow missing from {categories:?}"
    );
    assert!(
        categories.contains(&LessonCategory::UserPreference),
        "user correction missing from {categories:?}"
    );

    // The debugging recipe pairs the real failure with the real resolution.
    let recipe = items
        .iter()
        .find(|item| item.candidate.category == LessonCategory::DebuggingRecipe)
        .map(|item| item.candidate.summary().to_string())
        .unwrap_or_default();
    // The recipe pairs the descriptive failure (the assertion message naming the
    // real cause) with the real resolution. The terse `test … FAILED` status line
    // is intentionally not used — it is too short to read as a lesson.
    let recipe_lower = recipe.to_lowercase();
    assert!(
        recipe_lower.contains("assertion failed") || recipe_lower.contains("row_groups"),
        "recipe lacks the real failure: {recipe:?}"
    );
    assert!(
        recipe_lower.contains("passing") || recipe_lower.contains("fix"),
        "recipe lacks a resolution: {recipe:?}"
    );

    Ok(())
}

/// Memory-bound candidates are routed to concrete accepted-memory update
/// suggestions (supersede a correction, split a bundle) — review items, never
/// direct writes.
#[test]
fn memory_update_suggestions_are_routed_to_review() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    fs::write(
        temp_dir.path().join(".localmind.toml"),
        "[learning]\nenabled = true\n",
    )?;
    let transcript = "\
user: Lesson: don't use tabs; use spaces instead
user: Lesson: run clippy and run fmt and commit the change
";
    let config = ProjectConfig::discover(temp_dir.path())?;
    let import = TranscriptImporter::import_text(
        &config,
        transcript,
        SessionSource::GenericTranscript,
        TranscriptImportFormat::PlainText,
    )?;
    CloseoutProcessor::closeout_project_session(
        temp_dir.path(),
        &import.session_id,
        &DeterministicExtractor,
    )?;

    let queue = ReviewQueue::open_project(temp_dir.path())?;
    let actions: Vec<SuggestedAction> = queue
        .list()?
        .iter()
        .map(|item| item.candidate.suggested_action.clone())
        .collect();

    // A correction supersedes prior guidance; a bundle is split. Both stay in
    // the review queue rather than writing memory.
    assert!(
        actions.contains(&SuggestedAction::SupersedeExisting),
        "expected a supersede suggestion, got {actions:?}"
    );
    assert!(
        actions.contains(&SuggestedAction::Split),
        "expected a split suggestion, got {actions:?}"
    );
    Ok(())
}
