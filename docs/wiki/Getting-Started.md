# Getting started

LocalMind turns opted-in AI development sessions into reviewed project memory,
graph-connected knowledge, and reusable skills — local-first, with no cloud
dependency and no autonomous memory writes.

> **Do not edit on github.com.** This wiki is generated from in-repo Markdown
> under `docs/wiki/` and synced one-way on every push to `main`. Edit the source
> in `docs/wiki/`; web edits are overwritten on the next sync.

## Build

LocalMind is a Rust workspace. The local gate mirrors CI:

```powershell
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p localmind-cli -- --help
```

## Opt in per project

LocalMind refuses project memory writes unless the repository contains an
enabled `.localmind.toml`:

```toml
[learning]
enabled = true
local_only = true
memory_root = ".localmind/memory"
allowed_scopes = ["project"]
excluded_paths = ["target/**", ".git/**"]
```

Accepted memory is written as readable Markdown under `.localmind/memory/`; the
queue, audit log, and search index live in `.localmind/localmind.sqlite`.

## The core loop

```powershell
localmind import .\session.txt --project . --source open-ai-codex
localmind closeout session-1234 --project .
localmind review list --project .
localmind promote lesson-1234 --project .
localmind search "deterministic fixtures" --project .
```

Import → closeout (deterministic summary + candidate lessons) → review → promote
to durable memory → search. Nothing is written to durable memory until you
accept and promote it.

## Next steps

- [[How-To]] — import, review, promote, export context, skills.
- [[Examples]] — a full opt-in → memory walkthrough.
- [[Reference]] — the on-disk contract and the rest of the in-repo docs.
- [[Troubleshooting]] — common problems and fixes.
