```
╔═════╗   ╔═════╗		██╗      ██████╗  ██████╗ █████╗ ██╗     ███╗   ███╗██╗███╗   ██╗██████╗ 
║ ███ ║═══║ ███ ║		██║     ██╔═══██╗██╔════╝██╔══██╗██║     ████╗ ████║██║████╗  ██║██╔══██╗
║ ███ ║   ║ ███ ║║		██║     ██║   ██║██║     ███████║██║     ██╔████╔██║██║██╔██╗ ██║██║  ██║
║ ███ ║   ║ ███ ║║		██║     ██║   ██║██║     ██╔══██║██║     ██║╚██╔╝██║██║██║╚██╗██║██║  ██║
╚═════╝   ╚═════╝║		███████╗╚██████╔╝╚██████╗██║  ██║███████╗██║ ╚═╝ ██║██║██║ ╚████║██████╔╝
 ╚═══════════════╝		╚══════╝ ╚═════╝  ╚═════╝╚═╝  ╚═╝╚══════╝╚═╝     ╚═╝╚═╝╚═╝  ╚═══╝╚═════╝ 
```                                                                                        
# LocalMind

LocalMind is a local-first learning layer for AI-assisted development.

The goal is to turn opted-in AI development sessions into reviewed project
memory, graph-connected knowledge, and reusable skills without sending private
work to external services by default.

## LocalX Ecosystem

- [LocalStack](https://github.com/C0deGeek-dev/LocalStack) is the umbrella
  ecosystem for the LocalX tools.
- [LocalBox](https://github.com/C0deGeek-dev/LocalBox) is the model runtime
  and launcher for local GGUF models.
- [LocalMind](https://github.com/C0deGeek-dev/LocalMind) is this local-first
  memory and RAG layer.
- [LocalBench](https://github.com/C0deGeek-dev/LocalBench) is the benchmarking
  and evaluation companion for local model/runtime choices.
- [LocalPilot](https://github.com/C0deGeek-dev/LocalPilot) is the local CLI
  coding agent that embeds LocalMind natively.

## Architecture Posture

LocalMind is implemented as an extracted, host-neutral learning engine. The core
contracts live in Rust library crates so host runtimes can embed the same engine
without calling a separate service.

LocalPilot is the first native host: it should bundle LocalMind-backed learning
as built-in memory, review, context, and skill behavior. LocalMind core never
depends on LocalPilot; LocalPilot maps its session bundles, tool events, diffs,
test results, and recovery events into LocalMind contracts through an adapter.

The standalone `localmind` CLI is the shell around the same engine for generic
transcripts, Claude Code, OpenAI Codex, and future MCP clients.

## Repository Role

This repository is the implementation home for the system described in
`vision.md`.

The companion repository `c0degeek-ai` remains the reusable workflow asset
repository for prompts, planning templates, review checklists, and `SKILL.md`
files. LocalMind should consume, generate, and operationalize those kinds of
assets, not turn the workflow asset repository into an application codebase.

## Starting Point

Initial MVP target:

- Import session transcripts manually.
- Redact likely secrets before storage.
- Generate a session summary.
- Extract candidate lessons.
- Review lessons in a terminal queue.
- Save accepted lessons to Markdown memory files.
- Keep a SQLite audit log.
- Support basic search.
- Suggest `SKILL.md` files for repeated workflows.

Current workspace layout:

- `crates/localmind-core` — neutral session, evidence, lesson, review, memory,
  context, skill, audit, and host-adapter contracts.
- `crates/localmind-store` — Markdown memory plus queue/audit/index storage
  boundary.
- `crates/localmind-review` — manual review workflow boundary.
- `crates/localmind-search` — retrieval/search boundary.
- `crates/localmind-skills` — skill draft generation and maintenance boundary.
- `crates/localmind-cli` — standalone CLI entry point.
- `crates/localmind-mcp` — future MCP surface.

## Development

Baseline local commands:

```powershell
cargo fmt --check
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p localmind-cli -- --help
```

The MVP remains local-first and opt-in. No cloud dependency, autonomous memory
write, hidden transcript capture, or host-specific dependency belongs in
`localmind-core`.

## Project Opt-In

LocalMind refuses project memory writes unless the repository contains an
enabled `.localmind.toml` file:

```toml
[learning]
enabled = true
local_only = true
memory_root = ".localmind/memory"
allowed_scopes = ["project"]
excluded_paths = ["target/**", ".git/**"]
```

The default memory root is inside `.localmind/memory`, and write paths are
validated so accepted memory files stay inside that root. Durable memory is
serialized as readable Markdown with front matter for scope, category,
confidence, source session, evidence, related files/entities, and supersession
metadata.

## Transcript Import

Opted-in projects can import local transcript files manually:

```powershell
localmind import .\session.txt --project . --source open-ai-codex
```

Supported first-pass sources are `generic`, `claude-code`, `open-ai-codex`, and
`localpilot`. Supported first-pass formats are `plain-text`, `json-lines`, and
`markdown`; all are currently persisted as redacted raw transcript material for
later summary/extraction stages.

Imports write deterministic, content-derived session folders under
`.localmind/sessions/<session-id>/`:

- `transcript.redacted.txt`
- `metadata.json`

Likely API keys, bearer tokens, password/token assignments, connection-string
passwords, private keys, and configured sensitive paths are redacted before
those artifacts are written.

Imported sessions can be closed out into a deterministic summary and candidate
lesson set:

```powershell
localmind closeout session-1234 --project .
```

The first extractor is local and deterministic. It writes `summary.json` and
`candidates.json` beside the imported transcript, validates candidate confidence
and malformed output, and deduplicates repeated lesson text. Model-backed
extraction can be added later behind the same extractor trait without making
cloud calls the default.

Candidate lessons are queued in `.localmind/localmind.sqlite` for terminal
review. Closeout enqueues candidates idempotently; no durable memory write
happens until a later promotion stage consumes accepted review items.

```powershell
localmind review list --project .
localmind review inspect lesson-1234 --project .
localmind review accept lesson-1234 --project . --reviewer david
localmind review reject lesson-1234 --project . --note "not durable"
localmind review edit lesson-1234 "Prefer deterministic fixtures." --project .
localmind review defer lesson-1234 --project .
```

Accepted or edited review items can then be promoted into Markdown project
memory. Promotion writes the memory file, updates the local search index and
relationship metadata, and records a SQLite audit event:

```powershell
localmind promote lesson-1234 --project .
localmind search "deterministic fixtures" --project .
localmind audit --project .
```

Review decisions also write audit rows through the CLI. The audit log and search
index live in `.localmind/localmind.sqlite`; the durable accepted memory remains
readable Markdown below `.localmind/memory/project/`.

## Context Export And Skill Drafts

LocalMind can render accepted memory and disabled skill suggestions as concise
agent context:

```powershell
localmind context export "deterministic fixtures" --target open-ai-codex --project .
localmind context export "release checklist" --target localpilot --project .
```

Targets are `generic`, `claude-code`, `open-ai-codex`, and `localpilot`. The
LocalPilot target is a native-host fixture surface: LocalPilot can map its
session bundle into LocalMind contracts and render the returned context as
built-in learning behavior without requiring users to install LocalMind
separately.

Accepted review items that describe a repeated workflow or candidate skill can
generate disabled `SKILL.md` drafts under `.localmind/skill-drafts/`:

```powershell
localmind skills generate --project .
localmind skills list --project .
localmind skills inspect skill-lesson-1234 --project .
localmind skills export skill-lesson-1234 --project .
```

Generated drafts include a disabled front matter flag, name, description,
trigger conditions, workflow steps, constraints, verification steps, related
memories, source agents, and last-reviewed metadata. LocalMind never installs or
activates generated skills automatically.

## Planning

Before implementation, run the plan template from `c0degeek-ai` against this
repository and use the notes in `docs/planning-handoff.md` as seed context.

The existing Captain Hindsight workflow in `c0degeek-ai` is an early primitive
for LocalMind's close-out review and lesson extraction flow. LocalMind should
eventually make that kind of review repeatable, reviewable, auditable, and
connected to durable memory.

## Vision

See `vision.md`.
