//! `localmind ui`: a local web app for reviewing and managing memory.
//!
//! A synchronous `tiny_http` server (no async runtime) bound to `127.0.0.1`,
//! exposing a small JSON API that calls the **same** store methods the CLI
//! does — `ReviewQueue::decide`, `MemoryPersistence::{promote_review_item,
//! record_review_item_audit, search}` — so there is no logic duplication and no
//! way to bypass the review gate. The frontend is one self-contained HTML file
//! embedded at build time.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use localmind_codegraph::{compute_overview, OverviewOptions};
use localmind_core::{
    GraphEndpoint, MemoryEntryId, NodeKind, ReviewAction, ReviewDecision, ReviewItemId,
};
use localmind_mcp::{handle, GraphToolRequest};
use localmind_store::{GraphStore, MemoryPersistence, ReviewQueue};
use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, Server};

/// The self-contained review dashboard, embedded at build time.
const INDEX_HTML: &str = include_str!("ui/index.html");

pub fn serve(project: PathBuf, port: u16, open: bool, token: Option<String>) -> Result<()> {
    let addr = format!("127.0.0.1:{port}");
    let server = Server::http(&addr).map_err(|error| anyhow!("failed to bind {addr}: {error}"))?;
    let url = format!("http://{addr}");
    println!("LocalMind review UI: {url}");
    println!("Store: {}", project.display());
    if token.is_some() {
        println!("Token required (pass ?token=... in the browser URL).");
    }
    println!("Press Ctrl+C to stop.");
    if open {
        open_browser(&url);
    }

    for mut request in server.incoming_requests() {
        let response = route(&project, token.as_deref(), &mut request);
        let _ = request.respond(response);
    }
    Ok(())
}

fn route(project: &Path, token: Option<&str>, request: &mut Request) -> Response<Cursor<Vec<u8>>> {
    let method = request.method().clone();
    let raw_url = request.url().to_string();
    let (path, query) = raw_url.split_once('?').unwrap_or((raw_url.as_str(), ""));

    // Token gate (localhost bind is the primary control; token adds LAN safety).
    if let Some(expected) = token {
        if query_param(query, "token").as_deref() != Some(expected) {
            return json_response(401, &json!({ "error": "invalid or missing token" }));
        }
    }

    let mut body = String::new();
    let _ = request.as_reader().read_to_string(&mut body);

    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    let result = match (&method, segments.as_slice()) {
        (Method::Get, [""] | ["index.html"]) => return html_response(INDEX_HTML),
        (Method::Get, ["api", "status"]) => api_status(project),
        (Method::Get, ["api", "review"]) => api_review_list(project, query),
        (Method::Get, ["api", "review", id]) => api_review_get(project, id),
        (Method::Post, ["api", "review", "bulk"]) => api_bulk(project, &body),
        (Method::Post, ["api", "review", id, action]) => {
            api_review_action(project, id, action, &body)
        }
        (Method::Get, ["api", "memory"]) => api_memory_list(project, query),
        (Method::Get, ["api", "memory", "facets"]) => api_memory_facets(project),
        (Method::Get, ["api", "memory", id]) => api_memory_get(project, id),
        (Method::Delete, ["api", "memory", id]) => api_memory_delete(project, id),
        (Method::Get, ["api", "docs"]) => api_docs(project, query),
        (Method::Get, ["api", "docs", "files"]) => api_doc_files(project),
        (Method::Get, ["api", "docs", "file"]) => api_doc_file(project, query),
        (Method::Get, ["api", "graph"]) => api_graph(project, query),
        (Method::Get, ["api", "graph", "overview"]) => api_graph_overview(project),
        (Method::Get, ["api", "graph", "symbols"]) => api_graph_symbols(project, query),
        (Method::Get, ["api", "graph", "local"]) => api_graph_local(project, query),
        (Method::Get, ["api", "graph", "global"]) => api_graph_global(project, query),
        (Method::Get, ["api", "audit"]) => api_audit(project, query),
        (Method::Get, ["api", "stats"]) => api_stats(project),
        _ => Err(anyhow!("not found: {method:?} {path}")),
    };

    match result {
        Ok(value) => json_response(200, &value),
        Err(error) => json_response(400, &json!({ "error": error.to_string() })),
    }
}

fn api_status(project: &Path) -> Result<Value> {
    let queue = ReviewQueue::open_project(project)?;
    let pending = queue
        .list()?
        .into_iter()
        .filter(|item| format!("{:?}", item.state) == "Pending")
        .count();
    let persistence = MemoryPersistence::open_project(project)?;
    let accepted = persistence.list_memory()?.len();
    Ok(json!({
        "project": project.display().to_string(),
        "pending": pending,
        "accepted": accepted,
    }))
}

fn api_review_list(project: &Path, query: &str) -> Result<Value> {
    let want_state = query_param(query, "state");
    let queue = ReviewQueue::open_project(project)?;
    let items: Vec<Value> = queue
        .list()?
        .into_iter()
        .filter(|item| match &want_state {
            Some(state) => format!("{:?}", item.state).eq_ignore_ascii_case(state),
            None => true,
        })
        .map(|item| review_item_json(&item))
        .collect();
    Ok(json!({ "items": items }))
}

fn api_review_get(project: &Path, id: &str) -> Result<Value> {
    let queue = ReviewQueue::open_project(project)?;
    match queue.get(&ReviewItemId::new(id))? {
        Some(item) => Ok(review_item_json(&item)),
        None => Err(anyhow!("review item not found: {id}")),
    }
}

/// One review action: accept (accept + promote to memory), reject, defer, edit,
/// or promote. Accept and promote both write durable memory through the same
/// `promote_review_item` path the CLI uses.
fn api_review_action(project: &Path, id: &str, action: &str, body: &str) -> Result<Value> {
    let payload: Value = if body.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(body)?
    };
    let reviewer = payload
        .get("reviewer")
        .and_then(Value::as_str)
        .unwrap_or("ui")
        .to_string();
    let note = payload
        .get("note")
        .and_then(Value::as_str)
        .map(str::to_string);

    let persistence = MemoryPersistence::open_project(project)?;
    let item_id = ReviewItemId::new(id);

    match action {
        "accept" => {
            decide(
                project,
                &item_id,
                ReviewAction::Accept,
                reviewer,
                note,
                None,
            )?;
            let entry = persistence.promote_review_item(&item_id)?;
            Ok(json!({ "id": id, "state": "Accepted", "promoted": entry.id.to_string() }))
        }
        "accept_only" => {
            let state = decide(
                project,
                &item_id,
                ReviewAction::Accept,
                reviewer,
                note,
                None,
            )?;
            Ok(json!({ "id": id, "state": state, "promoted": Value::Null }))
        }
        "reject" => {
            let state = decide(
                project,
                &item_id,
                ReviewAction::Reject,
                reviewer,
                note,
                None,
            )?;
            Ok(json!({ "id": id, "state": state }))
        }
        "defer" => {
            let state = decide(
                project,
                &item_id,
                ReviewAction::MarkTemporary,
                reviewer,
                note,
                None,
            )?;
            Ok(json!({ "id": id, "state": state }))
        }
        "edit" => {
            let replacement = payload
                .get("replacement")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("edit requires a `replacement` field"))?
                .to_string();
            let state = decide(
                project,
                &item_id,
                ReviewAction::Edit,
                reviewer,
                note,
                Some(replacement),
            )?;
            Ok(json!({ "id": id, "state": state }))
        }
        "promote" => {
            let entry = persistence.promote_review_item(&item_id)?;
            Ok(json!({ "id": id, "promoted": entry.id.to_string() }))
        }
        other => Err(anyhow!("unknown action: {other}")),
    }
}

fn api_bulk(project: &Path, body: &str) -> Result<Value> {
    let payload: Value = serde_json::from_str(body)?;
    let action = payload
        .get("action")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("bulk requires an `action` field"))?;
    let ids = payload
        .get("ids")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("bulk requires an `ids` array"))?;
    let reviewer = payload
        .get("reviewer")
        .and_then(Value::as_str)
        .unwrap_or("ui");

    let mut done = 0usize;
    let mut errors: Vec<Value> = Vec::new();
    for id_value in ids {
        let Some(id) = id_value.as_str() else {
            continue;
        };
        let single = json!({ "reviewer": reviewer });
        match api_review_action(project, id, action, &single.to_string()) {
            Ok(_) => done += 1,
            Err(error) => errors.push(json!({ "id": id, "error": error.to_string() })),
        }
    }
    Ok(json!({ "action": action, "done": done, "errors": errors }))
}

/// Accepted memory: ranked search when `query` is present, else the full list,
/// with optional `scope`/`category` filters.
fn api_memory_list(project: &Path, query: &str) -> Result<Value> {
    let text = query_param(query, "query").filter(|q| !q.is_empty());
    let scope_filter = query_param(query, "scope");
    let category_filter = query_param(query, "category");
    let persistence = MemoryPersistence::open_project(project)?;

    if let Some(text) = text {
        let items: Vec<Value> = persistence
            .search(&text)?
            .into_iter()
            .filter(|r| matches_filter(&r.category, category_filter.as_deref()))
            .map(|r| {
                json!({
                    "id": r.memory_id.to_string(),
                    "category": r.category,
                    "score": r.score,
                    "path": r.path.display().to_string(),
                    "snippet": r.snippet,
                    "stale": r.stale_candidate,
                    "hit_count": r.hit_count,
                })
            })
            .collect();
        return Ok(json!({ "mode": "search", "items": items }));
    }

    let language_filter = query_param(query, "language");
    let items: Vec<Value> = persistence
        .list_memory()?
        .into_iter()
        .filter(|r| matches_filter(&r.scope, scope_filter.as_deref()))
        .filter(|r| matches_filter(&r.category, category_filter.as_deref()))
        .filter(|r| {
            matches_filter(
                r.language.as_deref().unwrap_or(""),
                language_filter.as_deref(),
            )
        })
        .map(|r| {
            json!({
                "id": r.memory_id.to_string(),
                "category": r.category,
                "scope": r.scope,
                "status": r.status,
                "language": r.language,
                "snippet": truncate(&r.body, 200),
                "hit_count": r.hit_count,
                "stale": r.stale_candidate,
                "contradicted": r.contradicted,
            })
        })
        .collect();
    Ok(json!({ "mode": "list", "items": items }))
}

/// Distinct filterable values in accepted memory, with counts — so the UI can
/// offer real dropdown choices instead of free-text guessing.
fn api_memory_facets(project: &Path) -> Result<Value> {
    let persistence = MemoryPersistence::open_project(project)?;
    let memory = persistence.list_memory()?;
    let mut scope: BTreeMap<String, usize> = BTreeMap::new();
    let mut category: BTreeMap<String, usize> = BTreeMap::new();
    let mut language: BTreeMap<String, usize> = BTreeMap::new();
    let mut status: BTreeMap<String, usize> = BTreeMap::new();
    let mut stale = 0usize;
    let mut conflict = 0usize;
    for record in &memory {
        *scope.entry(record.scope.clone()).or_default() += 1;
        *category.entry(record.category.clone()).or_default() += 1;
        *language
            .entry(
                record
                    .language
                    .clone()
                    .unwrap_or_else(|| "(agnostic)".to_string()),
            )
            .or_default() += 1;
        *status.entry(record.status.clone()).or_default() += 1;
        if record.stale_candidate {
            stale += 1;
        }
        if record.contradicted {
            conflict += 1;
        }
    }
    Ok(json!({
        "total": memory.len(),
        "scope": scope,
        "category": category,
        "language": language,
        "status": status,
        "stale": stale,
        "conflict": conflict,
    }))
}

/// One accepted memory in full, with its provenance ("why do you think that?").
fn api_memory_get(project: &Path, id: &str) -> Result<Value> {
    let persistence = MemoryPersistence::open_project(project)?;
    let record = persistence
        .list_memory()?
        .into_iter()
        .find(|r| r.memory_id.as_str() == id)
        .ok_or_else(|| anyhow!("accepted memory not found: {id}"))?;
    let provenance = persistence.provenance(&MemoryEntryId::new(id))?.map(|p| {
        json!({
            "source_session": p.source_session,
            "confidence": p.confidence,
            "epistemic_status": format!("{:?}", p.epistemic_status),
            "status": p.status,
            "stale": p.stale_candidate,
            "contradicts": p.contradicts.iter().map(|c| c.to_string()).collect::<Vec<_>>(),
        })
    });
    Ok(json!({
        "id": record.memory_id.to_string(),
        "category": record.category,
        "scope": record.scope,
        "status": record.status,
        "language": record.language,
        "body": record.body,
        "hit_count": record.hit_count,
        "last_used_at": record.last_used_at,
        "stale": record.stale_candidate,
        "contradicted": record.contradicted,
        "provenance": provenance,
    }))
}

/// Semantic search over ingested documentation.
fn api_docs(project: &Path, query: &str) -> Result<Value> {
    let q = query_param(query, "q")
        .or_else(|| query_param(query, "query"))
        .unwrap_or_default();
    let limit = query_param(query, "limit")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(10)
        .clamp(1, 50);
    let persistence = MemoryPersistence::open_project(project)?;
    let results: Vec<Value> = persistence
        .doc_search(&q, limit)?
        .into_iter()
        .map(|r| {
            json!({
                "path": r.path,
                "ordinal": r.ordinal,
                "heading": r.heading,
                "score": r.score,
                "body": r.body,
            })
        })
        .collect();
    Ok(json!({ "results": results }))
}

/// Every ingested documentation file with its chunk count (browse sidebar).
fn api_doc_files(project: &Path) -> Result<Value> {
    let persistence = MemoryPersistence::open_project(project)?;
    let files: Vec<Value> = persistence
        .doc_files()?
        .into_iter()
        .map(|(path, chunks)| json!({ "path": path, "chunks": chunks }))
        .collect();
    Ok(json!({ "total": files.len(), "files": files }))
}

/// One ingested file's chunks, in order (read after picking from the browser).
fn api_doc_file(project: &Path, query: &str) -> Result<Value> {
    let path = query_param(query, "path").ok_or_else(|| anyhow!("path is required"))?;
    let persistence = MemoryPersistence::open_project(project)?;
    let chunks: Vec<Value> = persistence
        .doc_chunks_for(&path)?
        .into_iter()
        .map(|(ordinal, heading, body)| json!({ "ordinal": ordinal, "heading": heading, "body": body }))
        .collect();
    Ok(json!({ "path": path, "chunks": chunks }))
}

/// Architecture overview of the code graph: file/symbol counts, languages,
/// busiest packages, and hotspots — the "what do I have" landing.
fn api_graph_overview(project: &Path) -> Result<Value> {
    let store = GraphStore::open_project(project)?;
    let overview = compute_overview(&store, OverviewOptions::default())?;
    Ok(serde_json::to_value(&overview)?)
}

/// Search/list code symbols by (qualified) name, so the graph is browsable.
fn api_graph_symbols(project: &Path, query: &str) -> Result<Value> {
    let needle = query_param(query, "query")
        .or_else(|| query_param(query, "q"))
        .unwrap_or_default()
        .to_lowercase();
    let limit = query_param(query, "limit")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(50)
        .clamp(1, 200);
    let store = GraphStore::open_project(project)?;
    let mut symbols = Vec::new();
    for kind in [NodeKind::Function, NodeKind::Test] {
        for node in store.nodes_by_kind(kind)? {
            if needle.is_empty() || node.qualified_name.to_lowercase().contains(&needle) {
                symbols.push(json!({
                    "qualified_name": node.qualified_name,
                    "kind": node.kind.as_str(),
                    "path": node.location.as_ref().map(|location| location.path.clone()),
                }));
                if symbols.len() >= limit {
                    break;
                }
            }
        }
        if symbols.len() >= limit {
            break;
        }
    }
    Ok(json!({ "symbols": symbols }))
}

/// A focus symbol's local graph — nodes (the symbol plus its neighbours) and the
/// edges among them — for the interactive force-graph view. Click-to-recenter on
/// the client turns this into free graph exploration.
fn api_graph_local(project: &Path, query: &str) -> Result<Value> {
    let symbol = query_param(query, "symbol").unwrap_or_default();
    let depth = query_param(query, "depth")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(1)
        .clamp(1, 2);
    let store = GraphStore::open_project(project)?;
    let focus = store
        .find_symbol(&symbol)?
        .into_iter()
        .find(|node| node.kind != NodeKind::Repository)
        .ok_or_else(|| anyhow!("symbol not in the graph: {symbol}"))?;

    let mut nodes = vec![focus.clone()];
    nodes.extend(store.neighbors(&focus.id, depth)?);
    let mut seen = std::collections::HashSet::new();
    nodes.retain(|node| seen.insert(node.id.as_str().to_string()));
    let ids: std::collections::HashSet<String> = nodes
        .iter()
        .map(|node| node.id.as_str().to_string())
        .collect();
    let focus_id = focus.id.as_str().to_string();

    let node_json: Vec<Value> = nodes
        .iter()
        .map(|node| {
            json!({
                "id": node.id.as_str(),
                "name": node.name,
                "kind": node.kind.as_str(),
                "qualified_name": node.qualified_name,
                "path": node.location.as_ref().map(|l| l.path.clone()),
                "focus": node.id.as_str() == focus_id,
            })
        })
        .collect();

    let mut edges = Vec::new();
    for edge in store.active_edges()? {
        if let (GraphEndpoint::Node(from), GraphEndpoint::Node(to)) = (&edge.from, &edge.to) {
            if ids.contains(from.as_str()) && ids.contains(to.as_str()) {
                edges.push(json!({
                    "from": from.as_str(),
                    "to": to.as_str(),
                    "kind": edge.kind.as_str(),
                }));
            }
        }
    }
    Ok(json!({ "focus": focus_id, "nodes": node_json, "edges": edges }))
}

/// A file-level view of the whole graph: files are nodes, and a symbol-to-symbol
/// edge is aggregated up to a file-to-file edge. Optionally filtered to a path
/// prefix (a repo), capped to the most-connected files so it stays drawable.
fn api_graph_global(project: &Path, query: &str) -> Result<Value> {
    let prefix = query_param(query, "path").unwrap_or_default();
    let limit = query_param(query, "limit")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(200)
        .clamp(20, 600);
    let store = GraphStore::open_project(project)?;

    // Map every symbol node to the file it lives in (a File node's file is its own
    // qualified name; another symbol's is its source location path).
    let mut id_to_file: HashMap<String, String> = HashMap::new();
    for kind in [
        NodeKind::File,
        NodeKind::Function,
        NodeKind::Test,
        NodeKind::Type,
        NodeKind::Module,
    ] {
        for node in store.nodes_by_kind(kind)? {
            let file = if kind == NodeKind::File {
                node.qualified_name.clone()
            } else {
                node.location
                    .as_ref()
                    .map(|l| l.path.clone())
                    .unwrap_or_default()
            };
            if !file.is_empty() && (prefix.is_empty() || file.starts_with(&prefix)) {
                id_to_file.insert(node.id.as_str().to_string(), file);
            }
        }
    }

    // Aggregate symbol→symbol edges into file→file edges, counting degree.
    let mut edge_set: HashSet<(String, String)> = HashSet::new();
    let mut degree: HashMap<String, usize> = HashMap::new();
    for edge in store.active_edges()? {
        if let (GraphEndpoint::Node(from), GraphEndpoint::Node(to)) = (&edge.from, &edge.to) {
            if let (Some(from_file), Some(to_file)) =
                (id_to_file.get(from.as_str()), id_to_file.get(to.as_str()))
            {
                if from_file != to_file && edge_set.insert((from_file.clone(), to_file.clone())) {
                    *degree.entry(from_file.clone()).or_default() += 1;
                    *degree.entry(to_file.clone()).or_default() += 1;
                }
            }
        }
    }

    // Keep the most-connected files (a drawable slice), then the edges among them.
    let mut ranked: Vec<(String, usize)> = degree.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let kept: HashSet<String> = ranked.iter().take(limit).map(|(f, _)| f.clone()).collect();
    let total = ranked.len();

    let nodes: Vec<Value> = kept
        .iter()
        .map(|path| {
            json!({
                "id": path,
                "name": path.rsplit('/').next().unwrap_or(path),
                "kind": "file",
                "qualified_name": path,
                "path": path,
                "focus": false,
            })
        })
        .collect();
    let edges: Vec<Value> = edge_set
        .into_iter()
        .filter(|(f, t)| kept.contains(f) && kept.contains(t))
        .map(|(f, t)| json!({ "from": f, "to": t, "kind": "depends" }))
        .collect();

    Ok(json!({
        "nodes": nodes,
        "edges": edges,
        "shown": kept.len(),
        "total_connected": total,
    }))
}

/// Code-graph query: one of neighborhood / connection / coverage / knowledge.
fn api_graph(project: &Path, query: &str) -> Result<Value> {
    let tool = query_param(query, "tool").unwrap_or_else(|| "neighborhood".to_string());
    let symbol = query_param(query, "symbol").unwrap_or_default();
    let request = match tool.as_str() {
        "connection" => GraphToolRequest::MemorySymbolConnection {
            from: query_param(query, "from").unwrap_or_default(),
            to: query_param(query, "to").unwrap_or_default(),
            max_hops: query_param(query, "max_hops")
                .and_then(|s| s.parse().ok())
                .unwrap_or(6),
        },
        "coverage" => GraphToolRequest::MemorySymbolCoverage { symbol },
        "knowledge" => GraphToolRequest::MemorySymbolKnowledge { symbol },
        _ => GraphToolRequest::MemorySymbolNeighborhood {
            symbol,
            depth: query_param(query, "depth")
                .and_then(|s| s.parse().ok())
                .unwrap_or(1),
        },
    };
    let store = GraphStore::open_project(project)?;
    // A typed graph error (unknown/ambiguous symbol) is a soft, user-facing
    // result, not a 400 — surface it in the payload.
    match handle(&store, &request) {
        Ok(response) => Ok(serde_json::to_value(&response)?),
        Err(error) => Ok(json!({ "graph_error": error.to_string() })),
    }
}

/// The audit trail, newest first.
fn api_audit(project: &Path, query: &str) -> Result<Value> {
    let limit = query_param(query, "limit")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(100)
        .clamp(1, 1000);
    let persistence = MemoryPersistence::open_project(project)?;
    let mut records = persistence.audit_records()?;
    records.reverse();
    let items: Vec<Value> = records
        .into_iter()
        .take(limit)
        .map(|r| {
            json!({
                "id": r.id,
                "kind": r.kind,
                "actor": r.actor,
                "subject": r.subject,
                "at": r.happened_at,
                "metadata": r.metadata_json,
            })
        })
        .collect();
    Ok(json!({ "items": items }))
}

/// Dashboard aggregates across the queue, accepted memory, and the doc index.
fn api_stats(project: &Path) -> Result<Value> {
    let queue = ReviewQueue::open_project(project)?;
    let queue_items = queue.list()?;
    let mut pending_by_category: BTreeMap<String, usize> = BTreeMap::new();
    let mut pending = 0usize;
    for item in &queue_items {
        if format!("{:?}", item.state) == "Pending" {
            pending += 1;
            *pending_by_category
                .entry(format!("{:?}", item.candidate.category))
                .or_default() += 1;
        }
    }
    let persistence = MemoryPersistence::open_project(project)?;
    let memory = persistence.list_memory()?;
    let mut accepted_by_scope: BTreeMap<String, usize> = BTreeMap::new();
    let mut accepted_by_category: BTreeMap<String, usize> = BTreeMap::new();
    for record in &memory {
        *accepted_by_scope.entry(record.scope.clone()).or_default() += 1;
        *accepted_by_category
            .entry(record.category.clone())
            .or_default() += 1;
    }
    Ok(json!({
        "store_path": project.display().to_string(),
        "pending": pending,
        "accepted": memory.len(),
        "doc_chunks": persistence.doc_chunk_count()?,
        "pending_by_category": pending_by_category,
        "accepted_by_scope": accepted_by_scope,
        "accepted_by_category": accepted_by_category,
    }))
}

fn matches_filter(value: &str, filter: Option<&str>) -> bool {
    filter.is_none_or(|f| f.is_empty() || value.eq_ignore_ascii_case(f))
}

fn truncate(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max {
        trimmed.to_string()
    } else {
        let cut: String = trimmed.chars().take(max).collect();
        format!("{cut}…")
    }
}

fn api_memory_delete(project: &Path, id: &str) -> Result<Value> {
    let persistence = MemoryPersistence::open_project(project)?;
    let deleted = persistence.delete_memory(&MemoryEntryId::new(id), "ui")?;
    if deleted {
        Ok(json!({ "id": id, "deleted": true }))
    } else {
        Err(anyhow!("accepted memory not found: {id}"))
    }
}

fn decide(
    project: &Path,
    item_id: &ReviewItemId,
    action: ReviewAction,
    reviewer: String,
    note: Option<String>,
    replacement_summary: Option<String>,
) -> Result<String> {
    let persistence = MemoryPersistence::open_project(project)?;
    let queue = ReviewQueue::open_project(project)?;
    let item = queue.decide(ReviewDecision {
        item_id: item_id.clone(),
        action,
        reviewer,
        decided_at: None,
        note,
        replacement_summary,
        evidence: Vec::new(),
    })?;
    persistence.record_review_item_audit(&item)?;
    Ok(format!("{:?}", item.state))
}

fn review_item_json(item: &localmind_store::ReviewQueueItem) -> Value {
    json!({
        "id": item.id.to_string(),
        "state": format!("{:?}", item.state),
        "session": item.session_id.to_string(),
        "summary": item.candidate.summary(),
        "category": format!("{:?}", item.candidate.category),
        "confidence": item.candidate.confidence.value(),
        "rationale": item.candidate.rationale.clone(),
        "replacement": item.replacement_summary.clone(),
        "note": item.note.clone(),
    })
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        if name == key {
            Some(percent_decode(value))
        } else {
            None
        }
    })
}

/// Minimal `application/x-www-form-urlencoded` decode (`+` → space, `%XX` → byte).
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        out.push(byte);
                        index += 3;
                    }
                    Err(_) => {
                        out.push(bytes[index]);
                        index += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn json_response(status: u16, value: &Value) -> Response<Cursor<Vec<u8>>> {
    let mut response = Response::from_string(value.to_string()).with_status_code(status);
    if let Ok(header) = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]) {
        response = response.with_header(header);
    }
    response
}

fn html_response(html: &str) -> Response<Cursor<Vec<u8>>> {
    let mut response = Response::from_string(html.to_string());
    if let Ok(header) = Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..]) {
        response = response.with_header(header);
    }
    response
}

fn open_browser(url: &str) {
    // Best-effort convenience only; a failure to open is never fatal.
    #[cfg(windows)]
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

#[cfg(test)]
mod tests {
    use super::{percent_decode, query_param};

    #[test]
    fn query_param_extracts_and_decodes() {
        assert_eq!(
            query_param("state=Pending&token=a%20b", "state").as_deref(),
            Some("Pending")
        );
        assert_eq!(
            query_param("state=Pending&token=a%20b", "token").as_deref(),
            Some("a b")
        );
        assert_eq!(query_param("state=Pending", "missing"), None);
    }

    #[test]
    fn percent_decode_handles_plus_and_hex() {
        assert_eq!(percent_decode("a+b"), "a b");
        assert_eq!(percent_decode("%2Fdocs%2Fx"), "/docs/x");
        assert_eq!(percent_decode("plain"), "plain");
    }

    #[test]
    fn index_html_is_embedded_and_self_contained() {
        // No external asset references — the page must be self-contained.
        assert!(super::INDEX_HTML.contains("LocalMind"));
        assert!(!super::INDEX_HTML.contains("src=\"http"));
        assert!(!super::INDEX_HTML.contains("cdn"));
    }
}
