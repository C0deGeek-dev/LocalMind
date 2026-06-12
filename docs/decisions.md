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

## D-LM-0002 — Inference uses a local OpenAI-compatible endpoint

- **Date**: 2026-06-12
- **Status**: accepted

LocalMind's model-backed features use an optional `[inference]` section in
`.localmind.toml`. The endpoint shape is OpenAI-compatible chat completions
and embeddings, but the endpoint is user-configured and local-first. LocalMind
does not depend on LocalBox, LocalPilot, or a remote service.

When `[inference]` is absent, the deterministic extractor, review queue,
search, skills, and batch jobs remain usable without network access. Inference
audit rows store feature, endpoint kind, model id, and token counts when the
server returns them; they never store raw content.

## D-LM-0003 — Vector storage is a rebuildable SQLite BLOB table

- **Date**: 2026-06-12
- **Status**: accepted

Semantic retrieval stores f32 vectors as little-endian BLOBs in
`vector_index` inside `.localmind/localmind.sqlite`. Similarity is exact
cosine in Rust. The vector index is never the source of truth: accepted
Markdown memory and graph rows can rebuild it, and delete-healing removes
memory vector rows with the rest of a memory delete.

This rejects sqlite extension dependencies and sidecar vector files for now:
the expected corpus size is small, workspace policy forbids unsafe extension
registration, and keeping one schema-versioned artifact makes repair easier.

## D-LM-0004 — Distillation and research are review-routed batch jobs

- **Date**: 2026-06-12
- **Status**: accepted

Distillation and research are explicit batch operations over accepted memory
and graph context. They are inert without configured chat inference. When run,
they produce review candidates and then apply the configured review mode; they
do not write accepted memory directly outside the review-mode rules.
