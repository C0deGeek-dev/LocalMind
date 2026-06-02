use crate::{CandidateLesson, EvidenceRef, ReviewItemId};
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
}
