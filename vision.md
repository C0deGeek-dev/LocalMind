# Local AI Learning Layer

## Working Title

**Local AI Learning Layer**
Alternative names: **LocalMind**, **MemoryForge**, **SkillSmith**, **GraphMind**, **SessionForge**

## Core Idea

Build a fully local, privacy-first learning system for AI-assisted development.

The system acts as a persistent intelligence layer around local AI sessions. It does not simply save chat history. Instead, it observes sessions, extracts lessons learned, stores them in structured memory, connects them through graph search, and gradually improves how the local AI helps the user over time.

The goal is to make a local AI environment that becomes better at helping with the user’s projects, codebases, preferences, workflows, architecture decisions, debugging patterns, and recurring tasks — without sending private data to external services.

By default, learning is turned off. The user must explicitly opt in per workspace, project, session, or category of memory.

## Implementation Status

Honest map from vision component to what exists in code today. "Implemented"
means tested and usable; "scaffolded" means the boundary exists but the
behavior does not; "absent" means not started.

| Component (section) | Status | Owning crate |
|---|---|---|
| Session capture — manual transcript import (§1) | implemented | `localmind-store` (`TranscriptImporter`), `localmind-cli` |
| Redaction before storage (§1, §12) | implemented — pattern table + entropy backstop, corpus-tested | `localmind-store` |
| Inference foundation | implemented — opt-in local OpenAI-compatible chat and embedding endpoints; unset config keeps deterministic behavior. The LocalPilot host selects the configured path when `[inference]` is set (off-machine endpoints gated behind an explicit opt-in) | `localmind-core`, `localmind-inference`, `localmind-store` |
| Lesson extraction (§2) | implemented — model-backed extractor when configured (selected by the LocalPilot host), deterministic fallback otherwise. **Extraction quality is gated by a golden eval** (`run_eval`/`default_fixtures` in `eval.rs`; the `golden_eval_meets_quality_threshold` test asserts mean precision/recall ≥ 0.9 over positive and negative fixtures, and the `fixture_session_produces_reviewable_candidates` acceptance bar) | `localmind-store` (`extraction.rs`, `eval.rs`), `localmind-inference` |
| Review queue — manual mode (§3) | implemented | `localmind-store` (`ReviewQueue`); `localmind-review` is the future home (see topology note) |
| Review queue — assisted/trusted/automatic modes (§3) | implemented — project config selects mode; assisted annotates, trusted/automatic audit auto-accepts and route conflicts to manual | `localmind-store` (`ReviewModeProcessor`) |
| Memory store: Markdown memory, SQLite index/audit (§4) | implemented — transactional, schema-versioned | `localmind-store` |
| Graph knowledge layer — code-structure graph (§5) | implemented — ingester, incremental reindex, memory-to-code join | `localmind-codegraph`, `localmind-store` (`GraphStore`) |
| Retrieval — keyword + graph (§6) | implemented — FTS5/bm25 plus graph-aware ranking | `localmind-search`, `localmind-store` |
| Retrieval — semantic/vector (§6) | implemented — exact cosine over schema-versioned f32 BLOB rows, blended with keyword ranking when embeddings exist | `localmind-store`, `localmind-search` |
| Skill generation (§7) | implemented — reviewable drafts plus activation into host-consumable active skills | `localmind-skills` (boundary), `localmind-store` (`SkillDraftStore`) |
| Skill maintenance (§8) | implemented — refresh scan surface, retirement state, source-memory provenance, audit rows | `localmind-store` |
| Research and distillation (§9, §10 stages 5+) | implemented — opt-in model-backed batch jobs that emit review candidates through active mode | `localmind-store` (`BatchInsightPipeline`) |
| MCP surface (§13) | implemented — graph query tools and active-skill listing/fetch contracts | `localmind-mcp` |
| Hosts | standalone CLI; LocalPilot embeds via its adapter crate | `localmind-cli`; adapter lives in the host |

**Host-integration note.** "Implemented (engine)" above means the behaviour is
tested and usable *through the LocalMind engine / `localmind` CLI*. Extraction
*quality* (precision of the candidates, usefulness of promoted memory) **is**
measured: the golden eval (`run_eval` / `default_fixtures` in `eval.rs`) gates it
through `golden_eval_meets_quality_threshold`, which holds mean precision/recall
and retrieval recall@k ≥ 0.9 **with per-fixture (per-category) minimums** across
explicit lesson markers, failure→resolution recipes, user corrections,
supersede/conflict signals, a noisy transcript, and negative no-memory cases
(dumped content, low-value chatter). The fixture set is deliberately small and
**growing**: it proves the engine does not regress and covers the hard
categories, not that every real session yields perfect memory — fixture breadth
should keep expanding toward real-world coverage. (The earlier caveat that the
LocalPilot host drove the deterministic extractor only no longer applies: the
host selects the configured model-backed path when `[inference]` is set,
deterministic by default, with the off-machine endpoint gated behind an explicit
opt-in.)

**Topology note.** `localmind-review` and `localmind-skills` stay as thin
boundary crates while storage-backed behavior lives in `localmind-store`: they
pin dependency direction for hosts, and the store owns durable queue, skill,
audit, and vector state.

**Extraction acceptance bar.** Model-backed extraction remains opt-in and must
emit strict JSON that validates through the same review queue as deterministic
candidates. Without `[inference]`, the deterministic extractor remains the
contract and the full suite passes unchanged.

## Vision

Most AI coding assistants are stateless or only lightly personalized. They forget important discoveries, repeat previous mistakes, and lack durable understanding of a project’s history.

This system creates a local feedback loop:

1. A session happens.
2. Useful decisions, mistakes, fixes, assumptions, commands, patterns, and preferences are extracted.
3. These are stored as raw session notes and candidate lessons.
4. Candidate lessons are reviewed, validated, merged, rejected, or promoted.
5. Promoted lessons update structured memory, project knowledge, and relevant `SKILL.md` files.
6. Future AI sessions retrieve this knowledge using graph search, semantic search, and project context.
7. The system periodically checks whether memories and skills are stale, contradicted, or improvable.

The result is a local AI assistant that improves continuously through use.

## Main Principles

### Local First

All data should remain local by default:

* Local LLMs
* Local embeddings
* Local vector search
* Local graph database
* Local file storage
* Local audit trail
* Local skill repository

Optional online research may exist, but only when explicitly enabled.

### Opt-In Learning

The system must never silently learn from everything.

Learning should be configurable at multiple levels:

* Global off by default
* Enable per project
* Enable per session
* Enable per folder
* Enable per memory category
* Exclude sensitive paths or file types
* Manual approval mode
* Automatic low-risk learning mode
* Full automation mode for trusted workspaces

### Memory Is Not Just Chat History

The system should distinguish between:

* Raw session transcript
* Session summary
* Candidate lessons learned
* Confirmed durable memory
* Project facts
* User preferences
* Coding conventions
* Architectural decisions
* Known mistakes
* Troubleshooting recipes
* Reusable workflows
* Generated skills
* Deprecated knowledge

Memory should be structured, searchable, versioned, and explainable.

### No Blind Self-Modification

The system should not blindly overwrite its own knowledge.

Every memory update should have:

* Source session
* Timestamp
* Confidence level
* Reason for promotion
* Related project/files
* Possible contradictions
* Whether user approved it
* Whether it supersedes older knowledge

## Core Components

## 1. Session Capture

The system records useful information from local AI sessions.

Possible sources:

* Chat transcript
* Tool calls
* Terminal commands
* Code edits
* Git diffs
* Test results
* Error messages
* User corrections
* Final accepted solution
* Rejected suggestions
* Manual notes

The capture layer should be careful not to store secrets.

It should detect and redact:

* API keys
* Passwords
* Tokens
* Connection strings
* Private keys
* Personal identifiers
* Sensitive file paths, where configured

## 2. Lesson Extraction

At the end of a session, the system produces a “lessons learned” report.

Examples:

* “When testing SQL Server temporal tables, use Testcontainers and preserve raw SQL claiming logic.”
* “For this project, prefer ArchUnitNET over NetArchTest.”
* “The user prefers KISS implementations and minimal abstraction for repository logic.”
* “This bug happened because EF Core changed query shape under concurrency.”
* “This PowerShell script must be pipeline-safe and use real parallelism.”
* “This project uses Rider and Visual Studio, but Rider is preferred for day-to-day work.”
* “Do not suggest replacing the raw CTE UPDATE OUTPUT query in the inbox/outbox implementation.”

Each lesson should include:

* Lesson text
* Category
* Confidence
* Evidence
* Related files
* Related entities
* Suggested destination
* Proposed action

Possible categories:

* User preference
* Project convention
* Architecture rule
* Code pattern
* Debugging recipe
* Tooling note
* Testing strategy
* Deployment rule
* Anti-pattern
* Security warning
* Documentation update
* Candidate skill

## 3. Review Queue

Extracted lessons go into a review queue.

The user can:

* Accept
* Reject
* Edit
* Merge
* Mark temporary
* Mark project-specific
* Mark global
* Convert to skill
* Convert to memory
* Convert to documentation
* Ignore once
* Always ignore similar lessons

Modes:

### Manual Mode

Every lesson requires approval.

### Assisted Mode

Low-risk lessons are suggested, but not applied.

### Trusted Workspace Mode

Low-risk lessons can be applied automatically, while high-impact changes still require approval.

### Fully Automatic Mode

For disposable or experimental environments only.

## 4. Memory Store

The memory store should contain durable knowledge.

It should support:

* Semantic search
* Graph search
* Keyword search
* Temporal queries
* Source provenance
* Versioning
* Contradiction detection
* Supersession
* Decay/staleness
* Confidence scoring

Memory should be split into scopes:

### Global User Memory

Long-term user preferences and general working style.

Example:

* “User prefers simple, maintainable solutions over over-engineered abstractions.”

### Project Memory

Facts and rules specific to one repository or workspace.

Example:

* “This project uses EF Core, SQL Server, Unit of Work, Outbox, Inbox, Polly, and MSTest.”

### Session Memory

Short-lived memory useful during the current working session.

### Skill Memory

Procedural instructions that can be loaded when a task matches.

### Research Memory

Summaries of external documentation or research, with timestamps and source references.

## 5. Graph Knowledge Layer

The system should use a graph layer to connect concepts.

Example entities:

* User
* Project
* Repository
* File
* Class
* Test
* Framework
* Tool
* Bug
* Decision
* Lesson
* Skill
* Command
* Error
* Dependency
* Architecture rule

Example relationships:

* `uses`
* `prefers`
* `rejected`
* `fixed_by`
* `caused_by`
* `supersedes`
* `contradicts`
* `belongs_to_project`
* `documented_in`
* `implemented_by`
* `tested_by`
* `requires_skill`
* `learned_from_session`

Example query:

> “We are working on message processing. What do we know about this user’s preferences, previous bugs, architectural constraints, and relevant skills?”

The answer should combine:

* Project architecture memory
* Prior lessons
* Relevant code conventions
* Known anti-patterns
* Related troubleshooting notes
* Applicable skills

## 6. Retrieval Layer

The assistant should not load all memory blindly.

It should retrieve selectively based on:

* Current task
* Open files
* Repository
* Error messages
* User intent
* Recent activity
* Similar past sessions
* Relevant skills
* Confidence and freshness

Retrieval should combine:

* Vector search for semantic similarity
* Graph traversal for relationships
* Keyword search for exact terms
* Recency weighting
* Project scope filtering
* Trust/confidence weighting

## 7. Skill Generation

The system should suggest `SKILL.md` files when repeated procedural knowledge appears.

Example trigger:

The user repeatedly asks for help with:

* EF Core integration tests
* Unraid troubleshooting
* PowerShell pipeline-safe scripts
* Shopify Dawn theme edits
* Local LLM server configuration
* WoW guide formatting
* Anna’s Light product descriptions

The system may suggest:

> “This looks like a recurring workflow. Create a skill for it?”

A skill should contain:

* Name
* Description
* When to use
* Required context
* Step-by-step workflow
* Constraints
* User preferences
* Common mistakes
* Verification steps
* Example prompts
* Example outputs
* Related memories
* Last reviewed date

## 8. Skill Maintenance

Skills should not be static forever.

The system should periodically check:

* Has this skill been used recently?
* Did the user correct it?
* Did a session produce a better method?
* Are dependencies outdated?
* Are commands still valid?
* Are project conventions changed?
* Is the skill too broad?
* Should it be split?
* Should it be merged?
* Is there conflicting memory?
* Does it need examples?

Skill updates should go through the same review process as memory updates.

## 9. Research and Distillation

Some lessons should trigger optional research.

Example:

A session produces this candidate lesson:

> “Use ArchUnitNET for architecture tests.”

The system can optionally research:

* Current ArchUnitNET documentation
* Examples
* Best practices
* Breaking changes
* Alternatives
* Version-specific notes

Then it can produce:

* Updated memory
* Improved skill
* Documentation snippet
* Test template
* Warning about outdated patterns

Research should be optional and clearly separated from local-only mode.

Modes:

### Pure Local Mode

Only uses local files, local docs, previous sessions, and local models.

### Approved Research Mode

The system asks before searching online.

### Research Agent Mode

The system can perform scheduled or manual research for selected topics.

## 10. Memory Distillation Pipeline

The system should have a staged pipeline:

### Stage 1: Raw Capture

Save transcript, commands, diffs, and outcomes.

### Stage 2: Session Summary

Summarize what happened.

### Stage 3: Candidate Lessons

Extract possible durable lessons.

### Stage 4: Validation

Check lessons against:

* Existing memory
* Project files
* Tests
* Documentation
* User corrections
* Known contradictions

### Stage 5: Distillation

Merge, rewrite, simplify, or promote lessons.

### Stage 6: Memory Update

Write accepted knowledge to the appropriate memory scope.

### Stage 7: Skill Update

Create or update relevant skills.

### Stage 8: Audit Trail

Record what changed, why, and from which session.

## 11. Optimized Memory Usage

The system should be designed for local hardware.

Important optimizations:

* Store raw transcripts compressed
* Summarize older sessions
* Keep only high-value extracted lessons in active memory
* Use embeddings selectively
* Chunk by semantic boundaries
* Avoid embedding huge files unnecessarily
* Cache retrieval results
* Scope memory by project
* Use smaller local models for extraction
* Use larger local models only when needed
* Support quantized models
* Use background indexing with limits
* Avoid loading entire graph into context
* Retrieve only what is needed

The goal is not to remember everything in the prompt.

The goal is to maintain a large external memory and retrieve the right small subset at the right time.

## 12. Safety and Trust

The system should be transparent.

Before using memory, the assistant should be able to explain:

* What memory was used
* Why it was relevant
* Where it came from
* Whether it is confirmed or inferred
* Whether it is stale
* Whether there are conflicting memories

The user should be able to say:

* “Why do you think that?”
* “Forget this.”
* “Show memory about this project.”
* “Do not learn from this session.”
* “Promote this to permanent memory.”
* “Make this project-only.”
* “Turn this into a skill.”
* “Show proposed memory changes before applying.”

## 13. Possible Architecture

> **Superseded (2026-06-15).** This section is an early *exploration* of shapes
> LocalMind might take (standalone service, REST/gRPC API, web UI, workspace
> watcher, plugin system). The chosen and implemented architecture is the
> **embedded learning engine + standalone `localmind` CLI** described in the
> Implementation Status table above: hosts (LocalPilot) embed the engine through
> an adapter; there is no LocalMind service, REST/gRPC API, web UI, or workspace
> watcher, and none is planned. The text is kept for historical context only —
> do not treat it as a roadmap.

### Frontend

* Local web UI
* VS Code/Rider plugin later
* CLI first
* Session dashboard
* Memory review queue
* Skill browser
* Graph explorer
* Project settings

### Backend

* Local service
* REST or gRPC API
* Plugin system
* Workspace watcher
* Git integration
* Session processor
* Memory processor
* Skill processor
* Research processor

### Storage

Possible storage layers:

* SQLite for metadata
* Local filesystem for transcripts and skills
* Vector database for embeddings
* Graph database for entity relationships
* Git repository for versioned memory and skills

### AI Layer

Different local models for different jobs:

* Small fast model for classification
* Embedding model for retrieval
* Medium model for lesson extraction
* Stronger model for distillation and skill writing
* Optional online model only when explicitly configured

## 14. Example Workflow

The user works with a local AI assistant on a bug.

During the session:

* The assistant reads files.
* The user corrects a bad assumption.
* A failing test is fixed.
* A specific project convention is discovered.
* A command is found that reliably reproduces the issue.

At the end of the session, the system proposes:

### Session Summary

“Fixed concurrency bug in message claiming logic. The issue involved transaction boundaries and raw SQL claim behavior.”

### Candidate Lessons

1. “Do not replace the raw SQL CTE claim query in this project.”
2. “ReceivedMessageRepository owns claim-and-process logic.”
3. “Concurrency tests should use SQL Server Testcontainers, not in-memory providers.”
4. “Add or update a skill for inbox/outbox debugging.”

The user accepts 1, 2, and 3, edits 4, and rejects one irrelevant suggestion.

The system then:

* Updates project memory
* Links the lesson to affected files
* Updates the graph
* Suggests a `SKILL.md`
* Stores the session summary
* Adds an audit entry

Next time the user asks about inbox/outbox code, the assistant retrieves these lessons automatically.

## 15. MVP

The MVP should be small and practical.

### MVP Goal

A local CLI tool that turns AI sessions into reviewed project memory and optional skills.

### MVP Features

* Import session transcript manually
* Generate session summary
* Extract candidate lessons
* Review lessons in terminal
* Save accepted lessons to Markdown memory files
* Basic vector search
* Basic graph relationships
* Generate suggested `SKILL.md`
* Keep audit log
* Project-level opt-in config

### MVP Non-Goals

* No full IDE plugin yet
* No automatic file watching yet
* No online research by default
* No model fine-tuning
* No autonomous code changes
* No hidden memory writes
* No cloud dependency

## 16. Future Features

* IDE plugin
* Git diff awareness
* Automatic end-of-session processing
* Memory conflict detection
* Skill staleness scanner
* Scheduled research jobs
* Local benchmark suite
* “What did we learn this week?” report
* “Show memories used for this answer”
* Memory graph visualization
* Per-project AI onboarding
* Team-shared memory repository
* Signed memory updates
* Encrypted memory store
* Secret redaction engine
* Memory export/import
* MCP server interface
* Integration with local coding agents

## 17. Key Design Decision

The system should not try to constantly retrain the model.

Instead, it should improve through:

* Better memory
* Better retrieval
* Better skills
* Better project understanding
* Better summaries
* Better rules
* Better context selection
* Better feedback loops

This makes it safer, cheaper, more explainable, and more realistic for local hardware.

## 18. One-Sentence Pitch

A local-first learning layer for AI development that turns every opted-in session into reviewed memory, graph-connected project knowledge, and reusable skills, so your local assistant becomes more useful over time without sending your private work to the cloud.

## 19. Short Pitch

Local AI assistants forget too much. This system gives them a durable, private, local memory.

It captures lessons learned from development sessions, stores them in structured graph-backed memory, and turns repeated workflows into reusable `SKILL.md` files. Every update is opt-in, reviewable, versioned, and explainable. Over time, the assistant becomes better at helping with your specific projects, tools, preferences, and architectural decisions.

## 20. Longer Pitch

The Local AI Learning Layer is a privacy-first memory and skill system for local AI development.

Instead of treating every AI session as disposable, it extracts useful lessons from each session: decisions, fixes, mistakes, project rules, preferred patterns, and recurring workflows. These lessons are reviewed and distilled into structured memory, connected through graph search, and used to improve future AI assistance.

When the system detects repeated procedures, it suggests creating or updating `SKILL.md` files. These skills become reusable instruction modules for tasks like testing, debugging, deployment, documentation, project conventions, or tool-specific workflows.

The system is local-first and opt-in by default. It is designed for developers who want their AI assistant to improve over time while keeping their code, conversations, and private project knowledge under their own control.

## 21. Open Questions

* Should the first version be CLI-only, or should it have a small local web UI?
* Should memory be stored mostly as Markdown first, with graph/vector indexes generated from it?
* Which graph backend should be used first?
* Should skills be project-local, global, or both?
* Should online research be completely separate from memory distillation?
* Should memory updates be Git-versioned from day one?
* Should the system integrate with existing local AI tools via MCP?
* Should the product target individual developers first, or self-hosting/local-AI power users more broadly?
