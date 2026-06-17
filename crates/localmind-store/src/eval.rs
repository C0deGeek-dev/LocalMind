//! Memory-quality evaluation: a golden-session harness that scores the engine's
//! extraction precision/recall and retrieval recall@k, so a regression in
//! either is caught by a number rather than going unnoticed — the failure mode
//! that let a noisy extractor ship undetected.
//!
//! Fixtures and the scorer live here in LocalMind (the engine owns extraction
//! and retrieval). LocalBench runs this through the `localmind eval` subcommand
//! and renders the report, making it the single "evidence" product for runtime
//! and memory quality.

use crate::{
    CloseoutProcessor, DeterministicExtractor, MemoryPersistence, MemorySearchResult,
    ProjectConfig, ReviewQueue, SessionExtractor, TranscriptImportFormat, TranscriptImporter,
};
use localmind_core::{ReviewAction, ReviewDecision, SessionSource};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use thiserror::Error;

/// A retrieval reranker the eval can apply before measuring recall@k, so the
/// retrieval lift of the optional rerank stage (subject 01) can be scored
/// against the keyword baseline. Object-safe and defined here so the store does
/// not depend on the search crate; a host that has the embedding reranker
/// injects it, and tests inject a stub.
pub trait EvalReranker {
    /// Reorder the search results for `query`. The eval measures recall@k before
    /// and after to report the retrieval lift.
    fn rerank(&self, query: &str, results: Vec<MemorySearchResult>) -> Vec<MemorySearchResult>;
}

/// One retrieval case: a query, and a substring that a top-k memory snippet must
/// contain for the case to count as answered.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RetrievalCase {
    pub query: String,
    pub expected_contains: String,
}

impl RetrievalCase {
    pub fn new(query: impl Into<String>, expected_contains: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            expected_contains: expected_contains.into(),
        }
    }
}

/// A golden evaluation fixture: a session transcript, the lesson texts a good
/// extractor should surface (matched case-insensitively as substrings of a
/// candidate summary), and the retrieval cases a good index answers after the
/// candidates are promoted.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvalFixture {
    pub name: String,
    pub transcript: String,
    pub expected_lessons: Vec<String>,
    pub retrieval_cases: Vec<RetrievalCase>,
}

/// Load golden fixtures from JSON (the on-disk fixture format). The built-in
/// [`default_fixtures`] are the committed seed set; this lets a host or
/// LocalBench supply its own fixture file in the same shape.
///
/// # Errors
/// Returns [`EvalError::Fixture`] when the JSON does not match the fixture shape.
pub fn load_fixtures(json: &str) -> Result<Vec<EvalFixture>, EvalError> {
    serde_json::from_str(json).map_err(EvalError::Fixture)
}

/// The difference in mean scores between a candidate report and a baseline —
/// the lift (positive) or regression (negative) of turning a feature on.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct EvalLift {
    pub extraction_precision_delta: f32,
    pub extraction_recall_delta: f32,
    pub retrieval_recall_at_k_delta: f32,
}

/// The lift of `candidate` over `baseline`: per-mean deltas. A rerank-on report
/// vs the keyword baseline gives retrieval lift; a model-extraction report vs
/// the deterministic baseline gives extraction lift.
#[must_use]
pub fn lift(baseline: &EvalReport, candidate: &EvalReport) -> EvalLift {
    EvalLift {
        extraction_precision_delta: candidate.mean_extraction_precision
            - baseline.mean_extraction_precision,
        extraction_recall_delta: candidate.mean_extraction_recall - baseline.mean_extraction_recall,
        retrieval_recall_at_k_delta: candidate.mean_retrieval_recall_at_k
            - baseline.mean_retrieval_recall_at_k,
    }
}

/// Scores for one fixture. Precision/recall treat an expected lesson as matched
/// when a candidate summary contains it (case-insensitive).
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct FixtureScore {
    pub name: String,
    pub candidate_count: usize,
    pub expected_count: usize,
    pub matched_expected: usize,
    pub extraction_precision: f32,
    pub extraction_recall: f32,
    pub retrieval_cases: usize,
    pub retrieval_hits: usize,
    pub retrieval_recall_at_k: f32,
}

/// The full report across all fixtures, with means used for the regression gate.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct EvalReport {
    pub k: usize,
    pub scores: Vec<FixtureScore>,
    pub mean_extraction_precision: f32,
    pub mean_extraction_recall: f32,
    pub mean_retrieval_recall_at_k: f32,
}

#[derive(Debug, Error)]
pub enum EvalError {
    #[error("failed to prepare eval workspace {path:?}: {source}")]
    Workspace {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error(transparent)]
    Config(#[from] crate::StoreConfigError),
    #[error(transparent)]
    Import(#[from] crate::ImportError),
    #[error(transparent)]
    Closeout(#[from] crate::CloseoutError),
    #[error(transparent)]
    Review(#[from] crate::ReviewQueueError),
    #[error(transparent)]
    Memory(#[from] crate::MemoryPersistenceError),
    #[error("failed to parse eval fixtures: {0}")]
    Fixture(serde_json::Error),
    #[error(transparent)]
    Inference(#[from] localmind_inference::InferenceError),
}

fn contains_ci(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

/// Score one fixture in an isolated project directory under `work_root`, using
/// `extractor` for close-out and optionally `reranker` before retrieval scoring.
fn score_fixture<E: SessionExtractor>(
    fixture: &EvalFixture,
    k: usize,
    work_root: &Path,
    extractor: &E,
    reranker: Option<&dyn EvalReranker>,
) -> Result<FixtureScore, EvalError> {
    let project = work_root.join(&fixture.name);
    fs::create_dir_all(&project).map_err(|source| EvalError::Workspace {
        path: project.clone(),
        source,
    })?;
    fs::write(
        project.join(".localmind.toml"),
        "[learning]\nenabled = true\n",
    )
    .map_err(|source| EvalError::Workspace {
        path: project.clone(),
        source,
    })?;

    let config = ProjectConfig::discover(&project)?;
    let import = TranscriptImporter::import_text(
        &config,
        &fixture.transcript,
        SessionSource::GenericTranscript,
        TranscriptImportFormat::PlainText,
    )?;
    CloseoutProcessor::closeout_project_session(&project, &import.session_id, extractor)?;

    let queue = ReviewQueue::open_project(&project)?;
    let items = queue.list()?;
    let summaries: Vec<String> = items
        .iter()
        .map(|item| item.candidate.summary().to_string())
        .collect();

    let candidate_count = items.len();
    let expected_count = fixture.expected_lessons.len();
    let matched_expected = fixture
        .expected_lessons
        .iter()
        .filter(|expected| summaries.iter().any(|s| contains_ci(s, expected)))
        .count();
    let matched_candidates = summaries
        .iter()
        .filter(|s| {
            fixture
                .expected_lessons
                .iter()
                .any(|expected| contains_ci(s, expected))
        })
        .count();

    // Precision: of the candidates produced, how many were expected. A fixture
    // that expects nothing and produces nothing scores a perfect 1.0 (no false
    // positives); producing candidates when none were expected scores 0.0.
    let extraction_precision = if candidate_count == 0 {
        if expected_count == 0 {
            1.0
        } else {
            0.0
        }
    } else {
        matched_candidates as f32 / candidate_count as f32
    };
    let extraction_recall = if expected_count == 0 {
        1.0
    } else {
        matched_expected as f32 / expected_count as f32
    };

    // Promote every candidate, then score retrieval over the promoted memory.
    let persistence = MemoryPersistence::open_project(&project)?;
    for item in &items {
        queue.decide(ReviewDecision {
            item_id: item.id.clone(),
            action: ReviewAction::Accept,
            reviewer: "eval".to_string(),
            decided_at: None,
            note: None,
            replacement_summary: None,
            evidence: Vec::new(),
        })?;
        persistence.promote_review_item(&item.id)?;
    }

    let mut retrieval_hits = 0;
    for case in &fixture.retrieval_cases {
        let mut results = persistence.search(&case.query)?;
        // Apply the optional rerank stage before the top-k cut, so its retrieval
        // lift over the keyword baseline is what recall@k measures.
        if let Some(reranker) = reranker {
            results = reranker.rerank(&case.query, results);
        }
        if results
            .iter()
            .take(k)
            .any(|result| contains_ci(&result.snippet, &case.expected_contains))
        {
            retrieval_hits += 1;
        }
    }
    let retrieval_cases = fixture.retrieval_cases.len();
    let retrieval_recall_at_k = if retrieval_cases == 0 {
        1.0
    } else {
        retrieval_hits as f32 / retrieval_cases as f32
    };

    Ok(FixtureScore {
        name: fixture.name.clone(),
        candidate_count,
        expected_count,
        matched_expected,
        extraction_precision,
        extraction_recall,
        retrieval_cases,
        retrieval_hits,
        retrieval_recall_at_k,
    })
}

/// Run the evaluation over `fixtures` with the deterministic extractor and no
/// reranker — the offline baseline. `k` is the retrieval cutoff for recall@k.
pub fn run_eval(
    fixtures: &[EvalFixture],
    k: usize,
    work_root: &Path,
) -> Result<EvalReport, EvalError> {
    run_eval_with(fixtures, k, work_root, &DeterministicExtractor, None)
}

/// Run the evaluation with a chosen `extractor` and optional `reranker`, so a
/// candidate configuration (model extraction on, rerank on) can be scored and
/// compared to the baseline via [`lift`]. Each fixture is scored in its own
/// project under `work_root`.
///
/// # Errors
/// Returns [`EvalError`] when a fixture workspace, import, close-out, review, or
/// retrieval step fails.
pub fn run_eval_with<E: SessionExtractor>(
    fixtures: &[EvalFixture],
    k: usize,
    work_root: &Path,
    extractor: &E,
    reranker: Option<&dyn EvalReranker>,
) -> Result<EvalReport, EvalError> {
    let mut scores = Vec::with_capacity(fixtures.len());
    for fixture in fixtures {
        scores.push(score_fixture(fixture, k, work_root, extractor, reranker)?);
    }

    let mean = |select: fn(&FixtureScore) -> f32| -> f32 {
        if scores.is_empty() {
            return 1.0;
        }
        scores.iter().map(select).sum::<f32>() / scores.len() as f32
    };
    let report = EvalReport {
        k,
        mean_extraction_precision: mean(|s| s.extraction_precision),
        mean_extraction_recall: mean(|s| s.extraction_recall),
        mean_retrieval_recall_at_k: mean(|s| s.retrieval_recall_at_k),
        scores,
    };
    Ok(report)
}

/// Run the baseline (deterministic) eval and a candidate eval that uses the
/// configured-inference extractor, and return the baseline report plus the
/// extraction lift of the model path over it. With no `inference` configured —
/// or an unreachable endpoint — the model path falls back to deterministic and
/// the lift is zero: the honest "no measured lift offline" signal that gates
/// turning model extraction on by default.
///
/// # Errors
/// Returns [`EvalError`] when a fixture run or the inference capability build
/// fails.
pub fn run_eval_lift(
    fixtures: &[EvalFixture],
    k: usize,
    work_root: &Path,
    inference: Option<&localmind_core::InferenceSettings>,
) -> Result<(EvalReport, EvalLift), EvalError> {
    let baseline = run_eval(fixtures, k, &work_root.join("baseline"))?;
    let capability = localmind_inference::InferenceCapability::from_settings(inference)?;
    let extractor = crate::ModelBackedExtractor::new(&capability);
    let candidate = run_eval_with(fixtures, k, &work_root.join("candidate"), &extractor, None)?;
    let lift = lift(&baseline, &candidate);
    Ok((baseline, lift))
}

/// The built-in golden fixtures. Original to this repository (no captured
/// workspace content). Seeded by the end-to-end loop fixture plus a negative
/// case that mirrors the dumped-file content which previously flooded the queue.
pub fn default_fixtures() -> Vec<EvalFixture> {
    vec![
        EvalFixture {
            name: "exporter-bugfix".to_string(),
            transcript: "\
user: the exporter test keeps failing on empty parquet files
assistant: error: assertion failed: row_groups == 0 in exporter/src/writer.rs
assistant: Fixed: flush the batch before clearing the buffer at the capacity boundary; the suite is passing now.
user: Lesson: exporter changes need the integration suite, the unit tests miss schema drift.
"
            .to_string(),
            expected_lessons: vec![
                "integration suite".to_string(),
                "assertion failed".to_string(),
            ],
            retrieval_cases: vec![
                RetrievalCase::new("exporter integration suite", "integration suite"),
                RetrievalCase::new("parquet row groups assertion", "assertion failed"),
            ],
        },
        EvalFixture {
            name: "dumped-file-content".to_string(),
            // The shape of the dogfood sessions: file paths, source, and docs.
            // A good extractor produces nothing here — no lesson is present.
            transcript: "\
LocalMind\\crates\\localmind-core\\src\\skill.rs
use thiserror::Error;
fn deserialize<D>(d: D) -> Result<Self, D::Error> {
## Implementation Status
- reusable skills, and agent context.
`crates/localmind-skills` — skill draft generation and maintenance boundary.
test result: ok. 19 passed; 0 failed; 0 ignored
"
            .to_string(),
            expected_lessons: Vec::new(),
            retrieval_cases: Vec::new(),
        },
    ]
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn work_root() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn fixtures_round_trip_through_the_loader() {
        let fixtures = default_fixtures();
        let json = serde_json::to_string(&fixtures).unwrap();
        let loaded = load_fixtures(&json).unwrap();
        assert_eq!(loaded.len(), fixtures.len());
        assert_eq!(loaded[0].name, fixtures[0].name);
        assert_eq!(loaded[0].expected_lessons, fixtures[0].expected_lessons);
        // A malformed payload is a typed error, not a panic.
        assert!(load_fixtures("{ not fixtures }").is_err());
    }

    #[test]
    fn the_seed_fixtures_score_as_expected() {
        let work = work_root();
        let report = run_eval(&default_fixtures(), 5, work.path()).unwrap();
        // Both seed fixtures are designed to score perfectly: the bugfix fixture
        // surfaces its two lessons and answers both queries; the dumped-content
        // fixture correctly produces nothing.
        assert_eq!(report.mean_extraction_precision, 1.0);
        assert_eq!(report.mean_extraction_recall, 1.0);
        assert_eq!(report.mean_retrieval_recall_at_k, 1.0);
    }

    #[test]
    fn lift_computes_per_mean_deltas() {
        let baseline = EvalReport {
            k: 5,
            scores: Vec::new(),
            mean_extraction_precision: 0.6,
            mean_extraction_recall: 0.5,
            mean_retrieval_recall_at_k: 0.4,
        };
        let candidate = EvalReport {
            k: 5,
            scores: Vec::new(),
            mean_extraction_precision: 0.9,
            mean_extraction_recall: 0.8,
            mean_retrieval_recall_at_k: 0.7,
        };
        let lift = lift(&baseline, &candidate);
        assert!((lift.extraction_precision_delta - 0.3).abs() < 1e-6);
        assert!((lift.extraction_recall_delta - 0.3).abs() < 1e-6);
        assert!((lift.retrieval_recall_at_k_delta - 0.3).abs() < 1e-6);
    }

    #[test]
    fn the_deterministic_baseline_has_zero_lift_against_itself() {
        let work = work_root();
        let baseline = run_eval(&default_fixtures(), 5, work.path()).unwrap();
        let again = run_eval_with(
            &default_fixtures(),
            5,
            work.path(),
            &DeterministicExtractor,
            None,
        )
        .unwrap();
        let lift = lift(&baseline, &again);
        assert_eq!(lift.extraction_precision_delta, 0.0);
        assert_eq!(lift.extraction_recall_delta, 0.0);
        assert_eq!(lift.retrieval_recall_at_k_delta, 0.0);
    }

    /// A reranker that drops every result, proving the rerank hook is wired into
    /// retrieval scoring: with it, recall@k collapses, so a real reranker's
    /// retrieval lift is measurable through the same seam.
    struct DroppingReranker;

    impl EvalReranker for DroppingReranker {
        fn rerank(
            &self,
            _query: &str,
            _results: Vec<MemorySearchResult>,
        ) -> Vec<MemorySearchResult> {
            Vec::new()
        }
    }

    #[test]
    fn the_reranker_hook_changes_retrieval_recall() {
        let work = work_root();
        let baseline = run_eval(&default_fixtures(), 5, work.path()).unwrap();
        assert_eq!(baseline.mean_retrieval_recall_at_k, 1.0);

        let work2 = work_root();
        let reranked = run_eval_with(
            &default_fixtures(),
            5,
            work2.path(),
            &DeterministicExtractor,
            Some(&DroppingReranker),
        )
        .unwrap();
        // The dropping reranker erases every retrieval hit — the hook is in the
        // scoring path, so a real reranker's lift is measurable here.
        assert!(reranked.mean_retrieval_recall_at_k < baseline.mean_retrieval_recall_at_k);
        let lift = lift(&baseline, &reranked);
        assert!(lift.retrieval_recall_at_k_delta < 0.0);
    }
}
