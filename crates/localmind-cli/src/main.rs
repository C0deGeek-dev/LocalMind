use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use localmind_core::SkillDraftId;
use localmind_core::{
    MemoryEntryId, ReviewAction, ReviewDecision, ReviewItemId, SessionId, SessionOutcome,
    SessionRecord, SessionSource,
};
use localmind_store::{
    sign_bundle, BundleImporter, BundleScope, CloseoutProcessor, ContextExportTarget,
    ContextExporter, ImportTrust, KeyStore, MemoryBundleExporter, MemoryPersistence, ReviewQueue,
    SignedBundle, SkillDraftStore, TranscriptImportFormat, TranscriptImporter,
};
use std::fs;
use std::path::PathBuf;

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
    Status,
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
    /// Research one topic against accepted memories and the code graph.
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

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command.unwrap_or(Command::Status) {
        Command::Status => {
            let _record = SessionRecord::new(
                localmind_core::SessionId::new("example-session"),
                SessionSource::GenericTranscript,
                SessionOutcome::Unknown,
            );
            println!("LocalMind core ready: {}", _record.source_label());
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
                let queue = ReviewQueue::open_project(project)?;
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
                let report = localmind_store::BatchInsightPipeline::distill(project)?;
                println!(
                    "Enqueued: {} Accepted by mode: {}",
                    report.enqueued, report.accepted_by_mode
                );
            }
            InsightCommand::Research { topic, project } => {
                let report = localmind_store::BatchInsightPipeline::research(project, &topic)?;
                println!(
                    "Enqueued: {} Accepted by mode: {}",
                    report.enqueued, report.accepted_by_mode
                );
            }
        },
        Command::Eval { k, json, with_lift } => {
            let work_root =
                std::env::temp_dir().join(format!("localmind-eval-{}", std::process::id()));
            fs::create_dir_all(&work_root)?;
            let fixtures = localmind_store::default_fixtures();
            // When --with-lift is set, score the deterministic baseline against
            // the configured-inference extractor (read from the current project's
            // config, if any) and report the lift. Offline, the model path falls
            // back to deterministic and the lift is zero.
            let outcome = if with_lift {
                let inference = localmind_store::ProjectConfig::discover(".")
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

trait SessionSourceLabel {
    fn source_label(&self) -> &'static str;
}

impl SessionSourceLabel for SessionRecord {
    fn source_label(&self) -> &'static str {
        match &self.source {
            SessionSource::GenericTranscript => "generic transcript",
            SessionSource::ClaudeCode => "claude code",
            SessionSource::OpenAiCodex => "openai codex",
            SessionSource::LocalPilot => "localpilot",
            SessionSource::Other(_) => "custom host",
        }
    }
}
