# On-disk contract

What LocalMind writes inside a project, precisely enough that a host other
than the bundled CLI could integrate from this document alone. Everything
lives under the project root; nothing is written outside it.

## Opt-in configuration: `.localmind.toml`

Learning is off until this file exists at the project root:

```toml
[learning]
enabled = true                       # required; false refuses all writes
memory_root = ".localmind/memory"    # optional; must stay inside the project
allowed_scopes = ["project"]         # optional; scopes hosts may write
```

A missing file, `enabled = false`, malformed TOML, or a `memory_root` that
escapes the project root are all hard, typed errors — never silent fallbacks.

## Directory layout

```
<project root>/
  .localmind.toml
  .localmind/
    localmind.sqlite          # single shared database (schema below)
    memory/
      project/<memory-id>.md  # one Markdown file per accepted memory
    sessions/
      <session-id>/
        metadata.json             # ImportedSession record
        transcript.redacted.txt   # transcript AFTER redaction; raw text is never stored
        summary.json              # SessionSummary (written at closeout)
        candidates.json           # Vec<CandidateLesson> (written at closeout)
```

Only the redacted transcript is persisted. Redaction runs before any write
(pattern table + entropy backstop; see `localmind-store/src/redaction.rs`
module docs for the caught / not-caught guarantee).

## Markdown memory format

One file per accepted memory at `memory/<scope>/<id>.md`: YAML-style front
matter between `---` fences, then the body.

| Front-matter key | Presence | Meaning |
|---|---|---|
| `id` | always | memory id (also the file stem) |
| `scope` | always | `Project`, `GlobalUser`, … |
| `category` | always | lesson category (`Process`, `DebuggingRecipe`, …) |
| `confidence` | always | fixed 3-decimal float in [0, 1] |
| `source_session` | optional | originating session id |
| `created_at` / `updated_at` | optional | RFC 3339 timestamps |
| `tags`, `related_files`, `related_entities` | when non-empty | string lists |
| `supersedes`, `contradicts` | when non-empty | memory-id lists |
| `evidence` | when non-empty | list of `{id, kind, label, redacted, uri?}` |

Scalars containing `: # ' "` or newlines are single-quoted with `''`
escaping. The Markdown file is the human-readable source of truth; the
database rows below are derived and rebuildable from it.

## Database schema: `.localmind/localmind.sqlite`

Database schema lifecycle is versioned with `PRAGMA user_version`
(currently **1**); every component steps the schema on open and refuses
databases newer than it understands. Version 1 tables:

| Table | Owner concern | Notes |
|---|---|---|
| `schema_migrations(version, applied_at)` | human-readable migration ledger | duplicate of user_version for inspection |
| `review_items(id, session_id, candidate_json, state, reviewer_action, reviewer, note, replacement_summary, created_at, updated_at)` | review queue | `candidate_json` is a serialized `CandidateLesson` |
| `audit_events(id, kind, actor, subject, metadata_json, happened_at)` | audit log | `metadata_json` is always valid JSON (serde-built) |
| `memory_index(memory_id, path, scope, category, body, source_session, status, created_at)` | search index over accepted memory | `status = 'active'` rows are live |
| `memory_fts(memory_id UNINDEXED, body)` | FTS5 index | queried with `MATCH` + bm25 |
| `memory_relationships(memory_id, relation_kind, target)` | typed relations | kinds: `category`, `session`, `file`, `entity` |
| `graph_nodes`, `graph_edges`, `graph_meta` | code-structure graph | payload format versioned separately via `graph_meta.format_version` (`GRAPH_FORMAT_VERSION`, currently 1) |

Write-consistency contract: multi-statement writes (promote, persist,
delete) commit atomically; the Markdown file write precedes the indexing
transaction, and deletion removes the file before the database rows so an
interrupted delete heals on retry.

## Versioning

- Database schema: `PRAGMA user_version` stepper (above).
- Graph payload: `graph_meta.format_version`.
- Crate/API versions: workspace version + `CHANGELOG.md`; release tags
  (`v<workspace version>`) mark contract-relevant changes. Hosts pinning a
  commit (e.g. a git submodule) get the contract as of that commit.
