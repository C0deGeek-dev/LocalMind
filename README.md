```
╔═════╗   ╔═════╗		██╗      ██████╗  ██████╗ █████╗ ██╗     ███╗   ███╗██╗███╗   ██╗██████╗
║ ███ ║═══║ ███ ║		██║     ██╔═══██╗██╔════╝██╔══██╗██║     ████╗ ████║██║████╗  ██║██╔══██╗
║ ███ ║   ║ ███ ║║		██║     ██║   ██║██║     ███████║██║     ██╔████╔██║██║██╔██╗ ██║██║  ██║
║ ███ ║   ║ ███ ║║		██║     ██║   ██║██║     ██╔══██║██║     ██║╚██╔╝██║██║██║╚██╗██║██║  ██║
╚═════╝   ╚═════╝║		███████╗╚██████╔╝╚██████╗██║  ██║███████╗██║ ╚═╝ ██║██║██║ ╚████║██████╔╝
 ╚═══════════════╝		╚══════╝ ╚═════╝  ╚═════╝╚═╝  ╚═╝╚══════╝╚═╝     ╚═╝╚═╝╚═╝  ╚═══╝╚═════╝
```

<div align="center">
  <h1>LocalMind</h1>
  <p><strong>Turn reviewed AI sessions into useful project memory. Locally.</strong></p>
  <p>
    <a href="docs/wiki/Getting-Started.md">Getting started</a> ·
    <a href="docs/on-disk-contract.md">On-disk contract</a> ·
    <a href="vision.md">Vision</a> ·
    <a href="https://c0degeek-dev.github.io/LocalStack/">LocalX</a>
  </p>
  <p>
    <a href="https://github.com/C0deGeek-dev/LocalMind/actions/workflows/ci.yml"><img alt="CI status" src="https://github.com/C0deGeek-dev/LocalMind/actions/workflows/ci.yml/badge.svg"></a>
    <img alt="LocalX release train 1.2.0" src="https://img.shields.io/badge/release%20train-v1.2.0-69d987?style=flat-square">
    <img alt="Markdown and SQLite storage" src="https://img.shields.io/badge/storage-Markdown%20%C2%B7%20SQLite-59636e?style=flat-square">
  </p>
</div>

LocalMind is a local-first learning layer for AI-assisted development. It imports
opted-in sessions, removes likely secrets, extracts candidate lessons, asks a
human to review them, and stores accepted knowledge as readable project files.

| At a glance | |
|---|---|
| **Use it when** | Your agent keeps rediscovering the same fixes, decisions, and project conventions |
| **It remembers** | Only lessons you explicitly accept or edit |
| **It stores** | Readable Markdown memory plus a local SQLite audit/search index |
| **It connects to** | LocalPilot natively; generic, Claude Code, and OpenAI Codex transcripts through the CLI |
| **Cloud required** | No |

## Privacy by design

LocalMind keeps the knowledge extracted from your work under your control.

- **No usage telemetry is sent.** Sessions, candidate lessons, searches, and
  memory activity are not reported to LocalX or an analytics service.
- **Memory stays local.** Accepted knowledge is stored as readable Markdown and
  a local SQLite index in paths you control.
- **Inference is optional.** Deterministic local behavior works without a cloud
  service; any configured inference or embedding endpoint is an explicit choice.
- **Nothing becomes memory silently.** Secret redaction and human review happen
  before durable project knowledge is written.

> [!IMPORTANT]
> LocalMind is opt-in and review-gated. It refuses project memory writes until
> `.localmind.toml` enables learning, and it never promotes a candidate lesson
> automatically.

## Quick start

Build the CLI:

```sh
git clone https://github.com/C0deGeek-dev/LocalMind.git
cd LocalMind
cargo build -p localmind-cli
cargo run -p localmind-cli -- --help
```

Enable local-only learning in the project you want LocalMind to serve:

```toml
# .localmind.toml
[learning]
enabled = true
local_only = true
memory_root = ".localmind/memory"
allowed_scopes = ["project"]
excluded_paths = ["target/**", ".git/**"]
```

Import a transcript, close the session out, and inspect the review queue:

```sh
localmind import ./session.txt --project . --source open-ai-codex
localmind closeout <session-id> --project .
localmind review list --project .
```

Accept one durable lesson, then promote and find it:

```sh
localmind review accept <lesson-id> --project . --reviewer <your-name>
localmind promote <lesson-id> --project .
localmind search "deterministic fixtures" --project .
```

If you are running from the checkout instead of an installed binary, prefix a
command with `cargo run -p localmind-cli --`.

## The learning loop

```text
opted-in session
      │
      ▼
redact ──> summarize ──> candidate lessons ──> human review
                                                   │
                               accepted only ──────┘
                                      │
                                      ▼
                         Markdown memory + local index
                                      │
                                      ▼
                           context for a later session
```

The current extractor is deterministic: explicit `Lesson:` markers plus
heuristics for failure-and-resolution pairs, repeated commands, and user
corrections. `SessionExtractor` is the seam for future model-backed extraction;
cloud inference is not the default.

## Review is the safety boundary

| Command | What happens |
|---|---|
| `localmind review list` | Show pending candidates |
| `localmind review inspect <id>` | Read the evidence before deciding |
| `localmind review accept <id>` | Mark the lesson as durable enough to keep |
| `localmind review edit <id> "…"` | Correct the lesson before accepting it |
| `localmind review reject <id>` | Reject it, optionally with a note |
| `localmind review defer <id>` | Leave it for later |
| `localmind promote <id>` | Write an accepted lesson to project memory |
| `localmind audit` | Inspect the local decision history |

Promotion writes readable Markdown below `.localmind/memory/project/`, updates
the local search and relationship index, and records an audit event in
`.localmind/localmind.sqlite`.

## What gets written

An imported session receives a deterministic folder under
`.localmind/sessions/<session-id>/`:

```text
transcript.redacted.txt
metadata.json
summary.json
candidates.json
```

Likely API keys, bearer tokens, token/password assignments, connection-string
passwords, private keys, and configured sensitive paths are redacted before the
transcript is persisted.

## Context and skill drafts

Accepted memory can be packaged for different agent hosts:

```sh
localmind context export "release checklist" --target localpilot --project .
localmind context export "deterministic fixtures" --target open-ai-codex --project .
```

Repeated workflows can become disabled `SKILL.md` drafts:

```sh
localmind skills generate --project .
localmind skills list --project .
localmind skills inspect <skill-id> --project .
localmind skills export <skill-id> --project .
```

LocalMind never installs or activates a generated skill by itself.

## Evidence so far

In the controlled `localbench-uplift-v1` evaluation, injecting accepted lessons
lifted a deliberately headroom-rich held-out suite from **0% to 100%**. The
effect held on a second local model. This is evidence that reviewed memory can
change outcomes, not a claim that every task becomes solvable.

## Architecture for host authors

The learning engine is split into host-neutral Rust crates. LocalPilot embeds it
through an adapter; the core never depends on LocalPilot. The standalone CLI
uses the same contracts for generic transcripts and other agent hosts.

| Area | Start here |
|---|---|
| Product scope and implementation status | [Vision](vision.md) |
| Files, schema, versioning, and host contracts | [On-disk contract](docs/on-disk-contract.md) |
| Architecture decisions | [Decisions](docs/decisions.md) |
| Research ingestion and distillation | [Research distillation](docs/research-distillation.md) |
| Full documentation map | [Docs index](docs/README.md) |
| Release history | [Changelog](CHANGELOG.md) |

<details>
<summary><strong>Developing LocalMind</strong></summary>

The local gate mirrors CI:

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo check --workspace
cargo run -p localmind-cli -- --help
```

CI aliases live in `.cargo/config.toml`: `cargo ci-fmt`, `cargo ci-lint`,
`cargo ci-test`, and `cargo ci-doctest`. `localmind eval` runs the memory-quality
regression suite and can emit JSON with `--json`.

</details>

## LocalX

LocalMind is the learning layer in the
[LocalX toolchain](https://c0degeek-dev.github.io/LocalStack/):

| Project | Role |
|---|---|
| [LocalBox](https://github.com/C0deGeek-dev/LocalBox) | Run local models |
| [LocalBench](https://github.com/C0deGeek-dev/LocalBench) | Find fast, stable settings |
| [LocalPilot](https://github.com/C0deGeek-dev/LocalPilot) | Code through the agent harness |
| **LocalMind** | Turn reviewed sessions into reusable project memory |
