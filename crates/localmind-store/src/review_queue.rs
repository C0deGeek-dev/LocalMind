use crate::{ProjectConfig, StoreConfigError};
use localmind_core::{
    CandidateLesson, ReviewAction, ReviewDecision, ReviewItemId, ReviewState, SessionId,
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
        let connection =
            Connection::open(&db_path).map_err(|source| ReviewQueueError::OpenDatabase {
                path: db_path,
                source,
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

    pub fn enqueue_candidates(
        &self,
        session_id: &SessionId,
        candidates: &[CandidateLesson],
    ) -> Result<usize, ReviewQueueError> {
        let mut inserted = 0;
        let created_at = now_string();

        for candidate in candidates {
            let item_id = ReviewItemId::new(candidate.id.as_str());
            let candidate_json =
                serde_json::to_string(candidate).map_err(ReviewQueueError::SerializeCandidate)?;
            let changed = self
                .connection
                .execute(
                    r#"
                    INSERT OR IGNORE INTO review_items
                    (id, session_id, candidate_json, state, created_at)
                    VALUES (?1, ?2, ?3, ?4, ?5)
                    "#,
                    params![
                        item_id.as_str(),
                        session_id.as_str(),
                        candidate_json,
                        state_name(&ReviewState::Pending),
                        created_at
                    ],
                )
                .map_err(ReviewQueueError::Sqlite)?;
            inserted += changed;
        }

        Ok(inserted)
    }

    pub fn list(&self) -> Result<Vec<ReviewQueueItem>, ReviewQueueError> {
        let mut statement = self
            .connection
            .prepare(
                r#"
                SELECT id, session_id, candidate_json, state, reviewer_action,
                       reviewer, note, replacement_summary, created_at, updated_at
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
                       reviewer, note, replacement_summary, created_at, updated_at
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
        let state = state_for_action(&decision.action);
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
                    updated_at = ?7
                WHERE id = ?1
                "#,
                params![
                    decision.item_id.as_str(),
                    state_name(&state),
                    action_name(&decision.action),
                    decision.reviewer,
                    decision.note,
                    decision.replacement_summary,
                    now_string()
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
    })
}

fn state_for_action(action: &ReviewAction) -> ReviewState {
    match action {
        ReviewAction::Accept | ReviewAction::ConvertToSkill => ReviewState::Accepted,
        ReviewAction::Reject | ReviewAction::IgnoreSimilar => ReviewState::Rejected,
        ReviewAction::Edit => ReviewState::Edited,
        ReviewAction::MergeInto(_) => ReviewState::Merged,
        ReviewAction::MarkTemporary => ReviewState::Deferred,
    }
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
