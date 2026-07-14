# Troubleshooting & FAQ

Common problems and fixes. Entries match shipped behaviour at the current
`VERSION`.

> **Do not edit on github.com.** This wiki is generated from in-repo Markdown
> under `docs/wiki/` and synced one-way on every push to `main`. Edit the source
> in `docs/wiki/`; web edits are overwritten on the next sync.

## "Project memory writes are refused"

LocalMind only writes project memory when the repo has an **enabled**
`.localmind.toml` (`[learning] enabled = true`). Add it (see
[[Getting-Started]]) and re-run.

## Nothing in the review queue after closeout

The first extractor is deterministic — it keys off explicit `Lesson:` markers
plus heuristics for failure→resolution pairs, repeated commands, and user
corrections. A transcript with none of those may yield no candidates. Closeout
also deduplicates repeated lesson text.

## A secret showed up in a stored transcript

Imports redact likely API keys, bearer tokens, password/token assignments,
connection-string passwords, and private keys before writing, plus any paths you
list under sensitive-path config. If a pattern slips through, treat it as a bug —
the redaction seam is deterministic and testable.

## Where does durable memory live?

Accepted memory is readable Markdown under `.localmind/memory/project/`. The
queue, audit log, and search index are in `.localmind/localmind.sqlite`. The
exact formats and versioning are in the on-disk contract:
[on-disk-contract.md](https://github.com/C0deGeek-dev/LocalMind/blob/main/docs/on-disk-contract.md).

## Is anything sent to the cloud?

No. LocalMind is local-first and opt-in: no cloud dependency, no autonomous
memory write, no hidden transcript capture. Model-backed extraction,
embeddings, and insights run only against an `[inference]` endpoint you
configure — typically a local server — and are skipped when none is set.
