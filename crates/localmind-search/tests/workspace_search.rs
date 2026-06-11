//! End-to-end retrieval over an ingested fixture workspace with seeded,
//! anchored memory: the headline scenario — code nodes and the lessons
//! learned about them, one query, one ranked list.

use localmind_codegraph::{anchor_memory, IngestBoundary, Ingester};
use localmind_core::{
    Confidence, EvidenceKind, EvidenceRef, LessonCategory, MemoryEntry, MemoryEntryId, MemoryScope,
    MemoryStatus, NodeKind,
};
use localmind_search::{search_workspace, RankedHit, RankingConfig, SearchHitKind, WorkspaceQuery};
use localmind_store::{GraphStore, MemoryPersistence};
use std::fs;
use std::path::Path;
use time::OffsetDateTime;

fn build_fixture(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    fs::write(
        root.join(".localmind.toml"),
        "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n",
    )?;
    fs::create_dir_all(root.join("src"))?;
    fs::write(
        root.join("src/geometry.rs"),
        r#"
pub struct Point { x: f64, y: f64 }

pub fn norm(point: &Point) -> f64 {
    (point.x * point.x + point.y * point.y).sqrt()
}

#[cfg(test)]
mod tests {
    #[test]
    fn norm_is_positive() {
        let value = super::norm(&super::Point { x: 3.0, y: 4.0 });
        assert!(value > 0.0);
    }
}
"#,
    )?;
    fs::write(
        root.join("src/audio.rs"),
        "pub fn play() -> bool { true }\n",
    )?;
    Ok(())
}

fn seed_lesson(
    root: &Path,
    store: &GraphStore,
    id: &str,
    body: &str,
    hint: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let memory = MemoryPersistence::open_project(root)?;
    let entry = MemoryEntry {
        id: MemoryEntryId::new(id),
        scope: MemoryScope::Project,
        body: body.to_string(),
        category: LessonCategory::CodePattern,
        confidence: Confidence::new(0.8)?,
        source_session: None,
        evidence: vec![EvidenceRef::new(EvidenceKind::ManualNote, "seeded")],
        tags: vec!["accepted".to_string()],
        related_files: Vec::new(),
        related_entities: vec![hint.to_string()],
        created_at: Some(OffsetDateTime::now_utc()),
        updated_at: None,
        supersedes: Vec::new(),
        contradicts: Vec::new(),
        status: MemoryStatus::Active,
    };
    memory.persist_memory_entry(&entry)?;
    let report = anchor_memory(store, &entry.id, &entry.related_entities)?;
    assert_eq!(report.anchored, 1, "fixture hint must anchor");
    Ok(())
}

fn ids(hits: &[RankedHit]) -> Vec<String> {
    hits.iter()
        .map(|hit| match &hit.kind {
            SearchHitKind::Code(node) => format!("code:{}", node.qualified_name),
            SearchHitKind::Memory { id, .. } => format!("memory:{}", id.as_str()),
        })
        .collect()
}

#[test]
fn module_query_returns_code_and_anchored_lessons_together(
) -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let root = temp_dir.path();
    build_fixture(root)?;
    let store = GraphStore::open_project(root)?;
    let boundary = IngestBoundary::new(root, Vec::new())?;
    Ingester::new()?.ingest(
        &boundary,
        &[root.join("src/geometry.rs"), root.join("src/audio.rs")],
        &store,
    )?;

    // The lesson's text never mentions "geometry": only the anchor edge can
    // surface it for this query.
    seed_lesson(
        root,
        &store,
        "memory-norm",
        "Prefer the squared form when comparing distances.",
        "src/geometry.rs::norm",
    )?;

    let memory = MemoryPersistence::open_project(root)?;
    let hits = search_workspace(
        &store,
        &memory,
        &WorkspaceQuery {
            text: "geometry".to_string(),
            focus: None,
        },
        &RankingConfig::default(),
    )?;
    let listing = ids(&hits);

    assert!(
        listing.iter().any(|id| id == "code:src/geometry.rs"),
        "expected the module file in {listing:?}"
    );
    assert!(
        listing.iter().any(|id| id == "memory:memory-norm"),
        "expected the anchored lesson in {listing:?}"
    );
    assert!(
        !listing.iter().any(|id| id.contains("audio")),
        "unrelated module must not surface: {listing:?}"
    );
    Ok(())
}

#[test]
fn focus_node_boosts_its_neighborhood() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let root = temp_dir.path();
    build_fixture(root)?;
    let store = GraphStore::open_project(root)?;
    let boundary = IngestBoundary::new(root, Vec::new())?;
    Ingester::new()?.ingest(
        &boundary,
        &[root.join("src/geometry.rs"), root.join("src/audio.rs")],
        &store,
    )?;

    let norm_id = localmind_core::stable_node_id(NodeKind::Function, "src/geometry.rs::norm");
    let memory = MemoryPersistence::open_project(root)?;
    let hits = search_workspace(
        &store,
        &memory,
        &WorkspaceQuery {
            text: String::new(),
            focus: Some(norm_id.clone()),
        },
        &RankingConfig::default(),
    )?;

    let listing = ids(&hits);
    assert!(
        listing
            .iter()
            .any(|id| id == "code:src/geometry.rs::tests::norm_is_positive"),
        "the focus node's test must be in its neighborhood: {listing:?}"
    );
    assert!(
        !listing.iter().any(|id| id.contains("audio")),
        "nodes outside the neighborhood must not surface: {listing:?}"
    );

    // The focus node itself outranks anything further away.
    let first = hits.first().ok_or("expected hits")?;
    match &first.kind {
        SearchHitKind::Code(node) => assert_eq!(node.id, norm_id),
        SearchHitKind::Memory { .. } => return Err("expected a code hit first".into()),
    }
    Ok(())
}

#[test]
fn anchored_memory_rideses_focus_proximity() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let root = temp_dir.path();
    build_fixture(root)?;
    let store = GraphStore::open_project(root)?;
    let boundary = IngestBoundary::new(root, Vec::new())?;
    Ingester::new()?.ingest(
        &boundary,
        &[root.join("src/geometry.rs"), root.join("src/audio.rs")],
        &store,
    )?;
    seed_lesson(
        root,
        &store,
        "memory-norm",
        "Prefer the squared form when comparing distances.",
        "src/geometry.rs::norm",
    )?;
    seed_lesson(
        root,
        &store,
        "memory-audio",
        "Audio playback must stay off the main thread.",
        "src/audio.rs::play",
    )?;

    let norm_id = localmind_core::stable_node_id(NodeKind::Function, "src/geometry.rs::norm");
    let memory = MemoryPersistence::open_project(root)?;
    let hits = search_workspace(
        &store,
        &memory,
        &WorkspaceQuery {
            text: String::new(),
            focus: Some(norm_id),
        },
        &RankingConfig::default(),
    )?;
    let listing = ids(&hits);

    assert!(
        listing.iter().any(|id| id == "memory:memory-norm"),
        "lesson anchored inside the neighborhood must surface: {listing:?}"
    );
    assert!(
        !listing.iter().any(|id| id == "memory:memory-audio"),
        "lesson anchored outside the neighborhood must not: {listing:?}"
    );
    Ok(())
}
