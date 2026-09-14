//! Which lab output may be removed, and the boundary it may never cross.
//!
//! The lab produces bulky output — check logs, per-trial output, verifier
//! output — and machine-generated uplift trials create sessions of their own.
//! All of it is removable after [`LAB_LOG_RETENTION_DAYS`]. The user's own
//! sessions never are.
//!
//! The dangerous part is not the age rule. A trial session and a real session
//! are the same kind of file, so any rule that selects by name, kind, or age
//! alone is one wrong path away from deleting real history. Eligibility is
//! therefore decided by **location**: an entry may be planned for removal only
//! if it sits strictly inside the staging root the lab created for its runs.
//! Everything else is refused and reported, never silently skipped.
//!
//! Like [`plan_retention`], this plans and does not delete. The runner that
//! produced the output acts on the plan. It must resolve symlinks and junctions
//! before acting: this function reasons about paths as given and cannot see
//! where a link points.

use crate::{plan_retention, RetentionPlan, RetentionPolicy, SessionEntry};
use localmind_core::LAB_LOG_RETENTION_DAYS;
use std::path::{Component, Path};
use std::time::SystemTime;

/// What a lab sweep would remove, keep, and refuse to touch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LabSweepPlan {
    /// Inside the staging root, split by the retention bound.
    pub retention: RetentionPlan,
    /// Outside the staging root, or not safely comparable with it. Never
    /// removable, whatever its age.
    pub refused: Vec<SessionEntry>,
}

/// Plan a sweep of lab output under `staging_root` at `now`.
///
/// An unusable root — relative, or containing a `..` component — refuses every
/// entry: with no trustworthy boundary there is nothing it is safe to remove.
#[must_use]
pub fn plan_lab_sweep(
    staging_root: &Path,
    entries: &[SessionEntry],
    now: SystemTime,
) -> LabSweepPlan {
    if !is_plain_absolute(staging_root) {
        return LabSweepPlan {
            retention: RetentionPlan::default(),
            refused: entries.to_vec(),
        };
    }

    let (inside, refused): (Vec<SessionEntry>, Vec<SessionEntry>) = entries
        .iter()
        .cloned()
        .partition(|entry| is_strictly_inside(staging_root, &entry.path));

    LabSweepPlan {
        retention: plan_retention(
            &inside,
            RetentionPolicy::WithinDays(LAB_LOG_RETENTION_DAYS),
            now,
        ),
        refused,
    }
}

/// Absolute, and free of `.`/`..` components that would let a lexical prefix
/// check be walked back out of.
fn is_plain_absolute(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|component| !matches!(component, Component::ParentDir | Component::CurDir))
}

/// `path` is below `root` and is not `root` itself. Component-wise, so
/// `/lab-runs-old` is not inside `/lab-runs`.
fn is_strictly_inside(root: &Path, path: &Path) -> bool {
    is_plain_absolute(path)
        && path.starts_with(root)
        && path.components().count() > root.components().count()
}

#[cfg(test)]
mod tests {
    use super::{plan_lab_sweep, LAB_LOG_RETENTION_DAYS};
    use crate::SessionEntry;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime};

    const DAY: u64 = 86_400;

    fn root() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(r"C:\work\repo\.localpilot\lab")
        } else {
            PathBuf::from("/work/repo/.localpilot/lab")
        }
    }

    fn at(path: PathBuf, age_days: u64, now: SystemTime) -> SessionEntry {
        SessionEntry {
            id: path.to_string_lossy().into_owned(),
            path,
            bytes: 1_024,
            modified: now - Duration::from_secs(age_days * DAY),
        }
    }

    fn ids(entries: &[SessionEntry]) -> Vec<String> {
        entries.iter().map(|entry| entry.id.clone()).collect()
    }

    #[test]
    fn old_lab_output_inside_the_root_is_removable_and_recent_output_is_kept() {
        let now = SystemTime::now();
        let old = at(
            root().join("run-1").join("check.log"),
            LAB_LOG_RETENTION_DAYS + 1,
            now,
        );
        let recent = at(root().join("run-2").join("check.log"), 2, now);
        let trial_session = at(
            root().join("run-1").join("sessions").join("trial-7"),
            45,
            now,
        );

        let plan = plan_lab_sweep(
            &root(),
            &[old.clone(), recent.clone(), trial_session.clone()],
            now,
        );

        assert_eq!(ids(&plan.retention.prunable), ids(&[old, trial_session]));
        assert_eq!(ids(&plan.retention.kept), ids(&[recent]));
        assert!(plan.refused.is_empty());
    }

    #[test]
    fn a_real_session_outside_the_root_survives_however_old_it_is() {
        let now = SystemTime::now();
        let workspace = root()
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let real = at(
            workspace
                .join(".localmind")
                .join("sessions")
                .join("session-1"),
            400,
            now,
        );

        let plan = plan_lab_sweep(&root(), &[real.clone()], now);

        assert!(
            plan.retention.prunable.is_empty(),
            "age is never enough on its own"
        );
        assert_eq!(ids(&plan.refused), ids(&[real]));
    }

    #[test]
    fn a_sibling_that_merely_shares_the_prefix_is_not_inside() {
        let now = SystemTime::now();
        let mut sibling_root = root().into_os_string();
        sibling_root.push("-archive");
        let sibling = at(PathBuf::from(sibling_root).join("check.log"), 90, now);

        let plan = plan_lab_sweep(&root(), &[sibling.clone()], now);
        assert_eq!(ids(&plan.refused), ids(&[sibling]));
    }

    #[test]
    fn a_path_that_climbs_back_out_is_refused() {
        let now = SystemTime::now();
        let escaping = at(
            root()
                .join("run-1")
                .join("..")
                .join("..")
                .join("..")
                .join(".localmind")
                .join("sessions"),
            90,
            now,
        );

        let plan = plan_lab_sweep(&root(), &[escaping.clone()], now);
        assert_eq!(ids(&plan.refused), ids(&[escaping]));
    }

    #[test]
    fn the_root_itself_is_never_a_removable_entry() {
        let now = SystemTime::now();
        let itself = at(root(), 90, now);

        let plan = plan_lab_sweep(&root(), &[itself.clone()], now);
        assert_eq!(ids(&plan.refused), ids(&[itself]));
    }

    #[test]
    fn an_untrustworthy_root_refuses_everything() {
        let now = SystemTime::now();
        let inside = at(root().join("run-1").join("check.log"), 90, now);

        for bad_root in [PathBuf::from("relative/lab"), root().join("..").join("lab")] {
            let plan = plan_lab_sweep(&bad_root, &[inside.clone()], now);
            assert!(plan.retention.prunable.is_empty());
            assert_eq!(
                plan.refused.len(),
                1,
                "{} is not a boundary",
                bad_root.display()
            );
        }
    }
}
