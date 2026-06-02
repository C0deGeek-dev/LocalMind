//! Retrieval and search boundary.

use localmind_core::ContextQuery;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchCapabilities {
    pub keyword: bool,
    pub sqlite_fts: bool,
    pub vector: bool,
    pub graph: bool,
}

impl SearchCapabilities {
    #[must_use]
    pub fn mvp() -> Self {
        Self {
            keyword: true,
            sqlite_fts: true,
            vector: false,
            graph: false,
        }
    }
}

pub fn query_needs_project_scope(query: &ContextQuery) -> bool {
    query.project_uri.is_some()
}

#[cfg(test)]
mod tests {
    use super::{query_needs_project_scope, SearchCapabilities};
    use localmind_core::ContextQuery;

    #[test]
    fn mvp_search_starts_with_local_indexes() {
        let capabilities = SearchCapabilities::mvp();

        assert!(capabilities.keyword);
        assert!(capabilities.sqlite_fts);
        assert!(!capabilities.vector);
    }

    #[test]
    fn context_queries_can_be_scoped_to_a_project() {
        let query = ContextQuery {
            text: "testing strategy".to_string(),
            project_uri: Some("file:///repo".to_string()),
            token_budget: Some(1_000),
        };

        assert!(query_needs_project_scope(&query));
    }
}
