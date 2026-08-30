use crate::{CandidateLesson, EvidenceRef, MemoryEntryId, ReviewItemId};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ReviewItem {
    pub id: ReviewItemId,
    pub candidate: CandidateLesson,
    pub state: ReviewState,
    pub created_at: Option<OffsetDateTime>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ReviewState {
    Pending,
    Accepted,
    Rejected,
    Edited,
    Merged,
    Deferred,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ReviewDecision {
    pub item_id: ReviewItemId,
    pub action: ReviewAction,
    pub reviewer: String,
    pub decided_at: Option<OffsetDateTime>,
    pub note: Option<String>,
    pub replacement_summary: Option<String>,
    pub evidence: Vec<EvidenceRef>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ReviewAction {
    Accept,
    Reject,
    Edit,
    MergeInto(ReviewItemId),
    MarkTemporary,
    ConvertToSkill,
    IgnoreSimilar,
    /// Accept this candidate as the replacement for an existing memory and retire
    /// that target. The reviewer (or a trusted/automatic mode with a clear
    /// conflict target) selects which memory to supersede; promotion records the
    /// new memory's `supersedes`, flips the target to `Superseded`, and audits it.
    Supersede(MemoryEntryId),
}
