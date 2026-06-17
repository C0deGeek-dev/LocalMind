use crate::{
    MemoryPersistence, MemoryPersistenceError, ProjectConfig, ReviewModeConfig, ReviewQueue,
    ReviewQueueError,
};
use localmind_core::{
    AuditEventKind, Confidence, ReviewAction, ReviewAnnotation, ReviewDecision, ReviewItemId,
};
use std::collections::BTreeSet;
use thiserror::Error;

/// Overlap (0..1) above which two summaries are treated as near-duplicates.
/// High enough that a single shared keyword is not a duplicate.
const DUPLICATE_SIMILARITY: f32 = 0.6;
/// Lower overlap above which an existing memory is "on the same topic" — used to
/// decide whether a corrective/negating candidate actually contradicts it.
const CONFLICT_TOPIC_OVERLAP: f32 = 0.3;

/// Very common words carry no topic signal; dropping them keeps similarity
/// keyed on the substantive terms.
const STOP_WORDS: [&str; 24] = [
    "the", "a", "an", "and", "or", "but", "to", "of", "in", "on", "for", "with", "is", "are", "be",
    "this", "that", "it", "as", "at", "by", "from", "use", "using",
];

/// The substantive, lowercased word set of a summary (alphanumeric tokens,
/// stop-words removed). Used for set-overlap similarity.
fn token_set(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .filter(|w| w.len() > 2 && !STOP_WORDS.contains(&w.as_str()))
        .collect()
}

/// Overlap coefficient `|A∩B| / min(|A|,|B|)` — robust to length differences,
/// so a short lesson contained in a longer memory still scores high.
fn similarity(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let intersection = a.intersection(b).count();
    let smaller = a.len().min(b.len());
    intersection as f32 / smaller as f32
}

/// Markers that a statement reverses or forbids prior guidance.
const NEGATION_MARKERS: [&str; 11] = [
    "do not",
    "don't",
    "never",
    "no longer",
    "instead",
    "avoid",
    "stop ",
    "rather than",
    "deprecated",
    "not ",
    "isn't",
];

/// Whether `candidate` contradicts existing accepted memory. With a topically
/// related memory, a corrective/negating candidate is treated as a contradiction
/// (it likely supersedes that memory). With none, only an explicit
/// "contradicts"/"no longer" reads as a standalone conflict.
fn is_contradiction(candidate: &str, related: Option<&str>) -> bool {
    let lower = candidate.to_ascii_lowercase();
    let has_negation = NEGATION_MARKERS.iter().any(|m| lower.contains(m));
    match related {
        Some(_) => has_negation,
        None => lower.contains("contradict") || lower.contains("no longer"),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewModeReport {
    pub annotated: usize,
    pub accepted: usize,
    pub manual: usize,
}

pub struct ReviewModeProcessor;

impl ReviewModeProcessor {
    pub fn apply_project(
        project_root: impl AsRef<std::path::Path>,
    ) -> Result<ReviewModeReport, ReviewModeError> {
        let config =
            ProjectConfig::discover(project_root.as_ref()).map_err(ReviewModeError::Config)?;
        let queue = ReviewQueue::open_project(&config.project_root)?;
        let persistence = MemoryPersistence::open_project(&config.project_root)?;
        let mut report = ReviewModeReport {
            annotated: 0,
            accepted: 0,
            manual: 0,
        };

        for mut item in queue.list()? {
            if !matches!(item.state, localmind_core::ReviewState::Pending) {
                continue;
            }
            let confidence = item.candidate.confidence.value();
            // Recall candidates with FTS, then keep only those that are actually
            // similar to the candidate — not merely sharing one keyword (the old
            // top-1 behaviour produced false duplicates and missed real ones).
            let summary = item.candidate.summary();
            let candidate_tokens = token_set(summary);
            let mut best: Option<(crate::MemorySearchResult, f32)> = None;
            for hit in persistence.search(summary)? {
                let sim = similarity(&candidate_tokens, &token_set(&hit.snippet));
                if best.as_ref().is_none_or(|(_, score)| sim > *score) {
                    best = Some((hit, sim));
                }
            }
            let duplicate = best
                .as_ref()
                .filter(|(_, sim)| *sim >= DUPLICATE_SIMILARITY)
                .map(|(hit, _)| hit.clone());
            // A genuine contradiction: a corrective/negating statement that
            // overlaps an existing memory's topic (it likely reverses it), or an
            // explicit "no longer"/"contradicts" assertion on its own.
            let related = best
                .as_ref()
                .filter(|(_, sim)| *sim >= CONFLICT_TOPIC_OVERLAP)
                .map(|(hit, _)| hit.snippet.as_str());
            // The memory a contradicting candidate would supersede: the same
            // topically-related hit, identified for an automated supersede.
            let related_target = best
                .as_ref()
                .filter(|(_, sim)| *sim >= CONFLICT_TOPIC_OVERLAP)
                .map(|(hit, _)| hit.memory_id.clone());
            let conflict = is_contradiction(summary, related);
            item.candidate.review_annotation = Some(ReviewAnnotation {
                score: Confidence::new(confidence)?,
                duplicate_of: duplicate.as_ref().map(|hit| hit.memory_id.to_string()),
                conflict,
                notes: if duplicate.is_some() {
                    "Similar accepted memory found; human review recommended.".to_string()
                } else {
                    "No close duplicate found in accepted memory.".to_string()
                },
            });

            match config.config.review.mode {
                ReviewModeConfig::Manual => {
                    report.manual += 1;
                }
                ReviewModeConfig::Assisted => {
                    queue.replace_candidate(&item.id, &item.candidate)?;
                    persistence.write_mode_audit("assisted", item.id.as_str(), false)?;
                    report.annotated += 1;
                }
                ReviewModeConfig::Trusted => {
                    queue.replace_candidate(&item.id, &item.candidate)?;
                    let above_threshold = confidence >= config.config.review.trusted_threshold;
                    // A contradiction with a clear target retires that memory; a
                    // clean novel candidate is accepted; everything else (a
                    // conflict with no clear target, a duplicate, low confidence)
                    // stays human-gated.
                    let decided = if above_threshold {
                        match (conflict, related_target.clone()) {
                            (true, Some(target)) => auto_decide(
                                &queue,
                                &persistence,
                                &item.id,
                                ReviewAction::Supersede(target),
                                "localmind-trusted",
                                "trusted mode auto-superseded a contradicted memory",
                                "trusted",
                            )?,
                            (false, _) if duplicate.is_none() => auto_decide(
                                &queue,
                                &persistence,
                                &item.id,
                                ReviewAction::Accept,
                                "localmind-trusted",
                                "trusted mode auto-accepted above threshold",
                                "trusted",
                            )?,
                            _ => false,
                        }
                    } else {
                        false
                    };
                    if decided {
                        report.accepted += 1;
                    } else {
                        persistence.write_mode_audit("trusted", item.id.as_str(), false)?;
                        report.manual += 1;
                    }
                }
                ReviewModeConfig::Automatic => {
                    queue.replace_candidate(&item.id, &item.candidate)?;
                    // Auto-retiring a human's prior memory is gated on the same
                    // confidence threshold as trusted mode (risk control); a clean
                    // novel candidate auto-accepts as before.
                    let above_threshold = confidence >= config.config.review.trusted_threshold;
                    let decided = match (conflict, related_target.clone()) {
                        (true, Some(target)) if above_threshold => auto_decide(
                            &queue,
                            &persistence,
                            &item.id,
                            ReviewAction::Supersede(target),
                            "localmind-automatic",
                            "automatic mode auto-superseded a contradicted memory",
                            "automatic",
                        )?,
                        (false, _) if duplicate.is_none() => auto_decide(
                            &queue,
                            &persistence,
                            &item.id,
                            ReviewAction::Accept,
                            "localmind-automatic",
                            "automatic mode auto-accepted",
                            "automatic",
                        )?,
                        _ => false,
                    };
                    if decided {
                        report.accepted += 1;
                    } else {
                        persistence.write_mode_audit("automatic", item.id.as_str(), false)?;
                        report.manual += 1;
                    }
                }
            }
        }

        Ok(report)
    }
}

/// Record an automated decision (accept or supersede), its review audit, and the
/// mode audit marking it auto-decided. Returns `true` so callers can tally it.
fn auto_decide(
    queue: &ReviewQueue,
    persistence: &MemoryPersistence,
    item_id: &ReviewItemId,
    action: ReviewAction,
    reviewer: &str,
    note: &str,
    mode: &str,
) -> Result<bool, ReviewModeError> {
    let decided = queue.decide(ReviewDecision {
        item_id: item_id.clone(),
        action,
        reviewer: reviewer.to_string(),
        decided_at: None,
        note: Some(note.to_string()),
        replacement_summary: None,
        evidence: Vec::new(),
    })?;
    persistence.record_review_item_audit(&decided)?;
    persistence.write_mode_audit(mode, item_id.as_str(), true)?;
    Ok(true)
}

trait ReviewModeAudit {
    fn write_mode_audit(
        &self,
        mode: &str,
        item_id: &str,
        auto_accepted: bool,
    ) -> Result<(), MemoryPersistenceError>;
}

impl ReviewModeAudit for MemoryPersistence {
    fn write_mode_audit(
        &self,
        mode: &str,
        item_id: &str,
        auto_accepted: bool,
    ) -> Result<(), MemoryPersistenceError> {
        self.record_custom_audit(
            AuditEventKind::ReviewModeApplied,
            "localmind",
            item_id,
            &serde_json::json!({ "mode": mode, "auto_accepted": auto_accepted }),
        )
    }
}

#[derive(Debug, Error)]
pub enum ReviewModeError {
    #[error(transparent)]
    Config(#[from] crate::StoreConfigError),
    #[error(transparent)]
    Queue(#[from] ReviewQueueError),
    #[error(transparent)]
    Persistence(#[from] MemoryPersistenceError),
    #[error(transparent)]
    Contract(#[from] localmind_core::ContractError),
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::{
        CloseoutProcessor, DeterministicExtractor, TranscriptImportFormat, TranscriptImporter,
    };
    use localmind_core::{
        CandidateLesson, EvidenceKind, EvidenceRef, LessonCategory, LessonId, MemoryEntry,
        MemoryEntryId, MemoryScope, MemoryStatus, ReviewState, SessionId, SessionSource,
        SuggestedAction,
    };

    fn seed_memory(id: &str, body: &str) -> MemoryEntry {
        MemoryEntry {
            id: MemoryEntryId::new(id),
            scope: MemoryScope::Project,
            body: body.to_string(),
            category: LessonCategory::ProjectConvention,
            confidence: Confidence::new(0.9).unwrap(),
            source_session: Some(SessionId::new("seed")),
            evidence: vec![EvidenceRef::new(EvidenceKind::Transcript, "redacted").redacted()],
            tags: vec!["accepted".to_string()],
            related_files: Vec::new(),
            related_entities: Vec::new(),
            created_at: None,
            updated_at: None,
            supersedes: Vec::new(),
            contradicts: Vec::new(),
            status: MemoryStatus::Active,
        }
    }

    #[test]
    fn similarity_flags_restatements_not_single_keyword_overlap() {
        let memory = token_set("always run the integration suite for exporter changes");
        let restatement = token_set("exporter changes require running the integration suite");
        assert!(
            similarity(&memory, &restatement) >= DUPLICATE_SIMILARITY,
            "a restatement of the same fact must read as a duplicate"
        );

        // Shares only "exporter" — the old top-1 FTS hit treated this as a
        // duplicate; the similarity threshold must not.
        let unrelated = token_set("the exporter dashboard needs a dark theme toggle");
        assert!(
            similarity(&memory, &unrelated) < DUPLICATE_SIMILARITY,
            "one shared keyword is not a duplicate"
        );
    }

    #[test]
    fn contradiction_requires_a_correction_over_a_related_memory() {
        let related = "use tabs for indentation in this project";
        assert!(is_contradiction(
            "do not use tabs for indentation; use spaces instead",
            Some(related)
        ));
        // A neutral restatement over a related memory is not a conflict.
        assert!(!is_contradiction(
            "always indent consistently across the project",
            Some(related)
        ));
        // A correction with no related memory is not a standalone conflict…
        assert!(!is_contradiction("do not use tabs for indentation", None));
        // …but an explicit reversal is.
        assert!(is_contradiction("that guidance no longer applies", None));
        assert!(is_contradiction("these notes contradict the policy", None));
    }

    fn candidate(summary: &str) -> CandidateLesson {
        CandidateLesson::new(
            LessonId::new(format!("lesson-{}", summary.len())),
            summary,
            LessonCategory::Process,
            Confidence::new(0.7).unwrap(),
            SuggestedAction::PromoteToMemory,
        )
        .with_evidence(EvidenceRef::new(EvidenceKind::Transcript, "redacted").redacted())
    }

    /// End-to-end: a near-duplicate of accepted memory is routed to manual review
    /// (not auto-accepted), while a genuinely novel candidate auto-accepts — the
    /// false-duplicate-on-one-keyword behaviour is gone.
    #[test]
    fn automatic_mode_blocks_near_duplicates_but_accepts_novel_candidates() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join(".localmind.toml"),
            "[learning]\nenabled = true\n\n[review]\nmode = \"automatic\"\n",
        )
        .unwrap();

        // Seed accepted memory by closing out and promoting an explicit lesson.
        let config = ProjectConfig::discover(root).unwrap();
        let import = TranscriptImporter::import_text(
            &config,
            "Lesson: always run the integration suite for exporter changes.\n",
            SessionSource::GenericTranscript,
            TranscriptImportFormat::PlainText,
        )
        .unwrap();
        CloseoutProcessor::closeout_project_session(
            root,
            &import.session_id,
            &DeterministicExtractor,
        )
        .unwrap();
        let queue = ReviewQueue::open_project(root).unwrap();
        let persistence = MemoryPersistence::open_project(root).unwrap();
        let seed = queue.list().unwrap();
        let seed_item = seed.first().expect("a seeded candidate");
        queue
            .decide(ReviewDecision {
                item_id: seed_item.id.clone(),
                action: ReviewAction::Accept,
                reviewer: "test".to_string(),
                decided_at: None,
                note: None,
                replacement_summary: None,
                evidence: Vec::new(),
            })
            .unwrap();
        persistence.promote_review_item(&seed_item.id).unwrap();

        // Enqueue a near-duplicate and a novel candidate, then apply automatic mode.
        let near_dup = candidate("exporter changes require running the integration suite");
        let novel = candidate("prefer ripgrep over grep when searching the codebase");
        queue
            .enqueue_candidates(&import.session_id, &[near_dup.clone(), novel.clone()])
            .unwrap();

        ReviewModeProcessor::apply_project(root).unwrap();

        let items = queue.list().unwrap();
        let find = |summary: &str| {
            items
                .iter()
                .find(|i| i.candidate.summary() == summary)
                .unwrap_or_else(|| panic!("missing item {summary}"))
        };
        let dup_item = find(near_dup.summary());
        let novel_item = find(novel.summary());

        assert_ne!(
            dup_item.state,
            ReviewState::Accepted,
            "a near-duplicate must not auto-accept"
        );
        assert!(
            dup_item
                .candidate
                .review_annotation
                .as_ref()
                .unwrap()
                .duplicate_of
                .is_some(),
            "the near-duplicate should be flagged against the accepted memory"
        );
        assert_eq!(
            novel_item.state,
            ReviewState::Accepted,
            "a novel candidate should auto-accept under automatic mode"
        );
    }

    #[test]
    fn automatic_mode_supersedes_a_contradiction_with_a_clear_target() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join(".localmind.toml"),
            "[learning]\nenabled = true\n\n[review]\nmode = \"automatic\"\ntrusted_threshold = 0.5\n",
        )
        .unwrap();

        // An accepted memory the contradiction should retire.
        let persistence = MemoryPersistence::open_project(root).unwrap();
        persistence
            .persist_memory_entry(&seed_memory(
                "mem-tabs",
                "use tabs for indentation in this project",
            ))
            .unwrap();

        let queue = ReviewQueue::open_project(root).unwrap();
        let contradiction = candidate("do not use tabs for indentation; use spaces instead");
        let novel = candidate("prefer ripgrep over grep when searching the codebase");
        queue
            .enqueue_candidates(
                &SessionId::new("session"),
                &[contradiction.clone(), novel.clone()],
            )
            .unwrap();

        ReviewModeProcessor::apply_project(root).unwrap();

        let items = queue.list().unwrap();
        let find = |summary: &str| {
            items
                .iter()
                .find(|item| item.candidate.summary() == summary)
                .unwrap_or_else(|| panic!("missing item {summary}"))
        };
        // The contradiction is auto-decided as a supersede of the seeded memory.
        let superseding = find(contradiction.summary());
        assert_eq!(superseding.state, ReviewState::Accepted);
        assert_eq!(superseding.reviewer_action.as_deref(), Some("supersede"));
        assert_eq!(
            superseding
                .supersede_target
                .as_ref()
                .map(MemoryEntryId::as_str),
            Some("mem-tabs")
        );
        // A clean novel candidate still plain-accepts (no target).
        let novel_item = find(novel.summary());
        assert_eq!(novel_item.state, ReviewState::Accepted);
        assert_eq!(novel_item.reviewer_action.as_deref(), Some("accept"));
    }

    #[test]
    fn manual_mode_leaves_a_contradiction_for_a_human_to_target() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join(".localmind.toml"),
            "[learning]\nenabled = true\n\n[review]\nmode = \"manual\"\n",
        )
        .unwrap();
        let persistence = MemoryPersistence::open_project(root).unwrap();
        persistence
            .persist_memory_entry(&seed_memory(
                "mem-tabs",
                "use tabs for indentation in this project",
            ))
            .unwrap();
        let queue = ReviewQueue::open_project(root).unwrap();
        let contradiction = candidate("do not use tabs for indentation; use spaces instead");
        queue
            .enqueue_candidates(&SessionId::new("session"), &[contradiction.clone()])
            .unwrap();

        ReviewModeProcessor::apply_project(root).unwrap();

        let item = queue
            .list()
            .unwrap()
            .into_iter()
            .find(|item| item.candidate.summary() == contradiction.summary())
            .unwrap();
        assert_eq!(
            item.state,
            ReviewState::Pending,
            "manual mode leaves the supersede target choice to a human"
        );
    }
}
