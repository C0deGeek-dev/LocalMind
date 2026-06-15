//! Re-extraction harness for a corpus of real redacted transcripts.
//!
//! This test is `#[ignore]`d and ships no transcripts of its own — it reads a
//! directory of `*/transcript.redacted.txt` files named by the
//! `LOCALMIND_EXTRACTION_CORPUS` environment variable, runs the deterministic
//! extractor over each through the real closeout path, and reports the
//! candidates. It exists so the extractor can be re-validated against a captured
//! corpus (e.g. a snapshot of a live `.localmind/`) without baking that corpus —
//! which may contain workspace-specific content — into the repository.
//!
//! Run:
//! ```text
//! LOCALMIND_EXTRACTION_CORPUS=/path/to/sessions \
//!   cargo test -p localmind-store --test dogfood_corpus -- --ignored --nocapture
//! ```
//! It prints per-session candidate counts and every surviving summary so a human
//! can confirm the queue is free of file paths / code fragments, and asserts no
//! single session floods the review queue. A clean, lesson-free corpus
//! legitimately yields zero candidates — that is a pass, not a failure.

use localmind_core::SessionSource;
use localmind_store::{
    CloseoutProcessor, DeterministicExtractor, ProjectConfig, TranscriptImportFormat,
    TranscriptImporter,
};
use std::fs;
use std::path::PathBuf;

#[test]
#[ignore = "requires LOCALMIND_EXTRACTION_CORPUS pointing at a transcript corpus"]
fn reextract_corpus_reports_clean_candidates() -> Result<(), Box<dyn std::error::Error>> {
    let corpus = match std::env::var("LOCALMIND_EXTRACTION_CORPUS") {
        Ok(dir) => PathBuf::from(dir),
        Err(_) => {
            eprintln!("LOCALMIND_EXTRACTION_CORPUS not set; nothing to do");
            return Ok(());
        }
    };

    let mut transcripts: Vec<PathBuf> = Vec::new();
    for entry in fs::read_dir(&corpus)? {
        let path = entry?.path();
        let candidate = path.join("transcript.redacted.txt");
        if candidate.is_file() {
            transcripts.push(candidate);
        }
    }
    transcripts.sort();

    // A session that produced more than this many candidates would be flooding
    // the review queue — the failure mode this harness guards against. Before
    // hardening, single sessions produced 100-217 candidates.
    const FLOOD_LIMIT: usize = 20;

    let mut total = 0usize;
    let mut worst = 0usize;
    for transcript_path in &transcripts {
        let session = transcript_path
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let text = fs::read_to_string(transcript_path)?;

        let temp_dir = tempfile::tempdir()?;
        fs::write(
            temp_dir.path().join(".localmind.toml"),
            "[learning]\nenabled = true\n",
        )?;
        let config = ProjectConfig::discover(temp_dir.path())?;
        let import = TranscriptImporter::import_text(
            &config,
            &text,
            SessionSource::GenericTranscript,
            TranscriptImportFormat::PlainText,
        )?;
        let report = CloseoutProcessor::closeout_project_session(
            temp_dir.path(),
            &import.session_id,
            &DeterministicExtractor,
        )?;
        let candidates_json = fs::read_to_string(&report.candidates_path)?;
        let candidates: Vec<serde_json::Value> = serde_json::from_str(&candidates_json)?;

        println!(
            "[{session}] {} candidate(s) from {} transcript lines",
            candidates.len(),
            text.lines().count()
        );
        for candidate in &candidates {
            println!(
                "    - ({}) {}",
                candidate["category"].as_str().unwrap_or("?"),
                candidate["summary"].as_str().unwrap_or("")
            );
        }
        total += candidates.len();
        worst = worst.max(candidates.len());
    }

    println!("TOTAL candidates across corpus: {total} (worst session: {worst})");
    assert!(
        worst <= FLOOD_LIMIT,
        "a session produced {worst} candidates (> {FLOOD_LIMIT}); the queue is flooding again"
    );
    Ok(())
}
