//! Deterministic cold-start repo primer.
//!
//! Turns an [`ArchitectureOverview`] into a candidate `Project` memory — the
//! orientation a session gets without reading files. The distiller is
//! **deterministic and templated**: no model, no inference, byte-stable for a
//! fixed overview. The primer is an inference about the repo, so it is honest
//! about it: category `ArchitectureRule`, confidence `< 1.0`, evidence pinned to
//! `repo@commit` with a `content_hash` over the overview for staleness. It is a
//! *candidate* — it goes through the review queue like any other memory, never
//! written as accepted truth directly.

use crate::overview::ArchitectureOverview;
use crate::CodeGraphError;
use localmind_core::{
    content_fingerprint, CandidateLesson, Confidence, EvidenceKind, EvidenceRef, LessonCategory,
    LessonId, SuggestedAction,
};
use std::fmt::Write as _;

/// The primer's confidence: an inference about the repo, never asserted as fact.
const PRIMER_CONFIDENCE: f32 = 0.7;

/// A content fingerprint over the overview's shape (languages, packages, entry
/// points, hotspots, counts) — **not** the repo or commit, so it changes only
/// when the source-derived structure drifts. Drives primer staleness.
#[must_use]
pub fn overview_content_hash(overview: &ArchitectureOverview) -> String {
    let mut digest = format!("f{}s{}", overview.file_count, overview.symbol_count);
    for language in &overview.languages {
        let _ = write!(digest, "|L{}:{}", language.language, language.file_count);
    }
    for package in &overview.top_packages {
        let _ = write!(digest, "|P{}:{}", package.path, package.file_count);
    }
    for entry in &overview.entry_points {
        let _ = write!(digest, "|E{}", entry.qualified_name);
    }
    for hotspot in &overview.hotspots {
        let _ = write!(digest, "|H{}:{}", hotspot.qualified_name, hotspot.in_degree);
    }
    content_fingerprint(&digest)
}

/// Distils a candidate repo primer from the overview. Deterministic: the same
/// overview, repo, and commit always produce a byte-identical candidate.
pub fn distill_primer(
    overview: &ArchitectureOverview,
    repo: &str,
    commit: &str,
) -> Result<CandidateLesson, CodeGraphError> {
    let content_hash = overview_content_hash(overview);
    let body = render_body(overview, repo, commit);

    // Id is stable for a given source structure (the content hash), so
    // re-distilling an unchanged repo dedups, and a drifted repo yields a new id
    // the reviewer can supersede the prior primer with.
    let id = LessonId::new(format!("repo-primer-{content_hash}"));
    let evidence = EvidenceRef::new(EvidenceKind::CodeParse, format!("repo primer for {repo}"))
        .with_uri(format!("{repo}@{commit}"))
        .with_content_hash(content_hash);

    let mut candidate = CandidateLesson::new(
        id,
        body,
        LessonCategory::ArchitectureRule,
        Confidence::new(PRIMER_CONFIDENCE)?,
        SuggestedAction::PromoteToMemory,
    )
    .with_evidence(evidence);
    candidate.related_files = overview
        .top_packages
        .iter()
        .map(|package| package.path.clone())
        .collect();
    candidate.related_entities = overview
        .hotspots
        .iter()
        .chain(overview.entry_points.iter())
        .map(|symbol| symbol.qualified_name.clone())
        .collect();
    Ok(candidate)
}

/// Whether a primer stamped with `stored_content_hash` is stale against the
/// current overview — i.e. the source-derived structure has drifted.
#[must_use]
pub fn primer_is_stale(stored_content_hash: &str, current: &ArchitectureOverview) -> bool {
    stored_content_hash != overview_content_hash(current)
}

/// The templated primer body: compact, deterministic prose + bullet lists.
fn render_body(overview: &ArchitectureOverview, repo: &str, commit: &str) -> String {
    let mut body = format!("Repository primer for {repo}@{commit}.\n");
    let _ = writeln!(
        body,
        "{} files, {} symbols.",
        overview.file_count, overview.symbol_count
    );

    if !overview.languages.is_empty() {
        let languages: Vec<String> = overview
            .languages
            .iter()
            .map(|stat| format!("{} ({})", stat.language, stat.file_count))
            .collect();
        let _ = writeln!(body, "Languages: {}.", languages.join(", "));
    }
    if !overview.top_packages.is_empty() {
        let packages: Vec<String> = overview
            .top_packages
            .iter()
            .map(|stat| format!("{} ({})", stat.path, stat.file_count))
            .collect();
        let _ = writeln!(body, "Top packages: {}.", packages.join(", "));
    }
    if !overview.entry_points.is_empty() {
        let entries: Vec<&str> = overview
            .entry_points
            .iter()
            .map(|stat| stat.qualified_name.as_str())
            .collect();
        let _ = writeln!(body, "Entry points: {}.", entries.join(", "));
    }
    if !overview.hotspots.is_empty() {
        let hotspots: Vec<String> = overview
            .hotspots
            .iter()
            .map(|stat| format!("{} ({} callers)", stat.qualified_name, stat.in_degree))
            .collect();
        let _ = writeln!(body, "Most-depended-on: {}.", hotspots.join(", "));
    }
    body.push_str(
        "This orientation is inferred from the code graph (heuristic); verify before relying on it.\n",
    );
    body
}

#[cfg(test)]
mod tests {
    use super::{distill_primer, overview_content_hash, primer_is_stale};
    use crate::overview::{ArchitectureOverview, LanguageStat, SymbolStat};
    use localmind_core::{EvidenceKind, LessonCategory, SuggestedAction};

    fn overview() -> ArchitectureOverview {
        ArchitectureOverview {
            file_count: 3,
            symbol_count: 5,
            languages: vec![LanguageStat {
                language: "rust".to_string(),
                file_count: 3,
            }],
            top_packages: Vec::new(),
            entry_points: vec![SymbolStat {
                qualified_name: "src/x.rs::run".to_string(),
                kind: "function".to_string(),
                in_degree: 0,
                out_degree: 2,
            }],
            hotspots: vec![SymbolStat {
                qualified_name: "src/x.rs::hub".to_string(),
                kind: "function".to_string(),
                in_degree: 3,
                out_degree: 0,
            }],
        }
    }

    #[test]
    fn distillation_is_byte_stable() -> Result<(), Box<dyn std::error::Error>> {
        let a = distill_primer(&overview(), "demo", "abc123")?;
        let b = distill_primer(&overview(), "demo", "abc123")?;
        assert_eq!(a.summary(), b.summary());
        assert_eq!(a.id, b.id);
        assert!(a.summary().contains("hub"));
        Ok(())
    }

    #[test]
    fn primer_carries_commit_pinned_evidence_and_sub_one_confidence(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let primer = distill_primer(&overview(), "demo", "abc123")?;
        assert_eq!(primer.category, LessonCategory::ArchitectureRule);
        assert!(primer.confidence.value() < 1.0);
        assert_eq!(primer.suggested_action, SuggestedAction::PromoteToMemory);

        let evidence = primer
            .evidence()
            .first()
            .ok_or("primer must carry evidence")?;
        assert_eq!(evidence.kind, EvidenceKind::CodeParse);
        assert_eq!(evidence.uri.as_deref(), Some("demo@abc123"));
        assert!(evidence.content_hash.is_some());
        Ok(())
    }

    #[test]
    fn staleness_tracks_overview_drift() -> Result<(), Box<dyn std::error::Error>> {
        let primer = distill_primer(&overview(), "demo", "abc123")?;
        let hash = primer
            .evidence()
            .first()
            .and_then(|evidence| evidence.content_hash.clone())
            .ok_or("primer must carry a content hash")?;

        // Same structure → fresh; drifted structure → stale.
        assert!(!primer_is_stale(&hash, &overview()));
        let mut drifted = overview();
        drifted.file_count += 1;
        drifted.hotspots[0].in_degree += 1;
        assert!(primer_is_stale(&hash, &drifted));
        assert_ne!(
            overview_content_hash(&overview()),
            overview_content_hash(&drifted)
        );
        Ok(())
    }
}
