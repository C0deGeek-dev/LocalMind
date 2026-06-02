use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use localmind_core::{SessionId, SessionOutcome, SessionRecord, SessionSource};
use localmind_store::{
    CloseoutProcessor, DeterministicExtractor, TranscriptImportFormat, TranscriptImporter,
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
        }
    }

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
