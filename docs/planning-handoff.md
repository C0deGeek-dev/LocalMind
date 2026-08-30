# Planning Handoff

Use this file as seed context when running the `c0degeek-ai` plan template on
this repository.

## Repo Boundary

`localmind` is the product/runtime repository.

It should contain:

- CLI implementation.
- Storage schema and migrations.
- Transcript/session importers.
- Redaction pipeline.
- Lesson extraction pipeline.
- Review queue.
- Memory writers.
- Search/indexing code.
- Tests and verification tools.
- Packaging and local install instructions.

`c0degeek-ai` remains the workflow asset repository.

It should contain:

- Prompts.
- `SKILL.md` workflows.
- Planning templates.
- Review checklists.
- MCP and tooling notes.
- Reusable agent instructions.

LocalMind may read or generate workflow assets, but the application code should
live here.

## Captain Hindsight Relationship

The existing `captain-hindsight` skill is an early manual primitive for the
LocalMind learning loop:

1. Review completed work.
2. Identify what should be kept, fixed, recorded, or treated as risk.
3. Decide whether closure is safe.
4. Capture durable lessons before context is lost.

LocalMind should operationalize that pattern by turning session close-out into a
pipeline:

1. Capture raw session material.
2. Summarize the session.
3. Extract candidate lessons.
4. Validate them against project files, tests, and prior memory.
5. Put them in a review queue.
6. Promote accepted items into scoped memory or suggested skills.
7. Write an audit trail.

## First Plan Shape

The first implementation plan should stay narrow and defer platform work.

Recommended first subjects:

1. Repository setup, package skeleton, and baseline tests.
2. Memory file format and project opt-in config.
3. Manual transcript import and redaction.
4. Session summary and candidate lesson extraction.
5. Terminal review queue.
6. Accepted lesson persistence and audit log.
7. Basic search over accepted memory.
8. Skill suggestion output.

Recommended non-goals for the first plan:

- No IDE plugin.
- No web UI.
- No background watcher.
- No autonomous memory writes.
- No cloud dependency.
- No online research mode.
- No full graph database until the Markdown/SQLite memory shape is proven.

## Open Technical Decisions

- Implementation language and package layout.
- Whether the first CLI should be Python, .NET, Rust, or Node.
- Markdown-first memory format.
- SQLite schema for audit trail and review queue.
- Local model interface.
- Embedding provider and fallback behavior.
- Secret redaction rules and test corpus.
- Exact shape of project opt-in config.
