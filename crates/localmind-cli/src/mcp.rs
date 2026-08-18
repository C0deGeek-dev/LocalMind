//! Hand-rolled stdio MCP server: newline-delimited JSON-RPC 2.0 over
//! stdin/stdout.
//!
//! No async runtime and no MCP SDK — one blocking read loop. Each line of
//! stdin is one JSON-RPC message; each response is one line of stdout. The
//! server exposes LocalMind's query tools plus one bounded, review-only lesson
//! proposal tool over the catalog declared in `localmind_mcp`.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::Result;
use localmind_mcp::{
    catalog, fetch_active_skill, handle, list_active_skills, GraphToolRequest, TOOL_DOC_SEARCH,
    TOOL_MEMORY_CONTEXT_EXPORT, TOOL_MEMORY_PROPOSE, TOOL_MEMORY_SEARCH, TOOL_MEMORY_STATUS,
    TOOL_SKILL_FETCH, TOOL_SKILL_LIST, TOOL_SYMBOL_CONNECTION, TOOL_SYMBOL_COVERAGE,
    TOOL_SYMBOL_KNOWLEDGE, TOOL_SYMBOL_NEIGHBORHOOD,
};
use localmind_store::{
    ContextExportTarget, ContextExporter, DocSearchStatus, GraphStore, MemoryPersistence,
    ProposeScope, ProposedLesson, ReviewQueue,
};
use serde_json::{json, Value};

/// MCP protocol revision this server speaks.
const PROTOCOL_VERSION: &str = "2025-06-18";
/// One stdio server process corresponds to one MCP client session. Bound writes
/// there so a looping agent cannot flood the human review queue indefinitely.
const MAX_PROPOSALS_PER_SESSION: usize = 20;

#[derive(Default)]
struct McpSessionState {
    proposals: usize,
}

struct ToolOutput {
    text: String,
    structured: Option<Value>,
}

impl ToolOutput {
    fn text(text: String) -> Self {
        Self {
            text,
            structured: None,
        }
    }

    fn structured(value: Value) -> Result<Self, ToolFailure> {
        let text = serde_json::to_string_pretty(&value).map_err(exec)?;
        Ok(Self {
            text,
            structured: Some(value),
        })
    }
}

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
    let mut state = McpSessionState::default();

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
        let response = dispatch(&project, method, &params, id, &mut state);
        write_message(&mut writer, &response)?;
    }

    Ok(())
}

fn dispatch(
    project: &Path,
    method: &str,
    params: &Value,
    id: Value,
    state: &mut McpSessionState,
) -> Value {
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
        "tools/call" => match call_tool(project, params, state) {
            Ok(output) => {
                let mut result = json!({
                    "content": [{ "type": "text", "text": output.text }],
                    "isError": false
                });
                if let Some(structured) = output.structured {
                    result["structuredContent"] = structured;
                }
                reply(id, result)
            }
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

fn call_tool(
    project: &Path,
    params: &Value,
    state: &mut McpSessionState,
) -> Result<ToolOutput, ToolFailure> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolFailure::Protocol("tools/call is missing a tool name".to_string()))?;
    let empty = Value::Object(serde_json::Map::new());
    let args = params.get("arguments").unwrap_or(&empty);

    match name {
        TOOL_MEMORY_SEARCH => memory_search(project, args).map(ToolOutput::text),
        TOOL_MEMORY_CONTEXT_EXPORT => memory_context_export(project, args).map(ToolOutput::text),
        TOOL_MEMORY_PROPOSE => memory_propose(project, args, state),
        TOOL_MEMORY_STATUS => memory_status(project),
        TOOL_DOC_SEARCH => doc_search(project, args).map(ToolOutput::text),
        TOOL_SYMBOL_NEIGHBORHOOD => graph_tool(
            project,
            GraphToolRequest::MemorySymbolNeighborhood {
                symbol: str_arg(args, "symbol")?,
                depth: u32_arg(args, "depth", 2),
            },
        )
        .map(ToolOutput::text),
        TOOL_SYMBOL_CONNECTION => graph_tool(
            project,
            GraphToolRequest::MemorySymbolConnection {
                from: str_arg(args, "from")?,
                to: str_arg(args, "to")?,
                max_hops: u32_arg(args, "max_hops", 6),
            },
        )
        .map(ToolOutput::text),
        TOOL_SYMBOL_COVERAGE => graph_tool(
            project,
            GraphToolRequest::MemorySymbolCoverage {
                symbol: str_arg(args, "symbol")?,
            },
        )
        .map(ToolOutput::text),
        TOOL_SYMBOL_KNOWLEDGE => graph_tool(
            project,
            GraphToolRequest::MemorySymbolKnowledge {
                symbol: str_arg(args, "symbol")?,
            },
        )
        .map(ToolOutput::text),
        TOOL_SKILL_LIST => {
            let skills = list_active_skills(project).map_err(exec)?;
            serde_json::to_string_pretty(&skills)
                .map(ToolOutput::text)
                .map_err(exec)
        }
        TOOL_SKILL_FETCH => {
            let id = str_arg(args, "id")?;
            match fetch_active_skill(project, &id).map_err(exec)? {
                Some(skill) => serde_json::to_string_pretty(&skill)
                    .map(ToolOutput::text)
                    .map_err(exec),
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

fn memory_propose(
    project: &Path,
    args: &Value,
    state: &mut McpSessionState,
) -> Result<ToolOutput, ToolFailure> {
    if state.proposals >= MAX_PROPOSALS_PER_SESSION {
        return Err(ToolFailure::Execution(format!(
            "this MCP session reached its proposal limit ({MAX_PROPOSALS_PER_SESSION}); review or restart the session before proposing more"
        )));
    }
    let scope = match optional_str_arg(args, "scope")?.as_deref() {
        None | Some("project") => ProposeScope::Project,
        Some("global") => ProposeScope::Global,
        Some(other) => {
            return Err(ToolFailure::Protocol(format!(
                "invalid scope {other:?}; expected project or global"
            )));
        }
    };
    let confidence = number_arg(args, "confidence", 0.7)?;
    if !(0.0..=1.0).contains(&confidence) {
        return Err(ToolFailure::Protocol(
            "confidence must be between 0 and 1".to_string(),
        ));
    }
    let proposal = ProposedLesson {
        title: str_arg(args, "title")?,
        body: optional_str_arg(args, "body")?,
        category: optional_str_arg(args, "category")?.unwrap_or_else(|| "Process".to_string()),
        scope,
        related_files: string_array_arg(args, "related_files")?,
        tags: string_array_arg(args, "tags")?,
        evidence: optional_str_arg(args, "evidence")?,
        idempotency_key: optional_str_arg(args, "idempotency_key")?,
        confidence,
    };
    let outcome = ReviewQueue::open_project(project)
        .map_err(exec)?
        .propose(&str_arg(args, "source")?, &proposal)
        .map_err(exec)?;
    if outcome.changed {
        state.proposals += 1;
    }
    ToolOutput::structured(json!({
        "candidate_id": outcome.candidate_id,
        "created": outcome.created,
        "changed": outcome.changed,
        "duplicate_of": outcome.duplicate_of,
        "quality_note": outcome.quality_note,
    }))
}

/// Read-only readiness snapshot of this server's project store — the same facts
/// `localmind status` reports, so an agent can check learning is on and the
/// review backlog before it proposes. No writes, no network.
fn memory_status(project: &Path) -> Result<ToolOutput, ToolFailure> {
    // Best-effort snapshot, shared with the CLI `status` command: a not-ready
    // project (missing/invalid config, unopenable store) returns a structured
    // `{ready:false, …}`, never a tool error. Read-only.
    let snapshot = localmind_store::StatusSnapshot::read(project);
    ToolOutput::structured(json!({
        "ready": snapshot.ready,
        "learning_enabled": snapshot.learning_enabled,
        "inference_configured": snapshot.inference_configured,
        "accepted_project": snapshot.accepted_project,
        "accepted_global": snapshot.accepted_global,
        "pending_review": snapshot.pending_review,
        "doc_chunks": snapshot.doc_chunks,
        "doc_vectors": snapshot.doc_vectors,
        "schema_version": snapshot.schema_version,
        "notes": snapshot.notes,
    }))
}

fn doc_search(project: &Path, args: &Value) -> Result<String, ToolFailure> {
    let query = str_arg(args, "query")?;
    let limit = usize::try_from(u32_arg(args, "limit", 5)).unwrap_or(5);
    let persistence = MemoryPersistence::open_project(project).map_err(exec)?;
    let report = persistence
        .doc_search_diagnosed(&query, limit)
        .map_err(exec)?;
    // Each capability state gets its own message, so an empty answer tells the
    // user which prerequisite is missing instead of one ambiguous "no match".
    match report.status {
        DocSearchStatus::NoDocChunks => {
            return Ok(
                "No documentation has been ingested yet (the doc index is empty). Run \
                 `localmind ingest docs <path>` to build it."
                    .to_string(),
            );
        }
        DocSearchStatus::EmbeddingsNotConfigured => {
            return Ok(
                "Semantic doc search needs embeddings: configure [inference] \
                 embedding_base_url + embedding_model in .localmind.toml."
                    .to_string(),
            );
        }
        DocSearchStatus::EmbeddingEndpointUnavailable { error } => {
            return Ok(format!(
                "The embedding endpoint is configured but unreachable ({error}). Start it \
                 (e.g. `localbox embed-serve`) and retry."
            ));
        }
        DocSearchStatus::NoDocVectors => {
            return Ok(
                "Documentation passages exist but none carries an embedding. Re-run \
                 `localmind ingest docs <path>` with the embedding endpoint reachable."
                    .to_string(),
            );
        }
        DocSearchStatus::IndexMismatch {
            indexed_models,
            query_dimensions,
        } => {
            return Ok(format!(
                "The doc index was embedded under a different model/dimensions than the \
                 active embedding model produces (indexed: {}; query dimensions: \
                 {query_dimensions}). Re-run `localmind ingest docs <path>` to re-embed.",
                indexed_models.join(", ")
            ));
        }
        DocSearchStatus::Searched => {}
    }
    if report.results.is_empty() {
        return Ok("No documentation matched this query.".to_string());
    }
    let mut out = String::new();
    for result in report.results {
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

fn optional_str_arg(args: &Value, key: &str) -> Result<Option<String>, ToolFailure> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(ToolFailure::Protocol(format!(
            "argument {key} must be a string"
        ))),
    }
}

fn string_array_arg(args: &Value, key: &str) -> Result<Vec<String>, ToolFailure> {
    let Some(value) = args.get(key) else {
        return Ok(Vec::new());
    };
    let values = value
        .as_array()
        .ok_or_else(|| ToolFailure::Protocol(format!("argument {key} must be an array")))?;
    values
        .iter()
        .map(|value| {
            value.as_str().map(str::to_string).ok_or_else(|| {
                ToolFailure::Protocol(format!("argument {key} must contain strings"))
            })
        })
        .collect()
}

fn number_arg(args: &Value, key: &str, default: f32) -> Result<f32, ToolFailure> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(value) => value
            .as_f64()
            .map(|value| value as f32)
            .ok_or_else(|| ToolFailure::Protocol(format!("argument {key} must be a number"))),
    }
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

    #[test]
    fn memory_propose_json_rpc_is_structured_idempotent_and_human_gated() {
        let dir = tempfile::tempdir().expect("project");
        std::fs::write(
            dir.path().join(".localmind.toml"),
            "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n\n[review]\nmode = \"automatic\"\n",
        )
        .expect("config");
        let params = json!({
            "name": TOOL_MEMORY_PROPOSE,
            "arguments": {
                "title": "Keep MCP write tools retry-safe",
                "body": "Stable idempotency keys prevent duplicate review work.",
                "source": "open-ai-codex",
                "scope": "project",
                "tags": ["mcp", "review"],
                "evidence": "docs/decisions.md#D-LM-0033",
                "idempotency_key": "codex-turn-42-lesson-1",
                "confidence": 0.9
            }
        });
        let mut state = McpSessionState::default();

        let first = dispatch(dir.path(), "tools/call", &params, json!(1), &mut state);
        assert_eq!(first["result"]["isError"], false);
        assert_eq!(first["result"]["structuredContent"]["created"], true);
        assert_eq!(first["result"]["structuredContent"]["changed"], true);
        let candidate_id = first["result"]["structuredContent"]["candidate_id"]
            .as_str()
            .expect("candidate id")
            .to_string();
        assert!(first["result"]["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains(&candidate_id)));

        let second = dispatch(dir.path(), "tools/call", &params, json!(2), &mut state);
        assert_eq!(second["result"]["structuredContent"]["created"], false);
        assert_eq!(second["result"]["structuredContent"]["changed"], false);
        assert_eq!(
            second["result"]["structuredContent"]["candidate_id"],
            candidate_id
        );

        let queue = ReviewQueue::open_project(dir.path()).expect("queue");
        let item = queue
            .get(&localmind_core::ReviewItemId::new(candidate_id))
            .expect("read candidate")
            .expect("candidate exists");
        assert_eq!(item.state, localmind_core::ReviewState::Pending);
        assert_eq!(item.seen_count, 1, "an exact retry has no write effect");
        assert_eq!(item.candidate.source.as_deref(), Some("open-ai-codex"));
        assert_eq!(
            item.candidate.evidence_text.as_deref(),
            Some("docs/decisions.md#D-LM-0033")
        );

        let report = localmind_store::ReviewModeProcessor::apply_project(dir.path())
            .expect("automatic review pass");
        assert_eq!(report.accepted, 0);
        assert_eq!(report.manual, 1);
    }

    #[test]
    fn memory_status_reports_readonly_store_facts_without_consuming_the_cap() {
        let dir = tempfile::tempdir().expect("project");
        std::fs::write(
            dir.path().join(".localmind.toml"),
            "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n",
        )
        .expect("config");
        let mut state = McpSessionState::default();
        // Seed one pending proposal so the pending count is observable.
        let propose = json!({
            "name": TOOL_MEMORY_PROPOSE,
            "arguments": { "title": "seed lesson", "source": "cli" }
        });
        dispatch(dir.path(), "tools/call", &propose, json!(1), &mut state);
        assert_eq!(state.proposals, 1);

        let params = json!({ "name": TOOL_MEMORY_STATUS, "arguments": {} });
        let response = dispatch(dir.path(), "tools/call", &params, json!(2), &mut state);
        assert_eq!(response["result"]["isError"], false);
        let structured = &response["result"]["structuredContent"];
        assert_eq!(structured["ready"], true);
        assert_eq!(structured["learning_enabled"], true);
        assert_eq!(structured["pending_review"], 1);
        assert_eq!(structured["accepted_project"], 0);
        assert_eq!(structured["accepted_global"], 0);
        assert_eq!(structured["doc_chunks"], 0);
        assert!(structured["schema_version"].as_i64().unwrap() >= 11);
        // Read-only: a status call must not consume the proposal write cap.
        assert_eq!(state.proposals, 1);
    }

    #[test]
    fn memory_status_of_an_unconfigured_project_is_structured_not_ready_not_an_error() {
        // A missing/invalid config must return structured {ready:false, …},
        // never a tool error — the advertised best-effort state.
        let dir = tempfile::tempdir().expect("project"); // no .localmind.toml
        let mut state = McpSessionState::default();
        let params = json!({ "name": TOOL_MEMORY_STATUS, "arguments": {} });
        let response = dispatch(dir.path(), "tools/call", &params, json!(1), &mut state);
        assert_eq!(response["result"]["isError"], false, "{response}");
        let structured = &response["result"]["structuredContent"];
        assert_eq!(structured["ready"], false);
        assert_eq!(structured["learning_enabled"], false);
        assert_eq!(structured["accepted_project"], 0);
        assert_eq!(structured["pending_review"], 0);
    }

    #[test]
    fn memory_propose_has_a_per_server_session_write_cap() {
        let dir = tempfile::tempdir().expect("project");
        std::fs::write(
            dir.path().join(".localmind.toml"),
            "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n",
        )
        .expect("config");
        let mut state = McpSessionState::default();
        for id in 0..MAX_PROPOSALS_PER_SESSION {
            let params = json!({
                "name": TOOL_MEMORY_PROPOSE,
                "arguments": {
                    "title": format!("Keep MCP write tool {id} bounded"),
                    "source": "open-ai-codex",
                    "idempotency_key": format!("bounded-proposal-{id}")
                }
            });
            let response = dispatch(dir.path(), "tools/call", &params, json!(id), &mut state);
            assert_eq!(response["result"]["isError"], false);
        }
        let params = json!({
            "name": TOOL_MEMORY_PROPOSE,
            "arguments": {
                "title": "Keep one more MCP write tool bounded",
                "source": "open-ai-codex",
                "idempotency_key": "bounded-proposal-over-limit"
            }
        });
        let blocked = dispatch(
            dir.path(),
            "tools/call",
            &params,
            json!(MAX_PROPOSALS_PER_SESSION),
            &mut state,
        );
        assert_eq!(blocked["result"]["isError"], true);
        assert!(blocked["result"]["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("proposal limit")));
        assert_eq!(state.proposals, MAX_PROPOSALS_PER_SESSION);
        let items = ReviewQueue::open_project(dir.path())
            .unwrap()
            .list()
            .unwrap();
        assert_eq!(items.len(), 1, "near duplicates share one queue row");
        assert_eq!(items[0].seen_count, MAX_PROPOSALS_PER_SESSION as i64);
    }

    #[test]
    fn initialize_advertises_the_annotation_and_structured_output_revision() {
        let mut state = McpSessionState::default();
        let response = dispatch(
            Path::new("."),
            "initialize",
            &json!({}),
            json!(1),
            &mut state,
        );
        assert_eq!(response["result"]["protocolVersion"], "2025-06-18");
    }
}
