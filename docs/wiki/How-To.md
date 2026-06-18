# How-To guides

Task-oriented recipes — each answers a single "how do I…?" against shipped
behaviour at the current `VERSION`. See **[[Getting-Started]]** first.

> **Do not edit on github.com.** This wiki is generated from in-repo Markdown
> under `docs/wiki/` and synced one-way on every push to `main`. Edit the source
> in `docs/wiki/`; web edits are overwritten on the next sync.

## Import a session transcript

```powershell
localmind import .\session.txt --project . --source open-ai-codex
```

Sources: `generic`, `claude-code`, `open-ai-codex`, `localpilot`. Formats:
`plain-text`, `json-lines`, `markdown`. Imports write a content-derived folder
under `.localmind/sessions/<id>/` with a **redacted** transcript and metadata —
likely API keys, tokens, private keys, and configured sensitive paths are
stripped before anything is stored.

## Close out a session into candidate lessons

```powershell
localmind closeout session-1234 --project .
```

The deterministic extractor writes `summary.json` and `candidates.json`, then
enqueues candidates idempotently for review. No durable memory is written yet.

## Review and promote lessons

```powershell
localmind review list --project .
localmind review inspect lesson-1234 --project .
localmind review accept lesson-1234 --project . --reviewer david
localmind review edit lesson-1234 "Prefer deterministic fixtures." --project .
localmind promote lesson-1234 --project .
```

Promotion writes the Markdown memory file, updates the search index and
relationship metadata, and records a SQLite audit event.

## Export agent context and draft skills

```powershell
localmind context export "deterministic fixtures" --target localpilot --project .
localmind skills generate --project .
localmind skills list --project .
```

Targets: `generic`, `claude-code`, `open-ai-codex`, `localpilot`. Generated
`SKILL.md` drafts land disabled under `.localmind/skill-drafts/`; LocalMind never
installs or activates skills automatically.

## Check memory quality

```powershell
localmind eval            # extraction precision/recall, retrieval recall@k
localmind eval -k 5 --json
```

A regression gate over a golden fixture set; `-k` sets the retrieval cutoff,
`--json` emits machine-readable output.
