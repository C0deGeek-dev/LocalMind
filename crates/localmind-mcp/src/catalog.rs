//! Static tool catalog for MCP `tools/list`.
//!
//! Pure data: each entry is a tool's wire name, human description, and JSON
//! Schema for its arguments. A host serialises these directly into a
//! `tools/list` result. Shapes here mirror the request contracts in `graph`
//! and `skills`; keeping the catalog in this crate keeps the server thin.

use serde::Serialize;
use serde_json::{json, Value};

use localmind_store::{
    PROPOSAL_BODY_MAX_CHARS, PROPOSAL_CATEGORY_MAX_CHARS, PROPOSAL_EVIDENCE_MAX_CHARS,
    PROPOSAL_KEY_MAX_CHARS, PROPOSAL_MAX_RELATED_FILES, PROPOSAL_MAX_TAGS,
    PROPOSAL_RELATED_FILE_MAX_CHARS, PROPOSAL_TAG_MAX_CHARS, PROPOSAL_TITLE_MAX_CHARS,
};

use crate::graph::{
    TOOL_SYMBOL_CONNECTION, TOOL_SYMBOL_COVERAGE, TOOL_SYMBOL_KNOWLEDGE, TOOL_SYMBOL_NEIGHBORHOOD,
};
use crate::skills::{TOOL_SKILL_FETCH, TOOL_SKILL_LIST};

/// Wire name of the accepted-memory keyword search tool.
pub const TOOL_MEMORY_SEARCH: &str = "memory_search";
/// Wire name of the agent context-pack export tool.
pub const TOOL_MEMORY_CONTEXT_EXPORT: &str = "memory_context_export";
/// Wire name of the review-gated lesson proposal tool.
pub const TOOL_MEMORY_PROPOSE: &str = "memory_propose";
/// Wire name of the read-only store-readiness tool.
pub const TOOL_MEMORY_STATUS: &str = "memory_status";
/// Wire name of the semantic documentation search tool.
pub const TOOL_DOC_SEARCH: &str = "doc_search";

/// One tool advertised in a `tools/list` response.
#[derive(Clone, Debug, Serialize)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
    #[serde(rename = "outputSchema", skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<ToolAnnotations>,
}

/// MCP risk hints. They describe behavior to clients; they do not authorize it.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolAnnotations {
    pub read_only_hint: bool,
    pub destructive_hint: bool,
    pub idempotent_hint: bool,
    pub open_world_hint: bool,
}

fn object_schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
    })
}

/// Every tool the LocalMind MCP server exposes, in a stable order.
#[must_use]
pub fn catalog() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: TOOL_MEMORY_SEARCH,
            description: "Search accepted LocalMind memory by keyword. Returns matching memory ids, scores, paths, and match-centred snippets.",
            input_schema: object_schema(
                json!({
                    "query": { "type": "string", "description": "Search query." },
                    "limit": { "type": "integer", "minimum": 1, "description": "Max results (default 8)." }
                }),
                &["query"],
            ),
            output_schema: None,
            annotations: None,
        },
        ToolSpec {
            name: TOOL_MEMORY_CONTEXT_EXPORT,
            description: "Export an agent-ready context pack (accepted memory plus suggested skills) for a query.",
            input_schema: object_schema(
                json!({
                    "query": { "type": "string", "description": "Task or question to pull memory for." },
                    "target": {
                        "type": "string",
                        "enum": ["generic", "claude-code", "open-ai-codex", "localpilot"],
                        "description": "Formatting target. Defaults to claude-code."
                    }
                }),
                &["query"],
            ),
            output_schema: None,
            annotations: None,
        },
        ToolSpec {
            name: TOOL_MEMORY_PROPOSE,
            description: "Propose one distilled lesson to LocalMind's human review queue. This never accepts or promotes memory automatically; retries with identical arguments are idempotent.",
            input_schema: object_schema(
                json!({
                    "title": {
                        "type": "string", "minLength": 1,
                        "maxLength": PROPOSAL_TITLE_MAX_CHARS,
                        "description": "One-line reusable lesson."
                    },
                    "source": {
                        "type": "string", "minLength": 1,
                        "maxLength": PROPOSAL_KEY_MAX_CHARS,
                        "description": "Calling agent id, such as claude-code or open-ai-codex."
                    },
                    "body": {
                        "type": "string", "maxLength": PROPOSAL_BODY_MAX_CHARS,
                        "description": "Why the lesson holds or how to apply it."
                    },
                    "category": {
                        "type": "string", "maxLength": PROPOSAL_CATEGORY_MAX_CHARS,
                        "description": "LessonCategory name; defaults to Process."
                    },
                    "scope": {
                        "type": "string", "enum": ["project", "global"],
                        "description": "Destination after human acceptance; defaults to project."
                    },
                    "related_files": {
                        "type": "array", "maxItems": PROPOSAL_MAX_RELATED_FILES,
                        "items": { "type": "string", "maxLength": PROPOSAL_RELATED_FILE_MAX_CHARS }
                    },
                    "tags": {
                        "type": "array", "maxItems": PROPOSAL_MAX_TAGS,
                        "items": { "type": "string", "maxLength": PROPOSAL_TAG_MAX_CHARS }
                    },
                    "evidence": {
                        "type": "string", "maxLength": PROPOSAL_EVIDENCE_MAX_CHARS,
                        "description": "Source pointer or bounded excerpt shown to the reviewer, never promoted as memory text."
                    },
                    "idempotency_key": {
                        "type": "string", "minLength": 1,
                        "maxLength": PROPOSAL_KEY_MAX_CHARS,
                        "description": "Stable retry key for this proposal."
                    },
                    "confidence": {
                        "type": "number", "minimum": 0, "maximum": 1,
                        "description": "Author confidence; defaults to 0.7."
                    }
                }),
                &["title", "source"],
            ),
            output_schema: Some(object_schema(
                json!({
                    "candidate_id": { "type": "string" },
                    "created": { "type": "boolean" },
                    "changed": { "type": "boolean" },
                    "duplicate_of": { "type": ["string", "null"] },
                    "quality_note": { "type": ["string", "null"] }
                }),
                &["candidate_id", "created", "changed", "duplicate_of", "quality_note"],
            )),
            annotations: Some(ToolAnnotations {
                read_only_hint: false,
                destructive_hint: false,
                idempotent_hint: true,
                open_world_hint: false,
            }),
        },
        ToolSpec {
            name: TOOL_MEMORY_STATUS,
            description: "Read-only readiness snapshot of this project's LocalMind store: whether learning is enabled, accepted-memory and pending-review counts, and doc-index counts. No writes, no network.",
            input_schema: object_schema(json!({}), &[]),
            output_schema: Some(object_schema(
                json!({
                    "ready": { "type": "boolean" },
                    "learning_enabled": { "type": "boolean" },
                    "inference_configured": { "type": "boolean" },
                    "accepted_project": { "type": "integer" },
                    "accepted_global": { "type": "integer" },
                    "pending_review": { "type": "integer" },
                    "doc_chunks": { "type": "integer" },
                    "doc_vectors": { "type": "integer" },
                    "schema_version": { "type": "integer" },
                    "notes": { "type": "array", "items": { "type": "string" } }
                }),
                &[
                    "ready",
                    "learning_enabled",
                    "inference_configured",
                    "accepted_project",
                    "accepted_global",
                    "pending_review",
                    "doc_chunks",
                    "doc_vectors",
                    "schema_version",
                    "notes",
                ],
            )),
            annotations: Some(ToolAnnotations {
                read_only_hint: true,
                destructive_hint: false,
                idempotent_hint: true,
                open_world_hint: false,
            }),
        },
        ToolSpec {
            name: TOOL_DOC_SEARCH,
            description: "Semantic search over ingested repository documentation. Returns the most relevant doc passages (path, heading, text) by meaning, not keyword.",
            input_schema: object_schema(
                json!({
                    "query": { "type": "string", "description": "Natural-language query." },
                    "limit": { "type": "integer", "minimum": 1, "description": "Max passages (default 5)." }
                }),
                &["query"],
            ),
            output_schema: None,
            annotations: None,
        },
        ToolSpec {
            name: TOOL_SYMBOL_NEIGHBORHOOD,
            description: "Graph neighbours of a code symbol within a hop bound.",
            input_schema: object_schema(
                json!({
                    "symbol": { "type": "string", "description": "Symbol name or qualified name." },
                    "depth": { "type": "integer", "minimum": 1, "description": "Hop bound (default 2)." }
                }),
                &["symbol"],
            ),
            output_schema: None,
            annotations: None,
        },
        ToolSpec {
            name: TOOL_SYMBOL_CONNECTION,
            description: "Shortest graph connection between two code symbols.",
            input_schema: object_schema(
                json!({
                    "from": { "type": "string", "description": "Source symbol." },
                    "to": { "type": "string", "description": "Target symbol." },
                    "max_hops": { "type": "integer", "minimum": 1, "description": "Hop bound (default 6)." }
                }),
                &["from", "to"],
            ),
            output_schema: None,
            annotations: None,
        },
        ToolSpec {
            name: TOOL_SYMBOL_COVERAGE,
            description: "Tests attached to a code symbol.",
            input_schema: object_schema(json!({ "symbol": { "type": "string" } }), &["symbol"]),
            output_schema: None,
            annotations: None,
        },
        ToolSpec {
            name: TOOL_SYMBOL_KNOWLEDGE,
            description: "Accepted knowledge (memory) anchored to a code symbol.",
            input_schema: object_schema(json!({ "symbol": { "type": "string" } }), &["symbol"]),
            output_schema: None,
            annotations: None,
        },
        ToolSpec {
            name: TOOL_SKILL_LIST,
            description: "List active LocalMind skills with id, name, and body.",
            input_schema: object_schema(json!({}), &[]),
            output_schema: None,
            annotations: None,
        },
        ToolSpec {
            name: TOOL_SKILL_FETCH,
            description: "Fetch one active LocalMind skill by id.",
            input_schema: object_schema(
                json!({ "id": { "type": "string", "description": "Skill id from the list tool." } }),
                &["id"],
            ),
            output_schema: None,
            annotations: None,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::{catalog, TOOL_MEMORY_PROPOSE, TOOL_MEMORY_STATUS};

    #[test]
    fn catalog_lists_all_tools_with_schemas() {
        let tools = catalog();
        assert_eq!(tools.len(), 11);
        for tool in &tools {
            assert!(!tool.name.is_empty());
            assert_eq!(tool.input_schema["type"], "object");
        }
    }

    #[test]
    fn status_tool_is_read_only_with_a_structured_output() -> Result<(), Box<dyn std::error::Error>>
    {
        let Some(tool) = catalog()
            .into_iter()
            .find(|tool| tool.name == TOOL_MEMORY_STATUS)
        else {
            return Err("status tool missing from catalog".into());
        };
        let wire = serde_json::to_value(tool)?;
        assert_eq!(wire["annotations"]["readOnlyHint"], true);
        assert_eq!(
            wire["outputSchema"]["properties"]["pending_review"]["type"],
            "integer"
        );
        assert_eq!(
            wire["outputSchema"]["properties"]["ready"]["type"],
            "boolean"
        );
        Ok(())
    }

    #[test]
    fn proposal_tool_advertises_bounded_input_structured_output_and_risk_hints(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Some(tool) = catalog()
            .into_iter()
            .find(|tool| tool.name == TOOL_MEMORY_PROPOSE)
        else {
            return Err("proposal tool missing from catalog".into());
        };
        let wire = serde_json::to_value(tool)?;

        assert_eq!(
            wire["inputSchema"]["required"],
            serde_json::json!(["title", "source"])
        );
        assert_eq!(
            wire["inputSchema"]["properties"]["scope"]["enum"],
            serde_json::json!(["project", "global"])
        );
        assert_eq!(
            wire["outputSchema"]["properties"]["created"]["type"],
            "boolean"
        );
        assert_eq!(wire["annotations"]["readOnlyHint"], false);
        assert_eq!(wire["annotations"]["destructiveHint"], false);
        assert_eq!(wire["annotations"]["idempotentHint"], true);
        assert_eq!(wire["annotations"]["openWorldHint"], false);
        Ok(())
    }
}
