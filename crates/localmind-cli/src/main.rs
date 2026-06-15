use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use localmind_core::SkillDraftId;
use localmind_core::{
    ReviewAction, ReviewDecision, ReviewItemId, SessionId, SessionOutcome, SessionRecord,
    SessionSource,
};
use localmind_store::{
    CloseoutProcessor, ContextExportTarget, ContextExporter, MemoryPersistence, ReviewQueue,
    SkillDraftStore, TranscriptImportFormat, TranscriptImporter,
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
        Command::Eval { k, json } => {
            let work_root =
                std::env::temp_dir().join(format!("localmind-eval-{}", std::process::id()));
            fs::create_dir_all(&work_root)?;
            let result =
                localmind_store::run_eval(&localmind_store::default_fixtures(), k, &work_root);
            let _ = fs::remove_dir_all(&work_root);
            let report = result?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
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
            }
        }
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
