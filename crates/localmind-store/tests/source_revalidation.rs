//! Opt-in source re-validation: the sample → check → flag pipeline is offline-
//! testable with a fixture verdict source (no model, no network) — the acceptance
//! bar (D008). A "no longer true" verdict routes to the existing review gate and
//! never deletes (D001); `Unknown` never flags; the sample is bounded.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use localmind_core::{
    Confidence, EvidenceKind, EvidenceRef, LessonCategory, MemoryEntry, MemoryEntryId, MemoryScope,
    MemoryStatus, SessionId,
};
use localmind_store::{MemoryPersistence, RevalidationConfig, RevalidationVerdict, VerdictSource};

/// A deterministic, offline verdict source: a body containing `DEAD` is judged no
/// longer true, one containing `OK` still current, anything else unknown.
struct FixtureSource;

impl VerdictSource for FixtureSource {
    fn judge(&self, body: &str) -> RevalidationVerdict {
        if body.contains("DEAD") {
            RevalidationVerdict::NoLongerTrue
        } else if body.contains("OK") {
            RevalidationVerdict::StillCurrent
        } else {
            RevalidationVerdict::Unknown
        }
    }
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

fn entry(id: &str, body: &str) -> MemoryEntry {
    MemoryEntry {
        id: MemoryEntryId::new(id),
        scope: MemoryScope::Project,
        body: body.to_string(),
        category: LessonCategory::DebuggingRecipe,
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

#[test]
fn only_version_sensitive_lessons_are_sampled() {
    let project = project_only();
    let persistence = MemoryPersistence::open_project(project.path()).unwrap();
    // Two version-sensitive lessons and one evergreen (never sampled).
    persistence
        .persist_memory_entry(&entry("dead", "the --foo flag was deprecated DEAD"))
        .unwrap();
    persistence
        .persist_memory_entry(&entry("live", "use MSRV 1.82 — OK"))
        .unwrap();
    persistence
        .persist_memory_entry(&entry("evergreen", "prefer guard clauses over nesting"))
        .unwrap();

    let report = persistence
        .revalidate_sources(&RevalidationConfig::default(), &FixtureSource, true)
        .unwrap();
    assert_eq!(report.sampled, 2, "only the version-sensitive lessons");
    assert_eq!(report.no_longer_true, 1);
    assert_eq!(report.still_current, 1);
    assert_eq!(report.flagged, vec!["dead".to_string()]);
}

#[test]
fn dry_run_writes_nothing_then_apply_routes_to_review() {
    let project = project_only();
    let persistence = MemoryPersistence::open_project(project.path()).unwrap();
    persistence
        .persist_memory_entry(&entry("dead", "the deprecated DEAD api"))
        .unwrap();

    // Dry run names the candidate but writes nothing.
    let dry = persistence
        .revalidate_sources(&RevalidationConfig::default(), &FixtureSource, true)
        .unwrap();
    assert_eq!(dry.flagged, vec!["dead".to_string()]);
    assert!(dry.dry_run);
    assert!(
        persistence.list_stale_candidates().unwrap().is_empty(),
        "a dry run must not flag anything"
    );

    // Applying routes it to review (sets stale_candidate); never deletes.
    let applied = persistence
        .revalidate_sources(&RevalidationConfig::default(), &FixtureSource, false)
        .unwrap();
    assert_eq!(applied.flagged, vec!["dead".to_string()]);
    let flagged = persistence.list_stale_candidates().unwrap();
    assert_eq!(flagged.len(), 1);
    assert_eq!(flagged[0].as_str(), "dead");
    assert_eq!(
        persistence.list_memory().unwrap().len(),
        1,
        "re-validation never deletes"
    );
}

#[test]
fn an_unknown_verdict_never_flags() {
    let project = project_only();
    let persistence = MemoryPersistence::open_project(project.path()).unwrap();
    // Version-sensitive, but the source returns neither token -> Unknown.
    persistence
        .persist_memory_entry(&entry("ambig", "the deprecated thing, who knows"))
        .unwrap();
    let report = persistence
        .revalidate_sources(&RevalidationConfig::default(), &FixtureSource, false)
        .unwrap();
    assert_eq!(report.sampled, 1);
    assert_eq!(report.unknown, 1);
    assert!(report.flagged.is_empty());
    assert!(persistence.list_stale_candidates().unwrap().is_empty());
}

#[test]
fn the_sample_size_bounds_egress_and_churn() {
    let project = project_only();
    let persistence = MemoryPersistence::open_project(project.path()).unwrap();
    for i in 0..5 {
        persistence
            .persist_memory_entry(&entry(
                &format!("dead-{i}"),
                &format!("deprecated DEAD api note {i}"),
            ))
            .unwrap();
    }
    let config = RevalidationConfig { sample_size: 2 };
    let report = persistence
        .revalidate_sources(&config, &FixtureSource, true)
        .unwrap();
    assert_eq!(report.sampled, 2, "the sample is bounded by sample_size");
}

#[test]
fn no_chat_endpoint_degrades_to_none() {
    let project = project_only();
    let persistence = MemoryPersistence::open_project(project.path()).unwrap();
    persistence
        .persist_memory_entry(&entry("dead", "deprecated DEAD api"))
        .unwrap();
    // No [inference] config -> no chat endpoint -> the live pass is unavailable,
    // reported as None rather than an error (best-effort, opt-in).
    let outcome = persistence
        .revalidate_with_model(&RevalidationConfig::default(), true)
        .unwrap();
    assert!(outcome.is_none(), "no model configured -> not available");
}
