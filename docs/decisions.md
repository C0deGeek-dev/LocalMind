# Decisions

Durable, engine-internal architecture decisions for LocalMind. Host-side
decisions live with the host; this file records choices that hold regardless
of which host embeds the engine.

## D-LM-0018 — Portable memory bundles are signed; verify is fail-closed and local-trust; verified author ≠ verified content

- **Date**: 2026-06-27
- **Status**: accepted

LocalMind can now export accepted memory to a portable bundle (D-LM-0018 builds
on the bundle format) and import it elsewhere. Moving knowledge across machines —
and especially *importing from other people* — is unsafe without integrity and
attribution: a pack could be tampered with in transit or forged. But over-trust
is the opposite failure: a valid signature must never be mistaken for "this
content is correct."

Decision — four parts.

1. **Cryptography (vetted, pinned, pure-Rust).** Bundles are signed with
   **Ed25519** (`ed25519-dalek =2.1.1`, pulling the audited `curve25519-dalek
   4.1.3` that closed RUSTSEC-2024-0344) over a **SHA-256** (`sha2 =0.10.8`)
   digest of the bundle's deterministic canonical bytes; the signing-key seed is
   drawn from the OS CSPRNG (`getrandom =0.2.15`). No bespoke crypto. The crates
   are pinned, MSRV-1.82-compatible, and `cargo deny check`-clean (advisories /
   bans / licenses / sources). `#![forbid(unsafe_code)]` holds — unsafe lives only
   inside the vendored crates.

2. **Local trust, no PKI (D002).** Trust is a local keypair plus a manual trust
   list — there is no key-distribution service, registry, or network. The author
   is a *key-bound fingerprint* (`sha256(public_key)[..16]`), so an author cannot
   be spoofed with a different key.

3. **Fail-closed verification with three outcomes.** On import the digest is
   recomputed, the signature verified, and the schema/version validated. Any
   doubt → **`Rejected`** (bad digest, bad signature, malformed key/signature,
   author/key mismatch, or unsupported schema/version). A valid signature by a
   *known* key (your own, or one you added to the trust list) → **`Trusted`**; a
   valid signature by an *unknown* key → **`Untrusted`** (allowed, but flagged for
   heavier review). A `Rejected` bundle never reaches the store.

4. **Verified author ≠ verified content.** A signature attests integrity and
   authorship only. Imported memory is *still* routed through the existing human
   review queue (D001 / D-LM-0006/0007) — never auto-promoted. The trust UX must
   say this plainly.

**Key handling.** The signing key is stored with the BYOK pattern (host ADR-0042):
a `0600` owner-only file under the per-user home, beside the machine-wide global
store (`<home>/.localmind/keys/`). Because the engine is host-neutral it cannot
depend on the host's keychain crate, so this `0600`-file tier is the engine floor;
the OS-keychain tier is a documented host enhancement. The private key never
leaves the keystore's audited write call — it is never serialized into a bundle,
logged, or `Debug`-printed (pinned by a test).

**Security review (recorded).** Crypto dependency: standard, audited, pinned,
pure-Rust; `cargo deny`/`machete` clean; the only `cargo audit` finding is the
*pre-existing* `time 0.3.37` RUSTSEC-2026-0009, already documented-ignored in the
workspace `deny.toml` — its fix (time ≥ 0.3.47) needs edition 2024 / Rust 1.88,
above MSRV 1.82, and its DoS vector is RFC-2822 parsing, which LocalMind never
does (timestamps are RFC-3339, including in imported bundles). Threat model: a
poisoned/forged/oversized/schema-invalid pack cannot reach active memory —
verification is fail-closed and content is review-gated even when `Trusted`.
Residual: redaction-on-export is best-effort (documented); a human tech-lead
sign-off on this review is mirrored in the plan's manual actions.

## D-LM-0017 — Machine-wide global memory: a separate store, a scope classifier, and project-precedence merged retrieval

- **Date**: 2026-06-27
- **Status**: accepted

The engine modelled a `GlobalUser` scope but never made it real: the memory root
was always `project_root/.localmind/memory` (even the `global/` subdir lived under
the project), `allowed_scopes` defaulted to `["project"]`, and the per-project
SQLite index meant a "global" memory written in one project was invisible to
every other. So "the more you use it the smarter it gets across projects" could
not happen.

Decision — three parts. Global scope is **on by default**: `allowed_scopes`
defaults to `["project", "global_user"]`, so cross-project knowledge accumulates
out of the box. A project that wants project-only memory narrows `allowed_scopes`
to `["project"]`. It stays `local_only` (same-machine, never remote). The three
parts:

1. **A true machine-wide store.** A `GlobalUser` memory resolves to a per-user
   home root (`~/.localmind/memory`, overridable by an absolute
   `global_memory_root`), with its own SQLite index beside it — distinct from any
   project store. The path resolver branches on scope; everything else stays
   project-rooted. The home is resolved cross-platform (Windows `USERPROFILE`,
   Unix `HOME`), so the path is tier-1.
2. **A conservative scope classifier.** `CandidateDestination::default_for_category`
   routes clearly cross-project categories (user preference, tool-use/tooling,
   debugging recipe, process, anti-pattern) to global; everything project-specific
   (conventions, architecture, code patterns, testing/deployment, security, docs,
   skills, and `Other`) stays project. The review-gate promotion honours an
   explicit `GlobalMemory` suggestion or this classifier, but **only** when the
   project opts in — otherwise it falls back to project scope (a safe default,
   never an error).
3. **Merged retrieval with project precedence.** `search` queries the project
   index, then the global index, and merges with project results leading and
   global results that are not already present (deduped by id and body) appended —
   so a project lesson always overrides a global one on conflict while a global
   lesson still surfaces when no project lesson applies. Provenance survives in
   each result's path (a global path lives under the user-home store).

Consequences: cross-project learning is real and on by default, so the engine
gets smarter across a user's projects without configuration. `local_only` still
holds; the global store is on the same machine, never remote. Reuses the existing
scope enums, path resolver, FTS index, review gate, and `delete_memory` (now
scope-aware) rather than a parallel memory system. The global store root is the
per-user home (`~/.localmind/memory`), overridable by an absolute
`global_memory_root` config or the `LOCALMIND_GLOBAL_ROOT` env — the special value
`@project` roots it under each project, which keeps tests/CI hermetic now that
global is default-on (each project's global store is its own; an installed binary
leaves the env unset and uses the home default). Known bound: cross-store
*contradiction* detection and cross-store bm25 score normalisation are not
attempted — precedence is project-leads ordering, not a unified relevance score.
A project that wants the old project-only behaviour sets
`allowed_scopes = ["project"]`.

## D-LM-0016 — One reasoned route-to-review flag for both invalidation and down-weighting

- **Date**: 2026-06-22
- **Status**: accepted

Flagging an accepted memory for review (never deleting it) is the engine's
response both when anchored code changes (change-aware invalidation) and when a
host's learning loop finds a lesson did not improve outcomes (outcome-aware
down-weighting). These are the same mechanism with different reasons, so the
engine exposes one `flag_for_review(memory_id, reason)` that sets the staleness
flag and audits the supplied reason; `mark_stale_candidate` becomes a thin
wrapper passing `"anchored code changed"`. The engine still never auto-deletes —
a human reviewer decides — and it does not itself judge outcomes; it only records
the host's reasoned request. This keeps the down-weighting signal honest in the
audit trail (the review queue shows *why* a memory needs review) without a second
flag column or a parallel review path.

## D-LM-0015 — Memory search results expose the lesson category

- **Date**: 2026-06-22
- **Status**: accepted

`MemorySearchResult` carries the matched memory's `category` (read from the
existing `memory_index.category` column, no schema change). A host injection
layer needs the category to gate or dedup what it injects — for example, to skip
injecting a memory whose category restates guidance the host's rule engine
already enforces — and forcing a second per-hit lookup would be wasteful and
racy. Exposing it at retrieval keeps the engine host-neutral (it states a fact
about the result; it does not decide injection policy) while letting the host
decide. Purely additive to the search contract.

## D-LM-0014 — Loop-outcome lessons reuse the review-gated memory path; rejected outcomes are negative signals

- **Date**: 2026-06-19
- **Status**: accepted

The host (LocalPilot) grows a human-gated developer-process self-improvement loop
(its ADR-0034 is the single source of truth for the loop and its invariant). When
a human approves or rejects a proposed patch, the outcome is written back as a
durable LocalMind lesson so the next run retrieves it and stops repeating a
mistake. This record fixes the engine-side commitment for that writeback; it is a
pointer to the host ADR, not a second loop definition.

The engine's commitment under that loop:

- **Loop-outcome lessons enter through the existing review-gated path, not a new
  store.** An approve/reject outcome becomes a candidate lesson carrying
  `{ trigger, what, why, applies_to, outcome, provenance_ref }` over the existing
  lesson/memory schema; promotion to accepted memory stays a human, review-gated
  step (D-LM-0008/0011). No new durable store, no host-local memory implementation.
- **A rejected outcome is a first-class negative signal**, not absence of a
  lesson: it records *what was proposed and why it was rejected* so retrieval can
  steer the next run away from it. This is the durable-memory realization of a
  debt-ledger → negative-memory feed.
- **Lessons carry provenance and outcome, and a bad lesson is curated, never
  silently trusted** — the supersede/curation path (D-LM-0011) guards against
  lesson-store pollution exactly as it does for any other accepted memory.

Reason: the loop's learning arc is valuable only if it is durable and trustworthy;
reusing the review-gated memory path means loop lessons inherit the same
provenance, epistemic-status (D-LM-0012), and curation guarantees as every other
accepted memory, and the engine stays host-neutral — it learns nothing about the
host's loop beyond the lesson records it already understands.

## D-LM-0013 — LocalMind skills stay advisory/read-only under the host's skill model

- **Date**: 2026-06-17
- **Status**: accepted

The host (LocalPilot) names a stack-wide skill model on two axes — **invocation**
(who reaches an artifact: user-only or discoverable) and **authority** (what reaching
it does: advisory or enforced) — with two artifact types: enforced harness rules and
advisory skills. Model discovery is pull-based (an on-demand skill search, not skill
descriptions pushed into every turn). The single source of truth for that model is the
host ADR (LocalPilot `docs/10-decisions.md`, ADR-0027); this record is the engine-side
pointer, not a copy.

The engine's commitment under that model is unchanged and reaffirmed: a LocalMind
**skill is advisory and read-only** — a reviewable prompt module distilled from
accepted memory, emitted disabled, carrying provenance, and surfaced to a host only as
content to read. The engine **never** executes, enables, disables, or auto-fires a
skill; enabling/retiring is a host-driven, human, review-gated step. Model-invocation
of an advisory skill (when the host opts in) changes only *who may reach* the content,
never the engine's no-side-effect guarantee. This is consistent with D-LM-0004
(distillation/research are review-routed) and the review-gating in D-LM-0008/0011.

Reason: the host owns invocation policy and the command surface; the engine owns
*advisory, review-gated* skill emission. Recording the boundary as a pointer keeps one
authoritative definition (the host ADR) while making the engine's standing obligation
explicit where engine contributors read decisions.

## D-LM-0012 — Memory trust is legible: epistemic status + contradiction flags at retrieval

- **Date**: 2026-06-17
- **Status**: accepted

Every accepted memory carries a deterministic **epistemic status**
(observation / hypothesis / fact / decision / procedure), classified by a total
function of its lesson category (`EpistemicStatus::from_category`) and stored on
`memory_index` (schema v6) plus the Markdown front matter. No model is involved;
the same category always yields the same status, so the agent can say *what kind*
of knowledge it is using.

Contradictions are detected **at promotion**, deterministically: a new memory
that shares a topic (`related_entities`) with an active memory but takes the
opposite recommendation polarity (one prohibits what the other endorses) creates
a `contradicts` relationship — stored both ways in `memory_relationships` and
flagging both rows `contradicted` — so retrieval surfaces the conflict instead of
asserting one side. Consistent with D-LM-0008, a contradiction is a *signal*, not
a deletion: both memories stay active and served, and a human resolves the
conflict (supersede / keep / refresh). A provenance answer (`provenance`) reports
source session, confidence, epistemic status, staleness, and the contradicted
memories for any id — the "why do you think that?" surface.

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
