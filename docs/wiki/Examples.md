# Examples

Copy-pasteable samples that match shipped behaviour at the current `VERSION`.

> **Do not edit on github.com.** This wiki is generated from in-repo Markdown
> under `docs/wiki/` and synced one-way on every push to `main`. Edit the source
> in `docs/wiki/`; web edits are overwritten on the next sync.

## Opt in, then run the loop end to end

```powershell
# 1. enable the project (writes .localmind.toml)
@'
[learning]
enabled = true
local_only = true
memory_root = ".localmind/memory"
allowed_scopes = ["project"]
excluded_paths = ["target/**", ".git/**"]
'@ | Set-Content .localmind.toml

# 2. import a transcript and close it out
localmind import .\session.txt --project . --source localpilot
localmind closeout session-1234 --project .

# 3. review, accept, promote
localmind review list --project .
localmind review accept lesson-1234 --project . --reviewer david
localmind promote lesson-1234 --project .

# 4. search and audit
localmind search "release checklist" --project .
localmind audit --project .
```

Expect: a redacted session folder under `.localmind/sessions/`, candidate
lessons in the review queue, a promoted Markdown memory file under
`.localmind/memory/project/`, and an audit row in `.localmind/localmind.sqlite`.

## Render context for a host agent

```powershell
localmind context export "deterministic fixtures" --target open-ai-codex --project .
```

Renders accepted memory (plus disabled skill suggestions) as concise agent
context for the chosen target.

## Ingest documentation, then search it in the web UI

```powershell
localmind ingest docs .\docs --project .
localmind ui --project . --open
```

Expect: an ingest summary (`Ingested N chunks (M embedded) from F files`), then
a browser tab at `http://127.0.0.1:8091` whose **Docs** tab searches the
ingested passages semantically, with a per-repo dropdown to narrow the file
list.

## Register the MCP server in a client

```json
{
  "mcpServers": {
    "localmind": {
      "command": "localmind",
      "args": ["mcp", "serve", "--project", "."]
    }
  }
}
```

Expect: the client lists nine `localmind` tools — memory search, context
export, doc search, the four code-graph tools, skill list/fetch — served over
stdio; no socket is opened.

See the on-disk contract for the exact file shapes:
[on-disk-contract.md](https://github.com/C0deGeek-dev/LocalMind/blob/main/docs/on-disk-contract.md).
