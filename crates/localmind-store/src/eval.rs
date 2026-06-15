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
    CloseoutProcessor, DeterministicExtractor, MemoryPersistence, ProjectConfig, ReviewQueue,
    TranscriptImportFormat, TranscriptImporter,
};
use localmind_core::{ReviewAction, ReviewDecision, SessionSource};
use serde::Serialize;
use std::fs;
use std::path::Path;
use thiserror::Error;

/// One retrieval case: a query, and a substring that a top-k memory snippet must
/// contain for the case to count as answered.
#[derive(Clone, Debug)]
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
#[derive(Clone, Debug)]
pub struct EvalFixture {
    pub name: String,
    pub transcript: String,
    pub expected_lessons: Vec<String>,
    pub retrieval_cases: Vec<RetrievalCase>,
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
}

fn contains_ci(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

/// Score one fixture in an isolated project directory under `work_root`.
fn score_fixture(fixture: &EvalFixture, k: usize, work_root: &Path) -> Result<FixtureScore, EvalError> {
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
    CloseoutProcessor::closeout_project_session(
        &project,
        &import.session_id,
        &DeterministicExtractor,
    )?;

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
        let results = persistence.search(&case.query)?;
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

/// Run the evaluation over `fixtures`, scoring each in its own project under
/// `work_root`. `k` is the retrieval cutoff for recall@k.
pub fn run_eval(
    fixtures: &[EvalFixture],
    k: usize,
    work_root: &Path,
) -> Result<EvalReport, EvalError> {
    let mut scores = Vec::with_capacity(fixtures.len());
    for fixture in fixtures {
        scores.push(score_fixture(fixture, k, work_root)?);
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
