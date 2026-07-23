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

/// Character budget for the accepted-memory corpus in one batch prompt
/// (~8k tokens at ~4 chars/token, leaving instruction and reply headroom on
/// the small local context windows the batch commands target). Truncation
/// happens at memory boundaries only — a memory is included whole or not at
/// all — so provenance ids never point at half a lesson.
const CORPUS_CHAR_BUDGET: usize = 32_000;
/// Stores at or below this size skip topic scoping: distilling a handful of
/// memories does not need selection, and threshold noise on a tiny corpus
/// could starve the pass entirely.
const SCOPING_MIN_STORE: usize = 12;

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
            None,
        )
    }

    /// Batch **accepted-memory distillation** scoped to one topic: gaps,
    /// contradictions, and recurring patterns *in what this store already
    /// knows* about the topic. Distinct from any web/host research workflow —
    /// no retrieval happens outside accepted memory, and the engine performs
    /// its own topic selection (host-neutral: no host index is consulted).
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
            Some(topic),
        )
    }

    fn run(
        project_root: impl AsRef<Path>,
        kind: &str,
        instruction: &str,
        topic: Option<&str>,
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
        let selected = select_memories(&persistence, memories, topic)?;
        if selected.is_empty() {
            // Nothing relevant to distill: an empty selection (empty store, or
            // no memory clears the topic's relevance gate) makes no model
            // call — the model cannot say anything about the topic from
            // memory it does not have.
            return Ok(BatchInsightReport {
                enqueued: 0,
                accepted_by_mode: 0,
            });
        }
        let corpus = build_corpus(&selected, CORPUS_CHAR_BUDGET);
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

/// Choose the memories one batch prompt gets to see. With no topic (distill),
/// or a store small enough that selection would only add noise, every memory
/// competes for the budget in store order. With a topic and a real store, the
/// disciplined accepted-memory search is the relevance gate (match-centred,
/// stopword-stripped, coverage-gated): only memories it returns — in its
/// relevance order — enter the prompt, so an unrelated lesson never consumes
/// prompt space merely by existing.
fn select_memories(
    persistence: &MemoryPersistence,
    memories: Vec<crate::MemoryRecord>,
    topic: Option<&str>,
) -> Result<Vec<(String, String)>, BatchInsightError> {
    let as_rows = |memories: Vec<crate::MemoryRecord>| {
        memories
            .into_iter()
            .map(|memory| (memory.memory_id.to_string(), memory.body))
            .collect::<Vec<_>>()
    };
    let Some(topic) = topic else {
        return Ok(as_rows(memories));
    };
    if memories.len() <= SCOPING_MIN_STORE {
        return Ok(as_rows(memories));
    }
    let by_id: std::collections::HashMap<String, String> = memories
        .into_iter()
        .map(|memory| (memory.memory_id.to_string(), memory.body))
        .collect();
    let mut selected = Vec::new();
    for hit in persistence.search(topic)? {
        let id = hit.memory_id.to_string();
        if let Some(body) = by_id.get(&id) {
            selected.push((id, body.clone()));
        }
    }
    Ok(selected)
}

/// Concatenate `id: body` lines under a character budget, truncating at
/// memory boundaries only, and preferring each memory's concise lesson text
/// over any attached raw evidence block — the fenced source dump a
/// research-origin memory can carry adds bulk, not signal, to distillation.
/// The id prefix (the provenance handle) is always kept.
fn build_corpus(selected: &[(String, String)], budget: usize) -> String {
    let mut corpus = String::new();
    for (id, body) in selected {
        let line = format!("{id}: {}", concise_body(body));
        let cost = line.chars().count() + usize::from(!corpus.is_empty());
        if !corpus.is_empty() && corpus.chars().count() + cost > budget {
            break;
        }
        if !corpus.is_empty() {
            corpus.push('\n');
        }
        corpus.push_str(&line);
    }
    corpus
}

/// A memory's concise lesson text: everything before its first fenced code
/// block (the shape research-origin memories use to carry raw evidence under
/// the claim). Falls back to the full body when the lead would be empty, so
/// nothing is ever silently dropped to nothing.
fn concise_body(body: &str) -> &str {
    match body.find("\n```") {
        Some(fence) => {
            let lead = body[..fence].trim();
            if lead.is_empty() {
                body
            } else {
                lead
            }
        }
        None => body,
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

    #[test]
    fn corpus_truncates_at_memory_boundaries_under_the_budget() {
        let selected = vec![
            ("m1".to_string(), "a".repeat(40)),
            ("m2".to_string(), "b".repeat(40)),
            ("m3".to_string(), "c".repeat(40)),
        ];
        // Budget fits two full entries but not three: the third is dropped
        // whole, never cut mid-memory.
        let corpus = build_corpus(&selected, 100);
        assert!(corpus.contains("m1: "));
        assert!(corpus.contains("m2: "));
        assert!(!corpus.contains("m3: "));
        assert!(corpus.chars().count() <= 100);
        // The first entry always rides, even when it alone exceeds the budget
        // (an empty prompt would silently distill nothing).
        let oversized = vec![("m1".to_string(), "x".repeat(500))];
        assert!(build_corpus(&oversized, 100).contains("m1: "));
    }

    #[test]
    fn concise_body_prefers_the_lesson_over_attached_evidence() {
        let body = "Prefer bounded channels for backpressure.\n\n```text\nhuge raw page dump\nmore dump\n```";
        assert_eq!(
            concise_body(body),
            "Prefer bounded channels for backpressure."
        );
        // A body that *starts* with a fence keeps everything — never reduce a
        // memory to the empty string.
        let fenced_only = "\n```rust\nlet x = 1;\n```";
        assert_eq!(concise_body(fenced_only), fenced_only);
        // No fence: unchanged.
        assert_eq!(concise_body("plain lesson"), "plain lesson");
    }

    #[test]
    fn topic_selection_excludes_off_topic_memories_on_a_real_store() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".localmind.toml"),
            "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n",
        )
        .unwrap();
        let persistence = MemoryPersistence::open_project(dir.path()).unwrap();
        // A store big enough to engage scoping: 12 filler memories, one
        // on-topic, one off-topic.
        let mut entries = Vec::new();
        for index in 0..12 {
            entries.push((
                format!("filler-{index}"),
                format!("Filler note {index} about assorted build machinery."),
            ));
        }
        entries.push((
            "on-topic".to_string(),
            "Retrieval ranking prefers keyword floors over vectors.".to_string(),
        ));
        entries.push((
            "off-topic".to_string(),
            "Tailwind stylesheets should stay under version control.".to_string(),
        ));
        for (id, body) in &entries {
            persistence
                .persist_memory_entry(&test_memory(id, body))
                .unwrap();
        }
        let memories = persistence.list_memory().unwrap();
        assert!(memories.len() > SCOPING_MIN_STORE);

        let selected =
            select_memories(&persistence, memories, Some("retrieval ranking keyword")).unwrap();

        let ids: Vec<&str> = selected.iter().map(|(id, _)| id.as_str()).collect();
        assert!(ids.contains(&"on-topic"), "selected: {ids:?}");
        assert!(!ids.contains(&"off-topic"), "selected: {ids:?}");

        // A small store (or no topic) skips scoping and keeps everything.
        let small = vec![crate::MemoryRecord {
            memory_id: localmind_core::MemoryEntryId::new("only"),
            path: std::path::PathBuf::new(),
            scope: "project".to_string(),
            category: "process".to_string(),
            status: "active".to_string(),
            body: "unrelated to anything".to_string(),
            hit_count: 0,
            last_used_at: None,
            stale_candidate: false,
            contradicted: false,
            language: None,
        }];
        let all = select_memories(&persistence, small, Some("retrieval")).unwrap();
        assert_eq!(all.len(), 1);
    }

    fn test_memory(id: &str, body: &str) -> localmind_core::MemoryEntry {
        localmind_core::MemoryEntry {
            id: localmind_core::MemoryEntryId::new(id),
            scope: localmind_core::MemoryScope::Project,
            body: body.to_string(),
            category: localmind_core::LessonCategory::ProjectConvention,
            confidence: localmind_core::Confidence::new(0.9).unwrap(),
            source_session: Some(SessionId::new("seed")),
            evidence: vec![EvidenceRef::new(EvidenceKind::Transcript, "redacted").redacted()],
            tags: vec!["accepted".to_string()],
            related_files: Vec::new(),
            related_entities: Vec::new(),
            created_at: None,
            updated_at: None,
            supersedes: Vec::new(),
            contradicts: Vec::new(),
            status: localmind_core::MemoryStatus::Active,
            sync_meta: localmind_core::SyncMeta::default(),
        }
    }
}
