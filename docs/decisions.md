# Decisions

Durable, engine-internal architecture decisions for LocalMind. Host-side
decisions live with the host; this file records choices that hold regardless
of which host embeds the engine.

## D-LM-0011 — Change-aware staleness flags memory for review, never deletes it

- **Date**: 2026-06-17
- **Status**: accepted

When code changes, memories anchored to the changed (or dependent) symbols may
no longer hold. The engine joins the change-impact walk (reverse `Calls`/`Uses`
BFS from the changed spans) to the memory↔code anchor edges and, **above a
conservative anchor-strength threshold** (default 0.6 — admits qualified 0.9 and
plain-name 0.6 anchors, rejects weaker links), flags each affected memory as a
`stale_candidate` (schema v5, additive column on `memory_index`) and enqueues one
review item.

The decision is that this is **flag-for-review, never auto-invalidate**: a flagged
memory stays `status = 'active'` and keeps being retrieved — search marks it
`stale_candidate` so callers can down-rank or surface it, but it is never silently
dropped or auto-superseded. Resolution stays human: a reviewer refreshes
(re-promote clears the flag), supersedes (D-LM-0008), or keeps it. The threshold
is tunable and conservative so a weak/distant link does not flood the review
queue. This is the same reviewed-promotion discipline as extraction (D-LM-0004):
a machine-inferred signal routes through review, it does not change durable memory
on its own.

## D-LM-0010 — Embedding rerank is an opt-in, additive stage over the deterministic blend

- **Date**: 2026-06-17
- **Status**: accepted

Ranked search can optionally rerank the top blended hits by cosine similarity
between the query embedding and each hit's embedding. The decision is that this
stays **strictly additive on top of the deterministic 3-signal blend**, never a
replacement: the blend (`search_workspace`) remains the reproducible, offline
floor, and the rerank stage (`rerank_hits` / `search_workspace_reranked`) only
reorders a bounded top-`window` of the already-ranked list.

It is gated like `review.semantic_dedup` (D-LM-0007): a `[retrieval].rerank`
flag **and** a configured `[inference]` embedding endpoint must both be present
(`ProjectConfig::rerank_active`). With either missing the stage is inert and the
ordering is byte-identical to the no-rerank path — a flag-off determinism test
pins that. The embedder is injected through a `RerankEmbedder` trait, so the
engine takes no new hard dependency on a network client and the path is testable
with a deterministic stub. This keeps retrieval reproducible and local-first by
default while letting a configured local embedding model lift relevance when the
user opts in.

## D-LM-0009 — The repo primer is a deterministic, review-gated, supersedable Project memory

- **Date**: 2026-06-17
- **Status**: accepted

The engine can distil a one-shot **repository primer** — the orientation an
agent needs to start work on an unfamiliar repo without reading files. It is
derived deterministically from the architecture overview (`compute_overview`):
a templated body over the language/package/entry-point/hotspot breakdown, with
**no model in the path**, so it is byte-stable for a fixed overview and ships
independent of any inference configuration.

The primer is an *inference about the repo*, and is honest about it:

- category `ArchitectureRule`, `Confidence < 1.0` — never asserted as parsed
  fact;
- evidence is an `EvidenceKind::CodeParse` ref pinned to `repo@commit` (`uri`)
  with a `content_hash` taken over the overview's shape (not the commit string),
  so the hash changes only when the source-derived structure drifts;
- it is produced as a `CandidateLesson` and routed through the **review queue**
  like any other memory (`remember`/D-LM-0004 discipline) — distillation never
  writes accepted memory directly.

Re-distillation reuses supersession (D-LM-0008): a drifted repo yields a primer
with a distinct content-hash id, which a reviewer accepts as
`ReviewAction::Supersede(prior)`; promotion retires the prior primer and records
`supersedes`. Staleness reuses the host's session-open refresh trigger — a primer
whose stored `content_hash` no longer matches the current overview is stale and
offered for re-distillation. One graph, one store: the primer adds no new
persistent store, only a `Project` memory.

## D-LM-0008 — Supersede is a reviewer-targeted decision that retires the prior memory

- **Date**: 2026-06-17
- **Status**: accepted

A review decision can now retire prior accepted guidance.
`ReviewAction::Supersede(target)` carries the memory id the reviewer — or a
trusted/automatic mode with a clear conflict target above the trusted threshold —
chooses to replace. The target is persisted on the review item (schema v4,
`supersede_target`) and applied at promotion: the new memory records
`supersedes = [target]`, the target's index status flips to `Superseded` in the
same transaction, and a `MemorySuperseded` audit row links both memories and the
reviewer. Retrieval already filters to `status = 'active'`, so the retired memory
stops being served while its Markdown body and index row are kept — supersession
is reversible and provenance survives (mirroring the code-graph supersede).

Superseded vs Stale, supersede vs contradict:

- **Superseded** is used only when there is a clear replacement — a new memory
  takes the old one's place. That is the wired path (`supersedes` + status
  `Superseded`).
- **Stale** stays in the model for a memory that is no longer valid but has no
  replacement (a contradiction with no clear successor). It is *not* auto-applied
  here: a contradiction without a clear target stays human-gated (manual/assisted
  modes, or trusted/automatic when no related memory clears the topic-overlap
  threshold), so a human decides whether to retire it and what replaces it.
- `contradicts` remains a descriptive annotation the extractor sets on a
  candidate; promotion does not auto-write `MemoryEntry.contradicts`. Only a
  chosen replacement (`supersedes`) justifies removing guidance from retrieval.

Auto-supersede is gated on the trusted threshold in **both** trusted and
automatic modes — a higher bar than a plain auto-accept — because retiring a
human's prior memory is higher-risk than adding a new one.

## D-LM-0007 — Review candidates are deduplicated at enqueue with a layered ladder

- **Date**: 2026-06-17
- **Status**: accepted

The review queue grew unbounded with restatements of the same lesson. Schema
v3 adds `canonical_hash` and `seen_count` to `review_items`, and
`enqueue_candidates` runs a deterministic dedup ladder against existing
*pending* candidates: an exact normalized-canonical-hash match (lowercased,
whitespace-collapsed, trailing punctuation stripped) or a lexical
near-duplicate (token-set overlap ≥ 0.7) is **merged** into the survivor —
bumping `seen_count` rather than inserting a new row — while distinct lessons
are kept.

Embedding-based dedup is opt-in (`review.semantic_dedup`) and only active when
an inference embedding endpoint is configured; otherwise the deterministic
lexical contract is the whole story, so the queue stays clean and testable with
no network access. Dedup is scoped to pending candidates; duplication against
*accepted* memory remains the review-mode annotation path (`duplicate_of` →
`IgnoreSimilar`). A one-time `purge_pending` clears an un-reviewed backlog,
leaving decided items and accepted-memory tables untouched.

## D-LM-0006 — Candidates pass a prose-admission gate; batch insights are strict-JSON

- **Date**: 2026-06-15
- **Status**: accepted

Refines D-LM-0005. Every deterministic-extractor candidate must pass a single
prose-admission gate before it can enter the review queue: it must read like a
human-written lesson, not a bare file path, a code/markup line, or a
punctuation/sub-token fragment. Author-declared markers (`Lesson:`) take a
lighter gate than the weaker heuristics (skill/workflow proposals require an
explicit intent phrase and are capped; failure→resolution requires a genuine
failure *report* on both sides). This is what stops dumped file content from
flooding the review queue.

Batch distillation and research return a strict-JSON envelope, not free text. A
non-JSON or wrong-shape reply is rejected outright and nothing is stored;
individually malformed insights are dropped. Distilled insights meet the same
admission bar as extracted lessons.

Both keep extraction/distillation review-routed and noise-resistant; quality is
measured by the golden-session evaluation rather than assumed.

## D-LM-0005 — Summaries and extraction candidates are evidence-grounded

- **Date**: 2026-06-14
- **Status**: accepted

LocalMind session summaries use shared digest sections for progress,
decisions, next steps, relevant command/failure outcomes, risks, and stale or
superseded facts. Candidate lessons must carry evidence before they can enter
the review queue. Candidate quality is represented as confidence, validation
status, and review annotation signals such as conflict notes.

Model-backed extraction remains optional and strict-schema. If inference is not
configured or the model output is malformed, LocalMind falls back to the
deterministic extractor. Extracted candidates are review items; they do not
write accepted memory directly.

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
