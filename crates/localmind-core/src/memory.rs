use crate::{Confidence, EvidenceRef, LessonCategory, MemoryEntryId, SessionId};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct MemoryEntry {
    pub id: MemoryEntryId,
    pub scope: MemoryScope,
    pub body: String,
    pub category: LessonCategory,
    pub confidence: Confidence,
    pub source_session: Option<SessionId>,
    pub evidence: Vec<EvidenceRef>,
    pub tags: Vec<String>,
    pub created_at: Option<OffsetDateTime>,
    pub updated_at: Option<OffsetDateTime>,
    pub supersedes: Vec<MemoryEntryId>,
    pub contradicts: Vec<MemoryEntryId>,
    pub status: MemoryStatus,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum MemoryScope {
    GlobalUser,
    Project,
    Session,
    Skill,
    Research,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum MemoryStatus {
    Active,
    Temporary,
    Superseded,
    Rejected,
    Stale,
}
