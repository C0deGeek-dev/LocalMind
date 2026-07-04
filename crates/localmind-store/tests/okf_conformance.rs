//! Conformance of the OKF adapter against a Google-`knowledge-catalog`-shaped
//! sample bundle.
//!
//! The fixtures under `tests/fixtures/okf/ga4-shaped/` are original documents
//! authored to the OKF v0.1 spec (see their `PROVENANCE.txt`); they model the
//! structure of the public GA4 sample bundle without copying it. They exercise
//! the shapes a real bundle uses: `type`-only concepts, inline-flow tags, quoted
//! scalars, `+00:00` and `Z` timestamps, nested directories, no-front-matter
//! `index.md` navigation, and a markdown-link concept graph.
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;

use localmind_store::{ImportTrust, MemoryPersistence, OkfFormat, OkfImporter, ReviewQueue};

use localmind_core::LessonCategory;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/okf/ga4-shaped")
}

fn project(dir: &tempfile::TempDir) -> PathBuf {
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

#[test]
fn google_shaped_bundle_imports_as_untrusted_review_candidates() {
    let dir = tempfile::tempdir().unwrap();
    let root = project(&dir);

    let report = OkfImporter::new(&root).import(&fixtures(), true).unwrap();

    assert_eq!(report.trust, ImportTrust::Untrusted);
    // Five concept files carry a `type`; five no-front-matter `index.md` files are
    // skipped (navigation, not concepts). PROVENANCE.txt is not a `.md`, so the
    // markdown walk ignores it.
    assert_eq!(report.total, 5, "five concepts");
    assert_eq!(report.skipped, 5, "five index.md navigation files");
    assert_eq!(report.added, 5);

    // Nothing was auto-promoted — every concept waits in review, flagged unsigned.
    let persistence = MemoryPersistence::open_project(&root).unwrap();
    assert!(persistence.list_memory().unwrap().is_empty());
    let items = ReviewQueue::open_project(&root).unwrap().list().unwrap();
    assert_eq!(items.len(), 5);
    assert!(items.iter().all(|item| item
        .candidate
        .rationale
        .as_deref()
        .unwrap()
        .contains("UNSIGNED")));
}

#[test]
fn every_concept_shape_parses_and_maps_type_to_category() {
    // A full-reserved-set concept with inline-flow tags and a `+00:00` timestamp.
    let events = std::fs::read_to_string(fixtures().join("tables/events.md")).unwrap();
    let entry = OkfFormat::from_okf(&events).unwrap();
    assert_eq!(
        entry.category,
        LessonCategory::Other("BigQuery Table".to_string())
    );
    assert_eq!(
        entry.tags,
        vec![
            "events".to_string(),
            "analytics".to_string(),
            "bigquery".to_string(),
            "ecommerce".to_string()
        ]
    );
    assert!(entry.updated_at.is_some(), "a +00:00 timestamp parses");
    assert!(entry.body.contains("events_YYYYMMDD"));

    // A quoted scalar title and a `Z` timestamp.
    let users = std::fs::read_to_string(fixtures().join("tables/users.md")).unwrap();
    assert!(OkfFormat::from_okf(&users).unwrap().updated_at.is_some());

    // A concept with only the required `type` field.
    let dataset = std::fs::read_to_string(fixtures().join("datasets/ga4_sample.md")).unwrap();
    let dataset_entry = OkfFormat::from_okf(&dataset).unwrap();
    assert_eq!(
        dataset_entry.category,
        LessonCategory::Other("Dataset".to_string())
    );
}

#[test]
fn a_foreign_concept_round_trips_through_export_and_reimport() {
    // from_okf → to_okf → from_okf is stable for a Google-shaped concept: once a
    // foreign concept is a LocalMind entry, re-exporting and re-parsing recovers
    // it exactly (the native block is now the source of truth).
    let events = std::fs::read_to_string(fixtures().join("tables/events.md")).unwrap();
    let entry = OkfFormat::from_okf(&events).unwrap();

    let round = OkfFormat::from_okf(&OkfFormat::to_okf(&entry)).unwrap();
    assert_eq!(round.id, entry.id);
    assert_eq!(round.category, entry.category);
    assert_eq!(round.tags, entry.tags);
    assert_eq!(round.body, entry.body);
    assert_eq!(round.confidence.value(), entry.confidence.value());
    assert_eq!(round.updated_at, entry.updated_at);
}
