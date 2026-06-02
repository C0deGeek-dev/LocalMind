use crate::{EvidenceRef, SkillDraftId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SkillDraft {
    pub id: SkillDraftId,
    pub name: String,
    pub description: String,
    pub body_markdown: String,
    pub disabled: bool,
    pub cooldown_key: Option<String>,
    pub evidence: Vec<EvidenceRef>,
}
