use crate::{ProjectConfig, StoreConfigError};
use localmind_core::{
    CandidateDestination, CandidateLesson, Confidence, LessonId, MemoryEntryId, ReviewAction,
    ReviewDecision, ReviewItemId, ReviewState, SessionId,
};
use rusqlite::{params, types::Type, Connection, OptionalExtension};
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;
use time::OffsetDateTime;

pub const REVIEW_DB_FILE_NAME: &str = "localmind.sqlite";
/// Maximum Unicode scalar values in a proposed lesson title.
pub const PROPOSAL_TITLE_MAX_CHARS: usize = 240;
/// Maximum Unicode scalar values in a proposal rationale/body.
pub const PROPOSAL_BODY_MAX_CHARS: usize = 16_384;
/// Maximum Unicode scalar values in a carried evidence pointer/excerpt.
pub const PROPOSAL_EVIDENCE_MAX_CHARS: usize = 4_096;
/// Maximum related-file cues on one proposal.
pub const PROPOSAL_MAX_RELATED_FILES: usize = 64;
/// Maximum free tags on one proposal.
pub const PROPOSAL_MAX_TAGS: usize = 32;
/// Maximum Unicode scalar values in one related-file cue.
pub const PROPOSAL_RELATED_FILE_MAX_CHARS: usize = 512;
/// Maximum Unicode scalar values in one tag.
pub const PROPOSAL_TAG_MAX_CHARS: usize = 128;
/// Maximum Unicode scalar values in a source or idempotency key.
pub const PROPOSAL_KEY_MAX_CHARS: usize = 128;
/// Maximum Unicode scalar values in a category name.
pub const PROPOSAL_CATEGORY_MAX_CHARS: usize = 64;

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
    /// The review item whose accumulated evidence this item was merged into.
    /// Historic `merge` records predate this field and therefore load as `None`.
    pub merge_target: Option<ReviewItemId>,
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
    config: ProjectConfig,
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
        let queue = Self { config, connection };
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
            let outcome = self.enqueue_candidate(session_id, candidate, &mut pending)?;
            inserted += usize::from(outcome.created);
        }

        Ok(inserted)
    }

    /// Submit an agent-proposed lesson into the review queue. Unlike transcript
    /// closeout — which extracts candidates from a session — this is a direct
    /// proposal (`memory_propose` / `localmind propose`) so an agent can record
    /// a distilled lesson it learned. It is **never** accepted automatically
    /// (D-LM-0016): it enters the queue as a pending candidate, is deduplicated
    /// against existing pending candidates the same way an extraction is, and
    /// carries the write-time quality classification (D-LM-0024) as a review
    /// note. The candidate records its `source` (the calling agent).
    ///
    /// # Errors
    /// [`ReviewQueueError::EmptyProposal`] when the title is blank, or a
    /// serialize/sqlite error on enqueue.
    pub fn propose(
        &self,
        source: &str,
        proposal: &ProposedLesson,
    ) -> Result<ProposeOutcome, ReviewQueueError> {
        let source = source.trim();
        let title = proposal.title.trim();
        if title.is_empty() {
            return Err(ReviewQueueError::EmptyProposal);
        }
        if source.is_empty() {
            return Err(ReviewQueueError::EmptyProposalField { field: "source" });
        }
        if !source
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(ReviewQueueError::InvalidProposalSource);
        }
        validate_proposal(source, proposal, title)?;
        let redactor = crate::Redactor::new(self.config.config.learning.excluded_paths.clone());
        let title = redactor.redact(title).redacted_text;
        let body = redactor
            .redact(proposal.body.as_deref().unwrap_or("").trim())
            .redacted_text;
        let category_name = redactor.redact(proposal.category.trim()).redacted_text;
        let category = crate::markdown::parse_category(&category_name);
        let confidence = Confidence::clamped(proposal.confidence, 0.7);
        let session_id = SessionId::new(format!("proposal-{source}"));
        let evidence = proposal
            .evidence
            .as_deref()
            .map(str::trim)
            .filter(|evidence| !evidence.is_empty())
            .map(|evidence| redactor.redact(evidence).redacted_text);
        let related_files = normalized_values(&proposal.related_files)
            .into_iter()
            .map(|value| redactor.redact(&value).redacted_text)
            .collect::<Vec<_>>();
        let tags = normalized_values(&proposal.tags)
            .into_iter()
            .map(|value| redactor.redact(&value).redacted_text)
            .collect::<Vec<_>>();
        let normalized = ProposedLesson {
            title: title.clone(),
            body: (!body.is_empty()).then_some(body.clone()),
            category: category_name,
            scope: proposal.scope,
            related_files,
            tags,
            evidence,
            idempotency_key: proposal
                .idempotency_key
                .as_deref()
                .map(str::trim)
                .map(str::to_string),
            confidence: confidence.value(),
        };
        let fingerprint = proposal_fingerprint(source, &normalized);
        let candidate_id = LessonId::new(propose_candidate_id(
            source,
            normalized.idempotency_key.as_deref(),
            &fingerprint,
        ));

        let mut candidate = CandidateLesson::new(
            candidate_id.clone(),
            &title,
            category.clone(),
            confidence,
            localmind_core::SuggestedAction::PromoteToMemory,
        )
        .with_source(source);
        if !body.is_empty() {
            candidate.rationale = Some(body.clone());
        }
        if let Some(evidence) = normalized.evidence.clone() {
            candidate = candidate.with_evidence_text(evidence);
        }
        candidate.suggested_destination = proposal.scope.destination();
        candidate.related_files = normalized.related_files.clone();
        candidate.related_entities = normalized.tags.clone();

        // Write-time quality classification: a low-quality proposal is still
        // queued (never dropped, never auto-accepted), with the reason attached
        // for the reviewer.
        let quality = crate::quality::classify_quality(&category, &title, &body);
        let quality_note = quality.review_note().map(str::to_string);
        if let Some(receipt) = self.proposal_receipt(candidate_id.as_str())? {
            if receipt.fingerprint != fingerprint {
                return Err(ReviewQueueError::IdempotencyConflict);
            }
            if let Some(existing) = self.get(&ReviewItemId::new(&receipt.survivor_id))? {
                return Ok(ProposeOutcome {
                    candidate_id: existing.id.to_string(),
                    created: false,
                    changed: false,
                    duplicate_of: existing
                        .candidate
                        .review_annotation
                        .as_ref()
                        .and_then(|annotation| annotation.duplicate_of.clone()),
                    quality_note,
                });
            }
            // A stale receipt can only be left by an older/manual database
            // mutation. Remove it so the proposal can safely be recorded again.
            self.connection
                .execute(
                    "DELETE FROM proposal_receipts WHERE proposal_id = ?1",
                    params![candidate_id.as_str()],
                )
                .map_err(ReviewQueueError::Sqlite)?;
        }
        // An exact replay returns the first result without bumping `seen_count`.
        // This makes the store operation genuinely idempotent for identical
        // arguments (and for an explicit idempotency key).
        let item_id = ReviewItemId::new(candidate_id.as_str());
        if let Some(existing) = self.get(&item_id)? {
            if !same_proposal(&existing.candidate, &candidate) {
                return Err(ReviewQueueError::IdempotencyConflict);
            }
            let survivor = self.remember_proposal_receipt(
                candidate_id.as_str(),
                &fingerprint,
                existing.id.as_str(),
            )?;
            return Ok(ProposeOutcome {
                candidate_id: survivor,
                created: false,
                changed: false,
                duplicate_of: existing
                    .candidate
                    .review_annotation
                    .as_ref()
                    .and_then(|annotation| annotation.duplicate_of.clone()),
                quality_note,
            });
        }

        // Compare accepted memory at propose time using the same lexical and
        // optional semantic ladder as review-mode processing. A match is only
        // annotated for human review; it never deletes or auto-merges memory.
        let persistence = crate::MemoryPersistence::open_project(&self.config.project_root)
            .map_err(|source| ReviewQueueError::AcceptedMemoryProbe {
                source: Box::new(source),
            })?;
        let accepted = crate::review_modes::accepted_memory_match(
            &self.config,
            &persistence,
            candidate.summary(),
        )
        .map_err(|source| ReviewQueueError::AcceptedMemoryProbe {
            source: Box::new(source),
        })?;
        let duplicate_of = accepted.duplicate_of;
        let mut notes = Vec::new();
        if duplicate_of.is_some() {
            notes.push(if accepted.borderline_duplicate {
                "Borderline semantic match (review band); human review required.".to_string()
            } else {
                "Similar accepted memory found; human review required.".to_string()
            });
        }
        if let Some(note) = &quality_note {
            notes.push(note.clone());
        }
        if !notes.is_empty() {
            candidate.review_annotation = Some(localmind_core::ReviewAnnotation {
                score: confidence,
                duplicate_of: duplicate_of.clone(),
                conflict: false,
                notes: notes.join(" "),
            });
        }

        let mut pending = self.pending_dedup_keys()?;
        let enqueued = self.enqueue_candidate(&session_id, &candidate, &mut pending)?;
        let survivor =
            self.remember_proposal_receipt(candidate_id.as_str(), &fingerprint, &enqueued.item_id)?;
        Ok(ProposeOutcome {
            candidate_id: survivor,
            created: enqueued.created,
            changed: enqueued.changed,
            duplicate_of,
            quality_note,
        })
    }

    fn enqueue_candidate(
        &self,
        session_id: &SessionId,
        candidate: &CandidateLesson,
        pending: &mut Vec<DedupKey>,
    ) -> Result<EnqueueCandidateOutcome, ReviewQueueError> {
        let summary = candidate.summary();
        let hash = crate::dedup::canonical_hash(summary);
        if let Some(survivor) = find_duplicate(pending, &hash, summary) {
            self.bump_seen_count(&survivor)?;
            return Ok(EnqueueCandidateOutcome {
                item_id: survivor,
                created: false,
                changed: true,
            });
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
        if changed > 0 {
            pending.push(DedupKey {
                id: item_id.to_string(),
                canonical_hash: hash,
                summary: summary.to_string(),
            });
        }
        Ok(EnqueueCandidateOutcome {
            item_id: item_id.to_string(),
            created: changed > 0,
            changed: changed > 0,
        })
    }

    fn proposal_receipt(
        &self,
        proposal_id: &str,
    ) -> Result<Option<ProposalReceipt>, ReviewQueueError> {
        self.connection
            .query_row(
                "SELECT fingerprint, survivor_id FROM proposal_receipts WHERE proposal_id = ?1",
                params![proposal_id],
                |row| {
                    Ok(ProposalReceipt {
                        fingerprint: row.get(0)?,
                        survivor_id: row.get(1)?,
                    })
                },
            )
            .optional()
            .map_err(ReviewQueueError::Sqlite)
    }

    fn remember_proposal_receipt(
        &self,
        proposal_id: &str,
        fingerprint: &str,
        survivor_id: &str,
    ) -> Result<String, ReviewQueueError> {
        self.connection
            .execute(
                "INSERT OR IGNORE INTO proposal_receipts
                 (proposal_id, fingerprint, survivor_id, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![proposal_id, fingerprint, survivor_id, now_string()],
            )
            .map_err(ReviewQueueError::Sqlite)?;
        let receipt = self
            .proposal_receipt(proposal_id)?
            .ok_or(ReviewQueueError::MissingProposalReceipt)?;
        if receipt.fingerprint != fingerprint {
            return Err(ReviewQueueError::IdempotencyConflict);
        }
        Ok(receipt.survivor_id)
    }

    /// Delete every pending review candidate, returning how many rows were
    /// removed. Accepted/rejected/edited/merged decisions and all accepted-memory
    /// tables are untouched — this clears only the un-reviewed backlog.
    pub fn purge_pending(&self) -> Result<usize, ReviewQueueError> {
        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(ReviewQueueError::Sqlite)?;
        transaction
            .execute(
                "DELETE FROM proposal_receipts
                 WHERE survivor_id IN (SELECT id FROM review_items WHERE state = ?1)",
                params![state_name(&ReviewState::Pending)],
            )
            .map_err(ReviewQueueError::Sqlite)?;
        let removed = transaction
            .execute(
                "DELETE FROM review_items WHERE state = ?1",
                params![state_name(&ReviewState::Pending)],
            )
            .map_err(ReviewQueueError::Sqlite)?;
        transaction.commit().map_err(ReviewQueueError::Sqlite)?;
        Ok(removed)
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
                       seen_count, supersede_target, merge_target
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
                       seen_count, supersede_target, merge_target
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
        let merge_target = match &decision.action {
            ReviewAction::MergeInto(target) => {
                if target == &decision.item_id {
                    return Err(ReviewQueueError::InvalidMergeTarget {
                        item_id: decision.item_id,
                        target: target.clone(),
                    });
                }
                if self.get(target)?.is_none() {
                    return Err(ReviewQueueError::MissingMergeTarget {
                        item_id: decision.item_id,
                        target: target.clone(),
                    });
                }
                Some(target.as_str().to_string())
            }
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
                    supersede_target = ?8,
                    merge_target = ?9
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
                    merge_target,
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
        merge_target: row.get::<_, Option<String>>(12)?.map(ReviewItemId::new),
    })
}

/// A pending candidate's dedup keys, loaded once per enqueue batch.
struct DedupKey {
    id: String,
    canonical_hash: String,
    summary: String,
}

struct EnqueueCandidateOutcome {
    item_id: String,
    created: bool,
    changed: bool,
}

struct ProposalReceipt {
    fingerprint: String,
    survivor_id: String,
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

/// Where a proposed lesson should live once accepted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProposeScope {
    /// This project's memory (the default).
    #[default]
    Project,
    /// The cross-project global store.
    Global,
}

impl ProposeScope {
    fn destination(self) -> CandidateDestination {
        match self {
            ProposeScope::Project => CandidateDestination::ProjectMemory,
            ProposeScope::Global => CandidateDestination::GlobalMemory,
        }
    }
}

/// An agent-proposed lesson: the distilled, reusable claim in `title`, an
/// optional `body` for the why/how, and light metadata. Turned into a review
/// candidate by [`ReviewQueue::propose`]; never a direct memory write.
#[derive(Clone, Debug)]
pub struct ProposedLesson {
    /// The one-line reusable lesson (becomes the candidate summary). Required.
    pub title: String,
    /// The rationale or how-to-apply detail. Optional.
    pub body: Option<String>,
    /// A `LessonCategory` name (e.g. `CodePattern`, `DebuggingRecipe`);
    /// unrecognized names become `Other(..)`.
    pub category: String,
    /// Project or global memory.
    pub scope: ProposeScope,
    /// Files this lesson relates to (retrieval cues).
    pub related_files: Vec<String>,
    /// Free tags / related entities (retrieval cues).
    pub tags: Vec<String>,
    /// A bounded source pointer or excerpt shown to the reviewer but never
    /// promoted as memory text.
    pub evidence: Option<String>,
    /// Stable caller key for retry-safe proposals. Reusing a key with different
    /// content is rejected instead of silently replacing the first proposal.
    pub idempotency_key: Option<String>,
    /// Author confidence in `[0, 1]`; out-of-range is clamped, absent is 0.7.
    pub confidence: f32,
}

impl Default for ProposedLesson {
    fn default() -> Self {
        Self {
            title: String::new(),
            body: None,
            category: "Process".to_string(),
            scope: ProposeScope::Project,
            related_files: Vec::new(),
            tags: Vec::new(),
            evidence: None,
            idempotency_key: None,
            confidence: 0.7,
        }
    }
}

/// What [`ReviewQueue::propose`] did with a proposal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProposeOutcome {
    /// The candidate's id in the review queue.
    pub candidate_id: String,
    /// `true` when a new pending candidate was enqueued; `false` for an exact
    /// idempotent replay or a merge into an existing pending candidate.
    pub created: bool,
    /// Whether this call changed the queue: a new row or a near-duplicate
    /// `seen_count` bump. `false` only for an exact idempotent replay.
    pub changed: bool,
    /// Accepted memory this proposal resembles, when the lexical/semantic
    /// duplicate ladder found one. This is review guidance, never an auto-merge.
    pub duplicate_of: Option<String>,
    /// The write-time quality note (D-LM-0024), when the proposal was flagged
    /// low-quality — it is still queued, never dropped.
    pub quality_note: Option<String>,
}

/// A stable candidate id for a proposal. An explicit idempotency key owns the
/// identity; otherwise all normalized arguments do, so retrying identical input
/// is a no-op while a materially different proposal still reaches dedup.
fn propose_candidate_id(source: &str, idempotency_key: Option<&str>, fingerprint: &str) -> String {
    let digest = if let Some(key) = idempotency_key {
        proposal_digest(["proposal-id", source, key])
    } else {
        fingerprint.to_string()
    };
    format!("lesson-{}", &digest[..16])
}

fn proposal_fingerprint(source: &str, proposal: &ProposedLesson) -> String {
    let mut values = vec![
        "proposal-content".to_string(),
        source.to_string(),
        proposal.title.clone(),
        proposal.body.clone().unwrap_or_default(),
        proposal.category.clone(),
        match proposal.scope {
            ProposeScope::Project => "project".to_string(),
            ProposeScope::Global => "global".to_string(),
        },
        proposal.evidence.clone().unwrap_or_default(),
    ];
    values.extend(proposal.related_files.iter().cloned());
    values.push("tags".to_string());
    values.extend(proposal.tags.iter().cloned());
    values.push(proposal.confidence.to_bits().to_string());
    proposal_digest(values.iter().map(String::as_str))
}

fn proposal_digest<'a>(values: impl IntoIterator<Item = &'a str>) -> String {
    let mut bytes = Vec::new();
    for value in values {
        bytes.extend_from_slice(&(value.len() as u64).to_le_bytes());
        bytes.extend_from_slice(value.as_bytes());
    }
    crate::digest_hex(&bytes)
}

fn normalized_values(values: &[String]) -> Vec<String> {
    values
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn validate_proposal(
    source: &str,
    proposal: &ProposedLesson,
    title: &str,
) -> Result<(), ReviewQueueError> {
    validate_length("source", source, PROPOSAL_KEY_MAX_CHARS)?;
    validate_length("title", title, PROPOSAL_TITLE_MAX_CHARS)?;
    validate_length(
        "category",
        proposal.category.trim(),
        PROPOSAL_CATEGORY_MAX_CHARS,
    )?;
    if let Some(body) = proposal.body.as_deref() {
        validate_length("body", body.trim(), PROPOSAL_BODY_MAX_CHARS)?;
    }
    if let Some(evidence) = proposal.evidence.as_deref() {
        validate_length("evidence", evidence.trim(), PROPOSAL_EVIDENCE_MAX_CHARS)?;
    }
    if let Some(key) = proposal.idempotency_key.as_deref() {
        let key = key.trim();
        if key.is_empty() {
            return Err(ReviewQueueError::EmptyProposalField {
                field: "idempotency_key",
            });
        }
        validate_length("idempotency_key", key, PROPOSAL_KEY_MAX_CHARS)?;
    }
    validate_values(
        "related_files",
        &proposal.related_files,
        PROPOSAL_MAX_RELATED_FILES,
        PROPOSAL_RELATED_FILE_MAX_CHARS,
    )?;
    validate_values(
        "tags",
        &proposal.tags,
        PROPOSAL_MAX_TAGS,
        PROPOSAL_TAG_MAX_CHARS,
    )?;
    Ok(())
}

fn validate_values(
    field: &'static str,
    values: &[String],
    max_values: usize,
    max_chars: usize,
) -> Result<(), ReviewQueueError> {
    if values.len() > max_values {
        return Err(ReviewQueueError::TooManyProposalValues {
            field,
            max: max_values,
        });
    }
    for value in values {
        validate_length(field, value.trim(), max_chars)?;
    }
    Ok(())
}

fn validate_length(field: &'static str, value: &str, max: usize) -> Result<(), ReviewQueueError> {
    if value.chars().count() > max {
        return Err(ReviewQueueError::ProposalTooLarge { field, max });
    }
    Ok(())
}

fn same_proposal(existing: &CandidateLesson, candidate: &CandidateLesson) -> bool {
    existing.summary() == candidate.summary()
        && existing.rationale == candidate.rationale
        && existing.category == candidate.category
        && existing.confidence == candidate.confidence
        && existing.evidence_text == candidate.evidence_text
        && existing.related_files == candidate.related_files
        && existing.related_entities == candidate.related_entities
        && existing.suggested_destination == candidate.suggested_destination
        && existing.source == candidate.source
}

#[derive(Debug, Error)]
pub enum ReviewQueueError {
    #[error(transparent)]
    Config(#[from] StoreConfigError),
    #[error("a proposed lesson needs a non-empty title")]
    EmptyProposal,
    #[error("a proposed lesson needs a non-empty {field}")]
    EmptyProposalField { field: &'static str },
    #[error("proposed lesson field {field} exceeds {max} characters")]
    ProposalTooLarge { field: &'static str, max: usize },
    #[error("proposed lesson field {field} exceeds {max} values")]
    TooManyProposalValues { field: &'static str, max: usize },
    #[error("a proposal idempotency key was already used with different content")]
    IdempotencyConflict,
    #[error("a proposal receipt disappeared immediately after it was recorded")]
    MissingProposalReceipt,
    #[error("proposal source may contain only ASCII letters, digits, '.', '_' and '-'")]
    InvalidProposalSource,
    #[error("failed to compare a proposal with accepted memory: {source}")]
    AcceptedMemoryProbe {
        source: Box<crate::MemoryPersistenceError>,
    },
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
    #[error("failed to serialize candidate lesson: {0}")]
    SerializeCandidate(serde_json::Error),
    #[error("review item does not exist: {item_id}")]
    MissingItem { item_id: ReviewItemId },
    #[error("edit decision for {item_id} requires non-empty replacement text")]
    InvalidEdit { item_id: ReviewItemId },
    #[error("review item {item_id} cannot be merged into itself ({target})")]
    InvalidMergeTarget {
        item_id: ReviewItemId,
        target: ReviewItemId,
    },
    #[error("review item {item_id} cannot be merged: target does not exist: {target}")]
    MissingMergeTarget {
        item_id: ReviewItemId,
        target: ReviewItemId,
    },
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
    fn historic_merge_action_without_a_stored_target_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let queue = open(dir.path());
        queue
            .enqueue_candidates(
                &session(),
                &[candidate("historic", "Historic merge record")],
            )
            .unwrap();
        queue
            .connection
            .execute(
                "UPDATE review_items
                 SET state = 'merged', reviewer_action = 'merge'
                 WHERE id = 'historic'",
                [],
            )
            .unwrap();

        let item = queue.get(&ReviewItemId::new("historic")).unwrap().unwrap();

        assert_eq!(item.state, ReviewState::Merged);
        assert_eq!(item.reviewer_action.as_deref(), Some("merge"));
        assert_eq!(item.merge_target, None);
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

#[cfg(test)]
mod propose_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn open(root: &std::path::Path) -> ReviewQueue {
        std::fs::write(root.join(".localmind.toml"), "[learning]\nenabled = true\n").unwrap();
        ReviewQueue::open_project(root).unwrap()
    }

    fn candidate(id: &str, summary: &str) -> CandidateLesson {
        CandidateLesson::new(
            LessonId::new(id),
            summary,
            localmind_core::LessonCategory::ProjectConvention,
            Confidence::new(0.8).unwrap(),
            localmind_core::SuggestedAction::PromoteToMemory,
        )
    }

    fn session() -> SessionId {
        SessionId::new("proposal-test-session")
    }

    #[test]
    fn a_proposed_lesson_enters_the_queue_pending_with_its_source() {
        let dir = tempfile::tempdir().unwrap();
        let queue = open(dir.path());

        let outcome = queue
            .propose(
                "claude-code",
                &ProposedLesson {
                    title: "Prefer ripgrep over grep for repo search".to_string(),
                    body: Some("It respects .gitignore and is faster on large trees.".to_string()),
                    category: "CodePattern".to_string(),
                    scope: ProposeScope::Project,
                    related_files: vec!["src/search.rs".to_string()],
                    tags: vec!["search".to_string()],
                    evidence: Some("docs/search.md#performance".to_string()),
                    idempotency_key: Some("search-rule-v1".to_string()),
                    confidence: 0.9,
                },
            )
            .unwrap();
        assert!(outcome.created, "a fresh proposal is newly enqueued");
        assert_eq!(outcome.duplicate_of, None);

        let items = queue.list().unwrap();
        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(item.state, ReviewState::Pending, "never auto-accepted");
        assert_eq!(item.candidate.source.as_deref(), Some("claude-code"));
        assert_eq!(
            item.candidate.evidence_text.as_deref(),
            Some("docs/search.md#performance")
        );
        assert_eq!(
            item.candidate.rationale.as_deref(),
            Some("It respects .gitignore and is faster on large trees.")
        );
        assert_eq!(
            item.candidate.suggested_destination,
            CandidateDestination::ProjectMemory
        );
    }

    #[test]
    fn a_blank_title_is_refused_before_touching_the_queue() {
        let dir = tempfile::tempdir().unwrap();
        let queue = open(dir.path());
        let err = queue
            .propose(
                "open-ai-codex",
                &ProposedLesson {
                    title: "   ".to_string(),
                    ..ProposedLesson::default()
                },
            )
            .unwrap_err();
        assert!(matches!(err, ReviewQueueError::EmptyProposal));
        assert_eq!(queue.list().unwrap().len(), 0);
    }

    #[test]
    fn the_same_proposal_twice_merges_instead_of_duplicating() {
        let dir = tempfile::tempdir().unwrap();
        let queue = open(dir.path());
        let make = || ProposedLesson {
            title: "Pin the toolchain to 1.82 for reproducible builds".to_string(),
            category: "TestingStrategy".to_string(),
            confidence: 0.7,
            ..ProposedLesson::default()
        };
        assert!(queue.propose("claude-code", &make()).unwrap().created);
        let second = queue.propose("claude-code", &make()).unwrap();
        assert!(!second.created, "an identical retry is idempotent");
        assert_eq!(queue.list().unwrap().len(), 1);
        assert_eq!(queue.list().unwrap()[0].seen_count, 1);
    }

    #[test]
    fn purging_a_pending_proposal_also_removes_its_retry_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let queue = open(dir.path());
        let proposal = ProposedLesson {
            title: "Keep generated output out of source control".to_string(),
            idempotency_key: Some("turn-19-lesson-1".to_string()),
            ..ProposedLesson::default()
        };
        let first = queue.propose("open-ai-codex", &proposal).unwrap();
        assert_eq!(queue.purge_pending().unwrap(), 1);

        let reproposed = queue.propose("open-ai-codex", &proposal).unwrap();

        assert!(reproposed.created);
        assert!(reproposed.changed);
        assert_eq!(reproposed.candidate_id, first.candidate_id);
        assert_eq!(queue.list().unwrap().len(), 1);
    }

    #[test]
    fn a_near_duplicate_returns_the_real_surviving_candidate_id() {
        let dir = tempfile::tempdir().unwrap();
        let queue = open(dir.path());
        let first = queue
            .propose(
                "claude-code",
                &ProposedLesson {
                    title: "Prefer ripgrep for fast repository text search".to_string(),
                    ..ProposedLesson::default()
                },
            )
            .unwrap();
        let proposal = ProposedLesson {
            title: "For fast repository text search prefer ripgrep".to_string(),
            ..ProposedLesson::default()
        };
        let merged = queue.propose("open-ai-codex", &proposal).unwrap();

        assert!(!merged.created);
        assert!(merged.changed);
        assert_eq!(merged.candidate_id, first.candidate_id);
        let retry = queue.propose("open-ai-codex", &proposal).unwrap();
        assert!(!retry.created);
        assert!(!retry.changed, "an exact near-merge retry is a no-op");
        assert_eq!(retry.candidate_id, first.candidate_id);
        let survivor = queue
            .get(&ReviewItemId::new(&merged.candidate_id))
            .unwrap()
            .expect("reported candidate id exists");
        assert_eq!(survivor.seen_count, 2);
    }

    #[test]
    fn an_idempotency_key_remains_bound_after_a_near_merge() {
        let dir = tempfile::tempdir().unwrap();
        let queue = open(dir.path());
        queue
            .propose(
                "claude-code",
                &ProposedLesson {
                    title: "Prefer ripgrep for fast repository text search".to_string(),
                    ..ProposedLesson::default()
                },
            )
            .unwrap();
        let proposal = |title: &str| ProposedLesson {
            title: title.to_string(),
            idempotency_key: Some("turn-18-lesson-1".to_string()),
            ..ProposedLesson::default()
        };
        queue
            .propose(
                "open-ai-codex",
                &proposal("For fast repository text search prefer ripgrep"),
            )
            .unwrap();

        let error = queue
            .propose(
                "open-ai-codex",
                &proposal("Retry forever until the service responds"),
            )
            .unwrap_err();

        assert!(matches!(error, ReviewQueueError::IdempotencyConflict));
        assert_eq!(queue.list().unwrap().len(), 1);
        assert_eq!(queue.list().unwrap()[0].seen_count, 2);
    }

    #[test]
    fn an_idempotency_key_cannot_be_reused_for_different_content() {
        let dir = tempfile::tempdir().unwrap();
        let queue = open(dir.path());
        let proposal = |title: &str| ProposedLesson {
            title: title.to_string(),
            idempotency_key: Some("turn-17-lesson-1".to_string()),
            ..ProposedLesson::default()
        };
        queue
            .propose("open-ai-codex", &proposal("Keep retries bounded"))
            .unwrap();
        let error = queue
            .propose(
                "open-ai-codex",
                &proposal("Retry forever until the service responds"),
            )
            .unwrap_err();

        assert!(matches!(error, ReviewQueueError::IdempotencyConflict));
        assert_eq!(queue.list().unwrap().len(), 1);
    }

    #[test]
    fn oversized_proposals_are_rejected_before_the_queue_is_touched() {
        let dir = tempfile::tempdir().unwrap();
        let queue = open(dir.path());
        let error = queue
            .propose(
                "open-ai-codex",
                &ProposedLesson {
                    title: "x".repeat(PROPOSAL_TITLE_MAX_CHARS + 1),
                    ..ProposedLesson::default()
                },
            )
            .unwrap_err();

        assert!(matches!(
            error,
            ReviewQueueError::ProposalTooLarge { field: "title", .. }
        ));
        assert!(queue.list().unwrap().is_empty());
    }

    #[test]
    fn proposal_secrets_are_redacted_before_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let queue = open(dir.path());
        let secret = "sk-abcdefghijklmnopqrstuvwx";
        let outcome = queue
            .propose(
                "open-ai-codex",
                &ProposedLesson {
                    title: format!("Never persist API key {secret}"),
                    body: Some(format!("token={secret}")),
                    evidence: Some(format!("captured from {secret}")),
                    ..ProposedLesson::default()
                },
            )
            .unwrap();
        let item = queue
            .get(&ReviewItemId::new(outcome.candidate_id))
            .unwrap()
            .expect("proposal");
        let serialized = serde_json::to_string(&item.candidate).unwrap();

        assert!(!serialized.contains(secret));
        assert!(serialized.contains("[REDACTED:"));
    }

    #[test]
    fn a_proposal_is_annotated_against_accepted_memory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".localmind.toml"),
            "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n",
        )
        .unwrap();
        let queue = ReviewQueue::open_project(dir.path()).unwrap();
        let accepted = candidate(
            "accepted-search-rule",
            "Prefer ripgrep for repository text search",
        );
        queue
            .enqueue_candidates(&session(), std::slice::from_ref(&accepted))
            .unwrap();
        queue
            .decide(ReviewDecision {
                item_id: ReviewItemId::new(accepted.id.as_str()),
                action: ReviewAction::Accept,
                reviewer: "test".to_string(),
                decided_at: None,
                note: None,
                replacement_summary: None,
                evidence: Vec::new(),
            })
            .unwrap();
        crate::MemoryPersistence::open_project(dir.path())
            .unwrap()
            .promote_review_item(&ReviewItemId::new(accepted.id.as_str()))
            .unwrap();

        let outcome = queue
            .propose(
                "open-ai-codex",
                &ProposedLesson {
                    title: "For repository text search prefer ripgrep".to_string(),
                    ..ProposedLesson::default()
                },
            )
            .unwrap();
        assert_eq!(
            outcome.duplicate_of.as_deref(),
            Some("accepted-search-rule")
        );
        let item = queue
            .get(&ReviewItemId::new(&outcome.candidate_id))
            .unwrap()
            .expect("proposal queued");
        assert_eq!(
            item.candidate
                .review_annotation
                .and_then(|annotation| annotation.duplicate_of)
                .as_deref(),
            Some("accepted-search-rule")
        );
    }

    #[test]
    fn automatic_review_mode_never_auto_accepts_an_agent_proposal() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".localmind.toml"),
            "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n\n[review]\nmode = \"automatic\"\n",
        )
        .unwrap();
        let queue = ReviewQueue::open_project(dir.path()).unwrap();
        let outcome = queue
            .propose(
                "claude-code",
                &ProposedLesson {
                    title: "Keep generated output out of source control".to_string(),
                    confidence: 1.0,
                    ..ProposedLesson::default()
                },
            )
            .unwrap();

        let report = crate::ReviewModeProcessor::apply_project(dir.path()).unwrap();
        assert_eq!(report.accepted, 0);
        assert_eq!(report.manual, 1);
        assert_eq!(
            queue
                .get(&ReviewItemId::new(outcome.candidate_id))
                .unwrap()
                .expect("proposal remains")
                .state,
            ReviewState::Pending
        );
    }

    #[test]
    fn a_global_scope_targets_the_global_store() {
        let dir = tempfile::tempdir().unwrap();
        let queue = open(dir.path());
        queue
            .propose(
                "claude-code",
                &ProposedLesson {
                    title: "Never commit secrets; assemble token-shaped fixtures at runtime"
                        .to_string(),
                    scope: ProposeScope::Global,
                    category: "SecurityWarning".to_string(),
                    confidence: 0.95,
                    ..ProposedLesson::default()
                },
            )
            .unwrap();
        assert_eq!(
            queue.list().unwrap()[0].candidate.suggested_destination,
            CandidateDestination::GlobalMemory
        );
    }
}
