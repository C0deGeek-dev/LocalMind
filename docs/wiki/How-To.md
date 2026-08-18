# How-To guides

Task-oriented recipes — each answers a single "how do I…?" against shipped
behaviour at the current `VERSION`. See **[[Getting-Started]]** first.

> **Do not edit on github.com.** This wiki is generated from in-repo Markdown
> under `docs/wiki/` and synced one-way on every push to `main`. Edit the source
> in `docs/wiki/`; web edits are overwritten on the next sync.

## Import a session transcript

```powershell
localmind import .\session.txt --project . --source open-ai-codex
```

Sources: `generic`, `claude-code`, `open-ai-codex`, `localpilot`. Formats:
`plain-text`, `json-lines`, `markdown`. Imports write a content-derived folder
under `.localmind/sessions/<id>/` with a **redacted** transcript and metadata —
likely API keys, tokens, private keys, and configured sensitive paths are
stripped before anything is stored.

## Import existing knowledge (memory files and lessons)

Bring knowledge that already lives outside LocalMind into the review queue. Both
importers enqueue **pending** candidates only (never auto-accepted), redact every
field first, dedup against the queue (so re-running is idempotent), and label each
candidate with a `--source`. Omit `--apply` for a dry-run count.

```powershell
# A directory of Claude Code memory files (front-matter Markdown):
localmind import-memory "$env:USERPROFILE\.claude\projects\<proj>\memory" `
  --project . --source claude-code-memory --apply

# lessons.md bullet lists (a file, or a tree scanned for lessons.md):
localmind import-lessons ..\LocalHub\plans\localmind\lessons.md `
  --project . --source localhub-lessons --apply
```

Review what landed with `localmind review list`. A large tree can enqueue a lot —
run the dry-run first and import a focused subset rather than everything at once.

## Keep an agent always feeding LocalMind (and opt out)

A coding agent (Claude Code, Codex) can feed LocalMind automatically at session
close: a session-end hook imports the transcript, runs `closeout`, and imports
the session's memory-file deltas (`import-memory`) — all into the **review
queue**, never auto-accepted. Point the hook at a build that has the write path,
prefer it over a stale PATH binary, and make it non-blocking.

Opt out for one session by setting `LOCALMIND_AGENT_OFF=1`, or persistently by
creating `~/.localmind/agent-off` — the hook checks both and skips when either is
present.

## Close out a session into candidate lessons

```powershell
localmind closeout session-1234 --project .
```

The deterministic extractor writes `summary.json` and `candidates.json`, then
enqueues candidates idempotently for review. No durable memory is written yet.

## Review and promote lessons

```powershell
localmind review list --project .
localmind review inspect lesson-1234 --project .
localmind review accept lesson-1234 --project . --reviewer david
localmind review edit lesson-1234 "Prefer deterministic fixtures." --project .
localmind promote lesson-1234 --project .
```

Promotion writes the Markdown memory file, updates the search index and
relationship metadata, and records a SQLite audit event.

An agent or script can skip transcript closeout when it already has one
distilled lesson. This still creates only a pending review item:

```powershell
localmind propose "Keep retry loops bounded" --source open-ai-codex `
  --body "Stop after the configured attempt budget." `
  --evidence "src/retry.rs#policy" --idempotency-key retry-policy-v1
```

`review list` and `review inspect` show the proposal source. Repeating the same
arguments is a no-op; reusing an idempotency key for different content is
refused. Trusted or automatic review mode never auto-accepts an agent proposal.

A hook with a long body can pass the whole proposal as one JSON object instead
of flags — `--json-file <path>`, or `-` for stdin (the keys mirror the flags:
`title`, `body`, `category`, `scope`, `source`, `tags`, `related_files`,
`evidence`, `idempotency_key`, `confidence`):

```powershell
'{ "title": "Keep retry loops bounded", "source": "open-ai-codex", "scope": "global" }' |
  localmind propose --json-file -
```

An MCP client can check readiness before proposing with the read-only
`memory_status` tool (learning on/off, accepted-memory / pending-review /
doc-index counts). `localmind mcp serve` binds to the workspace it is launched
in — it walks up to the nearest `.localmind.toml` — so no fixed `--project` is
needed in the client config.

## Export agent context and draft skills

```powershell
localmind context export "deterministic fixtures" --target localpilot --project .
localmind skills generate --project .
localmind skills list --project .
```

Targets: `generic`, `claude-code`, `open-ai-codex`, `localpilot`. Generated
`SKILL.md` drafts land disabled under `.localmind/skill-drafts/`; LocalMind never
installs or activates skills automatically.

## Review and browse in the web UI

```powershell
localmind ui --project . --open
```

Serves a localhost web app over the same store (default
`http://127.0.0.1:8091`; `--port` changes it): dashboard, review queue (`j`/`k`
to move, `a`/`r`/`d` to decide, `e` to edit, `x` to select for bulk actions),
memory browser with provenance and audited delete, semantic doc search, an
interactive code-graph view, and the audit log. Every endpoint calls the same
store methods as the CLI, so the review gate cannot be bypassed. The server
binds `127.0.0.1` only; add `--token <secret>` to also require `?token=` on
each request if the port is forwarded beyond the machine. Run from any
subdirectory — the store resolves by walking up to the nearest
`.localmind.toml`.

## Serve LocalMind tools to an MCP client

```powershell
localmind mcp serve --project .
```

A synchronous stdio MCP server (newline-delimited JSON-RPC 2.0, no async
runtime). Register it in an MCP-capable client, e.g. a `.mcp.json`:

```json
{
  "mcpServers": {
    "localmind": {
      "command": "localmind",
      "args": ["mcp", "serve", "--project", "."]
    }
  }
}
```

Ten tools: the nine existing read/query tools (`memory_search`,
`memory_context_export`, `doc_search`, four `memory_symbol_*` tools, and skill
list/fetch) plus `memory_propose`. The proposal tool is the only write: it is
bounded, capped per MCP server session, idempotent for identical arguments, and
can only enqueue a **pending** human-review candidate.

## Build the code graph over a repository

```powershell
localmind graph reindex . --project .
```

Walks the tree (VCS, build, and vendored directories are skipped; only source
and Markdown extensions are candidates, so a stray binary cannot abort the
pass) and drives the resumable reindexer to completion, printing progress per
batch. Query the result through the MCP graph tools or the UI Graph tab.

## Ingest documentation for semantic search

```powershell
localmind ingest docs .\docs --project .
```

Chunks Markdown at headings (an oversized section splits at paragraph
boundaries) and embeds each passage into the semantic doc index. Re-ingest is
idempotent — an edited file re-embeds in place and a shrunk file's stale
passages are pruned. Embedding is best-effort: without a configured
`[inference]` embedding endpoint the text is stored un-vectored and a notice
is printed. Search the result with the `doc_search` MCP tool or the UI Docs
tab.

## Check memory quality

```powershell
localmind eval            # extraction precision/recall, retrieval recall@k
localmind eval -k 5 --json
```

A regression gate over a golden fixture set; `-k` sets the retrieval cutoff,
`--json` emits machine-readable output.
