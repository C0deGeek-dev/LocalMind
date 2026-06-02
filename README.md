# LocalMind

LocalMind is a local-first learning layer for AI-assisted development.

The goal is to turn opted-in AI development sessions into reviewed project
memory, graph-connected knowledge, and reusable skills without sending private
work to external services by default.

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

## Planning

Before implementation, run the plan template from `c0degeek-ai` against this
repository and use the notes in `docs/planning-handoff.md` as seed context.

The existing Captain Hindsight workflow in `c0degeek-ai` is an early primitive
for LocalMind's close-out review and lesson extraction flow. LocalMind should
eventually make that kind of review repeatable, reviewable, auditable, and
connected to durable memory.

## Vision

See `vision.md`.
