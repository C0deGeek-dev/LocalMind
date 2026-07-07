use crate::{MemoryPersistence, ProjectConfig, ReviewModeProcessor, ReviewQueue};
use localmind_core::{
    CandidateLesson, Confidence, EvidenceKind, EvidenceRef, LessonCategory, LessonId, SessionId,
    SuggestedAction,
};
use localmind_inference::{ChatMessage, InferenceCapability};
use rusqlite::params;
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
        Self::run(
            project_root,
            "distillation",
            &insight_instruction(
                "Distill the accepted LocalMind memories into high-level principles.",
            ),
        )
    }

    pub fn research(
        project_root: impl AsRef<Path>,
        topic: &str,
    ) -> Result<BatchInsightReport, BatchInsightError> {
        Self::run(
            project_root,
            "research",
            &insight_instruction(&format!(
                "Research gaps, contradictions, and recurring patterns about {topic}."
            )),
        )
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
                "You produce LocalMind review candidates as strict JSON. Return only the JSON object, no prose.",
            ),
            ChatMessage::user(format!("{instruction}\n\nAccepted memory:\n{corpus}")),
        ])?;
        persistence.record_inference_call(kind, "chat", chat.model(), completion.usage.as_ref())?;
        let candidates = parse_distillation(kind, &completion.content)?;
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

/// Build the JSON-output instruction appended to a batch prompt.
fn insight_instruction(task: &str) -> String {
    format!(
        "{task} Return ONLY a JSON object of the form \
         {{\"insights\":[{{\"summary\":\"<one complete sentence>\",\"category\":\"process\",\"confidence\":0.7}}]}}. \
         category is one of process, tooling_note, architecture_rule, code_pattern, \
         debugging_recipe, testing_strategy, anti_pattern, security_warning. \
         Omit anything you are unsure about; return an empty insights array rather than guessing."
    )
}

#[derive(serde::Deserialize)]
struct DistillationOutput {
    #[serde(default)]
    insights: Vec<DistilledInsight>,
}

#[derive(serde::Deserialize)]
struct DistilledInsight {
    summary: String,
    #[serde(default)]
    category: String,
    #[serde(default = "default_insight_confidence")]
    confidence: f32,
}

fn default_insight_confidence() -> f32 {
    0.7
}

/// Parse a distillation/research model response into review candidates. The
/// response must be the agreed JSON envelope — a non-JSON or wrong-shape reply
/// is rejected outright (nothing is stored), replacing the old behaviour that
/// turned every output line, including preamble and noise, into a candidate.
/// Individually malformed insights (empty/non-sentence summary, out-of-range
/// confidence) are dropped; only schema-valid candidates survive.
fn parse_distillation(
    kind: &str,
    content: &str,
) -> Result<Vec<CandidateLesson>, BatchInsightError> {
    let parsed: DistillationOutput =
        serde_json::from_str(content.trim()).map_err(BatchInsightError::ModelOutput)?;
    let evidence = EvidenceRef::new(
        EvidenceKind::Other(format!("{kind} batch")),
        "batch insight",
    )
    .redacted();

    let mut candidates = Vec::new();
    for (index, insight) in parsed.insights.into_iter().enumerate() {
        let summary = insight.summary.trim();
        if !crate::extraction::is_admissible_text(summary) {
            continue;
        }
        let Ok(confidence) = Confidence::new(insight.confidence) else {
            continue;
        };
        candidates.push(
            CandidateLesson::new(
                LessonId::new(format!("{kind}-{index:04}")),
                summary,
                insight_category(&insight.category, kind),
                confidence,
                SuggestedAction::PromoteToMemory,
            )
            .with_evidence(evidence.clone()),
        );
    }
    Ok(candidates)
}

fn insight_category(category: &str, kind: &str) -> LessonCategory {
    match category {
        "process" => LessonCategory::Process,
        "tooling_note" => LessonCategory::ToolingNote,
        "architecture_rule" => LessonCategory::ArchitectureRule,
        "code_pattern" => LessonCategory::CodePattern,
        "debugging_recipe" => LessonCategory::DebuggingRecipe,
        "testing_strategy" => LessonCategory::TestingStrategy,
        "anti_pattern" => LessonCategory::AntiPattern,
        "security_warning" => LessonCategory::SecurityWarning,
        _ if kind == "research" => LessonCategory::ToolingNote,
        _ => LessonCategory::Process,
    }
}

fn record_distilled_rows(
    project_root: &Path,
    kind: &str,
    candidates: &[CandidateLesson],
) -> Result<(), BatchInsightError> {
    let state_dir = project_root.join(".localmind");
    fs::create_dir_all(&state_dir).map_err(BatchInsightError::Io)?;
    let connection = crate::schema::open_database(&state_dir.join("localmind.sqlite"))?;
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
    #[error("model output was not valid distillation JSON: {0}")]
    ModelOutput(serde_json::Error),
    #[error(transparent)]
    Contract(#[from] localmind_core::ContractError),
    #[error(transparent)]
    Schema(#[from] crate::SchemaError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn rejects_non_json_model_output() {
        // The old line-split path would have turned this prose into candidates.
        let result = parse_distillation(
            "distillation",
            "Here are some insights:\n- always test\n- ship often",
        );
        assert!(
            matches!(result, Err(BatchInsightError::ModelOutput(_))),
            "non-JSON output must be rejected, not stored"
        );
    }

    #[test]
    fn keeps_valid_insights_and_drops_malformed_ones() {
        let content = r#"{
            "insights": [
                {"summary": "Run the integration suite before declaring exporter work done.", "category": "testing_strategy", "confidence": 0.8},
                {"summary": ":{", "category": "process", "confidence": 0.7},
                {"summary": "Prefer ripgrep over grep when searching this codebase.", "category": "tooling_note", "confidence": 0.6},
                {"summary": "Out of range confidence is dropped.", "category": "process", "confidence": 9.0}
            ]
        }"#;
        let candidates = parse_distillation("distillation", content).unwrap();
        let summaries: Vec<&str> = candidates.iter().map(|c| c.summary()).collect();
        assert!(summaries.iter().any(|s| s.contains("integration suite")));
        assert!(summaries.iter().any(|s| s.contains("ripgrep")));
        // The punctuation fragment and the out-of-range-confidence insight are gone.
        assert!(!summaries.iter().any(|s| s.contains(":{")));
        assert!(!summaries.iter().any(|s| s.contains("Out of range")));
        assert_eq!(candidates.len(), 2);
    }

    #[test]
    fn empty_insights_array_is_valid_and_stores_nothing() {
        let candidates = parse_distillation("research", r#"{"insights": []}"#).unwrap();
        assert!(candidates.is_empty());
    }
}
