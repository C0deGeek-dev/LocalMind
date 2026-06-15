//! Regression gate for memory quality (M-04).
//!
//! Runs the golden-session evaluation and asserts the engine's extraction
//! precision/recall and retrieval recall@k stay above threshold. This is the
//! signal that would have caught the noisy extractor: a drop here fails CI
//! instead of silently shipping junk.

use localmind_store::{default_fixtures, run_eval};

/// The bar the engine must clear on the built-in golden fixtures.
const THRESHOLD: f32 = 0.9;

#[test]
fn golden_eval_meets_quality_threshold() -> Result<(), Box<dyn std::error::Error>> {
    let work = tempfile::tempdir()?;
    let report = run_eval(&default_fixtures(), 5, work.path())?;

    // Print the report so a failing run shows the numbers.
    println!(
        "extraction precision={:.3} recall={:.3} | retrieval recall@{}={:.3}",
        report.mean_extraction_precision,
        report.mean_extraction_recall,
        report.k,
        report.mean_retrieval_recall_at_k
    );
    for score in &report.scores {
        println!(
            "  {}: candidates={} precision={:.3} recall={:.3} recall@k={:.3}",
            score.name,
            score.candidate_count,
            score.extraction_precision,
            score.extraction_recall,
            score.retrieval_recall_at_k
        );
    }

    assert!(
        report.mean_extraction_precision >= THRESHOLD,
        "extraction precision {:.3} below {THRESHOLD}",
        report.mean_extraction_precision
    );
    assert!(
        report.mean_extraction_recall >= THRESHOLD,
        "extraction recall {:.3} below {THRESHOLD}",
        report.mean_extraction_recall
    );
    assert!(
        report.mean_retrieval_recall_at_k >= THRESHOLD,
        "retrieval recall@{} {:.3} below {THRESHOLD}",
        report.k,
        report.mean_retrieval_recall_at_k
    );

    // The negative fixture must produce no candidates (no false positives).
    let negative = report
        .scores
        .iter()
        .find(|s| s.name == "dumped-file-content")
        .ok_or("missing negative fixture")?;
    assert_eq!(
        negative.candidate_count, 0,
        "dumped file content produced false-positive candidates"
    );

    Ok(())
}
