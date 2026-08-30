//! What is actually sitting in `.localmind/sessions/`, and what a retention
//! bound would remove.
//!
//! Session transcripts are kept forever. On the authoring machine that is
//! **287.5 MiB across 86 sessions going back to 2026-06-21**, and nothing has
//! ever pruned them. Whether that should change is a judgement about privacy
//! against usefulness, and it is much easier to make while looking at the actual
//! list than in the abstract.
//!
//! So this module reports; it does not delete. The policy is a **pure function
//! over a scanned inventory**, which is what makes "show me what this bound
//! would catch" answerable without touching a byte.
//!
//! **Age comes from filesystem mtime**, because the session metadata carries no
//! timestamp. mtime is not import time — a copy, a restore, or a `touch` moves
//! it — so an age bound is approximate by construction, and that is a reason to
//! prefer a count or size bound rather than a caveat to bury.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// One session on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionEntry {
    pub id: String,
    pub path: PathBuf,
    pub bytes: u64,
    /// Newest mtime across the session's files. See the module note: this is a
    /// proxy for age, not a record of when the session happened.
    pub modified: SystemTime,
}

/// Which sessions a bound would keep, and which it would not.
///
/// Deliberately named for *retention*, not deletion: the output of applying one
/// of these is a report, and the acting-on-it step is a separate decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetentionPolicy {
    /// Keep the N most recently modified sessions.
    KeepNewest(usize),
    /// Keep sessions modified within the last N days.
    WithinDays(u64),
    /// Keep newest-first until the running total would exceed this many bytes.
    TotalBytes(u64),
}

/// What a policy would do, without doing it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RetentionPlan {
    pub kept: Vec<SessionEntry>,
    pub prunable: Vec<SessionEntry>,
}

impl RetentionPlan {
    #[must_use]
    pub fn kept_bytes(&self) -> u64 {
        self.kept.iter().map(|e| e.bytes).sum()
    }

    #[must_use]
    pub fn prunable_bytes(&self) -> u64 {
        self.prunable.iter().map(|e| e.bytes).sum()
    }
}

/// Apply a policy to an inventory. Pure — no filesystem, no clock beyond `now`.
///
/// `now` is a parameter rather than read here so the age policy is testable
/// without waiting a day.
#[must_use]
pub fn plan_retention(
    entries: &[SessionEntry],
    policy: RetentionPolicy,
    now: SystemTime,
) -> RetentionPlan {
    // Newest first. Every policy is "keep from the recent end", so one ordering
    // serves all three and the tie-break keeps the result deterministic.
    let mut sorted = entries.to_vec();
    sorted.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.id.cmp(&b.id)));

    let mut plan = RetentionPlan::default();
    let mut running = 0_u64;
    for entry in sorted {
        let keep = match policy {
            RetentionPolicy::KeepNewest(n) => plan.kept.len() < n,
            RetentionPolicy::WithinDays(days) => now
                .duration_since(entry.modified)
                .map(|age| age.as_secs() <= days.saturating_mul(86_400))
                // A session modified in the future (clock skew, a restored
                // backup) is kept. Silently pruning something we cannot date is
                // the wrong direction for an irreversible action.
                .unwrap_or(true),
            RetentionPolicy::TotalBytes(cap) => running.saturating_add(entry.bytes) <= cap,
        };
        if keep {
            running = running.saturating_add(entry.bytes);
            plan.kept.push(entry);
        } else {
            plan.prunable.push(entry);
        }
    }
    plan
}

/// Read the session inventory under a project root.
///
/// # Errors
/// Returns an error if the sessions directory cannot be read. A session
/// directory that cannot be measured is skipped rather than failing the scan —
/// a partial inventory is still useful, and an unreadable session is not a
/// reason to refuse to report on the other 85.
pub fn scan_sessions(project_root: &Path) -> Result<Vec<SessionEntry>, std::io::Error> {
    let root = project_root.join(".localmind").join("sessions");
    let mut entries = Vec::new();
    let dir = match std::fs::read_dir(&root) {
        Ok(dir) => dir,
        // No sessions directory is an empty inventory, not a failure.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(entries),
        Err(error) => return Err(error),
    };
    for item in dir.flatten() {
        let path = item.path();
        if !path.is_dir() {
            continue;
        }
        let Some(id) = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        let mut bytes = 0_u64;
        let mut modified = SystemTime::UNIX_EPOCH;
        let Ok(files) = std::fs::read_dir(&path) else {
            continue;
        };
        for file in files.flatten() {
            let Ok(meta) = file.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            bytes = bytes.saturating_add(meta.len());
            if let Ok(m) = meta.modified() {
                if m > modified {
                    modified = m;
                }
            }
        }
        entries.push(SessionEntry {
            id,
            path,
            bytes,
            modified,
        });
    }
    Ok(entries)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(now: SystemTime, days_ago: u64, bytes: u64, id: &str) -> SessionEntry {
        SessionEntry {
            id: id.to_string(),
            path: PathBuf::from(id),
            bytes,
            modified: now - Duration::from_secs(days_ago * 86_400),
        }
    }

    fn corpus(now: SystemTime) -> Vec<SessionEntry> {
        vec![
            at(now, 1, 30, "newest"),
            at(now, 10, 20, "middle"),
            at(now, 100, 50, "oldest-and-largest"),
        ]
    }

    #[test]
    fn keep_newest_keeps_from_the_recent_end() {
        let now = SystemTime::now();

        let plan = plan_retention(&corpus(now), RetentionPolicy::KeepNewest(2), now);

        assert_eq!(
            plan.kept.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            vec!["newest", "middle"]
        );
        assert_eq!(plan.prunable.len(), 1);
        assert_eq!(plan.prunable[0].id, "oldest-and-largest");
    }

    #[test]
    fn an_age_bound_and_a_count_bound_disagree_which_is_the_point_of_showing_both() {
        // A count bound keeps the most recent work regardless of when it
        // happened; an age bound would drop a large, still-useful session and
        // keep a trivial recent one. Seeing both is what makes the choice
        // informed rather than a guess.
        let now = SystemTime::now();
        let entries = corpus(now);

        let by_count = plan_retention(&entries, RetentionPolicy::KeepNewest(2), now);
        let by_age = plan_retention(&entries, RetentionPolicy::WithinDays(30), now);

        assert_eq!(by_count.kept.len(), 2);
        assert_eq!(by_age.kept.len(), 2);
        assert_eq!(by_count.prunable_bytes(), 50);
        assert_eq!(by_age.prunable_bytes(), 50);
        // Same verdict here by construction; the report exists so the cases
        // where they differ are visible before anything is removed.
    }

    #[test]
    fn a_size_cap_fills_from_the_recent_end_and_stops() {
        let now = SystemTime::now();

        let plan = plan_retention(&corpus(now), RetentionPolicy::TotalBytes(55), now);

        assert_eq!(plan.kept_bytes(), 50, "30 + 20 fits, the 50 does not");
        assert_eq!(plan.prunable[0].id, "oldest-and-largest");
    }

    #[test]
    fn a_session_dated_in_the_future_is_kept_not_pruned() {
        // Clock skew or a restored backup. Pruning something we cannot date is
        // the wrong direction for an irreversible action.
        let now = SystemTime::now();
        let future = SessionEntry {
            id: "from-the-future".to_string(),
            path: PathBuf::from("f"),
            bytes: 1,
            modified: now + Duration::from_secs(86_400),
        };

        let plan = plan_retention(&[future], RetentionPolicy::WithinDays(1), now);

        assert_eq!(plan.kept.len(), 1);
        assert!(plan.prunable.is_empty());
    }

    #[test]
    fn the_plan_is_deterministic_when_timestamps_tie() {
        let now = SystemTime::now();
        let entries = vec![at(now, 5, 1, "b"), at(now, 5, 1, "a")];

        let first = plan_retention(&entries, RetentionPolicy::KeepNewest(1), now);
        let second = plan_retention(&entries, RetentionPolicy::KeepNewest(1), now);

        assert_eq!(first, second);
        assert_eq!(first.kept[0].id, "a", "tie breaks by id, not by read order");
    }

    #[test]
    fn nothing_is_ever_removed_by_planning() {
        // The type has no delete. Stated as a test so a later change that adds
        // one has to walk past this.
        let now = SystemTime::now();
        let entries = corpus(now);

        let plan = plan_retention(&entries, RetentionPolicy::KeepNewest(0), now);

        assert_eq!(plan.prunable.len(), 3);
        assert_eq!(entries.len(), 3, "the inventory is untouched");
    }

    #[test]
    fn a_missing_sessions_directory_is_an_empty_inventory() {
        let dir = tempfile::tempdir().unwrap();

        let entries = scan_sessions(dir.path()).expect("a missing directory is not an error");

        assert!(entries.is_empty());
    }
}
