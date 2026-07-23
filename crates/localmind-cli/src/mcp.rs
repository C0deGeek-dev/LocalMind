//! Hand-rolled stdio MCP server: newline-delimited JSON-RPC 2.0 over
//! stdin/stdout.
//!
//! No async runtime and no MCP SDK — one blocking read loop. Each line of
//! stdin is one JSON-RPC message; each response is one line of stdout. The
//! server exposes LocalMind's read/query tools (memory search, context export,
//! code graph, skills) over the catalog declared in `localmind_mcp`.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::Result;
use localmind_mcp::{
    catalog, fetch_active_skill, handle, list_active_skills, GraphToolRequest, TOOL_DOC_SEARCH,
    TOOL_MEMORY_CONTEXT_EXPORT, TOOL_MEMORY_SEARCH, TOOL_SKILL_FETCH, TOOL_SKILL_LIST,
    TOOL_SYMBOL_CONNECTION, TOOL_SYMBOL_COVERAGE, TOOL_SYMBOL_KNOWLEDGE, TOOL_SYMBOL_NEIGHBORHOOD,
};
use localmind_store::{ContextExportTarget, ContextExporter, GraphStore, MemoryPersistence};
use serde_json::{json, Value};

/// MCP protocol revision this server speaks.
const PROTOCOL_VERSION: &str = "2024-11-05";

/// A tool call either fails the protocol (bad arguments) or fails at execution
/// (a store or graph error). Protocol failures become JSON-RPC errors;
/// execution failures become tool results flagged `isError`.
#[derive(Debug)]
enum ToolFailure {
    Protocol(String),
    Execution(String),
}

fn exec<E: std::fmt::Display>(error: E) -> ToolFailure {
    ToolFailure::Execution(error.to_string())
}

/// Runs the server until stdin reaches EOF.
pub fn serve(project: PathBuf) -> Result<()> {
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();
    let mut line = String::new();

    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break; // EOF: the client closed the pipe.
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let request: Value = match serde_json::from_str(trimmed) {
            Ok(value) => value,
            Err(error) => {
                write_message(&mut writer, &parse_error(&error.to_string()))?;
                continue;
            }
        };

        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        let id = match request.get("id").cloned() {
            Some(id) => id,
            None => {
                // A notification (no id) expects no response.
                if method == "exit" {
                    break;
                }
                continue;
            }
        };
        let params = request.get("params").cloned().unwrap_or(Value::Null);
        let response = dispatch(&project, method, &params, id);
        write_message(&mut writer, &response)?;
    }

    Ok(())
}

fn dispatch(project: &Path, method: &str, params: &Value, id: Value) -> Value {
    match method {
        "initialize" => reply(
            id,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "localmind", "version": env!("CARGO_PKG_VERSION") }
            }),
        ),
        "tools/list" => reply(id, json!({ "tools": catalog() })),
        "tools/call" => match call_tool(project, params) {
            Ok(text) => reply(
                id,
                json!({ "content": [{ "type": "text", "text": text }], "isError": false }),
            ),
            Err(ToolFailure::Protocol(message)) => error_reply(id, -32602, &message),
            Err(ToolFailure::Execution(message)) => reply(
                id,
                json!({ "content": [{ "type": "text", "text": message }], "isError": true }),
            ),
        },
        "ping" => reply(id, json!({})),
        other => error_reply(id, -32601, &format!("method not found: {other}")),
    }
}

fn call_tool(project: &Path, params: &Value) -> Result<String, ToolFailure> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolFailure::Protocol("tools/call is missing a tool name".to_string()))?;
    let empty = Value::Object(serde_json::Map::new());
    let args = params.get("arguments").unwrap_or(&empty);

    match name {
        TOOL_MEMORY_SEARCH => memory_search(project, args),
        TOOL_MEMORY_CONTEXT_EXPORT => memory_context_export(project, args),
        TOOL_DOC_SEARCH => doc_search(project, args),
        TOOL_SYMBOL_NEIGHBORHOOD => graph_tool(
            project,
            GraphToolRequest::MemorySymbolNeighborhood {
                symbol: str_arg(args, "symbol")?,
                depth: u32_arg(args, "depth", 2),
            },
        ),
        TOOL_SYMBOL_CONNECTION => graph_tool(
            project,
            GraphToolRequest::MemorySymbolConnection {
                from: str_arg(args, "from")?,
                to: str_arg(args, "to")?,
                max_hops: u32_arg(args, "max_hops", 6),
            },
        ),
        TOOL_SYMBOL_COVERAGE => graph_tool(
            project,
            GraphToolRequest::MemorySymbolCoverage {
                symbol: str_arg(args, "symbol")?,
            },
        ),
        TOOL_SYMBOL_KNOWLEDGE => graph_tool(
            project,
            GraphToolRequest::MemorySymbolKnowledge {
                symbol: str_arg(args, "symbol")?,
            },
        ),
        TOOL_SKILL_LIST => {
            let skills = list_active_skills(project).map_err(exec)?;
            serde_json::to_string_pretty(&skills).map_err(exec)
        }
        TOOL_SKILL_FETCH => {
            let id = str_arg(args, "id")?;
            match fetch_active_skill(project, &id).map_err(exec)? {
                Some(skill) => serde_json::to_string_pretty(&skill).map_err(exec),
                None => Err(ToolFailure::Execution(format!(
                    "no active skill with id {id}"
                ))),
            }
        }
        other => Err(ToolFailure::Protocol(format!("unknown tool: {other}"))),
    }
}

fn memory_search(project: &Path, args: &Value) -> Result<String, ToolFailure> {
    let query = str_arg(args, "query")?;
    // Model-facing surface: bounded by default so a broad query cannot flood
    // the caller's context with the whole matching tail.
    let limit = usize::try_from(u32_arg(args, "limit", 8)).unwrap_or(8);
    let persistence = MemoryPersistence::open_project(project).map_err(exec)?;
    let results = persistence.search(&query).map_err(exec)?;
    if results.is_empty() {
        return Ok("No accepted memory matched this query.".to_string());
    }
    let mut out = String::new();
    for result in results.iter().take(limit) {
        out.push_str(&format!(
            "{}\tscore={}\t{}\n{}\n\n",
            result.memory_id.as_str(),
            result.score,
            result.path.display(),
            result.snippet
        ));
    }
    Ok(out)
}

fn memory_context_export(project: &Path, args: &Value) -> Result<String, ToolFailure> {
    let query = str_arg(args, "query")?;
    let target = match args.get("target").and_then(Value::as_str) {
        Some("generic") => ContextExportTarget::Generic,
        Some("open-ai-codex") => ContextExportTarget::OpenAiCodex,
        Some("localpilot") => ContextExportTarget::LocalPilot,
        _ => ContextExportTarget::ClaudeCode,
    };
    let exporter = ContextExporter::open_project(project).map_err(exec)?;
    let export = exporter.export(&query, target).map_err(exec)?;
    Ok(export.body_markdown)
}

fn doc_search(project: &Path, args: &Value) -> Result<String, ToolFailure> {
    let query = str_arg(args, "query")?;
    let limit = usize::try_from(u32_arg(args, "limit", 5)).unwrap_or(5);
    let persistence = MemoryPersistence::open_project(project).map_err(exec)?;
    let results = persistence.doc_search(&query, limit).map_err(exec)?;
    if results.is_empty() {
        return Ok(
            "No documentation matched this query (or embeddings are not configured).".to_string(),
        );
    }
    let mut out = String::new();
    for result in results {
        let heading = result.heading.unwrap_or_default();
        out.push_str(&format!(
            "{}  #{}  {}  (score {:.3})\n{}\n\n",
            result.path, result.ordinal, heading, result.score, result.body
        ));
    }
    Ok(out)
}

fn graph_tool(project: &Path, request: GraphToolRequest) -> Result<String, ToolFailure> {
    let store = GraphStore::open_project(project).map_err(exec)?;
    let response = handle(&store, &request).map_err(exec)?;
    serde_json::to_string_pretty(&response).map_err(exec)
}

fn str_arg(args: &Value, key: &str) -> Result<String, ToolFailure> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| ToolFailure::Protocol(format!("missing string argument: {key}")))
}

fn u32_arg(args: &Value, key: &str, default: u32) -> u32 {
    args.get(key)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(default)
}

fn reply(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error_reply(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn parse_error(message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": message } })
}

fn write_message(out: &mut impl Write, message: &Value) -> Result<()> {
    let line = serde_json::to_string(message)?;
    out.write_all(line.as_bytes())?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use localmind_core::{
        CandidateLesson, Confidence, LessonCategory, LessonId, ReviewAction, ReviewDecision,
        SessionId, SuggestedAction,
    };
    use localmind_store::ReviewQueue;

    /// Promote `count` accepted memories that all share a searchable term.
    fn seeded_project(count: usize) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp project");
        std::fs::write(
            dir.path().join(".localmind.toml"),
            "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n",
        )
        .expect("config");
        let queue = ReviewQueue::open_project(dir.path()).expect("queue");
        let session = SessionId::new("fixture-session");
        // Each summary shares one searchable term (`wombat`) but is otherwise
        // topically distinct, so the queue's near-duplicate merge keeps all of
        // them as separate candidates.
        let topics = [
            "cache the cargo registry between builds",
            "index sqlite columns used in joins",
            "bound tokio channel capacity explicitly",
            "keep prompt templates under version control",
            "shard slow suites across runners",
            "treat clippy warnings as blocking errors",
            "verify offline links before publishing docs",
            "redraw fixed bands only on terminal resize",
            "pin serde schema versions in fixtures",
            "attach tracing spans to background jobs",
            "normalize verbatim paths before spawning",
            "budget review queues with per-family caps",
        ];
        let candidates: Vec<CandidateLesson> = topics
            .iter()
            .take(count)
            .enumerate()
            .map(|(index, topic)| {
                CandidateLesson::new(
                    LessonId::new(format!("fixture-{index:04}")),
                    format!("Wombat rule {index}: {topic}."),
                    LessonCategory::Process,
                    Confidence::new(0.8).expect("confidence"),
                    SuggestedAction::PromoteToMemory,
                )
            })
            .collect();
        queue
            .enqueue_candidates(&session, &candidates)
            .expect("enqueue");
        let persistence = MemoryPersistence::open_project(dir.path()).expect("store");
        for item in queue.list().expect("list") {
            queue
                .decide(ReviewDecision {
                    item_id: item.id.clone(),
                    action: ReviewAction::Accept,
                    reviewer: "test".to_string(),
                    decided_at: None,
                    note: None,
                    replacement_summary: None,
                    evidence: Vec::new(),
                })
                .expect("accept");
            persistence.promote_review_item(&item.id).expect("promote");
        }
        dir
    }

    #[test]
    fn memory_search_bounds_results_to_the_default_limit() {
        let dir = seeded_project(12);
        let out = memory_search(dir.path(), &json!({ "query": "wombat" })).expect("search");
        assert_eq!(out.matches("\tscore=").count(), 8, "default limit is 8");
    }

    #[test]
    fn memory_search_limit_argument_overrides_the_default() {
        let dir = seeded_project(5);
        let out =
            memory_search(dir.path(), &json!({ "query": "wombat", "limit": 2 })).expect("search");
        assert_eq!(out.matches("\tscore=").count(), 2);
        // Each result line leads with the memory id, so a promising snippet can
        // be expanded through the id-based inspect surfaces.
        assert!(out.lines().next().is_some_and(|line| !line.is_empty()));
    }
}
