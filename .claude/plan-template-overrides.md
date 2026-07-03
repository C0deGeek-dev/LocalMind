# Plan-Template Overrides — LocalMind

Project-specific content spliced into a copy of the canonical plan template
(the `plan-from-template` skill in the c0degeek-ai plugin). The canonical
template is generic; everything LocalMind-specific lives here. Never fork the
template — generic improvements go upstream to c0degeek-ai instead.

LocalMind has no dedicated planning skill, so the c0degeek `plan-from-template`
skill auto-splices this file from its conventional path
(`.claude/plan-template-overrides.md`). Each section below names the extension
point in the copied plan where its content lands.

> **LocalX workspace note.** Plans, tasks, and work tracking live in the private
> LocalHub repo (`LocalHub/plans/localmind/`), never in this repo. This repo
> keeps only its `docs/`, README, `vision.md`, and CHANGELOG. See
> `LocalX/CLAUDE.md`.

## §2 Verification-commands rows (repo defaults, mirror CI)

| Purpose | Command | Notes |
|---|---|---|
| Build | `cargo check --workspace` | from `.github/workflows/ci.yml` |
| Test | `cargo nextest run --workspace` then `cargo test --doc --workspace` | nextest skips doctests |
| Lint/format | `cargo fmt --check` then `cargo clippy --workspace --all-targets -- -D warnings` | both clean |
| Docs link-check | `lychee --no-progress --offline docs docs/wiki README.md` | adopt lychee (org-pinned) |

## §4 ADR promotion target

Durable architecture decisions graduate to `docs/decisions.md` (the repo's
decision log); cite them from the plan's decision log. Transient
build-sequencing choices stay in the plan.

## §6 plan-specific principles (slot 16)

- **Clean-room provenance is blocking** (LocalX §6.14). Code, prompts, tests,
  identifiers, and UI copy original to this repo; official public APIs or local
  servers only; never copy/translate proprietary or undocumented behaviour.
- **Doc-ownership map (which doc owns which area).** Match a change to its owning
  doc; do not restate an area in two places.
  - `README.md` — lean overview, install/quickstart, ecosystem links.
  - `vision.md` — product vision/direction (not shipped-behaviour reference).
  - `docs/on-disk-contract.md` — the storage/on-disk contract (canonical).
  - `docs/decisions.md` — ADRs / durable decisions.
  - `docs/research-distillation.md` — research notes feeding design.
  - `docs/planning-handoff.md` — cross-session planning handoff.
  - `docs/wiki/` — wiki source (see below).
  - `CHANGELOG.md` — every user-facing change, under an Unreleased/next heading.
- **Wiki source of truth is in-repo.** `docs/wiki/` is authoritative and
  PR-reviewed; the published GitHub Wiki is a one-way generated mirror — never
  hand-edited on github.com. Wiki Reference pages link the owned `docs/`, never
  duplicate them.
- **VERSION discipline, both directions.** No README/doc/wiki claim may exceed
  the current `VERSION` (read the `VERSION` file, never hardcode a literal), and
  no doc describes a retired stack as current; `vision.md` aspirations are marked
  as direction, not shipped behaviour.

## §7 plan-specific gates

- [ ] `cargo fmt --check`, clippy, nextest, and doctests pass or blockers
      recorded.
- [ ] Durable architecture decisions promoted to `docs/decisions.md` and cited.
- [ ] No README/doc/wiki claim exceeds the current `VERSION`.

## Captain Hindsight prompt — extra "Check specifically for" lines

- Clean-room provenance: any copied prompt/identifier/UI copy or
  private/undocumented behaviour.
- Any `README.md`/`docs/`/`docs/wiki/` claim that does not match shipped
  behaviour at the current `VERSION` (and `vision.md` aspiration leaking into
  shipped-behaviour docs), or a wiki page hand-edited on github.com.
- Whether a decision is durable enough to promote to `docs/decisions.md`.
