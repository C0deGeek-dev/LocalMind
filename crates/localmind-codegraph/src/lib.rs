//! Deterministic code-structure ingestion for the LocalMind graph.
//!
//! The host hands this crate a list of files it is allowed to read; the
//! ingester walks nothing on its own. Parsing is AST-based (tree-sitter, Rust
//! first), offline, and deterministic: no network and no model in the
//! pipeline. Extracted nodes and edges carry the same provenance and
//! confidence vocabulary as lessons and persist through the graph store.

mod boundary;
mod ingest;
mod parse;
mod reindex;
mod resolve;

pub use boundary::{AdmittedFile, BoundaryRejection, IngestBoundary};
pub use ingest::{IngestReport, Ingester};
pub use parse::{CallSite, ParsedFile, RustParser, UsePath};
pub use reindex::{ReindexBatchReport, ReindexPlan, Reindexer};
pub use resolve::{resolve_edges, resolve_file_edges, ResolutionContext};

use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CodeGraphError {
    #[error("workspace root {path:?} is not usable: {source}")]
    InvalidRoot {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to read {path:?}: {source}")]
    ReadSource {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("the Rust grammar failed to load: {0}")]
    Grammar(String),
    #[error("confidence value out of range: {0}")]
    Confidence(#[from] localmind_core::ContractError),
    #[error(transparent)]
    Store(#[from] localmind_store::GraphStoreError),
}
