use anyhow::Result;
use clap::{Parser, Subcommand};
use localmind_core::{SessionOutcome, SessionRecord, SessionSource};

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
