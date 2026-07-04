# On-disk contract

What LocalMind writes inside a project, precisely enough that a host other
than the bundled CLI could integrate from this document alone. Everything lives
under the project root, with **one exception**: machine-wide *global-scope*
memory — on by default — lives in a per-user home store (`~/.localmind/memory`)
so cross-project knowledge is shared (see §Global-scope store). A project that
wants project-only memory narrows `allowed_scopes` to `["project"]`.

## Configuration: `.localmind.toml`

This file is **required to enable learning**: a project with no `.localmind.toml`
refuses every learning write (a typed `MissingConfig` error), so a repo is never
learned from until its owner opts in by creating the file. Once it exists,
learning is **on** and `local_only` + review-gated by default, and the defaults
below apply to any key you omit. It lives at the project root, is where you opt
out (`enabled = false`), and is where you tune the engine:

```toml
[learning]
enabled = true                       # the default; `false` opts out (refuses all writes)
memory_root = ".localmind/memory"    # optional; must stay inside the project
allowed_scopes = ["project", "global_user"]  # optional; the default — narrow to ["project"] for project-only memory
global_memory_root = "/abs/path"     # optional; absolute; overrides ~/.localmind/memory (also the LOCALMIND_GLOBAL_ROOT env, or @project for under-project)

[inference]
chat_base_url = "http://127.0.0.1:8080"      # optional local OpenAI-compatible endpoint
chat_model = "local-chat-model"              # required if chat endpoint is used
embedding_base_url = "http://127.0.0.1:8080" # optional; falls back to chat_base_url
embedding_model = "local-embedding-model"    # required if embeddings are used
api_key_env = "LOCALMIND_API_KEY"            # optional env-var name only
timeout_secs = 120                           # optional; must be > 0

[inference.features]
extraction = true
review = true
embeddings = true
skills = true
research = true

[review]
mode = "manual"              # manual, assisted, trusted, or automatic
trusted_threshold = 0.82     # trusted auto-accept threshold
semantic_dedup = false       # opt in to embedding-cosine dedup of accepted memory; off = lexical only

[retrieval]
rerank = false               # opt in to the embedding rerank stage; off = deterministic blend only
rerank_window = 20           # how many top blended hits rerank may reorder
```

A **missing file refuses all learning writes** (a typed `MissingConfig` error) —
the file is the opt-in. Once it exists, an omitted key takes its default
(learning on, `local_only`, `["project", "global_user"]` scope); `enabled = false`
opts out. Malformed TOML, or a `memory_root` that escapes the project root, are
hard, typed errors — never silent fallbacks. When `[inference]` is absent, model-backed extraction,
embeddings, review
annotation, skill writing, research, and distillation are disabled and the
deterministic paths remain active.

`[retrieval].rerank` is opt-in and inert on its own: it reorders the top
`rerank_window` blended hits by embedding similarity **only** when an
`[inference]` embedding endpoint is also configured. Without both, the
deterministic blend order is the whole contract — ranking stays reproducible and
offline, byte-identical to the no-rerank path.

**Embedding accepted memory.** When an `[inference]` embedding endpoint **and**
`embedding_model` are configured (and `features.embeddings` is on — the default),
each accepted memory is embedded into `vector_index` at promotion/persist time
(its body → one f32 vector, keyed by a content fingerprint so an unchanged body
is not re-embedded). Embedding is opt-in in practice: with no `embedding_model`
set, no vectors are written and retrieval/dedup stay purely lexical. It is also
**best-effort** — if the endpoint is down, slow, or returns no vector, the
memory still persists (the Markdown file and index row are already durable); the
failure is recorded as an `InferenceCallFailed` audit row and the memory simply
carries no vector until it is re-embedded. A promotion never fails because
embedding could not run.

`[review].semantic_dedup` is opt-in and inert on its own (same gate shape as
rerank): when it is on **and** an `[inference]` embedding endpoint is configured,
the review-mode dedup of a candidate against accepted memory adds an
embedding-cosine pass after the lexical one — lexical token-overlap stays the
cheap first pass and the no-embeddings fallback, then a candidate the lexical
pass did not already flag is embedded and compared by cosine to accepted-memory
vectors. The cosine search scans **both** the project and the machine-wide
global `vector_index` (project precedence), so a `GlobalUser`-scoped lesson — whose
vector lives in the global index — is dedup-eligible just like a project one. A
match at cosine ≥ 0.86 flags a **confident** `duplicate_of`; a match in the
**route-to-review band** `[0.83, 0.86)` flags a **borderline** `duplicate_of`
(annotated as such). Both tiers route to review and are **never auto-deleted or
auto-merged** — the band only widens what a human sees, never what is removed.
Without both the flag and an endpoint, dedup is byte-identical to the lexical
contract.

**Write-time quality gate.** Alongside the dedup, confidence, and conflict
checks, every candidate carries a deterministic, offline **quality** verdict —
`general`, `over-fit`, or `tooling-noise` — computed from its category and text
(`classify_quality`). A *tooling-noise* lesson is a build-tool / shell /
working-directory / OS-env mechanic, not a code lesson; an *over-fit* lesson is a
claim welded to one exercise's identifiers/literals with no transferable
principle. The verdict is marked at extraction (in the candidate's
`review_annotation.notes`, visible in `candidates.json` and to an Assisted
reviewer) and **enforced at the accept seam**: under trusted/automatic mode a
non-`general` candidate is **withheld from auto-accept and routed to manual
review** — treated exactly like a duplicate. It is never discarded and never
deleted (the standing never-auto-delete invariant); a human still decides. An
error-code/diagnostic recipe (e.g. `error[E0107]`) is kept as `general`
(specific but generalizable), and a path/shell phrase inside a security or
architecture lesson is not read as tooling noise (the category gate). The
classifier is pure, so the contract is the verdict, not any keyword list.

The **freshness pass** reuses the same classifier retroactively: a `low-quality`
reason (the most-actionable, age-independent) flags an already-stored
tooling-noise or over-fit lesson — one that predates the write gate — for review
across the project and global stores, alongside the existing age /
never-retrieved / version-sensitive reasons. It only routes to review (sets
`stale_candidate`), never deletes, and honours the per-run cap and the dry-run.
An operator applies it with `learning freshness`.

## Directory layout

```
<project root>/
  .localmind.toml
  .localmind/
    localmind.sqlite          # single shared database (schema below)
    memory/
      project/<memory-id>.md  # one Markdown file per accepted memory
    skill-drafts/
      <skill-id>/SKILL.md      # generated skill draft Markdown
      <skill-id>/draft.json    # serialized SkillDraft
    sessions/
      <session-id>/
        metadata.json             # ImportedSession record
        transcript.redacted.txt   # transcript AFTER redaction; raw text is never stored
        summary.json              # SessionSummary (written at closeout)
        candidates.json           # Vec<CandidateLesson> (written at closeout)
```

Only the redacted transcript is persisted. Redaction runs before any write
(pattern table + entropy backstop; see `localmind-store/src/redaction.rs`
module docs for the caught / not-caught guarantee).

## Global-scope store (on by default)

Unless a project narrows `allowed_scopes` to `["project"]`, a machine-wide store
is opened alongside the project store, rooted at the per-user home (resolved
cross-platform from `USERPROFILE`/`HOME`), or at an absolute `global_memory_root`
override (or the `LOCALMIND_GLOBAL_ROOT` env; the value `@project` roots it under
the project, used by tests/CI):

```
~/.localmind/
  localmind.sqlite              # the global index, shared across every project
  memory/
    global/<memory-id>.md       # one Markdown file per accepted global memory
```

A `GlobalUser`-scoped memory is written here instead of under the project; every
other scope stays project-rooted. The global database is shared across projects,
so a global lesson written in one project is retrievable in another. Retrieval
merges the project and global indexes with **project precedence** (a project
memory overrides a global one on conflict; a global memory surfaces when no
project memory applies) — for both the keyword (FTS) and the semantic (vector)
paths: `vector_search` scans the project and global `vector_index` the same way,
so a global memory contributes a vector score in hybrid retrieval and is
dedup-eligible (D-LM-0023). A project that narrows `allowed_scopes` to
`["project"]` opens no global store and is byte-for-byte unchanged. See D-LM-0017.

## Markdown memory format

One file per accepted memory at `memory/<scope>/<id>.md`: YAML-style front
matter between `---` fences, then the body.

| Front-matter key | Presence | Meaning |
|---|---|---|
| `id` | always | memory id (also the file stem) |
| `scope` | always | `Project`, `GlobalUser`, … |
| `category` | always | lesson category (`Process`, `DebuggingRecipe`, …) |
| `epistemic_status` | always | trust class derived from category (`observation`/`hypothesis`/`fact`/`decision`/`procedure`) |
| `confidence` | always | fixed 3-decimal float in [0, 1] |
| `source_session` | optional | originating session id |
| `created_at` / `updated_at` | optional | RFC 3339 timestamps |
| `tags`, `related_files`, `related_entities` | when non-empty | string lists |
| `supersedes`, `contradicts` | when non-empty | memory-id lists |
| `evidence` | when non-empty | list of `{id, kind, label, redacted, uri?}` |

Scalars containing `: # ' "` or newlines are single-quoted with `''`
escaping. The Markdown file is the human-readable source of truth; the
database rows below are derived and rebuildable from it.

## Database schema: `.localmind/localmind.sqlite`

Database schema lifecycle is versioned with `PRAGMA user_version`
(currently **8**); every component steps the schema on open and refuses
databases newer than it understands. Tables:

| Table | Owner concern | Notes |
|---|---|---|
| `schema_migrations(version, applied_at)` | human-readable baseline marker | records only the baseline (`version = 1`); the stepper does **not** append a row per applied step, so `PRAGMA user_version` (above) is the authoritative schema version — read that, not this table, to gate on the schema |
| `review_items(id, session_id, candidate_json, state, reviewer_action, reviewer, note, replacement_summary, created_at, updated_at)` | review queue | `candidate_json` is a serialized `CandidateLesson` |
| `audit_events(id, kind, actor, subject, metadata_json, happened_at)` | audit log | `metadata_json` is always valid JSON (serde-built) |
| `memory_index(memory_id, path, scope, category, body, source_session, status, created_at, stale_candidate, epistemic_status, contradicted, confidence, language, hit_count, last_used_at)` | search index over accepted memory | `status = 'active'` rows are live; `stale_candidate = 1` flags change-aware staleness; `epistemic_status` ∈ {observation, hypothesis, fact, decision, procedure} (derived from category); `contradicted = 1` when in a `contradicts` relationship; `confidence` mirrors the entry's; `language` is the single programming language the lesson is about (NULL = general/cross-cutting, eligible for every task), used to filter off-language lessons in retrieval; `hit_count` (default 0) and `last_used_at` (NULL = never) are the **runtime usage signal** — bumped post-turn when a memory is injected, used by the freshness pass to surface never-retrieved dead weight. Unlike the other columns these two are **not** rebuildable from the Markdown source of truth: a reindex resets them to zero-usage (the same state as a pre-v8 upgrade), which is acceptable for a best-effort signal |
| `memory_fts(memory_id UNINDEXED, body)` | FTS5 index | queried with `MATCH` + bm25 |
| `memory_relationships(memory_id, relation_kind, target)` | typed relations | kinds: `category`, `session`, `file`, `entity`, `contradicts` |
| `vector_index(subject_kind, subject_id, source_fingerprint, model, dimensions, vector_blob, updated_at)` | rebuildable semantic index | f32 little-endian BLOBs; exact cosine in Rust |
| `distilled_records(id, kind, title, body, source_memory_ids_json, status, created_at, updated_at)` | distillation/research candidates | derived, review-routed; not source of truth |
| `skill_records(skill_id, draft_json, status, source_memory_ids_json, created_at, updated_at)` | active/retired skill lifecycle | activation and retirement are audited |
| `graph_nodes`, `graph_edges`, `graph_meta` | code-structure graph | payload format versioned separately via `graph_meta.format_version` (`GRAPH_FORMAT_VERSION`, currently 1) |

Write-consistency contract: multi-statement writes (promote, persist,
delete) commit atomically; the Markdown file write precedes the indexing
transaction, and deletion removes the file before the database rows so an
interrupted delete heals on retry.
`vector_index`, relationships, FTS, and memory index rows are all derived from
Markdown memory or graph state and may be rebuilt — except the `hit_count` /
`last_used_at` usage columns, which are runtime-accumulated and reset to
zero-usage on a rebuild. Inference audit rows record
feature, endpoint kind, model id, and token counts when available; raw prompt
or response content is never written to `audit_events`.

## Portable memory bundle

A **bundle** is a portable, self-describing JSON pack of *accepted* memory that
can be moved to another machine or shared with another person and re-imported —
unlike a context export (`localmind context export`), which renders memory as
prose for a prompt and is one-way. The bundle is built from the Markdown source
of truth (parsed back into entries via `MarkdownMemoryFormat::parse`), so it
reuses the canonical model serde and is **not** a second serialization of a
lesson.

Shape (`format_version` **1**):

```json
{
  "format_version": 1,
  "metadata": {
    "created_by": "<author id, or \"anonymous\">",
    "scope_selection": "project | global | both",
    "entry_count": <n>,
    "redaction_count": <total redaction hits applied on export>
  },
  "entries": [ <MemoryEntry>, ... ]
}
```

- **Accepted-only.** Only `status = active` memory is exported; the selected
  scope (`project`, `global`, or `both`) filters which entries are included.
- **Redacted on export.** Each entry's body and evidence labels/URIs are run
  through the same `Redactor` again (defense in depth on top of capture-time
  redaction); `metadata.redaction_count` and the returned `SecretScanReport` are
  the seam a caller uses to require an explicit confirm before sharing.
- **Deterministic + content-addressable.** Entries are ordered by id and the
  canonical bytes (entries sorted by id, compact JSON) are stable across runs and
  machines, so a digest/signature over them is reproducible.
- **Versioned.** A reader rejects a bundle whose `format_version` is newer than
  it understands, with a reason — old packs keep importing across upgrades.
- **Relationship to seed packs.** A seed pack (`localmind`/`localpilot learning
  seed`) is *author-curated input* (a `SeedLesson` list) written directly into
  accepted memory at authoring time; a bundle is *exported output* — a faithful,
  signed round-trip of memory the store already accepted. Seeds remain valid and
  unchanged; the bundle is the new re-importable, attributable format.

### Signed bundle

A bundle is made tamper-evident and attributable by wrapping it in a signature
envelope (`SignedBundle`), the on-disk shared/portable form:

```json
{
  "bundle": { <MemoryBundle> },
  "signature": {
    "alg": "ed25519",
    "digest_alg": "sha256",
    "schema_version": 1,
    "digest": "<hex sha256 of the bundle canonical bytes>",
    "signature": "<hex ed25519 signature (64 bytes)>",
    "public_key": "<hex ed25519 public key (32 bytes)>",
    "author": "<sha256(public_key)[..16] fingerprint>"
  }
}
```

- **Signing.** Ed25519 over the SHA-256 digest's input — the bundle's canonical
  bytes (entries sorted by id, compact JSON). The envelope carries only public
  material; the private key never appears in it.
- **Verification is fail-closed**, yielding one of:
  - `Rejected` — bad digest, bad signature, malformed key/signature,
    author/key mismatch, or unsupported schema/bundle version. Never imported.
  - `Untrusted` — a valid signature by an *unknown* key (heavier review).
  - `Trusted` — a valid signature by a *known* key (your own, or one in the
    local trust list).
- **Verified author ≠ verified content.** A signature attests integrity and
  authorship only; imported memory is still routed through the review queue,
  never auto-promoted.
- **Keys.** A local Ed25519 keypair under `<home>/.localmind/keys/signing.json`
  (`0600`, owner-only — the BYOK pattern, host ADR-0042), with a manual trust
  list in `trusted.json`. Trust is local: no PKI, registry, or network.
  The author is a key-bound fingerprint, so it cannot be spoofed with another
  key. See `docs/decisions.md` D-LM-0018.

## OKF (Open Knowledge Format) interop

LocalMind reads and writes Google Cloud's **Open Knowledge Format (OKF) v0.1** —
a directory of Markdown files with YAML front matter, only `type` required,
reserving `title`/`description`/`resource`/`tags`/`timestamp`, with
no-front-matter `index.md` navigation and a markdown-link concept graph. This is
an **import/export profile over the Markdown memory format above, not a second
store** (see `docs/decisions.md` D-LM-0025).

**Export** (`localmind okf export <dir>`) writes accepted memory as an OKF bundle:
one concept `.md` per memory, grouped into per-`type` directories, with an
`index.md` in each directory (and at the root). Each concept carries the OKF
reader-facing keys **and** the full native front matter (§Markdown memory format
above), so it is at once a conformant OKF concept and a lossless LocalMind memory:

```
---
okf_version: "0.1"
type: <category>              # OKF reader-facing keys
title: <derived from the body>
description: <derived, optional>
resource: <first related file / evidence uri, optional>
timestamp: <updated_at, RFC 3339, optional>
id: <memory id>               # the native block (§Markdown memory format)
scope: <...>
category: <...>
confidence: <...>
# ... the remaining native keys ...
---

<body>
```

- **Selection + redaction reuse the signed-bundle exporter**, so an OKF export
  applies the same scope filter and the same defence-in-depth secret redaction; it
  is read-only over the store.
- **`index.md` files carry no `type`**, so a re-import skips them (navigation, not
  concepts) — which is what keeps concept bodies byte-lossless across an
  export→import cycle. Cross-concept edges (`supersedes`/`contradicts`) live in each
  concept's native front matter, never injected into a body.

**Import** (`localmind okf import <dir>`) reads an OKF bundle and enqueues each
concept as a **review candidate** — never written straight to active memory.

- A **LocalMind-origin** file (native keys present) round-trips losslessly through
  the canonical parser.
- A **foreign** concept (reserved fields only) is synthesized into a low-trust entry
  (unknown `type` → `Other`, conservative confidence, `scope = Project`, a
  deterministic `okf-<hash>` id); the foreign reader also accepts inline-flow
  (`tags: [a, b]`) and quoted scalars.
- An OKF bundle is **unsigned**, so every concept is flagged **untrusted** (contrast
  the signed portable bundle above), is quality-gated (D-LM-0024) at the accept seam,
  and is embedded at promotion via the normal path — never at import, so the review
  gate is never bypassed.
- Non-conformant files (no `type`, e.g. an `index.md` or `log.md`) are skipped and
  counted. A `--dry-run` (the default) reports what an apply would enqueue without
  writing.

OKF v0.1 is a starting point, not a finished standard: the adapter depends only on
`type` + the reserved set, emits `okf_version` so drift is detectable, and treats
later versions as out of scope.

## Versioning

- Database schema: `PRAGMA user_version` stepper (above).
- Graph payload: `graph_meta.format_version`.
- Portable bundle: `format_version` (currently **1**).
- OKF profile: `okf_version` (currently **0.1**).
- Crate/API versions: workspace version + `CHANGELOG.md`; release tags
  (`v<workspace version>`) mark contract-relevant changes. Hosts pinning a
  commit (e.g. a git submodule) get the contract as of that commit.
