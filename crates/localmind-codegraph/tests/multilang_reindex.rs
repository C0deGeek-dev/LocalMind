//! Multi-language incremental reindex + memory-anchor join: editing one file
//! re-extracts only that file's nodes, node ids stay stable across the reindex,
//! and a memory anchored to an unchanged symbol survives.

use localmind_codegraph::{anchor_memory, IngestBoundary, Ingester, Reindexer};
use localmind_core::{stable_node_id, MemoryEntryId, NodeKind};
use localmind_store::GraphStore;
use std::fs;
use std::path::{Path, PathBuf};

fn write(root: &Path, relative: &str, body: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap_or(());
    }
    if fs::write(&path, body).is_err() {
        unreachable!("fixture write must succeed");
    }
}

fn candidates(root: &Path, relatives: &[&str]) -> Vec<PathBuf> {
    relatives
        .iter()
        .map(|relative| root.join(relative))
        .filter(|path| path.exists())
        .collect()
}

const PY_V1: &str = "def foo():\n    return 1\n";
const PY_V2: &str = "def foo():\n    return 1\n\ndef bar():\n    return foo()\n";
const GO_SRC: &str = "package main\n\nfunc Add(a, b int) int { return a + b }\n";

#[test]
fn mixed_language_reindex_keeps_ids_stable_and_anchors_survive(
) -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let root = temp_dir.path();
    fs::write(root.join(".localmind.toml"), "[learning]\nenabled = true\n")?;
    write(root, "src/a.py", PY_V1);
    write(root, "src/b.go", GO_SRC);

    let boundary = IngestBoundary::new(root, Vec::new())?;
    let store = GraphStore::open_project(root)?;
    let relatives = ["src/a.py", "src/b.go"];

    Ingester::new()?.ingest(&boundary, &candidates(root, &relatives), &store)?;

    // Both languages produced symbol nodes.
    let foo_id = stable_node_id(NodeKind::Function, "src/a.py::foo");
    let add_id = stable_node_id(NodeKind::Function, "src/b.go::Add");
    assert!(store.node(&foo_id)?.is_some(), "python foo must be indexed");
    assert!(store.node(&add_id)?.is_some(), "go Add must be indexed");

    // Anchor a memory to the python symbol.
    let memory_id = MemoryEntryId::new("memory-1");
    let report = anchor_memory(&store, &memory_id, &["src/a.py::foo".to_string()])?;
    assert_eq!(report.anchored, 1);
    let anchors = store.anchors_of_memory(&memory_id)?;
    let anchor_id = anchors.first().ok_or("anchor edge missing")?.id.clone();

    // Edit only the python file (foo unchanged; bar added). The Go file is
    // untouched, so the reindex must skip it.
    write(root, "src/a.py", PY_V2);
    let reindexer = Reindexer::new()?;
    let mut plan = reindexer.plan(&boundary, &candidates(root, &relatives), &store)?;
    assert_eq!(plan.unchanged, 1, "the Go file must be unchanged");
    assert_eq!(plan.index.len(), 1, "only the python file is reindexed");

    let mut reindexer = reindexer;
    reindexer.run(&boundary, &store, &mut plan, usize::MAX)?;

    // foo kept its stable id and the new bar appeared; Add was never touched.
    assert!(
        store.node(&foo_id)?.is_some(),
        "foo id stable across reindex"
    );
    assert!(
        store
            .node(&stable_node_id(NodeKind::Function, "src/a.py::bar"))?
            .is_some(),
        "the new python function must be extracted"
    );
    assert!(store.node(&add_id)?.is_some(), "go Add survives untouched");

    // The anchor to the unchanged symbol survives (revived, not retired).
    let revived = store.edge(&anchor_id)?.ok_or("anchor edge missing")?;
    assert!(
        revived.superseded_at.is_none(),
        "anchor to an unchanged symbol must survive reindex"
    );
    Ok(())
}
