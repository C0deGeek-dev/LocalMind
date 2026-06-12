use crate::{MemoryPersistence, ProjectConfig, ReviewModeProcessor, ReviewQueue};
use localmind_core::{
    CandidateLesson, Confidence, EvidenceKind, EvidenceRef, LessonCategory, LessonId, SessionId,
    SuggestedAction,
};
use localmind_inference::{ChatMessage, InferenceCapability};
use rusqlite::{params, Connection};
use std::fs;
use std::path::Path;
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatchInsightReport {
    pub enqueued: usize,
    pub accepted_by_mode: usize,
}

pub struct BatchInsightPipeline;

impl BatchInsightPipeline {
    pub fn distill(
        project_root: impl AsRef<Path>,
    ) -> Result<BatchInsightReport, BatchInsightError> {
        Self::run(project_root, "distillation", "Distill the accepted LocalMind memories into high-level principles. Return one insight per line.")
    }

    pub fn research(
        project_root: impl AsRef<Path>,
        topic: &str,
    ) -> Result<BatchInsightReport, BatchInsightError> {
        Self::run(project_root, "research", &format!("Research gaps, contradictions, and recurring patterns about {topic}. Return one insight per line."))
    }

    fn run(
        project_root: impl AsRef<Path>,
        kind: &str,
        instruction: &str,
    ) -> Result<BatchInsightReport, BatchInsightError> {
        let config =
            ProjectConfig::discover(project_root.as_ref()).map_err(BatchInsightError::Config)?;
        let capability = InferenceCapability::from_settings(config.config.inference.as_ref())?;
        let Some(chat) = capability.chat() else {
            return Ok(BatchInsightReport {
                enqueued: 0,
                accepted_by_mode: 0,
            });
        };
        let persistence = MemoryPersistence::open_project(&config.project_root)?;
        let memories = persistence.list_memory()?;
        if memories.is_empty() {
            return Ok(BatchInsightReport {
                enqueued: 0,
                accepted_by_mode: 0,
            });
        }
        let corpus = memories
            .iter()
            .map(|memory| format!("{}: {}", memory.memory_id, memory.body))
            .collect::<Vec<_>>()
            .join("\n");
        let completion = chat.complete(&[
            ChatMessage::system(
                "You produce concise LocalMind review candidates. Return plain lines only.",
            ),
            ChatMessage::user(format!("{instruction}\n\nAccepted memory:\n{corpus}")),
        ])?;
        persistence.record_inference_call(kind, "chat", chat.model(), completion.usage.as_ref())?;
        let candidates = line_candidates(kind, &completion.content)?;
        let queue = ReviewQueue::open_project(&config.project_root)?;
        let session_id = SessionId::new(format!("{kind}-batch"));
        let enqueued = queue.enqueue_candidates(&session_id, &candidates)?;
        record_distilled_rows(&config.project_root, kind, &candidates)?;
        let mode_report = ReviewModeProcessor::apply_project(&config.project_root)?;
        Ok(BatchInsightReport {
            enqueued,
            accepted_by_mode: mode_report.accepted,
        })
    }
}

fn line_candidates(kind: &str, content: &str) -> Result<Vec<CandidateLesson>, BatchInsightError> {
    let evidence = EvidenceRef::new(
        EvidenceKind::Other(format!("{kind} batch")),
        "batch insight",
    )
    .redacted();
    let mut candidates = Vec::new();
    for (index, line) in content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .enumerate()
    {
        let text = line.trim_start_matches("- ").trim();
        let category = if kind == "research" {
            LessonCategory::ToolingNote
        } else {
            LessonCategory::Process
        };
        candidates.push(
            CandidateLesson::new(
                LessonId::new(format!("{kind}-{index:04}")),
                text,
                category,
                Confidence::new(0.7)?,
                SuggestedAction::PromoteToMemory,
            )
            .with_evidence(evidence.clone()),
        );
    }
    Ok(candidates)
}

fn record_distilled_rows(
    project_root: &Path,
    kind: &str,
    candidates: &[CandidateLesson],
) -> Result<(), BatchInsightError> {
    let state_dir = project_root.join(".localmind");
    fs::create_dir_all(&state_dir).map_err(BatchInsightError::Io)?;
    let connection = Connection::open(state_dir.join("localmind.sqlite"))?;
    crate::schema::migrate(&connection)?;
    for candidate in candidates {
        connection.execute(
            r#"
            INSERT OR REPLACE INTO distilled_records
            (id, kind, title, body, source_memory_ids_json, status, created_at, updated_at)
            VALUES(?1, ?2, ?3, ?3, '[]', 'candidate', ?4, ?4)
            "#,
            params![
                candidate.id.as_str(),
                kind,
                candidate.summary(),
                time::OffsetDateTime::now_utc().to_string()
            ],
        )?;
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum BatchInsightError {
    #[error(transparent)]
    Config(#[from] crate::StoreConfigError),
    #[error(transparent)]
    Persistence(#[from] crate::MemoryPersistenceError),
    #[error(transparent)]
    Queue(#[from] crate::ReviewQueueError),
    #[error(transparent)]
    ReviewMode(#[from] crate::ReviewModeError),
    #[error(transparent)]
    Inference(#[from] localmind_inference::InferenceError),
    #[error(transparent)]
    Contract(#[from] localmind_core::ContractError),
    #[error(transparent)]
    Schema(#[from] crate::SchemaError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
