//! Future MCP surface for LocalMind.
//!
//! The first MVP is CLI-first. This crate exists so MCP-specific protocol code
//! has a home without pulling transport concerns into `localmind-core`.

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
