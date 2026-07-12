# Decisions

Durable, engine-internal architecture decisions for LocalMind. Host-side
decisions live with the host; this file records choices that hold regardless
of which host embeds the engine.

## D-LM-0027 — Cross-device sync is encrypted, folder-carried, and review-gated; it reuses the signed-bundle machinery rather than forking it

- **Date**: 2026-07-12
- **Status**: accepted

Memory should follow a user across their machines, but doing so safely rules out
the obvious shapes: a cloud service (a third party holds the knowledge), plaintext
in a synced folder (the transport provider reads it), and last-writer-wins merge
(silent knowledge loss). LocalMind already had the hard parts from the
signed-portable-bundle work (D-LM-0018): an Ed25519 identity, fail-closed
verification against a local trust list, and a review-gated importer, plus the
never-auto-delete/route-to-review invariant (D-LM-0016), embedding dedup
(D-LM-0020), and the write-time quality gate (D-LM-0024). Sync is therefore a thin
layer over that machinery, not a second system.

Decision — five parts:

1. **No networking; a dumb folder is the transport.** LocalMind opens no sockets
   for sync. The user points it at a folder their own tooling already
   replicates (Syncthing, OneDrive, a share, a private git repo). Sync is
   command-driven (`localmind sync run`/`status`), not a daemon.

2. **Per-device identity + out-of-band enrollment.** Each device gains an X25519
   encryption keypair beside its Ed25519 signing key (same `0600` key-store
   hygiene). A device publishes a **card** (its label + both public keys); the
   local trust list doubles as a **device registry** (an added, optional
   encryption key per entry). Enrollment is refused unless the fingerprint the
   user confirms out-of-band matches the card — the card's own JSON deliberately
   omits the fingerprint so it cannot vouch for itself. Revocation drops a device
   as both an encryption target and a trusted signer.

3. **Signed, then sealed, incremental op-bundles.** A payload is a set of
   create/update/supersede/tombstone ops over accepted memory, signed by the
   **same** Ed25519 signer as the portable bundle (the shared logic is extracted,
   so there is one signer, not two) and then sealed to every enrolled device with
   a NaCl sealed box (`crypto_box`: X25519 + XSalsa20Poly1305). **Fail-closed:** a
   bundle that cannot be encrypted to at least one enrolled device is never
   produced, so the folder only ever holds ciphertext under opaque,
   content-addressed names. Op identity is content-addressed and conflicts route
   to review, so the version stamp is a content fingerprint, not a causal counter
   (a vector clock would be an optimization the never-auto-delete posture does not
   need). The crypto crate is pinned pure-Rust within MSRV 1.82; no bespoke
   crypto.

4. **Review-gated import with fail-closed trust.** On a run, each peer bundle is
   parsed tolerantly (a partial transport write or a foreign file is skipped),
   decrypted with this device's key (skipped if not a recipient), and verified —
   an **unknown signer is rejected fail-closed**. Every accepted op becomes a
   *review candidate* (never straight to active memory), inheriting the existing
   dedup + quality gate at the accept seam. A same-memory divergence routes to
   review under a fresh id so promotion can never overwrite the local memory (no
   last-writer-wins); a proposed deletion flags the memory for review (never
   auto-deletes). A per-peer cursor plus a local-identical check make re-runs
   idempotent and prevent echo loops. Auto-accepting an enrolled device's ops is
   deliberately not done — the safe review-everything posture ships.

5. **Environment scoping; derived state never syncs.** Every memory carries a
   *sync disposition* (`sync`/`machine_local`/`sync_annotated`) over its scope —
   durable `Project`/`GlobalUser` knowledge syncs, `Session`/`Research`/`Skill`
   stay local — and syncable knowledge is stamped with the machine that wrote it.
   A path-independent project identity (the normalized git remote, else an
   explicit key) maps the same repo at different paths to one store. The vector
   index, code graph, and usage counters are never in a payload; an import
   re-embeds locally through the existing promotion hook. Retrieval down-weights —
   never drops — a lesson whose origin machine differs from the one retrieving it,
   so a machine-specific tip does not pollute the wrong box.

Consequences: cross-device memory is confidential to the owner's devices,
tamper-evident, and never merged without a human; the transport is replaceable and
untrusted; and the whole feature is a module behind CLI subcommands that can be
removed without touching the store. Reuses D-LM-0016 (route-to-review /
never-auto-delete), D-LM-0018 (signed bundles, local trust, fail-closed verify),
D-LM-0020 (dedup), and D-LM-0024 (quality gate). Known residual, documented
limits: the ciphertext's size and the recipient count/timing leak even though the
content does not; conflict reconciliation is human, not automatic. See
`docs/on-disk-contract.md` for the storage contract.

## D-LM-0026 — The retrieval rerank stage is host-wired, not deleted; the stored-vector entry point is the host seam

- **Date**: 2026-07-07
- **Status**: accepted

`localmind-search` (hybrid memory search, the deterministic rank blend, and
the opt-in embedding rerank stage) plus the `[retrieval] rerank` /
`rerank_window` config keys shipped engine-side with **zero consumers**,
while the reference host ran its own retrieval — the exact
parallel-implementations drift the ecosystem rules exist to prevent. The
owner decision is **wire, not delete**: the capability was built and
extended deliberately (D-LM-0020/0022/0023 all invest in this crate), the
config keys are shipped and documented, and the CPU embedding infrastructure
already exists.

How it is wired:

1. **`rerank_scored`** joins `rerank_hits` as the crate's second rerank entry
   point: it reorders an already-ranked candidate list by *precomputed*
   query-to-hit cosines. A host that has already scored its candidates
   against the stored `vector_index` (the injection path does, for its
   relevance gate) reuses those scores instead of re-embedding hit texts per
   query — no second embedding pass on a latency-sensitive path.
2. **Same contract as `rerank_hits`:** only the top `rerank_window` hits may
   move; the tail keeps blend order; a hit without a stored vector keeps its
   exact slot (unknown relevance never demotes below the deterministic
   blend); fewer than two scored hits, or a sub-2 window, is the identity.
3. **The `[retrieval]` keys govern it end-to-end:** `rerank = false` (the
   default) or no embedding endpoint ⇒ the host path is byte-identical to
   the unranked flow (`ProjectConfig::rerank_active` stays the single gate).
4. The keyword search stays the **candidate floor** (D-LM-0022): rerank
   reorders keyword candidates, it never introduces hits of its own.

The host consumption itself (which host, which call site) is a host decision;
the engine guarantee is that both rerank entry points behave identically with
the stage off. Supersedes the "library-only, wire-or-remove" limbo recorded
by the 2026-07-06 environment review.

## D-LM-0025 — OKF (Open Knowledge Format) interop is an import/export profile over the canonical Markdown format, not a storage switch

- **Date**: 2026-07-04
- **Status**: accepted

Google Cloud published the Open Knowledge Format (OKF) v0.1 (2026-06): organizational
knowledge as a directory of Markdown files with YAML front matter, only `type`
required, reserving `title`/`description`/`resource`/`tags`/`timestamp`, with
no-front-matter `index.md` navigation and a markdown-link concept graph. LocalMind
already stores accepted memory as Markdown + front matter with a richer schema — and
adds embeddings, semantic dedup, signed portable bundles, review-gating, and a
quality gate, none of which OKF v0.1 has. The value of OKF is therefore
interoperability, not a better engine.

Decision — a bidirectional OKF **profile** over the canonical `MarkdownMemoryFormat`,
never a second store:

1. **Export** (`OkfFormat::to_okf`) reuses `MarkdownMemoryFormat::serialize` verbatim
   and prepends the OKF reader-facing keys (`type`/`title`/`description`/`resource`/
   `timestamp` + `okf_version`). The native block is still present, so the file is at
   once a conformant OKF concept and a lossless LocalMind memory — losslessness is
   *structural* (the native serialization stays the source of truth), not a separate
   field map that can drift.
2. **Import** (`OkfFormat::from_okf`) delegates to `MarkdownMemoryFormat::parse` for a
   LocalMind-origin file (detected by the native keys — a lossless round-trip) and
   synthesizes a low-trust entry from the reserved fields for a *foreign* concept
   (unknown `type` → `Other`, conservative confidence, `scope = Project`, a
   deterministic `okf-<hash>` id). The foreign reader accepts inline-flow YAML the
   canonical block reader does not, scoped to the OKF module.
3. **Bundle import is review-gated and untrusted.** An OKF bundle is unsigned, so
   every concept enters the review queue flagged untrusted — never auto-accepted
   (D-LM-0016) — is quality-gated (D-LM-0024) at the accept seam, and is embedded at
   promotion (never at import, so the gate is never bypassed). This is the OKF-shaped
   sibling of the signed-bundle import (D-LM-0018): OKF has no provenance, the Ed25519
   bundle does; the two coexist and are not merged.
4. **Export reuses the signed-bundle exporter** for scope selection and
   defence-in-depth secret redaction, then writes concepts into per-`type` directories
   with no-front-matter `index.md` navigation. The concept graph lives in the index
   files and each concept's native `supersedes`/`contradicts`, never injected into a
   concept body (which would break the byte-lossless round-trip).

Scope: OKF is import/export only; the SQLite+Markdown store stays canonical, and
embeddings/signing/dedup/review-gating are unchanged. Pinned to OKF v0.1
(self-described "not a finished standard"): the adapter depends only on `type` + the
reserved set and emits `okf_version` so drift is detectable; later versions are out of
scope. Conformance is proven against Google-`knowledge-catalog`-shaped fixtures. CLI:
`localmind okf import <dir>` / `localmind okf export <dir>`. Reuses D-LM-0016
(never auto-delete / route-to-review), D-LM-0018 (bundle import + trust classes),
D-LM-0024 (quality gate).

## D-LM-0024 — Lesson quality is a write-time gate and a retroactive freshness reason, routed to review, never deleted

- **Date**: 2026-06-30
- **Status**: accepted

A model-pinned benchmark showed accepted learning is net-positive but noisy:
tooling/process artifacts (a working-directory or build-wrapper note) and
over-fit, exercise-specific claims (a concrete method call welded to one
exercise's identifiers) auto-accepted and then mis-injected into unrelated
tasks, regressing them. The accept path had dedup, confidence, and conflict
gates but no **quality** dimension, and tooling/process/debugging lessons
globalize machine-wide, so a bad one spread.

Decision — one deterministic, offline classifier, `classify_quality(category,
summary, body) -> {General, OverFit, ToolingNoise}`, used at three seams (mark,
enforce, retroactive) so they can never diverge:

1. **Marked at extraction** (`annotate_candidate`, both the deterministic and the
   model paths) in `review_annotation.notes`, so the verdict shows in
   `candidates.json` and to an Assisted reviewer.
2. **Enforced at the accept seam** (`review_modes::apply_project`): under
   trusted/automatic mode a non-`General` candidate is **withheld from auto-accept
   and routed to manual review**, treated exactly like a duplicate. Manual and
   Assisted modes are unchanged. Nothing is discarded.
3. **Retroactive** (`freshness::classify`): a `LowQuality` reason — the most
   actionable, **age-independent** — flags an already-stored bad lesson (one that
   predates the gate) for review across the project and global stores, honouring
   the existing per-run cap and dry-run.

The classifier is conservative (route, don't drop): the cost of a wrong label is
bounded to "a human re-judges it", because **nothing here deletes** — the standing
never-auto-delete invariant (D-LM-0016), the same route-to-review path the
freshness pass already uses. An error-code/diagnostic recipe stays `General`
(specific but generalizable), and a path/shell phrase inside a security or
architecture lesson is not read as tooling noise (the category gate). Markers
match whole words/phrases, never bare substrings, so a marker cannot fire inside
an unrelated word. Refines D-LM-0005 (evidence-grounded extraction) with a quality
axis; reuses D-LM-0016.

## D-LM-0023 — The semantic (vector) rung reaches the machine-wide global store

- **Date**: 2026-06-29
- **Status**: accepted

D-LM-0020 made accepted-memory dedup embedding-backed but scoped it explicitly
"to the project vector store … broadening to global vectors is a later step."
That later step was never taken, and the gap was load-bearing: `GlobalUser`-scoped
lessons (cross-project tooling, debugging, process, anti-pattern knowledge —
D-LM-0017) embed their vectors into the **global** `vector_index`, but
`MemoryPersistence::vector_search` queried the **project** index only. So the
semantic dedup rung embedded the candidate, searched an empty project index, found
nothing, and — under automatic mode — auto-accepted near-identical global
paraphrases (a warm run accepted several "PowerShell `&&`" restatements; three
pairs measured cosine 0.881–0.896, well above the 0.86 bar, yet `review_items =
0`). The lexical rung was already global-aware (`search_lang`), so this was an
asymmetry: a primitive that forgot `self.global`. The **same** one-line asymmetry
also blinded global semantic *retrieval* — `localmind-search::hybrid_memory_search`
surfaces a global memory by keyword but, using the same project-only
`vector_search`, never gave it a vector-score contribution.

Decision — three parts, refining D-LM-0020 and reusing D-LM-0010/0017:

1. **`vector_search` is global-aware.** It scans the project **and** the global
   `vector_index`, merging scored candidates with **project precedence** (project
   rows lead; a global row whose `(subject_kind, subject_id)` is already present is
   dropped), then ranks and truncates the combined set — mirroring the
   already-shipped `search`/`search_lang` project+global merge. The fix is made
   once in the shared primitive, so **both** callers (review-mode dedup and
   hybrid retrieval) become global-aware together; no second vector store, no
   forked path.
2. **Duplicate-probe headroom.** `vector_duplicate_of` fetches several nearest
   vectors (mirroring retrieval's `limit.max(20)`) and filters to
   `subject_kind == "memory"` before taking the top, so a higher-ranked
   non-memory subject (e.g. an ingested code chunk) cannot hide a real
   accepted-memory duplicate behind it.
3. **A route-to-review band, not a lowered merge bar.** The confident bar stays
   `VECTOR_DUPLICATE_SIMILARITY = 0.86`; a new `VECTOR_REVIEW_BAND = 0.83` flags a
   *borderline* duplicate for the cosine range `[0.83, 0.86)`. Both tiers route to
   review (the annotation's `duplicate_of` is set and automatic mode holds the item
   for a human); the band only widens what a reviewer sees and **never** auto-merges
   or deletes — the standing **D-LM-0016** invariant. The edges stay test-pinned
   named constants (D-LM-0020's posture), not a learned or TOML-tunable knob; 0.83
   is evidence-derived from the observed paraphrase cluster (0.80–0.95) and sits well
   above the ≲0.6 distinct region.

Safety holds end to end: with embeddings unavailable the semantic rung is skipped
and behaviour is byte-identical to the lexical-only contract (best-effort, D-LM-0020
posture), pinned by a no-regression test. The net effect is *more* candidates
routed to human review and global semantic retrieval restored — never a deletion,
never a silent merge. See `docs/on-disk-contract.md` for the storage contract.

## D-LM-0022 — Ingested chunk knowledge is semantically retrievable: language-tagged, embedded, hybrid-retrieved — lifting the keyword-only-v1 deferral

- **Date**: 2026-06-28
- **Status**: accepted

The ingest chunk store behind a host's `knowledge_search` was keyword-only
(FTS5 + bm25). Chunk embeddings were deferred in the IngestKnowledgeHardening
work — **D009 (2026-06-17): "Subject 05 (gap C, ingest chunk embeddings)
ABANDONED"**, on YAGNI / no-evidence grounds (the D005 evidence bar — a fixture
showing keyword-only ranking a semantically-relevant chunk below the budget
cutoff where an embed would surface it — was unmet), with the explicit clause
*"Gap C may be reopened as a fresh plan if recall proves insufficient at scale."*

That reopen clause is now exercised, and the precondition has changed: the
SemanticMemoryEmbeddings work (D-LM-0020) stood up a reusable CPU embedding
server and the full embed/vector/language infrastructure for accepted memory.
So ingested chunk knowledge becomes semantically retrievable by **reusing the
engine's existing primitives**, not by building a second path:

- **Language tag** each chunk by its file extension via `language_for_extension`
  (the same map accepted-memory tagging uses), and filter retrieval to the
  workspace's dominant language (`detect_workspace_language`), `NULL` always
  eligible — mirroring accepted-memory `search_lang`.
- **Embed** each chunk best-effort into a chunk vector index that mirrors the
  `vector_index` shape (D-LM-0003: rebuildable SQLite LE-f32 BLOB, exact cosine
  in Rust), reusing the embedding endpoint and the `InferenceCapability` gate, and
  content-fingerprinted so an unchanged chunk is not re-embedded.
- **Hybrid retrieval**: blend keyword (bm25) and cosine over the stored chunk
  vectors. Keyword stays the **floor** — a keyword hit always outranks a
  vector-only hit; cosine only sub-orders and adds recall. This is the same
  additive-over-deterministic posture as D-LM-0010, applied to the ingest layer.

This **supersedes** the IngestKnowledgeHardening D009 deferral. The guarantees of
the deferred-from state are preserved: embeddings are opt-in (only active when an
embedding model is configured) and best-effort (a down/unconfigured endpoint
writes no vectors and never fails ingest), and with embeddings absent the keyword
contract is byte-identical to before. The chunk store and `knowledge_search` are
host-side (LocalPilot); this decision records the **engine-pattern reuse** that
makes the lift correct — one embedding path, the `vector_index`/cosine pattern,
and the language map shared across accepted memory and ingest.

## D-LM-0021 — Accepted memory has a proactive lifecycle: usage-tracked, freshness-flagged, review-gated, never auto-deleted

- **Date**: 2026-06-28
- **Status**: accepted

The store accumulated and was semantic-deduped and language-tagged, but had no
*proactive* lifecycle: a lesson about a deprecated flag/API lived forever unless
something contradicted it, and the change-aware staleness flag only fires for
memory anchored to a project's code — so the ~140 non-code-anchored global lessons
(language idioms, tooling notes) were never re-checked, and there was no usage
signal to surface dead weight. This adds the missing freshness/relevance half.

Decision — three additive pieces, all routing through the **existing** review gate
(`flag_for_review` → review queue), **never auto-deleting** (precedent D-LM-0016):

- **Usage tracking (schema v8).** `memory_index` gains `hit_count` (default 0) and
  `last_used_at`, bumped when a memory is injected into a turn. The bump is
  best-effort and **post-turn, never on the retrieval read path** — retrieval stays
  read-only and fast. The columns are runtime-accumulated (a reindex resets them to
  zero-usage); pre-v8 rows upgrade as zero-usage.
- **A deterministic, offline freshness pass.** It flags accepted memory for review
  by three independent, conservative heuristics — age, never-retrieved-after-a-grace
  (the grace is essential since every memory starts at zero usage), and a
  version-sensitive keyword/semver heuristic — across the **project and global**
  stores, covering the non-code-anchored lessons the change-aware flag misses. It
  only *flags for review*; it never deletes, never re-ranks, and a per-run cap plus
  a dry-run keep it from flooding the queue. The selection logic is pure and
  fully offline-testable.
- **Optional source re-validation (opt-in, default-off, disclosed).** A sampled
  pass asks the configured model whether version-sensitive lessons are still
  current and routes a "no longer true" verdict to review. It is the deeper,
  network-touching check; the offline freshness pass is the default. Egress is the
  caller's explicit, disclosed choice (a preview contacts nothing). The
  sample→check→flag logic is decoupled from the model by a verdict-source trait, so
  it is offline-testable with a fixture; the live run is opportunistic.

A human (or the existing automatic-review mode) decides every flagged lesson;
retirement is supersede (reversible, D-LM-0008) or an explicit delete. Nothing in
the lifecycle silently loses a lesson, and nothing slows a turn. Operator-invoked
via the host CLI (no background daemon). Acceptance is the deterministic offline
suite; a live re-validation run is opportunistic (egress policy, offline-bar
policy). Refines D-LM-0011 (extends staleness to non-code-anchored lessons) and
reuses D-LM-0016's one route-to-review path.

## D-LM-0020 — Accepted-memory dedup is embedding-backed: lexical pre-filter → vector cosine, opt-in, review-routed

- **Date**: 2026-06-28
- **Status**: accepted

The review-mode dedup of a candidate against **accepted memory** was
lexical-only — an FTS recall plus a token-set overlap ≥ 0.6 (`DUPLICATE_SIMILARITY`).
That misses paraphrases that mean the same thing but share few words ("a failed
edit should rewrite the whole file" vs "replace the entire file when an edit
operation fails" — overlap ~0.33), so near-duplicate lessons both promote and the
warm store accumulates restatements.

Decision: when `review.semantic_dedup` is on **and** an `[inference]` embedding
endpoint is configured (`ProjectConfig::semantic_dedup_active`), the dedup is a
layered ladder rather than lexical-only:

- **Lexical stays the cheap first pass and the no-embeddings fallback.** The FTS
  + token-overlap check runs first; an overlap ≥ 0.6 is a duplicate exactly as
  before, with no embedding call.
- **Vector cosine is the semantic confirmation.** Only when lexical did *not*
  already flag a duplicate, the candidate's summary is embedded and compared by
  cosine to accepted-memory vectors (`vector_index`, via the existing
  `vector_search`). A nearest match at **cosine ≥ 0.86** — a fixed, conservative,
  test-pinned constant, not a learned threshold — flags `duplicate_of`.
- **A flag routes to review, never deletes.** A semantic duplicate takes the same
  path as a lexical one: it sets `duplicate_of` on the review annotation and is
  held for a human (or blocked from auto-accept in trusted/automatic mode). The
  candidate is never silently merged or removed, so the cost of a wrong cut is
  bounded.

Safety and reproducibility hold: with embeddings unavailable the behaviour is
byte-identical to the lexical contract (pinned by a no-regression test), and the
vector path is **best-effort** — a down endpoint, an empty index, or an embed
failure degrades to lexical and never errors the closeout. The threshold is
conservative because normalized embeddings put genuine paraphrases high
(~0.85–0.95) and distinct technical lessons well below (~0.3–0.6).

This refines **D-LM-0007** (which named the accepted-memory duplication path as
the review-mode `duplicate_of` annotation and left embedding dedup as an opt-in
not yet wired) and reuses the same embeddings as **D-LM-0010**'s rerank, so a
single configured local embedding model lifts both dedup and retrieval. Dedup is
scoped to the project vector store (the existing `vector_search` primitive);
broadening to global vectors is a later step.

## D-LM-0019 — Learning is on by default (machine-wide, local-only, review-gated)

- **Date**: 2026-06-27
- **Status**: accepted

`LearningConfig.enabled` now defaults to **`true`** (was `false`), so a project
accumulates reviewed memory out of the box and the machine-wide global store
(D-LM-0017) fills with cross-project lessons — language idioms, tool-use and
shell patterns, build recipes, and their anti-patterns — from ordinary use. The
prior opt-in default left the learning loop dormant for anyone who never wrote a
`.localmind.toml`, which is the common case; the value of accumulated lessons
compounds with use, so on-by-default is the right posture.

Safety is unchanged and load-bearing: the store stays **`local_only`**
(same-machine, never remote), every captured lesson is a **review candidate**
(redacted), never auto-active memory, and a project opts out with
`[learning] enabled = false`. `allowed_scopes` already defaults to
`["project", "global_user"]` (D-LM-0017), so global scope rides the same flip.

**Clean-room measurement caveat (host concern).** A capability measurement that
must read/write *no* accumulated memory has to disable learning explicitly
(`enabled = false`) or isolate the store. The host's clean-room solver path is
expected to stay opt-in to writeback (see the host CHANGELOG / `eval --learn`):
the default flip changes *interactive and agentic* runs, not a measurement that
deliberately turns learning off.

## D-LM-0018 — Portable memory bundles are signed; verify is fail-closed and local-trust; verified author ≠ verified content

- **Date**: 2026-06-27
- **Status**: accepted

LocalMind can now export accepted memory to a portable bundle and import it
elsewhere. Moving knowledge across machines —
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

2. **Local trust, no PKI.** Trust is a local keypair plus a manual trust
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
   review queue (D-LM-0006/0007) — never auto-promoted. The trust UX must
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
