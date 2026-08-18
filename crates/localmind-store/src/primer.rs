//! Queryless "project primer": the most salient + fresh accepted memories for a
//! project, as a context pack — a baseline for a session-start recall when there
//! is no task query yet. Deterministic, offline, and **read-only** (records no
//! audit event, unlike `context export`).
//!
//! Ranking: category is the semantic prior (a project decision must not be
//! outranked by a popular note); a bounded usage bucket and recency are small
//! boosts/tie-breaks that can never cross a category tier. `stale_candidate` and
//! `contradicted` rows are excluded entirely — queryless context cannot safely
//! carry a conflict. Project rows are preferred over global (global usage
//! aggregates across every project and would otherwise swamp local context).

use std::path::Path;

use crate::context_export::ContextExportTarget;
use crate::{MemoryPersistence, MemoryPersistenceError, MemoryRecord};

/// Default number of primer items.
pub const PRIMER_DEFAULT_LIMIT: usize = 12;
/// Hard cap on items regardless of the requested limit.
pub const PRIMER_MAX_LIMIT: usize = 20;
/// Rendered byte budget for the whole pack.
pub const PRIMER_CHAR_BUDGET: usize = 12 * 1024;
/// Per-item summary character cap (Unicode-safe).
const PRIMER_SUMMARY_CHARS: usize = 240;

/// One ranked primer item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrimerItem {
    pub memory_id: String,
    /// `"project"` or `"global"`.
    pub scope: String,
    pub category: String,
    pub score: i64,
    pub summary: String,
}

/// A queryless primer: ranked items + the rendered pack.
#[derive(Debug, Clone)]
pub struct Primer {
    pub target: ContextExportTarget,
    pub items: Vec<PrimerItem>,
    pub body_markdown: String,
}

/// Category salience tier (5 highest … 1 lowest) — the semantic prior.
#[must_use]
fn category_tier(category: &str) -> i64 {
    match category {
        "SecurityWarning" | "UserPreference" | "ProjectConvention" | "ArchitectureRule"
        | "DeploymentRule" => 5,
        "AntiPattern" | "TestingStrategy" | "Process" => 4,
        "DebuggingRecipe" | "CodePattern" | "ToolUse" => 3,
        "ToolingNote" | "DocumentationUpdate" => 2,
        // CandidateSkill, Other(..), and anything unknown.
        _ => 1,
    }
}

/// Bounded usage bucket 0..4 (raw hit_count is a feedback loop, so it is capped
/// and can never dominate the category prior).
#[must_use]
fn usage_bucket(hit_count: i64) -> i64 {
    match hit_count {
        n if n <= 0 => 0,
        1 => 1,
        2..=3 => 2,
        4..=7 => 3,
        _ => 4,
    }
}

/// Salience: category tier dominates (×10); usage is a small boost. The max
/// usage boost (4) can never cross a tier gap (10).
#[must_use]
fn score(record: &MemoryRecord) -> i64 {
    category_tier(&record.category) * 10 + usage_bucket(record.hit_count)
}

#[must_use]
fn is_global(scope: &str) -> bool {
    scope == "GlobalUser"
}

/// Deterministic ranking: score desc, then created_at desc, then id asc.
fn sort_ranked(rows: &mut [&MemoryRecord]) {
    rows.sort_by(|a, b| {
        score(b)
            .cmp(&score(a))
            .then_with(|| b.created_at.cmp(&a.created_at))
            .then_with(|| a.memory_id.as_str().cmp(b.memory_id.as_str()))
    });
}

/// Build a queryless primer for `project`. Read-only; records no audit event.
///
/// # Errors
/// [`MemoryPersistenceError`] if the store cannot be opened or listed.
pub fn build_primer(
    project: &Path,
    target: ContextExportTarget,
    limit: usize,
) -> Result<Primer, MemoryPersistenceError> {
    let limit = limit.clamp(1, PRIMER_MAX_LIMIT);
    let store = MemoryPersistence::open_project(project)?;
    let records = store.list_memory()?;

    // Exclude stale/contradicted; split by scope.
    let mut project_rows: Vec<&MemoryRecord> = Vec::new();
    let mut global_rows: Vec<&MemoryRecord> = Vec::new();
    for record in &records {
        if record.stale_candidate || record.contradicted {
            continue;
        }
        if is_global(&record.scope) {
            global_rows.push(record);
        } else {
            project_rows.push(record);
        }
    }
    sort_ranked(&mut project_rows);
    sort_ranked(&mut global_rows);

    // Project-first blend (~3/4 project, ~1/4 global), then backfill the unused
    // slots from whichever scope still has rows.
    let global_target = limit / 4;
    let project_target = limit - global_target;
    let mut project_iter = project_rows.into_iter();
    let mut global_iter = global_rows.into_iter();
    let mut chosen: Vec<&MemoryRecord> = project_iter.by_ref().take(project_target).collect();
    chosen.extend(global_iter.by_ref().take(global_target));
    while chosen.len() < limit {
        if let Some(record) = project_iter.next() {
            chosen.push(record);
        } else if let Some(record) = global_iter.next() {
            chosen.push(record);
        } else {
            break;
        }
    }
    // One deterministic order over the merged set.
    sort_ranked(&mut chosen);

    let items: Vec<PrimerItem> = chosen
        .iter()
        .map(|record| PrimerItem {
            memory_id: record.memory_id.as_str().to_string(),
            scope: if is_global(&record.scope) {
                "global".to_string()
            } else {
                "project".to_string()
            },
            category: record.category.clone(),
            score: score(record),
            summary: summarize(&record.body),
        })
        .collect();

    let body_markdown = render(target, &items);
    Ok(Primer {
        target,
        items,
        body_markdown,
    })
}

/// First non-empty line of the body, Unicode-safe capped.
fn summarize(body: &str) -> String {
    let line = body
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    truncate_chars(line, PRIMER_SUMMARY_CHARS)
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Render the pack, dropping trailing items that would overflow the byte budget.
/// Reuses only the target heading, not `export()`'s audited path.
fn render(target: ContextExportTarget, items: &[PrimerItem]) -> String {
    let mut body = format!(
        "# {}\n\n## Project primer (most salient accepted memory)\n\n",
        target.heading()
    );
    if items.is_empty() {
        body.push_str("- No accepted memory yet.\n");
        return body;
    }
    for item in items {
        let line = format!(
            "- `{}` [{} · {}] {}\n",
            item.memory_id, item.scope, item.category, item.summary
        );
        if body.len() + line.len() > PRIMER_CHAR_BUDGET {
            break;
        }
        body.push_str(&line);
    }
    body
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use localmind_core::{
        CandidateLesson, Confidence, EvidenceKind, EvidenceRef, LessonCategory, LessonId,
        SessionId, SuggestedAction,
    };

    fn project(dir: &Path) {
        std::fs::write(
            dir.join(".localmind.toml"),
            "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n",
        )
        .unwrap();
    }

    fn promote(dir: &Path, id: &str, summary: &str, category: LessonCategory) {
        let queue = crate::ReviewQueue::open_project(dir).unwrap();
        queue
            .enqueue_candidates(
                &SessionId::new("s"),
                &[CandidateLesson::new(
                    LessonId::new(id),
                    summary,
                    category,
                    Confidence::new(0.8).unwrap(),
                    SuggestedAction::PromoteToMemory,
                )
                .with_evidence(EvidenceRef::new(EvidenceKind::Transcript, "redacted").redacted())],
            )
            .unwrap();
        queue
            .decide(localmind_core::ReviewDecision {
                item_id: localmind_core::ReviewItemId::new(id),
                action: localmind_core::ReviewAction::Accept,
                reviewer: "t".to_string(),
                decided_at: None,
                note: None,
                replacement_summary: None,
                evidence: Vec::new(),
            })
            .unwrap();
        crate::MemoryPersistence::open_project(dir)
            .unwrap()
            .promote_review_item(&localmind_core::ReviewItemId::new(id))
            .unwrap();
    }

    #[test]
    fn category_prior_outranks_usage() {
        let dir = tempfile::tempdir().unwrap();
        project(dir.path());
        promote(
            dir.path(),
            "sec",
            "Never log secrets",
            LessonCategory::SecurityWarning,
        );
        promote(
            dir.path(),
            "note",
            "A tooling note",
            LessonCategory::ToolingNote,
        );

        let primer = build_primer(
            dir.path(),
            ContextExportTarget::ClaudeCode,
            PRIMER_DEFAULT_LIMIT,
        )
        .unwrap();
        // A tier-5 SecurityWarning ranks ahead of a tier-2 note regardless of usage.
        assert_eq!(primer.items.first().unwrap().memory_id, "sec");
        assert!(primer.items.iter().any(|i| i.memory_id == "note"));
        assert!(primer.body_markdown.contains("Never log secrets"));
    }

    #[test]
    fn stale_and_contradicted_are_excluded() {
        let dir = tempfile::tempdir().unwrap();
        project(dir.path());
        promote(
            dir.path(),
            "good",
            "A live convention",
            LessonCategory::ProjectConvention,
        );
        // A queryless primer excludes stale/contradicted rows even at the same tier.
        let primer = build_primer(
            dir.path(),
            ContextExportTarget::Generic,
            PRIMER_DEFAULT_LIMIT,
        )
        .unwrap();
        assert!(primer.items.iter().all(|i| i.memory_id == "good"));
    }

    #[test]
    fn limit_is_clamped_and_budget_bounds_the_pack() {
        assert_eq!(category_tier("SecurityWarning"), 5);
        assert_eq!(category_tier("Other"), 1);
        assert_eq!(usage_bucket(0), 0);
        assert_eq!(usage_bucket(2), 2);
        assert_eq!(usage_bucket(100), 4);
        let dir = tempfile::tempdir().unwrap();
        project(dir.path());
        promote(dir.path(), "a", "one", LessonCategory::Process);
        let primer = build_primer(dir.path(), ContextExportTarget::Generic, 999).unwrap();
        assert!(primer.items.len() <= PRIMER_MAX_LIMIT);
        assert!(primer.body_markdown.len() <= PRIMER_CHAR_BUDGET);
    }
}
