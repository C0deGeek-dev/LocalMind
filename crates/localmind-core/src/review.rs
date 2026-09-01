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
    /// The dedup existing-item counterpart to [`ReviewAction::MergeInto`]: the
    /// target is an already-*accepted* memory (no retained review-item row to
    /// merge into) rather than another pending candidate. Consolidates the
    /// candidate's wording into the target exactly like [`Self::Supersede`] —
    /// same promotion mechanics, retire-and-replace — recorded as a distinct
    /// action so the audit trail shows the reviewer chose to *merge* a
    /// near-duplicate, not correct an outdated one.
    MergeIntoMemory(MemoryEntryId),
    /// The dedup existing-item counterpart to [`ReviewAction::IgnoreSimilar`]:
    /// the existing accepted memory this candidate resembles is retired
    /// outright (never resurfaces, D-LM-0016 route-to-review invariant still
    /// holds — this is an explicit reviewer decision, not an automated one),
    /// and — unlike [`Self::Supersede`]/[`Self::MergeIntoMemory`] — the
    /// candidate itself is **not** promoted as its replacement: if the
    /// candidate were worth keeping, the reviewer would merge/supersede
    /// instead.
    DeleteExisting(MemoryEntryId),
}
