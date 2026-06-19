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

    // Per-category (per-fixture) minimums: a strong mean must not hide a weak
    // category. Every fixture clears the bar on its own, and a negative fixture
    // (one that expects no lessons) must produce zero candidates — no false
    // positives. This is what keeps the broadened fixture set honest as it grows.
    for score in &report.scores {
        let is_negative = score.expected_count == 0;
        if is_negative {
            assert_eq!(
                score.candidate_count, 0,
                "negative fixture '{}' produced {} false-positive candidate(s)",
                score.name, score.candidate_count
            );
        } else {
            assert!(
                score.extraction_recall >= THRESHOLD,
                "fixture '{}' extraction recall {:.3} below {THRESHOLD}",
                score.name,
                score.extraction_recall
            );
        }
        assert!(
            score.extraction_precision >= THRESHOLD,
            "fixture '{}' extraction precision {:.3} below {THRESHOLD}",
            score.name,
            score.extraction_precision
        );
        assert!(
            score.retrieval_recall_at_k >= THRESHOLD,
            "fixture '{}' retrieval recall@k {:.3} below {THRESHOLD}",
            score.name,
            score.retrieval_recall_at_k
        );
    }

    // The category set the broadened eval must keep covering: explicit markers,
    // failure→resolution, user corrections, supersede/conflict signals, a noisy
    // transcript, and low-value/dumped content that yields no durable memory.
    for required in [
        "exporter-bugfix",
        "dumped-file-content",
        "stale-superseded-retry-budget",
        "contradictory-preference-tabs-spaces",
        "failed-tool-recovery-enospc",
        "noisy-transcript-single-lesson",
        "low-value-closeout",
    ] {
        assert!(
            report.scores.iter().any(|s| s.name == required),
            "missing required fixture category: {required}"
        );
    }

    Ok(())
}
