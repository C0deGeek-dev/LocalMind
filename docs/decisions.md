# Decisions

Durable, engine-internal architecture decisions for LocalMind. Host-side
decisions live with the host; this file records choices that hold regardless
of which host embeds the engine.

## D-LM-0001 — Code-structure graph is built natively on tree-sitter

- **Date**: 2026-06-11
- **Status**: accepted

The code-structure side of the graph knowledge layer (vision §5) is produced
by a native ingester crate in this workspace using tree-sitter and the
official language grammars (Rust first), persisting into the existing SQLite
store and queried through the existing search and MCP surfaces.

LocalMind does not consume code graphs from external tools, over MCP or
otherwise: extraction must stay offline, deterministic, and attributable, and
an external tool's output shape would couple the store to a foreign schema.
The tree-sitter C-grammar build was verified on Windows, Linux, and macOS
under the workspace's minimum supported Rust version before this decision was
accepted.
