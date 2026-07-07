use crate::{ProjectConfig, StoreConfigError};
use localmind_core::{
    CandidateLesson, MemoryEntryId, ReviewAction, ReviewDecision, ReviewItemId, ReviewState,
    SessionId,
};
use rusqlite::{params, types::Type, Connection, OptionalExtension};
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;
use time::OffsetDateTime;

pub const REVIEW_DB_FILE_NAME: &str = "localmind.sqlite";

#[derive(Clone, Debug, PartialEq)]
pub struct ReviewQueueItem {
    pub id: ReviewItemId,
    pub session_id: SessionId,
    pub candidate: CandidateLesson,
    pub state: ReviewState,
    pub reviewer_action: Option<String>,
    pub reviewer: Option<String>,
    pub note: Option<String>,
    pub replacement_summary: Option<String>,
    pub created_at: String,
    pub updated_at: Option<String>,
    /// How many times this candidate (or a trivial/near-duplicate variant) has
    /// been proposed. Starts at 1; dedup at enqueue bumps the survivor instead of
    /// inserting a new row.
    pub seen_count: i64,
    /// The memory a `Supersede` decision retires, carried from the decision to
    /// promotion. `None` for every other decision.
    pub supersede_target: Option<MemoryEntryId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewQueueSummary {
    pub pending: usize,
    pub accepted: usize,
    pub rejected: usize,
    pub edited: usize,
    pub deferred: usize,
}

pub struct ReviewQueue {
    connection: Connection,
}

impl ReviewQueue {
    pub fn open_project(project_root: impl AsRef<Path>) -> Result<Self, ReviewQueueError> {
        let config = ProjectConfig::discover(project_root).map_err(ReviewQueueError::Config)?;
        let state_dir = config.project_root.join(".localmind");
        fs::create_dir_all(&state_dir).map_err(|source| ReviewQueueError::CreateStateDir {
            path: state_dir.clone(),
            source,
        })?;
        let db_path = state_dir.join(REVIEW_DB_FILE_NAME);
        let connection = crate::schema::open_database(&db_path).map_err(|source| {
            ReviewQueueError::OpenDatabase {
                path: db_path,
                source,
            }
        })?;
        let queue = Self { connection };
        queue.migrate()?;
        Ok(queue)
    }

    pub fn migrate(&self) -> Result<(), ReviewQueueError> {
        crate::schema::migrate(&self.connection).map_err(ReviewQueueError::Schema)?;
        // Human-readable ledger row alongside PRAGMA user_version (kept for
        // databases and tools that already read schema_migrations).
        let applied_at = now_string();
        self.connection
            .execute(
                "INSERT OR IGNORE INTO schema_migrations(version, applied_at) VALUES(1, ?1)",
                params![applied_at],
            )
            .map_err(ReviewQueueError::Sqlite)?;
        Ok(())
    }

    pub fn enqueue_session_candidates(
        project_root: impl AsRef<Path>,
        session_id: &SessionId,
    ) -> Result<usize, ReviewQueueError> {
        let queue = Self::open_project(project_root.as_ref())?;
        let candidates_path = project_root
            .as_ref()
            .join(".localmind")
            .join("sessions")
            .join(session_id.as_str())
            .join("candidates.json");
        let candidates_json = fs::read_to_string(&candidates_path).map_err(|source| {
            ReviewQueueError::ReadCandidates {
                path: candidates_path.clone(),
                source,
            }
        })?;
        let candidates =
            serde_json::from_str::<Vec<CandidateLesson>>(&candidates_json).map_err(|source| {
                ReviewQueueError::ParseCandidates {
                    path: candidates_path,
                    source,
                }
            })?;
        queue.enqueue_candidates(session_id, &candidates)
    }

    /// Enqueue candidates with a deduplication ladder so the queue does not grow
    /// with restatements of the same lesson. For each candidate, in order:
    /// an exact canonical-hash match or a lexical near-duplicate of an existing
    /// *pending* candidate is **merged** — the survivor's `seen_count` is bumped
    /// and no new row is created (merge-not-drop); otherwise the candidate is
    /// inserted with its canonical hash. Returns the number of newly inserted
    /// rows (merges are not counted as new).
    pub fn enqueue_candidates(
        &self,
        session_id: &SessionId,
        candidates: &[CandidateLesson],
    ) -> Result<usize, ReviewQueueError> {
        let mut inserted = 0;
        // Existing pending candidates compete for the merge; later candidates in
        // this same batch also dedup against earlier ones once inserted.
        let mut pending = self.pending_dedup_keys()?;

        for candidate in candidates {
            let summary = candidate.summary();
            let hash = crate::dedup::canonical_hash(summary);
            if let Some(survivor) = find_duplicate(&pending, &hash, summary) {
                self.bump_seen_count(&survivor)?;
                continue;
            }

            let item_id = ReviewItemId::new(candidate.id.as_str());
            let candidate_json =
                serde_json::to_string(candidate).map_err(ReviewQueueError::SerializeCandidate)?;
            let changed = self
                .connection
                .execute(
                    r#"
                    INSERT OR IGNORE INTO review_items
                    (id, session_id, candidate_json, state, created_at, canonical_hash, seen_count)
                    VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1)
                    "#,
                    params![
                        item_id.as_str(),
                        session_id.as_str(),
                        candidate_json,
                        state_name(&ReviewState::Pending),
                        now_string(),
                        hash,
                    ],
                )
                .map_err(ReviewQueueError::Sqlite)?;
            inserted += changed;
            if changed > 0 {
                pending.push(DedupKey {
                    id: item_id.to_string(),
                    canonical_hash: hash,
                    summary: summary.to_string(),
                });
            }
        }

        Ok(inserted)
    }

    /// Delete every pending review candidate, returning how many rows were
    /// removed. Accepted/rejected/edited/merged decisions and all accepted-memory
    /// tables are untouched — this clears only the un-reviewed backlog.
    pub fn purge_pending(&self) -> Result<usize, ReviewQueueError> {
        self.connection
            .execute(
                "DELETE FROM review_items WHERE state = ?1",
                params![state_name(&ReviewState::Pending)],
            )
            .map_err(ReviewQueueError::Sqlite)
    }

    /// The dedup keys of every pending candidate, for merge detection at enqueue.
    fn pending_dedup_keys(&self) -> Result<Vec<DedupKey>, ReviewQueueError> {
        let mut statement = self
            .connection
            .prepare("SELECT id, canonical_hash, candidate_json FROM review_items WHERE state = ?1")
            .map_err(ReviewQueueError::Sqlite)?;
        let rows = statement
            .query_map(params![state_name(&ReviewState::Pending)], |row| {
                let id: String = row.get(0)?;
                let canonical_hash: Option<String> = row.get(1)?;
                let candidate_json: String = row.get(2)?;
                Ok((id, canonical_hash, candidate_json))
            })
            .map_err(ReviewQueueError::Sqlite)?;
        let mut keys = Vec::new();
        for row in rows {
            let (id, canonical_hash, candidate_json) = row.map_err(ReviewQueueError::Sqlite)?;
            let summary = serde_json::from_str::<CandidateLesson>(&candidate_json)
                .map(|candidate| candidate.summary().to_string())
                .unwrap_or_default();
            keys.push(DedupKey {
                id,
                // Backfill the hash for rows written before canonical_hash existed.
                canonical_hash: canonical_hash
                    .unwrap_or_else(|| crate::dedup::canonical_hash(&summary)),
                summary,
            });
        }
        Ok(keys)
    }

    /// Bump a survivor's `seen_count` when a duplicate is merged into it.
    fn bump_seen_count(&self, survivor: &str) -> Result<(), ReviewQueueError> {
        self.connection
            .execute(
                "UPDATE review_items SET seen_count = seen_count + 1, updated_at = ?2 WHERE id = ?1",
                params![survivor, now_string()],
            )
            .map_err(ReviewQueueError::Sqlite)?;
        Ok(())
    }

    pub fn list(&self) -> Result<Vec<ReviewQueueItem>, ReviewQueueError> {
        let mut statement = self
            .connection
            .prepare(
                r#"
                SELECT id, session_id, candidate_json, state, reviewer_action,
                       reviewer, note, replacement_summary, created_at, updated_at,
                       seen_count, supersede_target
                FROM review_items
                ORDER BY created_at, id
                "#,
            )
            .map_err(ReviewQueueError::Sqlite)?;
        let rows = statement
            .query_map([], row_to_item)
            .map_err(ReviewQueueError::Sqlite)?;
        let mut items = Vec::new();
        for row in rows {
            items.push(row.map_err(ReviewQueueError::Sqlite)?);
        }
        Ok(items)
    }

    pub fn get(&self, item_id: &ReviewItemId) -> Result<Option<ReviewQueueItem>, ReviewQueueError> {
        self.connection
            .query_row(
                r#"
                SELECT id, session_id, candidate_json, state, reviewer_action,
                       reviewer, note, replacement_summary, created_at, updated_at,
                       seen_count, supersede_target
                FROM review_items
                WHERE id = ?1
                "#,
                params![item_id.as_str()],
                row_to_item,
            )
            .optional()
            .map_err(ReviewQueueError::Sqlite)
    }

    pub fn replace_candidate(
        &self,
        item_id: &ReviewItemId,
        candidate: &CandidateLesson,
    ) -> Result<(), ReviewQueueError> {
        let candidate_json =
            serde_json::to_string(candidate).map_err(ReviewQueueError::SerializeCandidate)?;
        let changed = self
            .connection
            .execute(
                "UPDATE review_items SET candidate_json = ?2, updated_at = ?3 WHERE id = ?1",
                params![item_id.as_str(), candidate_json, now_string()],
            )
            .map_err(ReviewQueueError::Sqlite)?;
        if changed == 0 {
            return Err(ReviewQueueError::MissingItem {
                item_id: item_id.clone(),
            });
        }
        Ok(())
    }

    pub fn decide(&self, decision: ReviewDecision) -> Result<ReviewQueueItem, ReviewQueueError> {
        let state = localmind_review::state_after_decision(&decision);
        if matches!(decision.action, ReviewAction::Edit)
            && decision
                .replacement_summary
                .as_deref()
                .map(str::trim)
                .unwrap_or_default()
                .is_empty()
        {
            return Err(ReviewQueueError::InvalidEdit {
                item_id: decision.item_id,
            });
        }
        let supersede_target = match &decision.action {
            ReviewAction::Supersede(target) => Some(target.as_str().to_string()),
            _ => None,
        };

        let changed = self
            .connection
            .execute(
                r#"
                UPDATE review_items
                SET state = ?2,
                    reviewer_action = ?3,
                    reviewer = ?4,
                    note = ?5,
                    replacement_summary = ?6,
                    updated_at = ?7,
                    supersede_target = ?8
                WHERE id = ?1
                "#,
                params![
                    decision.item_id.as_str(),
                    state_name(&state),
                    action_name(&decision.action),
                    decision.reviewer,
                    decision.note,
                    decision.replacement_summary,
                    now_string(),
                    supersede_target,
                ],
            )
            .map_err(ReviewQueueError::Sqlite)?;

        if changed == 0 {
            return Err(ReviewQueueError::MissingItem {
                item_id: decision.item_id,
            });
        }

        self.get(&decision.item_id)?
            .ok_or(ReviewQueueError::MissingItem {
                item_id: decision.item_id,
            })
    }

    pub fn summary(&self) -> Result<ReviewQueueSummary, ReviewQueueError> {
        let items = self.list()?;
        Ok(ReviewQueueSummary {
            pending: items
                .iter()
                .filter(|item| item.state == ReviewState::Pending)
                .count(),
            accepted: items
                .iter()
                .filter(|item| item.state == ReviewState::Accepted)
                .count(),
            rejected: items
                .iter()
                .filter(|item| item.state == ReviewState::Rejected)
                .count(),
            edited: items
                .iter()
                .filter(|item| item.state == ReviewState::Edited)
                .count(),
            deferred: items
                .iter()
                .filter(|item| item.state == ReviewState::Deferred)
                .count(),
        })
    }
}

fn row_to_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<ReviewQueueItem> {
    let id: String = row.get(0)?;
    let session_id: String = row.get(1)?;
    let candidate_json: String = row.get(2)?;
    let state: String = row.get(3)?;
    let candidate = serde_json::from_str::<CandidateLesson>(&candidate_json).map_err(|source| {
        rusqlite::Error::FromSqlConversionFailure(2, Type::Text, Box::new(source))
    })?;
    Ok(ReviewQueueItem {
        id: ReviewItemId::new(id),
        session_id: SessionId::new(session_id),
        candidate,
        state: parse_state(&state),
        reviewer_action: row.get(4)?,
        reviewer: row.get(5)?,
        note: row.get(6)?,
        replacement_summary: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
        seen_count: row.get(10)?,
        supersede_target: row.get::<_, Option<String>>(11)?.map(MemoryEntryId::new),
    })
}

/// A pending candidate's dedup keys, loaded once per enqueue batch.
struct DedupKey {
    id: String,
    canonical_hash: String,
    summary: String,
}

/// The id of an existing pending candidate that `summary`/`hash` duplicates —
/// an exact canonical match or a lexical near-duplicate — or `None` when novel.
fn find_duplicate(pending: &[DedupKey], hash: &str, summary: &str) -> Option<String> {
    pending
        .iter()
        .find(|key| {
            key.canonical_hash == hash || crate::dedup::is_near_duplicate(&key.summary, summary)
        })
        .map(|key| key.id.clone())
}

fn parse_state(value: &str) -> ReviewState {
    match value {
        "accepted" => ReviewState::Accepted,
        "rejected" => ReviewState::Rejected,
        "edited" => ReviewState::Edited,
        "merged" => ReviewState::Merged,
        "deferred" => ReviewState::Deferred,
        _ => ReviewState::Pending,
    }
}

fn state_name(state: &ReviewState) -> &'static str {
    match state {
        ReviewState::Pending => "pending",
        ReviewState::Accepted => "accepted",
        ReviewState::Rejected => "rejected",
        ReviewState::Edited => "edited",
        ReviewState::Merged => "merged",
        ReviewState::Deferred => "deferred",
    }
}

fn action_name(action: &ReviewAction) -> &'static str {
    match action {
        ReviewAction::Accept => "accept",
        ReviewAction::Reject => "reject",
        ReviewAction::Edit => "edit",
        ReviewAction::MergeInto(_) => "merge",
        ReviewAction::MarkTemporary => "defer",
        ReviewAction::ConvertToSkill => "convert_to_skill",
        ReviewAction::IgnoreSimilar => "ignore_similar",
        ReviewAction::Supersede(_) => "supersede",
    }
}

fn now_string() -> String {
    OffsetDateTime::now_utc().to_string()
}

#[derive(Debug, Error)]
pub enum ReviewQueueError {
    #[error(transparent)]
    Config(#[from] StoreConfigError),
    #[error(transparent)]
    Schema(#[from] crate::schema::SchemaError),
    #[error("failed to create LocalMind state directory {path:?}: {source}")]
    CreateStateDir {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to open review queue database {path:?}: {source}")]
    OpenDatabase {
        path: PathBuf,
        source: rusqlite::Error,
    },
    #[error("failed to read candidate lessons {path:?}: {source}")]
    ReadCandidates {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse candidate lessons {path:?}: {source}")]
    ParseCandidates {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("failed to serialize candidate lesson: {0}")]
    SerializeCandidate(serde_json::Error),
    #[error("review item does not exist: {item_id}")]
    MissingItem { item_id: ReviewItemId },
    #[error("edit decision for {item_id} requires non-empty replacement text")]
    InvalidEdit { item_id: ReviewItemId },
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use localmind_core::{Confidence, LessonCategory, SuggestedAction};

    fn open(root: &std::path::Path) -> ReviewQueue {
        std::fs::write(root.join(".localmind.toml"), "[learning]\nenabled = true\n").unwrap();
        ReviewQueue::open_project(root).unwrap()
    }

    fn candidate(id: &str, summary: &str) -> CandidateLesson {
        CandidateLesson::new(
            localmind_core::LessonId::new(id),
            summary,
            LessonCategory::ProjectConvention,
            Confidence::new(0.6).unwrap(),
            SuggestedAction::PromoteToMemory,
        )
    }

    fn session() -> SessionId {
        SessionId::new("test-session")
    }

    #[test]
    fn trivial_variants_collapse_to_a_single_row() {
        let dir = tempfile::tempdir().unwrap();
        let queue = open(dir.path());

        // Same statement, differing only in case, spacing, and trailing
        // punctuation, with distinct ids — content dedup, not id dedup.
        let inserted = queue
            .enqueue_candidates(
                &session(),
                &[
                    candidate("a", "Use ripgrep over grep."),
                    candidate("b", "use  ripgrep   over grep"),
                ],
            )
            .unwrap();

        assert_eq!(inserted, 1, "trivial variants must enqueue once");
        let items = queue.list().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].seen_count, 2, "the survivor counts the variant");
    }

    #[test]
    fn a_reworded_near_duplicate_merges() {
        let dir = tempfile::tempdir().unwrap();
        let queue = open(dir.path());
        queue
            .enqueue_candidates(
                &session(),
                &[candidate(
                    "a",
                    "run the integration suite after every exporter change",
                )],
            )
            .unwrap();

        // A reworded restatement enqueued later merges into the survivor.
        let inserted = queue
            .enqueue_candidates(
                &session(),
                &[candidate(
                    "b",
                    "after an exporter change, run the integration suite",
                )],
            )
            .unwrap();

        assert_eq!(inserted, 0, "a near-duplicate must not create a new row");
        let items = queue.list().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].seen_count, 2);
    }

    #[test]
    fn a_distinct_lesson_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let queue = open(dir.path());
        let inserted = queue
            .enqueue_candidates(
                &session(),
                &[
                    candidate("a", "run the integration suite after exporter changes"),
                    candidate("b", "prefer ripgrep over grep when searching"),
                ],
            )
            .unwrap();
        assert_eq!(inserted, 2, "distinct lessons are both kept");
        assert_eq!(queue.list().unwrap().len(), 2);
    }

    #[test]
    fn a_repeat_proposal_bumps_seen_count_without_a_new_row() {
        let dir = tempfile::tempdir().unwrap();
        let queue = open(dir.path());
        let lesson = "redact secrets before persisting them";
        for id in ["a", "b", "c"] {
            queue
                .enqueue_candidates(&session(), &[candidate(id, lesson)])
                .unwrap();
        }
        let items = queue.list().unwrap();
        assert_eq!(items.len(), 1, "repeats merge into one row");
        assert_eq!(items[0].seen_count, 3);
    }

    #[test]
    fn purge_pending_clears_pending_but_keeps_decided_items() {
        let dir = tempfile::tempdir().unwrap();
        let queue = open(dir.path());
        queue
            .enqueue_candidates(
                &session(),
                &[
                    candidate("keep", "accept me"),
                    candidate("drop", "leave me pending"),
                ],
            )
            .unwrap();
        // Accept one so it is no longer pending.
        queue
            .decide(ReviewDecision {
                item_id: ReviewItemId::new("keep"),
                action: ReviewAction::Accept,
                reviewer: "tester".to_string(),
                decided_at: None,
                note: None,
                replacement_summary: None,
                evidence: Vec::new(),
            })
            .unwrap();

        let removed = queue.purge_pending().unwrap();

        assert_eq!(removed, 1, "only the single pending row is purged");
        let states: Vec<_> = queue.list().unwrap().into_iter().map(|i| i.state).collect();
        assert_eq!(
            states,
            vec![ReviewState::Accepted],
            "the decided item survives"
        );
    }
}
