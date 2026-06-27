# Changelog

Notable changes, newest first. Contract-relevant entries reference
`docs/on-disk-contract.md`.

## Unreleased

- **Signed, fail-closed-verified bundles.** A bundle can be signed (Ed25519 over
  a SHA-256 digest of its canonical bytes) and verified on the way in: a tampered
  byte, bad signature, malformed key, author/key mismatch, or unsupported version
  is `Rejected` and never imported; a valid unknown-key bundle is `Untrusted`
  (heavier review); your own/trusted key is `Trusted`. A verified author is **not**
  verified content — imported memory is still review-gated. Trust is local — an
  Ed25519 keypair in a `0600` file under `~/.localmind/keys/` (the BYOK pattern,
  host ADR-0042) plus a manual trust list; no PKI or network. The private key is
  never serialized into a bundle or logged. New crypto deps (`ed25519-dalek`,
  `sha2`, `getrandom`) are pinned + `cargo deny`-clean. Decision: D-LM-0018;
  contract: `docs/on-disk-contract.md` §Signed bundle.

- **Portable memory bundle (format v1).** Accepted memory can be exported to a
  portable, versioned, self-describing JSON pack (`MemoryBundle`) and parsed back,
  the basis for moving knowledge across your own machines and sharing it. The
  bundle is built from the Markdown source of truth via the new
  `MarkdownMemoryFormat::parse` (the inverse of `serialize`), reusing the
  canonical model serde — no second serialization of a lesson. Export is
  accepted-only, scope-selectable (`project`/`global`/`both`), re-redacted with a
  pre-export `SecretScanReport` seam, and deterministic/content-addressable; a
  reader rejects an unknown newer `format_version`. Signing/verify and the
  import/merge path layer on top (later changes). Contract:
  `docs/on-disk-contract.md` §Portable memory bundle.

- **Machine-wide global memory (on by default).** The modelled-but-dormant
  `GlobalUser` scope is now real and on by default: `allowed_scopes` defaults to
  `["project", "global_user"]`, so cross-project knowledge accumulates in a
  separate per-user-home store (`~/.localmind/memory`, overridable by an absolute
  `global_memory_root` or the `LOCALMIND_GLOBAL_ROOT` env — `@project` roots it
  under each project for hermetic tests) with its own index, shared across
  projects. A conservative scope classifier
  (`CandidateDestination::default_for_category`) routes clearly cross-project
  lessons (tool-use, debugging recipe, process, anti-pattern, user preference) to
  global and keeps project-specific lessons project; promotion stays review-gated.
  Retrieval merges project + global with **project precedence**, and
  `delete_memory` is now scope-aware. `local_only` still holds (same-machine,
  never remote). A project that wants project-only memory sets
  `allowed_scopes = ["project"]`. Contract: `docs/on-disk-contract.md`
  §Global-scope store; D-LM-0017.

## v1.0.0 - 2026-06-24

Coordinated LocalX 1.0 release. First stable: the on-disk contract and engine
surface are now SemVer-stable. Validated cross-model (lesson-injection uplift
holds on a second local model).

- Model-backed lesson extraction (`closeout` with `[inference]`) now tolerates the
  output real local models actually produce. A reasoning model wraps its JSON in a
  `<think>...</think>` block and a Markdown code fence, which made a raw parse fail at
  column 1 and abort the whole closeout. The chat client gained
  `extract_json_payload`, which strips the think block and fence and isolates the
  outer JSON span; the extractor now also requests a JSON object via `response_format`
  (retried once without it if the server rejects the constraint) and, on any extraction
  or parse failure, falls back to the deterministic extractor instead of erroring.
- `flag_for_review(memory_id, reason)` generalizes the route-to-review staleness
  flag to carry a caller-supplied reason, so outcome-aware down-weighting (a
  lesson that didn't improve eval outcomes) and change-aware invalidation share
  one audited, never-auto-delete path. `mark_stale_candidate` is now a thin
  wrapper over it (unchanged behaviour). (D-LM-0016)
- `MemorySearchResult` now carries the matched memory's `category`, so a host can
  gate or dedup context injection by category without a second store lookup. The
  field is populated from the existing `memory_index.category` column — purely
  additive to the search contract (D-LM-0015).

## v0.3.0-beta.3 - 2026-06-18

Coordinated LocalX beta release.

### 2026-06-19 - Memory quality

- Broadened the golden eval `default_fixtures` from three to eight, adding the
  hard categories the small set missed: stale/superseded knowledge, contradictory
  preferences, a failed-tool→recovery recipe, a long noisy transcript with a
  single buried lesson, and a low-value close-out that must yield no durable
  memory. All score precision/recall/recall@k 1.0.
- The `golden_eval_meets_quality_threshold` gate now enforces **per-fixture
  (per-category) minimums** and a negative-fixture zero-candidate check, so a
  strong mean can no longer hide a weak category.
- Reconciled the `vision.md` host-integration note, which still said extraction
  quality was "not yet measured / until the quality eval lands" — it now states
  the eval exists, is fixture-backed and per-category gated, and should keep
  growing toward real-world coverage. Prose and status table now agree.

### 2026-06-19 - Release hygiene

- Stamped every crate's `Cargo.toml` package version at `0.3.0-beta.3` to match
  the top-level `VERSION`; the coordinated cut had moved `VERSION` but left the
  Rust packages a train behind.

### 2026-06-18 - Memory quality

- Added a third golden extraction fixture (`lock-order-deadlock`) to
  `default_fixtures`, broadening the gated eval beyond the exporter case while
  keeping mean precision/recall at 1.0.
- Corrected the `vision.md` status table: lesson-extraction quality **is** gated
  by the golden eval (`golden_eval_meets_quality_threshold`, mean precision/recall
  ≥ 0.9 with a negative no-false-positive fixture) and the extraction acceptance
  bar — the previous "not yet measured" line was stale.

### 2026-06-17 - Documentation

- Added an in-repo wiki source (`docs/wiki/`) that is one-way CI-synced to the
  GitHub Wiki, a `docs/README.md` doc-ownership index, and an offline link check
  over the docs.
- Documented the `localmind eval` memory-quality command in the README.

## v0.3.0-beta.2 - 2026-06-15

Coordinated LocalX beta release. Makes the learning loop produce usable results.

- Hardened the deterministic extractor: candidates pass a prose-admission gate
  (no bare file paths, code/markup, or punctuation fragments), and each heuristic
  family is tightened and capped. Stops the review queue flooding with noise.
- Batch distillation and research now emit strict-JSON, schema-validated
  candidates; non-JSON model output is rejected, not stored.
- Replaced the naive review-mode conflict/duplicate detection (top-1 keyword hit;
  literal "contradict") with token-overlap similarity and a real
  corrective-contradiction check.
- Added a golden-session memory-quality evaluation (extraction precision/recall,
  retrieval recall@k) with a regression threshold, exposed as `localmind eval`.
- Recorded engine decision `D-LM-0006`.
- Docs: vision Implementation-Status table discloses host-unwired/quality-
  unmeasured; section 13 marked superseded; README MCP wording corrected.

## v0.3.0-beta.1 - 2026-06-12

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
