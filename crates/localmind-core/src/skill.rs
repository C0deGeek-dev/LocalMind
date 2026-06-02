use crate::{EvidenceRef, MemoryEntryId, SkillDraftId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SkillDraft {
    pub id: SkillDraftId,
    pub name: String,
    pub description: String,
    pub trigger_conditions: Vec<String>,
    pub workflow_steps: Vec<String>,
    pub constraints: Vec<String>,
    pub verification_steps: Vec<String>,
    pub related_memories: Vec<MemoryEntryId>,
    pub source_agents: Vec<String>,
    pub last_reviewed_at: Option<String>,
    pub body_markdown: String,
    pub disabled: bool,
    pub cooldown_key: Option<String>,
    pub evidence: Vec<EvidenceRef>,
}
