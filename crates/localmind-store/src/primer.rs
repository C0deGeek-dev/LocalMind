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

/// A queryless primer: ranked items + the rendered pack. `items` is exactly the
/// set rendered into `body_markdown` (both are budgeted together).
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
    let store = MemoryPersistence::open_project(project)?;
    let records = store.list_memory()?;
    Ok(rank_and_render(&records, target, limit))
}

/// Pure ranking + rendering over already-loaded records — the full contract
/// (exclusions, project-first blend + backfill, clamp, budget) with no store, so
/// it is exhaustively testable.
fn rank_and_render(records: &[MemoryRecord], target: ContextExportTarget, limit: usize) -> Primer {
    let limit = limit.clamp(1, PRIMER_MAX_LIMIT);

    // Exclude stale/contradicted; split by scope.
    let mut project_rows: Vec<&MemoryRecord> = Vec::new();
    let mut global_rows: Vec<&MemoryRecord> = Vec::new();
    for record in records {
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

    // Build the items and the text pack together, applying the byte budget once,
    // so the structured items are exactly the memories the text pack renders (no
    // phantom items when long/multibyte summaries hit the budget).
    let mut body = format!(
        "# {}\n\n## Project primer (most salient accepted memory)\n\n",
        target.heading()
    );
    let mut items: Vec<PrimerItem> = Vec::new();
    for record in chosen {
        let item = PrimerItem {
            memory_id: record.memory_id.as_str().to_string(),
            scope: if is_global(&record.scope) {
                "global".to_string()
            } else {
                "project".to_string()
            },
            category: record.category.clone(),
            score: score(record),
            summary: summarize(&record.body),
        };
        let line = format!(
            "- `{}` [{} · {}] {}\n",
            item.memory_id, item.scope, item.category, item.summary
        );
        if body.len() + line.len() > PRIMER_CHAR_BUDGET {
            break;
        }
        body.push_str(&line);
        items.push(item);
    }
    if items.is_empty() {
        body.push_str("- No accepted memory yet.\n");
    }

    Primer {
        target,
        items,
        body_markdown: body,
    }
}

/// The first Markdown paragraph of the body as one whitespace-normalized line,
/// Unicode-safe truncated. Memory bodies are hard-wrapped, so a single physical
/// line would be an incomplete fragment.
fn summarize(body: &str) -> String {
    let mut paragraph = String::new();
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if paragraph.is_empty() {
                continue; // skip leading blank lines
            }
            break; // first blank line ends the first paragraph
        }
        if !paragraph.is_empty() {
            paragraph.push(' ');
        }
        paragraph.push_str(trimmed);
    }
    let normalized = paragraph.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_chars(&normalized, PRIMER_SUMMARY_CHARS)
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use localmind_core::MemoryEntryId;

    /// Synthetic record with full control over every ranking input.
    #[allow(clippy::too_many_arguments)]
    fn rec(
        id: &str,
        category: &str,
        hit_count: i64,
        scope: &str,
        stale: bool,
        contradicted: bool,
        created_at: &str,
        body: &str,
    ) -> MemoryRecord {
        MemoryRecord {
            memory_id: MemoryEntryId::new(id),
            path: std::path::PathBuf::new(),
            scope: scope.to_string(),
            category: category.to_string(),
            status: "active".to_string(),
            body: body.to_string(),
            hit_count,
            last_used_at: None,
            stale_candidate: stale,
            contradicted,
            language: None,
            created_at: Some(created_at.to_string()),
        }
    }

    fn ids(primer: &Primer) -> Vec<&str> {
        primer.items.iter().map(|i| i.memory_id.as_str()).collect()
    }

    #[test]
    fn buckets_and_tiers() {
        assert_eq!(category_tier("SecurityWarning"), 5);
        assert_eq!(category_tier("Process"), 4);
        assert_eq!(category_tier("CodePattern"), 3);
        assert_eq!(category_tier("ToolingNote"), 2);
        assert_eq!(category_tier("CandidateSkill"), 1);
        assert_eq!(category_tier("Other(\"x\")"), 1);
        assert_eq!(usage_bucket(0), 0);
        assert_eq!(usage_bucket(1), 1);
        assert_eq!(usage_bucket(3), 2);
        assert_eq!(usage_bucket(7), 3);
        assert_eq!(usage_bucket(1000), 4);
    }

    #[test]
    fn category_prior_beats_popularity_and_usage_only_breaks_ties() {
        // A tier-2 note with huge usage still ranks below a never-used tier-5.
        let records = vec![
            rec(
                "note",
                "ToolingNote",
                999,
                "project",
                false,
                false,
                "2026",
                "note",
            ),
            rec(
                "dec",
                "ArchitectureRule",
                0,
                "project",
                false,
                false,
                "2026",
                "decision",
            ),
            // Two same-tier rows: higher usage wins, then recency, then id.
            rec(
                "conv_old_hi",
                "ProjectConvention",
                5,
                "project",
                false,
                false,
                "2026-01",
                "a",
            ),
            rec(
                "conv_new_lo",
                "ProjectConvention",
                0,
                "project",
                false,
                false,
                "2026-09",
                "b",
            ),
        ];
        let primer = rank_and_render(&records, ContextExportTarget::Generic, 12);
        assert_eq!(primer.items[0].memory_id, "conv_old_hi"); // tier5 + usage bucket 3
        assert_eq!(primer.items[1].memory_id, "conv_new_lo"); // tier5 + usage 0, newer
        assert_eq!(primer.items[2].memory_id, "dec"); // tier5 arch, usage 0, older key
        assert_eq!(*ids(&primer).last().unwrap(), "note"); // tier2 last
    }

    #[test]
    fn stale_and_contradicted_are_excluded() {
        let records = vec![
            rec(
                "good",
                "ProjectConvention",
                0,
                "project",
                false,
                false,
                "2026",
                "good",
            ),
            rec(
                "stale",
                "SecurityWarning",
                9,
                "project",
                true,
                false,
                "2026",
                "stale",
            ),
            rec(
                "conflict",
                "SecurityWarning",
                9,
                "project",
                false,
                true,
                "2026",
                "conflict",
            ),
        ];
        let primer = rank_and_render(&records, ContextExportTarget::Generic, 12);
        assert_eq!(ids(&primer), vec!["good"], "stale/contradicted dropped");
    }

    #[test]
    fn project_first_blend_and_backfill() {
        // 10 project + 10 global, all same tier; limit 12 → 9 project + 3 global.
        let mut records = Vec::new();
        for n in 0..10 {
            records.push(rec(
                &format!("p{n:02}"),
                "Process",
                0,
                "project",
                false,
                false,
                "2026",
                "p",
            ));
            records.push(rec(
                &format!("g{n:02}"),
                "Process",
                0,
                "GlobalUser",
                false,
                false,
                "2026",
                "g",
            ));
        }
        let primer = rank_and_render(&records, ContextExportTarget::Generic, 12);
        assert_eq!(primer.items.len(), 12);
        let project = primer.items.iter().filter(|i| i.scope == "project").count();
        let global = primer.items.iter().filter(|i| i.scope == "global").count();
        assert_eq!((project, global), (9, 3), "project-first 9/3 quota");

        // Backfill: only 2 project rows but 10 global → fill remaining from global.
        let mut few = vec![
            rec("p0", "Process", 0, "project", false, false, "2026", "p"),
            rec("p1", "Process", 0, "project", false, false, "2026", "p"),
        ];
        for n in 0..10 {
            few.push(rec(
                &format!("g{n}"),
                "Process",
                0,
                "GlobalUser",
                false,
                false,
                "2026",
                "g",
            ));
        }
        let primer = rank_and_render(&few, ContextExportTarget::Generic, 12);
        assert_eq!(primer.items.len(), 12);
        assert_eq!(
            primer.items.iter().filter(|i| i.scope == "project").count(),
            2
        );
    }

    #[test]
    fn limit_is_clamped_to_the_hard_max() {
        let records: Vec<MemoryRecord> = (0..40)
            .map(|n| {
                rec(
                    &format!("m{n:02}"),
                    "Process",
                    0,
                    "project",
                    false,
                    false,
                    "2026",
                    "x",
                )
            })
            .collect();
        let primer = rank_and_render(&records, ContextExportTarget::Generic, 999);
        assert_eq!(primer.items.len(), PRIMER_MAX_LIMIT);
    }

    #[test]
    fn budget_keeps_items_and_pack_identical_with_long_multibyte_summaries() {
        // 20 rows whose multibyte summaries together exceed the 12 KiB budget.
        let big = "各".repeat(PRIMER_SUMMARY_CHARS); // 3 bytes/char, truncated to 240 chars
        let records: Vec<MemoryRecord> = (0..PRIMER_MAX_LIMIT)
            .map(|n| {
                rec(
                    &format!("m{n:02}"),
                    "Process",
                    0,
                    "project",
                    false,
                    false,
                    "2026",
                    &big,
                )
            })
            .collect();
        let primer = rank_and_render(&records, ContextExportTarget::Generic, PRIMER_MAX_LIMIT);
        assert!(primer.body_markdown.len() <= PRIMER_CHAR_BUDGET);
        // Every structured item must appear in the text pack — no phantom items.
        assert!(primer.items.len() < PRIMER_MAX_LIMIT, "budget dropped some");
        for item in &primer.items {
            assert!(
                primer
                    .body_markdown
                    .contains(&format!("`{}`", item.memory_id)),
                "structured item {} missing from the pack",
                item.memory_id
            );
        }
        // And the pack renders no more item lines than there are structured items.
        let rendered = primer.body_markdown.matches("\n- `").count();
        assert_eq!(rendered, primer.items.len());
    }

    #[test]
    fn summarize_joins_a_hard_wrapped_paragraph() {
        let body = "The release train cuts all five repos\nto one VERSION and tag.\n\nA second paragraph is ignored.";
        assert_eq!(
            summarize(body),
            "The release train cuts all five repos to one VERSION and tag."
        );
    }

    #[test]
    fn empty_store_renders_a_placeholder() {
        let primer = rank_and_render(&[], ContextExportTarget::Generic, 12);
        assert!(primer.items.is_empty());
        assert!(primer.body_markdown.contains("No accepted memory yet"));
    }
}
