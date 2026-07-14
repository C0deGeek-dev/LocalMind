# LocalMind docs

Documentation index and doc-ownership map. Match a change to its owning doc
before editing; don't restate the same area in two places.

| Area | Owning doc |
|---|---|
| Project overview, install/quickstart, ecosystem links | top-level `README.md` |
| Web UI (`ui`), MCP server (`mcp serve`), `graph reindex`, `ingest docs` — CLI surface | top-level `README.md` |
| Web UI / MCP / code- and doc-indexing recipes | [`wiki/How-To.md`](wiki/How-To.md) |
| Product vision / direction (not shipped-behaviour reference) | top-level `vision.md` |
| Storage / on-disk contract (canonical, incl. `doc_chunk` and the graph tables) | [`on-disk-contract.md`](on-disk-contract.md) |
| Architecture decisions / ADRs | [`decisions.md`](decisions.md) |
| Research notes feeding design | [`research-distillation.md`](research-distillation.md) |
| Cross-session planning handoff | [`planning-handoff.md`](planning-handoff.md) |

Aspirational content stays in `vision.md`, marked as direction — it does not
leak into shipped-behaviour docs.

## Wiki

User-facing guides (Getting Started, How-Tos, Examples, Troubleshooting) are
authored as in-repo Markdown under `docs/wiki/` and one-way CI-synced to the
GitHub Wiki. The in-repo source is authoritative — never edit pages on
github.com. Wiki Reference pages link these `docs/` pages rather than
duplicating them.

## Changelog & version

Every user-facing change updates the top-level `CHANGELOG.md` in the same
checkpoint. No doc, README, or wiki page may claim behaviour beyond the current
`VERSION`. Durable architecture decisions are promoted to `decisions.md`.
