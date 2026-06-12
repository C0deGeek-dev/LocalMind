//! MCP surface for LocalMind.
//!
//! Transport-agnostic protocol shapes. The graph module defines the code-graph
//! query tools (names, request/response contracts, and a dispatcher over the
//! project store); a host MCP server mounts them by name.

mod graph;
mod skills;

pub use graph::{
    handle, tool_names, AnchoredKnowledge, GraphToolError, GraphToolRequest, GraphToolResponse,
    SymbolSummary, TOOL_SYMBOL_CONNECTION, TOOL_SYMBOL_COVERAGE, TOOL_SYMBOL_KNOWLEDGE,
    TOOL_SYMBOL_NEIGHBORHOOD,
};
pub use skills::{
    list_active_skills, ActiveSkillSummary, SkillToolError, TOOL_SKILL_FETCH, TOOL_SKILL_LIST,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpSurface {
    pub resources: bool,
    pub tools: bool,
    pub prompts: bool,
}

impl McpSurface {
    #[must_use]
    pub fn planned() -> Self {
        Self {
            resources: true,
            tools: true,
            prompts: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::McpSurface;

    #[test]
    fn mcp_surface_is_planned_but_not_core_behavior() {
        let surface = McpSurface::planned();

        assert!(surface.resources);
        assert!(surface.tools);
    }
}
