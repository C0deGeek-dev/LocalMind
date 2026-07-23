use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use localmind_core::SkillDraftId;
use localmind_core::{
    MemoryEntryId, ReviewAction, ReviewDecision, ReviewItemId, SessionId, SessionSource,
};
use localmind_store::{
    sign_bundle, BundleImporter, BundleScope, CloseoutProcessor, ContextExportTarget,
    ContextExporter, ImportTrust, KeyStore, MemoryBundleExporter, MemoryPersistence, OkfExporter,
    OkfImporter, ReviewQueue, SignedBundle, SkillDraftStore, TranscriptImportFormat,
    TranscriptImporter,
};
use std::fs;
use std::path::{Path, PathBuf};

mod graph;
mod ingest;
mod mcp;
mod store_root;
mod sync;
mod ui;

#[derive(Debug, Parser)]
#[command(name = "localmind")]
#[command(version)]
#[command(about = "Local-first learning engine for AI development sessions.")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print the current engine readiness status.
    Status {
        /// Project root containing .localmind.toml.
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// Import a transcript into an opted-in project.
    Import {
        /// Transcript file to import.
        input: PathBuf,
        /// Project root containing .localmind.toml.
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// Source agent or transcript style.
        #[arg(long, value_enum, default_value_t = SourceArg::Generic)]
        source: SourceArg,
        /// Input transcript format.
        #[arg(long, value_enum, default_value_t = FormatArg::PlainText)]
        format: FormatArg,
    },
    /// Summarize an imported session and extract candidate lessons.
    Closeout {
        /// Imported session id to process.
        session_id: String,
        /// Project root containing .localmind.toml.
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// Review extracted candidate lessons before promotion.
    Review {
        #[command(subcommand)]
        command: ReviewCommand,
    },
    /// Promote accepted review items into Markdown memory.
    Promote {
        item_id: String,
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// Apply configured non-manual review mode to pending items.
    ReviewMode {
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// Search accepted memory.
    Search {
        query: String,
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// Print audit records.
    Audit {
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// Export accepted memory and suggested skills as agent-ready context.
    Context {
        #[command(subcommand)]
        command: ContextCommand,
    },
    /// Portable signed memory bundles: export accepted memory, import a verified pack.
    Memory {
        #[command(subcommand)]
        command: MemoryCommand,
    },
    /// Interoperate with Google's Open Knowledge Format (OKF): import a bundle as
    /// review candidates, export accepted memory as a conformant OKF bundle.
    Okf {
        #[command(subcommand)]
        command: OkfCommand,
    },
    /// Cross-device sync: enroll this machine's other devices and manage them.
    Sync {
        #[command(subcommand)]
        command: SyncCommand,
    },
    /// Generate and inspect disabled skill suggestions.
    Skills {
        #[command(subcommand)]
        command: SkillsCommand,
    },
    /// Run batch distillation or research passes.
    Insights {
        #[command(subcommand)]
        command: InsightCommand,
    },
    /// Build the code graph over a repository tree (structure ingest).
    Graph {
        #[command(subcommand)]
        command: GraphCommand,
    },
    /// Ingest documentation (prose) into the semantic doc index.
    Ingest {
        #[command(subcommand)]
        command: IngestCommand,
    },
    /// Serve LocalMind query tools to an MCP client over stdio.
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },
    /// Serve the local review/management web UI (localhost only).
    Ui {
        /// Project root containing .localmind.toml.
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// Port to bind on 127.0.0.1.
        #[arg(long, default_value_t = 8091)]
        port: u16,
        /// Open the UI in the default browser on start.
        #[arg(long)]
        open: bool,
        /// Require this token as a `?token=` query parameter (for LAN safety).
        #[arg(long)]
        token: Option<String>,
    },
    /// Score memory quality (extraction precision/recall, retrieval recall@k)
    /// over the built-in golden fixtures.
    Eval {
        /// Retrieval cutoff for recall@k.
        #[arg(long, default_value_t = 5)]
        k: usize,
        /// Emit the report as JSON instead of a text summary.
        #[arg(long)]
        json: bool,
        /// Also report the lift of the configured-inference extractor over the
        /// deterministic baseline. Without a configured local model the model
        /// path falls back to deterministic, so the lift is zero — an honest
        /// "no measured lift offline" signal that gates default-on extraction.
        #[arg(long)]
        with_lift: bool,
        /// Project root whose [inference] config the --with-lift pass reads.
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum GraphCommand {
    /// Walk a tree, parse supported sources, and reindex the code graph.
    Reindex {
        /// Repository root to ingest.
        path: PathBuf,
        /// Project root containing .localmind.toml (holds the graph store).
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum IngestCommand {
    /// Chunk and embed Markdown docs under a path into the semantic doc index.
    Docs {
        /// Root directory to ingest Markdown from.
        path: PathBuf,
        /// Project root containing .localmind.toml.
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum McpCommand {
    /// Serve the MCP protocol over stdin/stdout (JSON-RPC 2.0).
    Serve {
        /// Project root containing .localmind.toml.
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum ReviewCommand {
    /// List review queue items.
    List {
        /// Project root containing .localmind.toml.
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// Inspect one review queue item.
    Inspect {
        item_id: String,
        /// Project root containing .localmind.toml.
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// Accept one review queue item.
    Accept(ReviewDecisionArgs),
    /// Reject one review queue item.
    Reject(ReviewDecisionArgs),
    /// Defer one review queue item.
    Defer(ReviewDecisionArgs),
    /// Edit one review queue item before accepting it.
    Edit {
        item_id: String,
        replacement: String,
        /// Project root containing .localmind.toml.
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// Reviewer identifier to record.
        #[arg(long, default_value = "cli")]
        reviewer: String,
        /// Optional review note.
        #[arg(long)]
        note: Option<String>,
    },
    /// Accept one item as the replacement for an existing memory, retiring it.
    Supersede {
        item_id: String,
        /// The memory id this candidate replaces (flipped to superseded on promote).
        target: String,
        /// Project root containing .localmind.toml.
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// Reviewer identifier to record.
        #[arg(long, default_value = "cli")]
        reviewer: String,
        /// Optional review note.
        #[arg(long)]
        note: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum ContextCommand {
    /// Export a context pack for a target agent.
    Export {
        query: String,
        #[arg(long, default_value = ".")]
        project: PathBuf,
        #[arg(long, value_enum, default_value_t = ContextTargetArg::Generic)]
        target: ContextTargetArg,
    },
}

#[derive(Debug, Subcommand)]
enum MemoryCommand {
    /// Export accepted memory to a portable, signed bundle file.
    Export {
        /// Destination file for the signed bundle.
        #[arg(long)]
        out: PathBuf,
        /// Project root containing .localmind.toml.
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// Which scopes to include.
        #[arg(long, value_enum, default_value_t = ScopeArg::Both)]
        scope: ScopeArg,
        /// Emit a JSON report instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Import a signed bundle: verify, then (with --apply) enqueue for review.
    Import {
        /// Signed bundle file to import.
        input: PathBuf,
        /// Project root containing .localmind.toml.
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// Write the imported entries as review candidates. Without it this is a
        /// dry run that reports what would change and writes nothing.
        #[arg(long)]
        apply: bool,
        /// Emit a JSON report instead of text.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum OkfCommand {
    /// Import an OKF bundle directory: parse each concept and (with --apply)
    /// enqueue it for review. An OKF bundle is unsigned, so every concept is
    /// flagged untrusted.
    Import {
        /// OKF bundle directory to import.
        input: PathBuf,
        /// Project root containing .localmind.toml.
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// Write the parsed concepts as review candidates. Without it this is a
        /// dry run that reports what would change and writes nothing.
        #[arg(long)]
        apply: bool,
        /// Emit a JSON report instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Export accepted memory as a conformant OKF bundle directory.
    Export {
        /// Destination directory for the OKF bundle.
        out: PathBuf,
        /// Project root containing .localmind.toml.
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// Which scopes to include.
        #[arg(long, value_enum, default_value_t = ScopeArg::Both)]
        scope: ScopeArg,
        /// Emit a JSON report instead of text.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ScopeArg {
    Project,
    Global,
    Both,
}

impl From<ScopeArg> for localmind_store::BundleScope {
    fn from(value: ScopeArg) -> Self {
        match value {
            ScopeArg::Project => Self::Project,
            ScopeArg::Global => Self::Global,
            ScopeArg::Both => Self::Both,
        }
    }
}

#[derive(Debug, Subcommand)]
enum SyncCommand {
    /// Print this machine's shareable device card (public keys + fingerprint).
    DeviceCard {
        /// Project root containing .localmind.toml.
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// Label for this device; defaults to `[sync] device_label` or the hostname.
        #[arg(long)]
        label: Option<String>,
        /// Emit JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Enroll a peer device from its card, confirming the out-of-band fingerprint.
    Enroll {
        /// Path to the peer's device card JSON (`-` or omitted reads stdin).
        #[arg(long)]
        card: Option<PathBuf>,
        /// The fingerprint read off the other machine; enrollment fails if it
        /// does not match the card.
        #[arg(long)]
        confirm_fingerprint: String,
        /// Project root containing .localmind.toml.
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// Emit JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// List this machine's identity and its enrolled peer devices.
    Devices {
        /// Project root containing .localmind.toml.
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// Emit JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Revoke an enrolled device by its fingerprint or label.
    Revoke {
        /// The device's fingerprint or label.
        device: String,
        /// Project root containing .localmind.toml.
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// Emit JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Export this device's memory and import peers' via the sync folder.
    Run {
        /// Sync folder; defaults to `[sync] folder` in .localmind.toml.
        #[arg(long)]
        folder: Option<PathBuf>,
        /// Project root containing .localmind.toml.
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// Emit JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Show the sync folder, enrolled peers, cursors, and pending review count.
    Status {
        /// Sync folder; defaults to `[sync] folder` in .localmind.toml.
        #[arg(long)]
        folder: Option<PathBuf>,
        /// Project root containing .localmind.toml.
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// Emit JSON instead of text.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum SkillsCommand {
    /// Generate disabled skill drafts from accepted review items.
    Generate {
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// List generated skill drafts.
    List {
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// Inspect one generated skill draft.
    Inspect {
        draft_id: String,
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// Export a generated SKILL.md draft to stdout or a path.
    Export {
        draft_id: String,
        #[arg(long, default_value = ".")]
        project: PathBuf,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Activate a reviewed skill draft for host consumption.
    Activate {
        draft_id: String,
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// List active host-consumable skills.
    Active {
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// Retire an active skill without deleting its provenance.
    Retire {
        draft_id: String,
        #[arg(long, default_value = ".")]
        project: PathBuf,
        #[arg(long, default_value = "retired by user")]
        reason: String,
    },
}

#[derive(Debug, Subcommand)]
enum InsightCommand {
    /// Distill accepted memories into higher-level review candidates.
    Distill {
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// Research one topic against accepted memories (model-backed; requires
    /// a configured `[inference]` endpoint).
    Research {
        topic: String,
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
}

#[derive(Debug, Parser)]
struct ReviewDecisionArgs {
    item_id: String,
    /// Project root containing .localmind.toml.
    #[arg(long, default_value = ".")]
    project: PathBuf,
    /// Reviewer identifier to record.
    #[arg(long, default_value = "cli")]
    reviewer: String,
    /// Optional review note.
    #[arg(long)]
    note: Option<String>,
}

#[derive(Clone, Debug, ValueEnum)]
enum SourceArg {
    Generic,
    ClaudeCode,
    OpenAiCodex,
    /// The product name is one word, so the flag value is too.
    #[value(name = "localpilot")]
    LocalPilot,
}

impl From<SourceArg> for SessionSource {
    fn from(value: SourceArg) -> Self {
        match value {
            SourceArg::Generic => Self::GenericTranscript,
            SourceArg::ClaudeCode => Self::ClaudeCode,
            SourceArg::OpenAiCodex => Self::OpenAiCodex,
            SourceArg::LocalPilot => Self::LocalPilot,
        }
    }
}

#[derive(Clone, Debug, ValueEnum)]
enum FormatArg {
    PlainText,
    JsonLines,
    Markdown,
}

#[derive(Clone, Debug, ValueEnum)]
enum ContextTargetArg {
    Generic,
    ClaudeCode,
    OpenAiCodex,
    /// The product name is one word, so the flag value is too.
    #[value(name = "localpilot")]
    LocalPilot,
}

impl From<ContextTargetArg> for ContextExportTarget {
    fn from(value: ContextTargetArg) -> Self {
        match value {
            ContextTargetArg::Generic => Self::Generic,
            ContextTargetArg::ClaudeCode => Self::ClaudeCode,
            ContextTargetArg::OpenAiCodex => Self::OpenAiCodex,
            ContextTargetArg::LocalPilot => Self::LocalPilot,
        }
    }
}

impl From<FormatArg> for TranscriptImportFormat {
    fn from(value: FormatArg) -> Self {
        match value {
            FormatArg::PlainText => Self::PlainText,
            FormatArg::JsonLines => Self::JsonLines,
            FormatArg::Markdown => Self::Markdown,
        }
    }
}

/// Print a notice (to stderr) when a batch insight pass has no inference
/// endpoint configured, so an empty "Enqueued: 0" is not mistaken for "ran and
/// found nothing" — the model-backed pass is silently skipped without config.
fn warn_if_no_inference(project: &std::path::Path, pass: &str) {
    let configured = localmind_store::ProjectConfig::discover(project)
        .ok()
        .and_then(|c| c.config.inference)
        .is_some();
    if !configured {
        eprintln!(
            "note: no [inference] endpoint configured — model-backed {pass} was skipped (nothing ran). Configure chat_base_url/chat_model in .localmind.toml to enable it."
        );
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command.unwrap_or(Command::Status {
        project: PathBuf::from("."),
    }) {
        Command::Status { project } => {
            // Real readiness for the named project: config, store/DB, review
            // queue, and inference — not a canned "ready".
            let mut ready = true;
            match localmind_store::ProjectConfig::discover(&project) {
                Ok(pc) => {
                    let learning = &pc.config.learning;
                    println!(
                        "config:  {} (learning {}, scopes {:?})",
                        pc.config_path.display(),
                        if learning.enabled {
                            "enabled"
                        } else {
                            "disabled"
                        },
                        learning.allowed_scopes
                    );
                    match &pc.config.inference {
                        Some(inf) => println!(
                            "inference: configured ({})",
                            inf.chat_base_url.as_deref().unwrap_or("chat endpoint set")
                        ),
                        None => println!(
                            "inference: not configured (deterministic paths only; model-backed extraction/insights are skipped)"
                        ),
                    }
                }
                Err(error) => {
                    ready = false;
                    println!("config:  not usable — {error}");
                    println!("         learning is disabled until a valid .localmind.toml exists in this project");
                }
            }
            match MemoryPersistence::open_project(&project) {
                Ok(store) => {
                    match store.list_memory() {
                        Ok(memories) => {
                            println!("store:   open, {} accepted memory item(s)", memories.len());
                        }
                        Err(error) => {
                            ready = false;
                            println!("store:   open but memory list failed — {error}");
                        }
                    }
                    // Doc-index readiness: chunk and vector counts diverging is
                    // the "ingested without embeddings" state a bare doc_search
                    // miss cannot explain.
                    match (store.doc_chunk_count(), store.doc_vector_count()) {
                        (Ok(chunks), Ok(vectors)) => {
                            println!("docs:    {chunks} chunk(s), {vectors} with embeddings");
                        }
                        (Err(error), _) | (_, Err(error)) => {
                            ready = false;
                            println!("docs:    count failed — {error}");
                        }
                    }
                }
                Err(error) => {
                    ready = false;
                    println!("store:   not open — {error}");
                }
            }
            match ReviewQueue::open_project(&project) {
                Ok(queue) => match queue.list() {
                    Ok(items) => {
                        // Pending means pending: an accepted/rejected/deferred
                        // item is no longer awaiting review.
                        let pending = items
                            .iter()
                            .filter(|item| item.state == localmind_core::ReviewState::Pending)
                            .count();
                        println!("review:  {pending} candidate(s) pending");
                    }
                    Err(error) => {
                        ready = false;
                        println!("review:  queue read failed — {error}");
                    }
                },
                Err(error) => {
                    ready = false;
                    println!("review:  queue not open — {error}");
                }
            }
            println!(
                "status:  {}",
                if ready {
                    "ready"
                } else {
                    "not ready (see above)"
                }
            );
            if !ready {
                std::process::exit(1);
            }
        }
        Command::Import {
            input,
            project,
            source,
            format,
        } => {
            let report =
                TranscriptImporter::import_file(project, input, source.into(), format.into())?;
            println!("Imported session {}", report.session_id);
            println!(
                "Redacted transcript: {}",
                report.redacted_transcript_path.display()
            );
            println!("Metadata: {}", report.metadata_path.display());
            println!("Redactions: {}", report.redactions.len());
        }
        Command::Closeout {
            session_id,
            project,
        } => {
            let report = CloseoutProcessor::closeout_project_session_with_configured_inference(
                project,
                &SessionId::new(session_id),
            )?;
            println!("Closed out session {}", report.session_id);
            println!("Summary: {}", report.summary_path.display());
            println!("Candidates: {}", report.candidates_path.display());
            println!("Candidate count: {}", report.candidate_count);
            println!("Enqueued: {}", report.enqueued_count);
        }
        Command::Review { command } => match command {
            ReviewCommand::List { project } => {
                let Some(root) = store_root::resolve_or_report(&project) else {
                    return Ok(());
                };
                let queue = ReviewQueue::open_project(&root)?;
                for item in queue.list()? {
                    println!(
                        "{}\t{:?}\t{}\t{}",
                        item.id,
                        item.state,
                        item.session_id,
                        item.candidate.summary()
                    );
                }
            }
            ReviewCommand::Inspect { item_id, project } => {
                let queue = ReviewQueue::open_project(project)?;
                if let Some(item) = queue.get(&ReviewItemId::new(item_id))? {
                    println!("ID: {}", item.id);
                    println!("State: {:?}", item.state);
                    println!("Session: {}", item.session_id);
                    println!("Summary: {}", item.candidate.summary());
                    println!("Category: {:?}", item.candidate.category);
                    println!("Confidence: {:.3}", item.candidate.confidence.value());
                    if let Some(replacement) = item.replacement_summary {
                        println!("Replacement: {replacement}");
                    }
                    if let Some(note) = item.note {
                        println!("Note: {note}");
                    }
                } else {
                    println!("Review item not found");
                }
            }
            ReviewCommand::Accept(args) => apply_review_decision(args, ReviewAction::Accept)?,
            ReviewCommand::Reject(args) => apply_review_decision(args, ReviewAction::Reject)?,
            ReviewCommand::Defer(args) => apply_review_decision(args, ReviewAction::MarkTemporary)?,
            ReviewCommand::Edit {
                item_id,
                replacement,
                project,
                reviewer,
                note,
            } => {
                let persistence = MemoryPersistence::open_project(&project)?;
                let queue = ReviewQueue::open_project(project)?;
                let item = queue.decide(ReviewDecision {
                    item_id: ReviewItemId::new(item_id),
                    action: ReviewAction::Edit,
                    reviewer,
                    decided_at: None,
                    note,
                    replacement_summary: Some(replacement),
                    evidence: Vec::new(),
                })?;
                persistence.record_review_item_audit(&item)?;
                println!("{} -> {:?}", item.id, item.state);
            }
            ReviewCommand::Supersede {
                item_id,
                target,
                project,
                reviewer,
                note,
            } => {
                let persistence = MemoryPersistence::open_project(&project)?;
                let queue = ReviewQueue::open_project(project)?;
                let item = queue.decide(ReviewDecision {
                    item_id: ReviewItemId::new(item_id),
                    action: ReviewAction::Supersede(MemoryEntryId::new(target)),
                    reviewer,
                    decided_at: None,
                    note,
                    replacement_summary: None,
                    evidence: Vec::new(),
                })?;
                persistence.record_review_item_audit(&item)?;
                println!(
                    "{} -> {:?} (promote to retire the target)",
                    item.id, item.state
                );
            }
        },
        Command::Promote { item_id, project } => {
            let persistence = MemoryPersistence::open_project(project)?;
            let entry = persistence.promote_review_item(&ReviewItemId::new(item_id))?;
            println!("Promoted memory {}", entry.id);
        }
        Command::ReviewMode { project } => {
            let report = localmind_store::ReviewModeProcessor::apply_project(project)?;
            println!(
                "Annotated: {} Accepted: {} Manual: {}",
                report.annotated, report.accepted, report.manual
            );
        }
        Command::Search { query, project } => {
            let persistence = MemoryPersistence::open_project(project)?;
            for result in persistence.search(&query)? {
                println!(
                    "{}\t{}\t{}",
                    result.memory_id,
                    result.score,
                    result.path.display()
                );
                println!("{}", result.snippet);
            }
        }
        Command::Audit { project } => {
            let persistence = MemoryPersistence::open_project(project)?;
            for record in persistence.audit_records()? {
                println!(
                    "{}\t{}\t{}\t{}",
                    record.id, record.kind, record.actor, record.subject
                );
            }
        }
        Command::Context { command } => match command {
            ContextCommand::Export {
                query,
                project,
                target,
            } => {
                let exporter = ContextExporter::open_project(project)?;
                let export = exporter.export(&query, target.into())?;
                println!("{}", export.body_markdown);
            }
        },
        Command::Memory { command } => match command {
            MemoryCommand::Export {
                out,
                project,
                scope,
                json,
            } => memory_export(&out, &project, scope.into(), json)?,
            MemoryCommand::Import {
                input,
                project,
                apply,
                json,
            } => memory_import(&input, &project, apply, json)?,
        },
        Command::Okf { command } => match command {
            OkfCommand::Import {
                input,
                project,
                apply,
                json,
            } => okf_import(&input, &project, apply, json)?,
            OkfCommand::Export {
                out,
                project,
                scope,
                json,
            } => okf_export(&out, &project, scope.into(), json)?,
        },
        Command::Sync { command } => match command {
            SyncCommand::DeviceCard {
                project,
                label,
                json,
            } => sync::device_card(&project, label, json)?,
            SyncCommand::Enroll {
                card,
                confirm_fingerprint,
                project,
                json,
            } => sync::enroll(&project, card, &confirm_fingerprint, json)?,
            SyncCommand::Devices { project, json } => sync::devices(&project, json)?,
            SyncCommand::Revoke {
                device,
                project,
                json,
            } => sync::revoke(&project, &device, json)?,
            SyncCommand::Run {
                folder,
                project,
                json,
            } => sync::run(&project, folder, json)?,
            SyncCommand::Status {
                folder,
                project,
                json,
            } => sync::status(&project, folder, json)?,
        },
        Command::Skills { command } => match command {
            SkillsCommand::Generate { project } => {
                let store = SkillDraftStore::open_project(project)?;
                let records = store.generate_from_review_queue()?;
                for record in records {
                    println!("{}\t{}", record.draft.id, record.draft_path.display());
                }
            }
            SkillsCommand::List { project } => {
                let store = SkillDraftStore::open_project(project)?;
                for record in store.list()? {
                    println!(
                        "{}\t{}\t{}",
                        record.draft.id, record.draft.name, record.draft.disabled
                    );
                }
            }
            SkillsCommand::Inspect { draft_id, project } => {
                let store = SkillDraftStore::open_project(project)?;
                if let Some(record) = store.get(&SkillDraftId::new(draft_id))? {
                    println!("ID: {}", record.draft.id);
                    println!("Name: {}", record.draft.name);
                    println!("Disabled: {}", record.draft.disabled);
                    println!("Description: {}", record.draft.description);
                    println!("Path: {}", record.draft_path.display());
                } else {
                    println!("Skill draft not found");
                }
            }
            SkillsCommand::Export {
                draft_id,
                project,
                output,
            } => {
                let store = SkillDraftStore::open_project(project)?;
                if let Some(record) = store.get(&SkillDraftId::new(draft_id))? {
                    if let Some(output) = output {
                        fs::write(&output, &record.draft.body_markdown)?;
                        println!("{}", output.display());
                    } else {
                        println!("{}", record.draft.body_markdown);
                    }
                } else {
                    println!("Skill draft not found");
                }
            }
            SkillsCommand::Activate { draft_id, project } => {
                let store = SkillDraftStore::open_project(project)?;
                if let Some(record) = store.activate(&SkillDraftId::new(draft_id))? {
                    println!("{}\t{}", record.skill.id, record.status);
                } else {
                    println!("Skill draft not found");
                }
            }
            SkillsCommand::Active { project } => {
                let store = SkillDraftStore::open_project(project)?;
                for record in store.active()? {
                    println!("{}\t{}", record.skill.id, record.skill.name);
                }
            }
            SkillsCommand::Retire {
                draft_id,
                project,
                reason,
            } => {
                let store = SkillDraftStore::open_project(project)?;
                println!("{}", store.retire(&SkillDraftId::new(draft_id), &reason)?);
            }
        },
        Command::Insights { command } => match command {
            InsightCommand::Distill { project } => {
                warn_if_no_inference(&project, "distillation");
                let report = localmind_store::BatchInsightPipeline::distill(&project)?;
                println!(
                    "Enqueued: {} Accepted by mode: {}",
                    report.enqueued, report.accepted_by_mode
                );
            }
            InsightCommand::Research { topic, project } => {
                warn_if_no_inference(&project, "research");
                let report = localmind_store::BatchInsightPipeline::research(&project, &topic)?;
                println!(
                    "Enqueued: {} Accepted by mode: {}",
                    report.enqueued, report.accepted_by_mode
                );
            }
        },
        Command::Graph { command } => match command {
            GraphCommand::Reindex { path, project } => graph::reindex(path, project)?,
        },
        Command::Ingest { command } => match command {
            IngestCommand::Docs { path, project } => ingest::docs(path, project)?,
        },
        Command::Mcp { command } => match command {
            McpCommand::Serve { project } => mcp::serve(project)?,
        },
        Command::Ui {
            project,
            port,
            open,
            token,
        } => {
            if let Some(root) = store_root::resolve_or_report(&project) {
                ui::serve(root, port, open, token)?;
            }
        }
        Command::Eval {
            k,
            json,
            with_lift,
            project,
        } => {
            let work_root =
                std::env::temp_dir().join(format!("localmind-eval-{}", std::process::id()));
            fs::create_dir_all(&work_root)?;
            let fixtures = localmind_store::default_fixtures();
            // When --with-lift is set, score the deterministic baseline against
            // the configured-inference extractor (read from the current project's
            // config, if any) and report the lift. Offline, the model path falls
            // back to deterministic and the lift is zero.
            let outcome = if with_lift {
                let inference = localmind_store::ProjectConfig::discover(&project)
                    .ok()
                    .and_then(|config| config.config.inference);
                localmind_store::run_eval_lift(&fixtures, k, &work_root, inference.as_ref())
                    .map(|(report, lift)| (report, Some(lift)))
            } else {
                localmind_store::run_eval(&fixtures, k, &work_root).map(|report| (report, None))
            };
            let _ = fs::remove_dir_all(&work_root);
            let (report, lift) = outcome?;
            if json {
                let mut value = serde_json::to_value(&report)?;
                if let (Some(lift), Some(object)) = (&lift, value.as_object_mut()) {
                    object.insert("lift".to_string(), serde_json::to_value(lift)?);
                }
                println!("{}", serde_json::to_string_pretty(&value)?);
            } else {
                println!("Memory-quality evaluation (recall@{}):", report.k);
                for score in &report.scores {
                    println!(
                        "  {:<22} candidates={:<3} precision={:.3} recall={:.3} recall@k={:.3}",
                        score.name,
                        score.candidate_count,
                        score.extraction_precision,
                        score.extraction_recall,
                        score.retrieval_recall_at_k
                    );
                }
                println!(
                    "  {:<22} {:<14} precision={:.3} recall={:.3} recall@k={:.3}",
                    "MEAN",
                    "",
                    report.mean_extraction_precision,
                    report.mean_extraction_recall,
                    report.mean_retrieval_recall_at_k
                );
                if let Some(lift) = &lift {
                    println!(
                        "  {:<22} {:<14} precision={:+.3} recall={:+.3} recall@k={:+.3}",
                        "LIFT (model)",
                        "",
                        lift.extraction_precision_delta,
                        lift.extraction_recall_delta,
                        lift.retrieval_recall_at_k_delta
                    );
                }
            }
        }
    }

    Ok(())
}

/// `memory export`: export accepted memory to a portable, signed bundle file.
fn memory_export(out: &PathBuf, project: &PathBuf, scope: BundleScope, json: bool) -> Result<()> {
    let exporter = MemoryBundleExporter::open_project(project)?;
    // The signing key's fingerprint is the author identity; generate it the first
    // time so an export is always attributable.
    let signing_key = KeyStore::open(project)?.load_or_generate()?;
    let author = localmind_store::author_fingerprint(&signing_key.verifying_key().to_bytes());
    let outcome = exporter.export(scope, &author)?;
    let signed = sign_bundle(&outcome.bundle, &signing_key)?;
    fs::write(out, signed.to_pretty_json()?)?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "output": out.display().to_string(),
                "entries": outcome.bundle.entries.len(),
                "redactions": outcome.scan.redactions,
                "author": author,
                "digest": signed.signature.digest,
            }))?
        );
    } else {
        println!(
            "Exported {} accepted memor{} to {}",
            outcome.bundle.entries.len(),
            if outcome.bundle.entries.len() == 1 {
                "y"
            } else {
                "ies"
            },
            out.display()
        );
        println!(
            "Signed by author {author} (digest {})",
            signed.signature.digest
        );
        if outcome.scan.found_secrets() {
            println!(
                "Redacted {} apparent secret(s) across {} entr{} before export.",
                outcome.scan.redactions,
                outcome.scan.entries_with_redactions,
                if outcome.scan.entries_with_redactions == 1 {
                    "y"
                } else {
                    "ies"
                }
            );
        }
    }
    Ok(())
}

/// `okf export`: write accepted memory as a conformant OKF bundle directory.
fn okf_export(out: &Path, project: &Path, scope: BundleScope, json: bool) -> Result<()> {
    let report = OkfExporter::new(project).export(out, scope)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    println!(
        "Exported {} accepted memor{} to OKF bundle {} ({} categor{}, {} redaction{}).",
        report.total,
        if report.total == 1 { "y" } else { "ies" },
        out.display(),
        report.categories,
        if report.categories == 1 { "y" } else { "ies" },
        report.redactions,
        if report.redactions == 1 { "" } else { "s" }
    );
    Ok(())
}

/// `okf import`: read an OKF bundle directory and (with `--apply`) enqueue its
/// concepts for review. The default is a dry run that writes nothing. An OKF
/// bundle is unsigned, so every concept is flagged untrusted.
fn okf_import(input: &Path, project: &Path, apply: bool, json: bool) -> Result<()> {
    let report = OkfImporter::new(project).import(input, apply)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    println!(
        "{} {} OKF concept{} (unsigned, untrusted): {} new, {} duplicate, {} skipped.",
        if report.applied {
            "Enqueued for review from"
        } else {
            "Dry run over"
        },
        report.total,
        if report.total == 1 { "" } else { "s" },
        report.added,
        report.duplicate,
        report.skipped
    );
    println!("An OKF bundle is unsigned — imported concepts are reviewed before they are used.");
    if !report.applied {
        println!("Re-run with --apply to enqueue these for review.");
    }
    Ok(())
}

/// `memory import`: verify a signed bundle, then (with `--apply`) enqueue its
/// entries for review. The default is a dry run that writes nothing.
fn memory_import(input: &PathBuf, project: &PathBuf, apply: bool, json: bool) -> Result<()> {
    let signed = SignedBundle::from_json(&fs::read_to_string(input)?)?;
    let report = BundleImporter::new(project).import(&signed, apply)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    match &report.trust {
        ImportTrust::Rejected(reason) => {
            println!("Rejected: {reason}. Nothing was imported.");
            return Ok(());
        }
        ImportTrust::Trusted => println!("Verified: trusted author."),
        ImportTrust::Untrusted => {
            println!(
                "Verified: UNTRUSTED author (valid signature, unknown key) — review carefully."
            );
        }
    }
    println!(
        "{} {} entr{}: {} new, {} duplicate.",
        if report.applied {
            "Enqueued for review from"
        } else {
            "Dry run over"
        },
        report.total,
        if report.total == 1 { "y" } else { "ies" },
        report.added,
        report.duplicate
    );
    // The honest trust UX: a signature attests the author, not the content.
    println!(
        "A verified author is not verified content — imported memory is reviewed before it is used."
    );
    if !report.applied {
        println!("Re-run with --apply to enqueue these for review.");
    }
    Ok(())
}

fn apply_review_decision(args: ReviewDecisionArgs, action: ReviewAction) -> Result<()> {
    let persistence = MemoryPersistence::open_project(&args.project)?;
    let queue = ReviewQueue::open_project(args.project)?;
    let item = queue.decide(ReviewDecision {
        item_id: ReviewItemId::new(args.item_id),
        action,
        reviewer: args.reviewer,
        decided_at: None,
        note: args.note,
        replacement_summary: None,
        evidence: Vec::new(),
    })?;
    persistence.record_review_item_audit(&item)?;
    println!("{} -> {:?}", item.id, item.state);
    Ok(())
}
