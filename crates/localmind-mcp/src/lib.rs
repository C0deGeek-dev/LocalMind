//! MCP surface for LocalMind.
//!
//! Transport-agnostic protocol shapes. The graph module defines the code-graph
//! query tools (names, request/response contracts, and a dispatcher over the
//! project store); a host MCP server mounts them by name.

mod catalog;
mod graph;
mod skills;

pub use catalog::{catalog, ToolSpec, TOOL_MEMORY_CONTEXT_EXPORT, TOOL_MEMORY_SEARCH};
pub use graph::{
    handle, tool_names, AnchoredKnowledge, GraphToolError, GraphToolRequest, GraphToolResponse,
    SymbolSummary, TOOL_SYMBOL_CONNECTION, TOOL_SYMBOL_COVERAGE, TOOL_SYMBOL_KNOWLEDGE,
    TOOL_SYMBOL_NEIGHBORHOOD,
};
pub use skills::{
    fetch_active_skill, list_active_skills, ActiveSkillSummary, SkillToolError, TOOL_SKILL_FETCH,
    TOOL_SKILL_LIST,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpSurface {
    pub resources: bool,
    pub tools: bool,
    pub prompts: bool,
}

impl McpSurface {
    /// The MCP surface LocalMind *plans* to expose: resources and tools, no
    /// prompts.
    ///
    /// Library-only/unwired: this descriptor is the documented "future home"
    /// boundary shape (see the topology note in `vision.md`). No host mounts an
    /// `McpSurface` yet, so only this crate's tests reference it; it is retained
    /// (not deleted) as the declared intent the graph/skills tools above already
    /// implement.
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
