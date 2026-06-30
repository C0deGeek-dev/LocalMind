//! The deterministic freshness pass flags accepted memory for review by age,
//! never-retrieved-after-grace, and version-sensitivity — across the project and
//! global stores — with a per-run cap, a dry-run that writes nothing, and no
//! false-positive on an evergreen, healthy lesson. It only ever routes to review
//! (sets `stale_candidate`); it never deletes.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use localmind_core::{
    Confidence, EvidenceKind, EvidenceRef, LessonCategory, MemoryEntry, MemoryEntryId, MemoryScope,
    MemoryStatus, SessionId,
};
use localmind_store::{FreshnessReason, FreshnessScope, FreshnessThresholds, MemoryPersistence};
use time::{Duration, OffsetDateTime};

fn project_with_global(global_root: &Path) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".localmind.toml"),
        format!(
            "[learning]\nenabled = true\nallowed_scopes = [\"project\", \"global_user\"]\nglobal_memory_root = '{}'\n",
            global_root.display()
        ),
    )
    .unwrap();
    dir
}

fn project_only() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".localmind.toml"),
        "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n",
    )
    .unwrap();
    dir
}

fn entry(id: &str, body: &str, scope: MemoryScope) -> MemoryEntry {
    entry_cat(id, body, scope, LessonCategory::DebuggingRecipe)
}

fn entry_cat(id: &str, body: &str, scope: MemoryScope, category: LessonCategory) -> MemoryEntry {
    MemoryEntry {
        id: MemoryEntryId::new(id),
        scope,
        body: body.to_string(),
        category,
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
    }
}

/// Memories are persisted "now"; the pass is run with a `now` shifted into the
/// future to simulate aging deterministically, so no row backdating is needed.
fn future(days: i64) -> OffsetDateTime {
    OffsetDateTime::now_utc() + Duration::days(days)
}

#[test]
fn each_heuristic_flags_the_right_memory_and_evergreen_is_safe() {
    let project = project_only();
    let persistence = MemoryPersistence::open_project(project.path()).unwrap();
    persistence
        .persist_memory_entry(&entry(
            "versioned",
            "the --foo flag was deprecated; use MSRV 1.82",
            MemoryScope::Project,
        ))
        .unwrap();
    persistence
        .persist_memory_entry(&entry(
            "unused",
            "an evergreen lesson nobody has needed yet",
            MemoryScope::Project,
        ))
        .unwrap();
    persistence
        .persist_memory_entry(&entry(
            "healthy",
            "an evergreen lesson that is actively used",
            MemoryScope::Project,
        ))
        .unwrap();
    // "healthy" is retrieved, so it is not dead weight.
    persistence
        .record_memory_usage(&[MemoryEntryId::new("healthy")])
        .unwrap();

    // 200 days on: past the version floor (180) and the unused grace (90), but
    // under the max age (365). version-sensitive + unused flag; the used evergreen
    // does not.
    let report = persistence
        .freshness_pass_at(
            &FreshnessThresholds::default(),
            FreshnessScope::Both,
            true,
            future(200),
        )
        .unwrap();
    assert_eq!(report.scanned, 3);
    let reason = |id: &str| {
        report
            .flagged
            .iter()
            .find(|f| f.memory_id == id)
            .map(|f| f.reason)
    };
    assert_eq!(reason("versioned"), Some(FreshnessReason::VersionSensitive));
    assert_eq!(reason("unused"), Some(FreshnessReason::Unused));
    assert_eq!(reason("healthy"), None, "a used evergreen lesson is safe");
}

#[test]
fn a_fresh_store_flags_nothing() {
    let project = project_only();
    let persistence = MemoryPersistence::open_project(project.path()).unwrap();
    persistence
        .persist_memory_entry(&entry("v", "deprecated flag in v1.2", MemoryScope::Project))
        .unwrap();
    // 10 days on: under every floor.
    let report = persistence
        .freshness_pass_at(
            &FreshnessThresholds::default(),
            FreshnessScope::Both,
            true,
            future(10),
        )
        .unwrap();
    assert!(
        report.flagged.is_empty(),
        "nothing is stale yet: {report:?}"
    );
    assert_eq!(report.total_candidates(), 0);
}

#[test]
fn dry_run_writes_nothing_then_apply_flags_for_review() {
    let project = project_only();
    let persistence = MemoryPersistence::open_project(project.path()).unwrap();
    persistence
        .persist_memory_entry(&entry(
            "old",
            "an old evergreen lesson",
            MemoryScope::Project,
        ))
        .unwrap();

    // Dry run reports the candidate but writes nothing.
    let dry = persistence
        .freshness_pass_at(
            &FreshnessThresholds::default(),
            FreshnessScope::Both,
            true,
            future(120),
        )
        .unwrap();
    assert_eq!(dry.flagged.len(), 1);
    assert!(dry.dry_run);
    assert!(
        persistence.list_stale_candidates().unwrap().is_empty(),
        "a dry run must not flag anything"
    );

    // Applying flags it for review (sets stale_candidate); the memory stays.
    let applied = persistence
        .freshness_pass_at(
            &FreshnessThresholds::default(),
            FreshnessScope::Both,
            false,
            future(120),
        )
        .unwrap();
    assert_eq!(applied.flagged.len(), 1);
    let flagged = persistence.list_stale_candidates().unwrap();
    assert_eq!(flagged.len(), 1);
    assert_eq!(flagged[0].as_str(), "old");
    assert_eq!(
        persistence.list_memory().unwrap().len(),
        1,
        "flagging never deletes the memory"
    );

    // Re-running is idempotent: the already-flagged memory is not re-scanned.
    let again = persistence
        .freshness_pass_at(
            &FreshnessThresholds::default(),
            FreshnessScope::Both,
            false,
            future(120),
        )
        .unwrap();
    assert_eq!(again.scanned, 0, "an already-flagged memory is skipped");
    assert!(again.flagged.is_empty());
}

#[test]
fn the_per_run_cap_bounds_the_flags_and_keeps_the_most_actionable() {
    let project = project_only();
    let persistence = MemoryPersistence::open_project(project.path()).unwrap();
    // Three version-sensitive (most-actionable) and three plain-unused lessons.
    for i in 0..3 {
        persistence
            .persist_memory_entry(&entry(
                &format!("ver-{i}"),
                &format!("deprecated api note {i}"),
                MemoryScope::Project,
            ))
            .unwrap();
        persistence
            .persist_memory_entry(&entry(
                &format!("plain-{i}"),
                &format!("an evergreen lesson {i}"),
                MemoryScope::Project,
            ))
            .unwrap();
    }
    let thresholds = FreshnessThresholds {
        max_flags: 2,
        ..FreshnessThresholds::default()
    };
    let report = persistence
        .freshness_pass_at(&thresholds, FreshnessScope::Both, true, future(200))
        .unwrap();
    assert!(report.capped, "more candidates than the cap");
    assert_eq!(report.flagged.len(), 2, "bounded by the cap");
    assert!(
        report
            .flagged
            .iter()
            .all(|f| f.reason == FreshnessReason::VersionSensitive),
        "the most-actionable reasons survive the cap: {:?}",
        report.flagged
    );
    // Counts reflect all matches before the cap.
    assert_eq!(report.version_sensitive, 3);
    assert_eq!(report.unused, 3);
}

#[test]
fn the_pass_reaches_the_global_store() {
    let global = tempfile::tempdir().unwrap();
    let global_root = global.path().join("memory");
    let project = project_with_global(&global_root);
    let persistence = MemoryPersistence::open_project(project.path()).unwrap();
    persistence
        .persist_memory_entry(&entry(
            "g-old",
            "an old global evergreen lesson",
            MemoryScope::GlobalUser,
        ))
        .unwrap();

    let report = persistence
        .freshness_pass_at(
            &FreshnessThresholds::default(),
            FreshnessScope::Both,
            false,
            future(120),
        )
        .unwrap();
    assert_eq!(report.scanned, 1);
    assert_eq!(report.flagged.len(), 1);
    assert_eq!(report.flagged[0].memory_id, "g-old");
    // The global memory is now flagged for review; the (cross-store) stale list
    // surfaces it, proving the flag landed in the global store.
    let flagged = persistence.list_stale_candidates().unwrap();
    assert_eq!(flagged.len(), 1);
    assert_eq!(flagged[0].as_str(), "g-old");
}

#[test]
fn low_quality_lessons_are_flagged_across_stores_but_good_ones_are_not() {
    // Retroactive cleanup: tooling-noise / over-fit lessons that predate the write
    // gate are flagged for review (project + global) regardless of age, while
    // healthy general lessons are left alone. Reuses the write gate's classifier.
    let global = tempfile::tempdir().unwrap();
    let global_root = global.path().join("memory");
    let project = project_with_global(&global_root);
    let persistence = MemoryPersistence::open_project(project.path()).unwrap();

    // Bad, recently created: a tooling-noise lesson (project) and an over-fit one
    // (global). Both must flag even though no time has passed.
    persistence
        .persist_memory_entry(&entry_cat(
            "p-tooling",
            "Use ./gradlew instead of gradlew on Windows if the wrapper is in the current directory.",
            MemoryScope::Project,
            LessonCategory::Process,
        ))
        .unwrap();
    persistence
        .persist_memory_entry(&entry_cat(
            "g-overfit",
            "Avoid `zip(words, letters)` when emitting an initial state before any letter arrives.",
            MemoryScope::GlobalUser,
            LessonCategory::CodePattern,
        ))
        .unwrap();
    // Good general lessons (project + global) must not flag.
    persistence
        .persist_memory_entry(&entry_cat(
            "p-good",
            "always acquire locks in a consistent global order to avoid deadlocks",
            MemoryScope::Project,
            LessonCategory::DebuggingRecipe,
        ))
        .unwrap();
    persistence
        .persist_memory_entry(&entry_cat(
            "g-good",
            "ensure function signatures match the test expectations before implementing",
            MemoryScope::GlobalUser,
            LessonCategory::TestingStrategy,
        ))
        .unwrap();

    // `now` = creation time, so age-based reasons cannot fire; only low-quality can.
    let report = persistence
        .freshness_pass_at(
            &FreshnessThresholds::default(),
            FreshnessScope::Both,
            false,
            OffsetDateTime::now_utc(),
        )
        .unwrap();
    let reason = |id: &str| {
        report
            .flagged
            .iter()
            .find(|f| f.memory_id == id)
            .map(|f| f.reason)
    };
    assert_eq!(reason("p-tooling"), Some(FreshnessReason::LowQuality));
    assert_eq!(reason("g-overfit"), Some(FreshnessReason::LowQuality));
    assert_eq!(
        reason("p-good"),
        None,
        "a good project lesson is not flagged"
    );
    assert_eq!(
        reason("g-good"),
        None,
        "a good global lesson is not flagged"
    );
    assert_eq!(report.low_quality, 2);

    // Routed to review in both stores; nothing deleted.
    let flagged = persistence.list_stale_candidates().unwrap();
    assert!(flagged.iter().any(|i| i.as_str() == "p-tooling"));
    assert!(flagged.iter().any(|i| i.as_str() == "g-overfit"));
    assert_eq!(
        persistence.list_memory().unwrap().len(),
        4,
        "flagging never deletes a memory"
    );
}

#[test]
fn the_low_quality_detector_is_word_boundary_safe_and_dry_run_writes_nothing() {
    // The substring-vs-word-boundary bug: "function" / "uncertain" must not read as
    // a tooling/over-fit marker, so a good lesson mentioning them is not flagged;
    // and a dry run reports without writing.
    let project = project_only();
    let persistence = MemoryPersistence::open_project(project.path()).unwrap();
    persistence
        .persist_memory_entry(&entry_cat(
            "good",
            "keep each function small and document any uncertain assumption clearly",
            MemoryScope::Project,
            LessonCategory::CodePattern,
        ))
        .unwrap();
    persistence
        .persist_memory_entry(&entry_cat(
            "bad",
            "Gradle build issues occurred due to path formatting in Windows shell commands.",
            MemoryScope::Project,
            LessonCategory::Process,
        ))
        .unwrap();

    let dry = persistence
        .freshness_pass_at(
            &FreshnessThresholds::default(),
            FreshnessScope::Both,
            true,
            OffsetDateTime::now_utc(),
        )
        .unwrap();
    assert_eq!(
        dry.low_quality, 1,
        "only the real tooling-noise lesson flags"
    );
    assert_eq!(dry.flagged.len(), 1);
    assert_eq!(dry.flagged[0].memory_id, "bad");
    assert!(dry.dry_run);
    assert!(
        persistence.list_stale_candidates().unwrap().is_empty(),
        "a dry run must not flag anything"
    );
}

#[test]
fn scope_narrows_the_pass_to_one_store() {
    let global = tempfile::tempdir().unwrap();
    let global_root = global.path().join("memory");
    let project = project_with_global(&global_root);
    let persistence = MemoryPersistence::open_project(project.path()).unwrap();
    persistence
        .persist_memory_entry(&entry(
            "p-old",
            "an old project lesson",
            MemoryScope::Project,
        ))
        .unwrap();
    persistence
        .persist_memory_entry(&entry(
            "g-old",
            "an old global lesson",
            MemoryScope::GlobalUser,
        ))
        .unwrap();

    // Project-only scope sees just the project lesson.
    let project_only = persistence
        .freshness_pass_at(
            &FreshnessThresholds::default(),
            FreshnessScope::Project,
            true,
            future(120),
        )
        .unwrap();
    assert_eq!(project_only.scanned, 1);
    assert_eq!(project_only.flagged.len(), 1);
    assert_eq!(project_only.flagged[0].memory_id, "p-old");

    // Global-only scope sees just the global lesson.
    let global_only = persistence
        .freshness_pass_at(
            &FreshnessThresholds::default(),
            FreshnessScope::Global,
            true,
            future(120),
        )
        .unwrap();
    assert_eq!(global_only.scanned, 1);
    assert_eq!(global_only.flagged[0].memory_id, "g-old");

    // Both sees the pair.
    let both = persistence
        .freshness_pass_at(
            &FreshnessThresholds::default(),
            FreshnessScope::Both,
            true,
            future(120),
        )
        .unwrap();
    assert_eq!(both.scanned, 2);
}
