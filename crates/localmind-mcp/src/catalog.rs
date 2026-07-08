//! Static tool catalog for MCP `tools/list`.
//!
//! Pure data: each entry is a tool's wire name, human description, and JSON
//! Schema for its arguments. A host serialises these directly into a
//! `tools/list` result. Shapes here mirror the request contracts in `graph`
//! and `skills`; keeping the catalog in this crate keeps the server thin.

use serde::Serialize;
use serde_json::{json, Value};

use crate::graph::{
    TOOL_SYMBOL_CONNECTION, TOOL_SYMBOL_COVERAGE, TOOL_SYMBOL_KNOWLEDGE, TOOL_SYMBOL_NEIGHBORHOOD,
};
use crate::skills::{TOOL_SKILL_FETCH, TOOL_SKILL_LIST};

/// Wire name of the accepted-memory keyword search tool.
pub const TOOL_MEMORY_SEARCH: &str = "memory_search";
/// Wire name of the agent context-pack export tool.
pub const TOOL_MEMORY_CONTEXT_EXPORT: &str = "memory_context_export";
/// Wire name of the semantic documentation search tool.
pub const TOOL_DOC_SEARCH: &str = "doc_search";

/// One tool advertised in a `tools/list` response.
#[derive(Clone, Debug, Serialize)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
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
            description: "Search accepted LocalMind memory by keyword. Returns matching memory ids, scores, paths, and snippets.",
            input_schema: object_schema(
                json!({ "query": { "type": "string", "description": "Search query." } }),
                &["query"],
            ),
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
        },
        ToolSpec {
            name: TOOL_SYMBOL_COVERAGE,
            description: "Tests attached to a code symbol.",
            input_schema: object_schema(json!({ "symbol": { "type": "string" } }), &["symbol"]),
        },
        ToolSpec {
            name: TOOL_SYMBOL_KNOWLEDGE,
            description: "Accepted knowledge (memory) anchored to a code symbol.",
            input_schema: object_schema(json!({ "symbol": { "type": "string" } }), &["symbol"]),
        },
        ToolSpec {
            name: TOOL_SKILL_LIST,
            description: "List active LocalMind skills with id, name, and body.",
            input_schema: object_schema(json!({}), &[]),
        },
        ToolSpec {
            name: TOOL_SKILL_FETCH,
            description: "Fetch one active LocalMind skill by id.",
            input_schema: object_schema(
                json!({ "id": { "type": "string", "description": "Skill id from the list tool." } }),
                &["id"],
            ),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::catalog;

    #[test]
    fn catalog_lists_all_tools_with_schemas() {
        let tools = catalog();
        assert_eq!(tools.len(), 9);
        for tool in &tools {
            assert!(!tool.name.is_empty());
            assert_eq!(tool.input_schema["type"], "object");
        }
    }
}
