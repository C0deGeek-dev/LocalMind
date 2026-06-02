use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use localmind_core::{
    ReviewAction, ReviewDecision, ReviewItemId, SessionId, SessionOutcome, SessionRecord,
    SessionSource,
};
use localmind_store::{
    CloseoutProcessor, DeterministicExtractor, MemoryPersistence, ReviewQueue,
    TranscriptImportFormat, TranscriptImporter,
};
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
    Unshackled,
}

impl From<SourceArg> for SessionSource {
    fn from(value: SourceArg) -> Self {
        match value {
            SourceArg::Generic => Self::GenericTranscript,
            SourceArg::ClaudeCode => Self::ClaudeCode,
            SourceArg::OpenAiCodex => Self::OpenAiCodex,
            SourceArg::Unshackled => Self::Unshackled,
        }
    }
}

#[derive(Clone, Debug, ValueEnum)]
enum FormatArg {
    PlainText,
    JsonLines,
    Markdown,
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
            let report = CloseoutProcessor::closeout_project_session(
                project,
                &SessionId::new(session_id),
                &DeterministicExtractor,
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
            SessionSource::Unshackled => "unshackled",
            SessionSource::Other(_) => "custom host",
        }
    }
}
