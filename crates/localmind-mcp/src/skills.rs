use localmind_store::{SkillDraftError, SkillDraftStore};
use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;

pub const TOOL_SKILL_LIST: &str = "localmind.skill.list";
pub const TOOL_SKILL_FETCH: &str = "localmind.skill.fetch";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ActiveSkillSummary {
    pub id: String,
    pub name: String,
    pub body_markdown: String,
}

pub fn list_active_skills(
    project_root: impl AsRef<Path>,
) -> Result<Vec<ActiveSkillSummary>, SkillToolError> {
    let store = SkillDraftStore::open_project(project_root)?;
    let records = store.active()?;
    Ok(records
        .into_iter()
        .map(|record| ActiveSkillSummary {
            id: record.skill.id.to_string(),
            name: record.skill.name,
            body_markdown: record.skill.body_markdown,
        })
        .collect())
}

#[derive(Debug, Error)]
pub enum SkillToolError {
    #[error(transparent)]
    Store(#[from] SkillDraftError),
}
