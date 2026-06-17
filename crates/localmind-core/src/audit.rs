use crate::{AuditEventId, EvidenceRef};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use time::OffsetDateTime;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LearningAuditEvent {
    pub id: AuditEventId,
    pub kind: AuditEventKind,
    pub actor: String,
    pub subject: String,
    pub happened_at: Option<OffsetDateTime>,
    pub evidence: Vec<EvidenceRef>,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum AuditEventKind {
    SessionImported,
    TranscriptRedacted,
    SummaryCreated,
    CandidateLessonCreated,
    ReviewDecisionRecorded,
    MemoryPromoted,
    MemoryDeleted,
    MemorySuperseded,
    MemoryFlaggedStale,
    SkillDraftCreated,
    InferenceCallCompleted,
    VectorIndexUpdated,
    ReviewModeApplied,
    SkillActivated,
    SkillRetired,
    DistillationCreated,
    ResearchInsightCreated,
    ContextPackExported,
}
