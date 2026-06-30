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
/// Cosine ≥ this is a **confident** semantic duplicate of an existing accepted
/// memory. Conservative on purpose: normalized embeddings put genuine
/// paraphrases high (~0.85–0.95) and distinct technical lessons well below
/// (~0.3–0.6), so a high cut catches restatements without merging distinct
/// lessons. A match only *flags* `duplicate_of` (routes to review); it never
/// deletes, so the cost of a wrong cut is bounded.
const VECTOR_DUPLICATE_SIMILARITY: f32 = 0.86;

/// Lower edge of the **route-to-review band**: a cosine in
/// `[VECTOR_REVIEW_BAND, VECTOR_DUPLICATE_SIMILARITY)` is a *borderline*
/// paraphrase — surfaced for a human (the annotation notes it as borderline) but
/// never auto-merged or deleted. This widens what is routed to review without
/// lowering the confident-merge bar above: observed real paraphrases cluster at
/// 0.80–0.95 (warm-store evidence: 3 pairs at 0.881–0.896 cleared the hard bar,
/// several borderline ones sat at 0.80–0.85), so a 0.83 lower edge catches the
/// borderline cluster while genuinely-distinct lessons (≲0.80) stay clear. Both
/// tiers route to review under automatic mode; the band only changes *how many*
/// candidates a human sees, never whether one is deleted.
const VECTOR_REVIEW_BAND: f32 = 0.83;

/// How many nearest vectors to fetch when looking for a semantic duplicate.
/// More than one because the `vector_index` also holds non-memory subjects (e.g.
/// ingested code chunks): a non-memory vector ranking first would hide a real
/// accepted-memory duplicate behind it if we only asked for the single nearest.
/// Mirrors the retrieval path's `limit.max(20)` candidate window.
const VECTOR_DUPLICATE_CANDIDATES: usize = 20;

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
            // Lexical overlap is the cheap first pass and the no-embeddings
            // fallback. When semantic dedup is active (the `review.semantic_dedup`
            // opt-in plus a configured embedding endpoint) and lexical did not
            // already flag a duplicate, confirm against accepted-memory vectors:
            // this catches paraphrases that mean the same thing but share few
            // words. With embeddings unavailable, behaviour is exactly the lexical
            // contract. A semantic match only *flags* `duplicate_of` → routed to
            // review like a lexical duplicate, never auto-deleted.
            let mut duplicate_of = best
                .as_ref()
                .filter(|(_, sim)| *sim >= DUPLICATE_SIMILARITY)
                .map(|(hit, _)| hit.memory_id.to_string());
            // Whether the duplicate was a *borderline* semantic match (in the
            // route-to-review band, below the confident bar) — surfaced to a human
            // with that caveat, never auto-merged. A lexical duplicate is always
            // confident.
            let mut borderline_duplicate = false;
            if duplicate_of.is_none() && config.semantic_dedup_active() {
                if let Some(found) = vector_duplicate_of(&persistence, summary)? {
                    borderline_duplicate = !found.confident;
                    duplicate_of = Some(found.memory_id);
                }
            }
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
            // The lesson-quality verdict: a tooling-noise or over-fit candidate
            // is withheld from auto-accept below (routed to manual review like a
            // duplicate), and the reason is surfaced to the reviewer here. The
            // classifier only labels — it never deletes (D-LM-0016).
            let quality = crate::classify_quality(&item.candidate.category, summary, "");
            let mut notes = match (duplicate_of.is_some(), borderline_duplicate) {
                (true, true) => {
                    "Borderline semantic match (review band); human review recommended.".to_string()
                }
                (true, false) => {
                    "Similar accepted memory found; human review recommended.".to_string()
                }
                (false, _) => "No close duplicate found in accepted memory.".to_string(),
            };
            if let Some(note) = quality.review_note() {
                notes = format!("{notes} {note}");
            }
            item.candidate.review_annotation = Some(ReviewAnnotation {
                score: Confidence::new(confidence)?,
                duplicate_of: duplicate_of.clone(),
                conflict,
                notes,
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
                            (false, _) if duplicate_of.is_none() && quality.is_general() => {
                                auto_decide(
                                    &queue,
                                    &persistence,
                                    &item.id,
                                    ReviewAction::Accept,
                                    "localmind-trusted",
                                    "trusted mode auto-accepted above threshold",
                                    "trusted",
                                )?
                            }
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
                        (false, _) if duplicate_of.is_none() && quality.is_general() => {
                            auto_decide(
                                &queue,
                                &persistence,
                                &item.id,
                                ReviewAction::Accept,
                                "localmind-automatic",
                                "automatic mode auto-accepted",
                                "automatic",
                            )?
                        }
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
    // A queue decision only *marks* the item accepted; writing the durable memory
    // (with project/global scope routing) is a separate promote step. Automatic
    // and trusted modes must promote here — otherwise auto-accept reports success
    // while nothing is persisted or retrievable. Only an accepted/edited (incl.
    // supersede) decision is promotable; anything else is left as decided.
    if matches!(
        decided.state,
        localmind_core::ReviewState::Accepted | localmind_core::ReviewState::Edited
    ) {
        persistence.promote_review_item(item_id)?;
    }
    Ok(true)
}

/// A semantic duplicate found by the vector rung: the accepted memory's id and
/// whether the match cleared the confident bar ([`VECTOR_DUPLICATE_SIMILARITY`])
/// or only the lower route-to-review band edge ([`VECTOR_REVIEW_BAND`]).
struct VectorDuplicate {
    memory_id: String,
    /// `true` when cosine ≥ [`VECTOR_DUPLICATE_SIMILARITY`] (confident);
    /// `false` for a borderline match in the route-to-review band.
    confident: bool,
}

/// The accepted memory whose embedding is a semantic duplicate of `summary`
/// (cosine ≥ [`VECTOR_REVIEW_BAND`]), with a confident/borderline tier, or
/// `None`. Both tiers route to review (the caller sets `duplicate_of`); the tier
/// only colours the reviewer-facing note, and neither ever auto-merges or
/// deletes. Best-effort: with no embedding endpoint, an unreachable one, or no
/// stored vectors, this returns `None` and dedup falls back to the lexical
/// contract — it never errors the closeout on an embedding hiccup.
fn vector_duplicate_of(
    persistence: &MemoryPersistence,
    summary: &str,
) -> Result<Option<VectorDuplicate>, ReviewModeError> {
    let Some(vector) = persistence.embed_query(summary)? else {
        return Ok(None);
    };
    // Fetch candidate headroom, then filter to memory subjects before taking the
    // top match — so a higher-ranked non-memory vector cannot drop a real memory
    // duplicate. The candidates are score-ordered, so the first memory hit at or
    // above the band edge is the nearest accepted-memory duplicate.
    let nearest = persistence.vector_search(&vector, VECTOR_DUPLICATE_CANDIDATES)?;
    Ok(nearest
        .into_iter()
        .filter(|hit| hit.subject_kind == "memory")
        .find(|hit| hit.score >= VECTOR_REVIEW_BAND)
        .map(|hit| VectorDuplicate {
            memory_id: hit.subject_id,
            confident: hit.score >= VECTOR_DUPLICATE_SIMILARITY,
        }))
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
            "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n\n[review]\nmode = \"automatic\"\n",
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

    /// The write-time quality gate: under automatic mode a tooling-noise and an
    /// over-fit candidate are withheld from auto-accept (left for manual review),
    /// while a general lesson still auto-accepts. None is deleted.
    #[test]
    fn automatic_mode_withholds_low_quality_candidates_but_accepts_a_general_one() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join(".localmind.toml"),
            "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n\n[review]\nmode = \"automatic\"\n",
        )
        .unwrap();

        // A tooling-noise lesson (build/cwd mechanic), an over-fit lesson (a call
        // welded to one exercise's identifiers), and a general principle.
        let tooling = candidate(
            "Initial shell commands failed due to incorrect working directory assumptions.",
        );
        let overfit = candidate(
            "Avoid `zip(words, letters)` when emitting an initial state before any letter arrives.",
        );
        let general = candidate("prefer ripgrep over grep when searching the codebase");
        let queue = ReviewQueue::open_project(root).unwrap();
        queue
            .enqueue_candidates(
                &SessionId::new("session"),
                &[tooling.clone(), overfit.clone(), general.clone()],
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
        assert_ne!(
            find(tooling.summary()).state,
            ReviewState::Accepted,
            "a tooling-noise candidate must not auto-accept"
        );
        assert_ne!(
            find(overfit.summary()).state,
            ReviewState::Accepted,
            "an over-fit candidate must not auto-accept"
        );
        assert_eq!(
            find(general.summary()).state,
            ReviewState::Accepted,
            "a general candidate still auto-accepts under automatic mode"
        );
        // Nothing was deleted: all three candidates are still present (the bad two
        // stay in the queue for a human, never discarded).
        assert_eq!(items.len(), 3, "no candidate was discarded: {items:?}");
        // The reviewer sees the quality reason on a withheld candidate.
        assert!(
            find(tooling.summary())
                .candidate
                .review_annotation
                .as_ref()
                .is_some_and(|a| a.notes.contains("tooling-noise")),
            "the tooling-noise reason must be surfaced to the reviewer"
        );
    }

    #[test]
    fn automatic_mode_supersedes_a_contradiction_with_a_clear_target() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join(".localmind.toml"),
            "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n\n[review]\nmode = \"automatic\"\ntrusted_threshold = 0.5\n",
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
            "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n\n[review]\nmode = \"manual\"\n",
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
