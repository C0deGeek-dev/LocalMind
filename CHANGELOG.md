# Changelog

Notable changes, newest first. Contract-relevant entries reference
`docs/on-disk-contract.md`.

## v0.2.0-beta.1 - 2026-06-12

Coordinated LocalX beta release.

- Added opt-in local OpenAI-compatible inference for chat completions and
  embeddings, with deterministic behavior when inference is not configured.
- Added schema-versioned vector storage and hybrid memory retrieval.
- Added model-backed extraction, review annotations, and manual/assisted/
  trusted/automatic review modes.
- Added active skill lifecycle records and MCP host surface.
- Added batch research and distillation jobs that route through the review
  pipeline.

## v0.1.0 — 2026-06-12

First tagged snapshot of the host-neutral learning engine.

- 3-OS CI (fmt, clippy `-D warnings`, nextest, doctests, check).
- Redaction: data-driven pattern table (AWS, Slack, JWT, GitHub, OpenAI-style
  keys, bearer headers, assignment/JSON/.env shapes, URL credentials) plus a
  Shannon-entropy backstop; corpus-tested in both directions; documented
  caught / not-caught guarantee.
- Persistence integrity: promote/persist/delete commit atomically; audit
  metadata is serde-built JSON; interrupted deletions heal on retry
  (including on Windows).
- Database schema versioned with a `PRAGMA user_version` stepper; the
  baseline step is a no-op on databases created before versioning existed;
  newer databases are refused with a typed error.
- Search serves from the FTS5 index (`MATCH` + bm25) with operator-safe
  query construction.
- Extraction: deterministic heuristics over explicit `Lesson:` markers,
  failure→resolution pairs, repeated commands, and user corrections, with a
  per-family cap and a fixture-backed acceptance bar (vision.md).
- Code-structure graph: schema/store, tree-sitter ingester, incremental
  reindex, graph-aware retrieval, memory-to-code join, MCP graph tools.
- `docs/on-disk-contract.md` documents the full on-disk contract for hosts.
