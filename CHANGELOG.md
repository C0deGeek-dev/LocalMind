# Changelog

Notable changes, newest first. Contract-relevant entries reference
`docs/on-disk-contract.md`.

## Unreleased

- **Retroactive low-quality freshness flag (D-LM-0024).** The freshness pass gains
  a `low-quality` reason that reuses the write-time quality classifier to flag an
  already-stored tooling-noise or over-fit lesson — one that predates the write
  gate — for review, across the project and global stores. It is the most
  actionable reason and is **age-independent** (a bad lesson is flagged on the
  first pass regardless of age), while still only routing to review (never
  deleting) and honouring the per-run cap and dry-run. Markers match whole
  words/phrases, so `function` / `uncertain` never mis-flag. An operator applies it
  with `learning freshness`.

- **Write-time lesson-quality gate (D-LM-0024).** A model-pinned benchmark showed
  accepted learning is net-positive but noisy: tooling/process artifacts (a
  working-directory or build-wrapper note) and over-fit, exercise-specific claims
  auto-accepted and then mis-injected into unrelated tasks. A new deterministic,
  offline classifier (`classify_quality`) labels every candidate `general`,
  `over-fit`, or `tooling-noise`. The verdict is marked at extraction (in
  `review_annotation.notes`, both the deterministic and the model paths) and
  enforced at the accept seam: under trusted/automatic mode a non-`general`
  candidate is **withheld from auto-accept and routed to manual review** — treated
  like a duplicate, never discarded, **never auto-deleted**. An error-code recipe
  stays `general` (specific but generalizable), and a path/shell phrase inside a
  security or architecture lesson is not read as tooling noise (the category gate).
  See `docs/on-disk-contract.md`.

## v1.1.0 - 2026-06-29

Coordinated LocalX release.

- **The semantic (vector) rung now reaches the machine-wide global store
  (D-LM-0023).** `vector_search` scanned only the project `vector_index`, while
  `GlobalUser`-scoped lessons embed their vectors into the global index — so the
  embedding-cosine dedup rung and the hybrid-retrieval vector boost were both
  blind to global memories (a global paraphrase auto-accepted under automatic
  mode; a global memory never gained a vector score). `vector_search` now scans
  **project + global** vectors, merged by score with project precedence, mirroring
  the keyword path's `search`/`search_lang` merge. The duplicate probe also fetches
  candidate headroom and filters to memory subjects, so a higher-ranked non-memory
  vector (e.g. an ingested code chunk) can no longer hide a real memory duplicate.
  Adds a **route-to-review band**: cosine ≥ 0.86 is a confident `duplicate_of`,
  `[0.83, 0.86)` is a borderline one — both routed to review, **never auto-deleted
  or auto-merged** (the band only widens what a human sees). No-embeddings behaviour
  stays byte-identical to the lexical contract. Refines D-LM-0020; reuses
  D-LM-0010/0017; honours D-LM-0016. See `docs/on-disk-contract.md`.

- **Accepted memory now tracks usage (schema v8).** `memory_index` gains
  `hit_count` (default 0) and `last_used_at` (NULL = never), bumped post-turn
  when a memory is injected into context, so never-retrieved dead weight and
  high-value lessons are both visible. The bump is best-effort and never on the
  retrieval read path; the columns are runtime-accumulated (a reindex resets them
  to zero-usage). New store queries surface never-retrieved and most-used
  memories, and search results expose the count. Pre-v8 databases upgrade cleanly
  (rows read as zero-usage). See `docs/on-disk-contract.md`.

- **A proactive freshness pass flags stale memory for review (D-LM-0021).** A
  deterministic, offline pass (`freshness_pass`) selects accepted memory for
  review by age, never-retrieved-after-a-grace, and a version-sensitive
  keyword/semver heuristic — across the project **and** global stores, covering
  the non-code-anchored global lessons the change-aware staleness flag never
  reaches. It only routes to the existing review gate (it never deletes or
  re-ranks), with conservative configurable thresholds, a per-run cap, and a
  dry-run; re-runs are idempotent. `list_stale_candidates` now spans both stores.

- **Optional source re-validation (opt-in, default-off).** `revalidate_sources`
  samples version-sensitive lessons and asks a `VerdictSource` whether each still
  holds, routing a "no longer true" verdict to review (never deletes); an
  `Unknown` verdict never flags. The logic is model-agnostic (offline-testable
  with a fixture); `revalidate_with_model` drives it with the configured chat
  model, best-effort (degrades to unavailable when none is configured).

- **Language detection is whole-word and covers more languages.** Keyword
  matching was substring-based, so "cpp" matched a project name like `llama.cpp`
  and bare names risked colliding with English; and Go-by-name, C#, PowerShell,
  and Bash lessons were never tagged at all (only `golang` was a Go keyword). The
  matcher now requires whole-word boundaries (`contains_word`) so "go" matches
  "Go:" but not "going"/"cargo", "java" never matches inside "javascript", and
  `llama.cpp` is not read as C++; and the table adds **Go** (via `goroutine`/`go
  build`/`go:` markers, not the ambiguous bare token), **C#** (`c#`/`dotnet`),
  **PowerShell** (`pwsh`/`cmdlet`), and **Bash** (`pipefail`) for both prose tags
  and `.cs`/`.ps1`/`.sh` workspace detection. Tagging stays conservative — a
  lesson naming zero or several languages is left untagged.

- **Accepted-memory dedup is now semantic, not just lexical (opt-in,
  review-routed).** A candidate that means the same thing as an accepted lesson
  but shares few words (token overlap ~0.33, under the 0.6 lexical bar) used to
  slip through and promote a near-duplicate. When `review.semantic_dedup` is on
  **and** an embedding endpoint is configured, dedup now layers: lexical overlap
  stays the cheap first pass and the no-embeddings fallback; if it does not
  already flag a duplicate, the candidate is embedded and compared by cosine to
  accepted-memory vectors, and a nearest match at **cosine ≥ 0.86** (fixed,
  conservative, test-pinned) flags `duplicate_of` → routed to review like any
  duplicate, **never auto-deleted**. With embeddings unavailable the behaviour is
  byte-identical to the lexical contract (pinned by a no-regression test), and the
  vector path is best-effort. Decision **D-LM-0020** (refines D-LM-0007, reuses
  D-LM-0010's embeddings).

- **Accepted memory is embedded into `vector_index`, best-effort.** When an
  `[inference]` embedding endpoint **and** `embedding_model` are configured, each
  accepted memory is embedded at promotion/persist time (keyed by a content
  fingerprint so an unchanged body is not re-embedded), making the warm store
  vector-searchable for dedup and rerank. Embedding is **best-effort**: a down or
  slow endpoint no longer fails the promotion — the memory still persists (the
  lexical contract is the fallback), the failure is logged as a new
  `InferenceCallFailed` audit row, and the memory carries no vector until it is
  re-embedded. With no `embedding_model` set, no vectors are written and behaviour
  is byte-identical to the lexical-only path. See `docs/on-disk-contract.md`.

- **Language tagging no longer under-tags lessons named only by idiom.** Body-text
  detection alone missed clearly language-specific lessons that never spell the
  language out (a Go `sort.Strings` anti-pattern, a Rust borrow recipe), so they
  stored untagged and leaked across languages. `resolve_memory_language` now lets
  a **language-bound category** (code pattern, anti-pattern, debugging recipe,
  test strategy) inherit the **session's language** — the dominant language of the
  workspace the lesson was learned in — while an explicit language in the body
  still wins and cross-cutting categories (tooling, process) stay untagged.
  Workspace-language detection (`detect_workspace_language`) moved here too, so the
  workspace signal and the stored tag share one source of truth.

- **Accepted memory is tagged with a programming language and retrieval filters
  by it.** A lesson clearly about one language ("In Python, …") is noise in a
  task in another — a Python idiom injected into a Rust task degrades the
  solution. The single language a lesson names is now detected once at write
  time from the full body (new shared `language` module — the same table drives
  the host's workspace-language detection so the two cannot drift) and stored on
  `memory_index.language` (**schema v7**, see `docs/on-disk-contract.md`). The
  column is **nullable**: a lesson that names no single language (general) or
  several (cross-cutting) stays untagged and eligible for every task, and every
  pre-v7 row upgrades cleanly (treated as agnostic until a reindex re-detects
  it). `MemoryPersistence::search_lang(query, language)` excludes off-language
  memories **inside the FTS query**, so retrieval returns rows that are already
  language-relevant instead of ranking N and dropping the off-language ones
  afterward. `search` is unchanged (no filter).

- **Learning is on by default.** `[learning] enabled` now defaults to **`true`**
  (was `false`), so a project accumulates reviewed memory — and the machine-wide
  global store (D-LM-0017) fills with cross-project lessons (language/tooling/
  shell/build patterns and their anti-patterns) — out of the box. It stays
  `local_only` and review-gated (candidates, never auto-active memory); opt out
  with `[learning] enabled = false`. A measurement that must touch no accumulated
  memory disables learning explicitly. See `docs/on-disk-contract.md` and D-LM-0019.

- **Scope-aware, review-gated bundle import.** A verified bundle can be imported:
  a `Rejected` bundle never reaches the store; a `Trusted`/`Untrusted` one has each
  entry routed by scope (project → project store, global → the machine-wide global
  store, D-LM-0017) and **enqueued as a review candidate** carrying import
  provenance (origin author, trust class, bundle digest) — never written straight
  to active memory. The existing dedup ladder makes a re-import idempotent,
  and a `--dry-run` (the CLI default) reports added/duplicate/rejected counts
  without writing. Rollback is the existing path: discard un-reviewed candidates
  with `review purge`, or remove a promoted memory with `memory delete`.

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
