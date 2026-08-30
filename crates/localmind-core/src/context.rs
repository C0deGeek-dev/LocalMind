use crate::{MemoryEntry, SkillDraft};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextQuery {
    pub text: String,
    pub project_uri: Option<String>,
    pub token_budget: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ContextPack {
    pub query: ContextQuery,
    pub sources: Vec<ContextSource>,
    pub token_budget: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub enum ContextSource {
    Memory(MemoryEntry),
    SkillDraft(SkillDraft),
    Note { title: String, body: String },
}
