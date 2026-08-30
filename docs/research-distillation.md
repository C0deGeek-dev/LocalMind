# Research and distillation pipeline

LocalMind runs research and distillation as explicit batch jobs. They are
disabled unless `[inference]` config provides a chat endpoint.

Inputs:

- Accepted Markdown memory through `memory_index`.
- Code-graph context when a host scopes the job to code symbols or topics.
- Existing distilled records for deduplication and stale-state checks.

Outputs:

- `CandidateLesson` rows in the review queue.
- Derived `distilled_records` rows for traceability.
- Audit rows for inference calls and review-mode application.

Record types:

- `distillation`: higher-level principles, project truths, recurring patterns.
- `research`: gaps, contradictions, recurring failure patterns, and suggested
  follow-up investigations.

Scheduling:

- Jobs are invoked by CLI or host adapters. There is no background daemon.
- Hosts may schedule periodic runs, but LocalMind treats each run as an
  explicit batch with reviewable output.

Provenance and healing:

- Candidates carry evidence that marks the batch source.
- Accepted memory remains the source of truth; derived rows are rebuildable.
- Deleting or retiring source memory should trigger host-visible refresh or
  retirement scans rather than silent deletion of derived insight.

Authoring quality:

- Generated agent-facing bodies (distilled lessons, `SKILL.md` skill drafts) are
  written and pruned per the ecosystem [prompt-authoring doctrine](https://github.com/David-c0degeek/c0degeek-ai/blob/main/instructions/prompt-authoring-doctrine.md)
  — predictability of process, the information hierarchy, and the no-op pruning test.
  Link it; do not restate it here.
