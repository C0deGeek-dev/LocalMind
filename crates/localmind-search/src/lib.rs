//! Retrieval and search boundary.
//!
//! One search API over the code-structure graph and accepted memory,
//! ranked deterministically (graph proximity, half-life recency, query-term
//! match) with no model and no network.

mod rank;
mod workspace;

pub use rank::{combined_score, proximity_score, temporal_score, RankingConfig, SearchWeights};
pub use workspace::{search_workspace, RankedHit, SearchHitKind, WorkspaceQuery};

use localmind_core::ContextQuery;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SearchError {
    #[error("invalid ranking weights: {detail}")]
    InvalidWeights { detail: String },
    #[error(transparent)]
    Graph(#[from] localmind_store::GraphStoreError),
    #[error(transparent)]
    Memory(#[from] localmind_store::MemoryPersistenceError),
}

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
            graph: true,
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
    fn mvp_search_runs_on_local_indexes_and_the_graph() {
        let capabilities = SearchCapabilities::mvp();

        assert!(capabilities.keyword);
        assert!(capabilities.sqlite_fts);
        assert!(capabilities.graph);
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
