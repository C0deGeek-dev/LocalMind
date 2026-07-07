use crate::{
    ImportedSession, MemoryPersistence, ProjectConfig, ReviewQueue, ReviewQueueError,
    StoreConfigError,
};
use localmind_core::{
    AuditEventKind, LessonCategory, ReviewState, SkillDraft, SkillDraftId, SuggestedAction,
};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SkillDraftRecord {
    pub draft: SkillDraft,
    pub draft_path: PathBuf,
    pub metadata_path: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ActiveSkillRecord {
    pub skill: SkillDraft,
    pub status: String,
    pub source_memory_ids: Vec<String>,
}

pub struct SkillDraftStore {
    config: ProjectConfig,
}

impl SkillDraftStore {
    pub fn open_project(project_root: impl AsRef<Path>) -> Result<Self, SkillDraftError> {
        let config = ProjectConfig::discover(project_root).map_err(SkillDraftError::Config)?;
        Ok(Self { config })
    }

    pub fn generate_from_review_queue(&self) -> Result<Vec<SkillDraftRecord>, SkillDraftError> {
        let queue = ReviewQueue::open_project(&self.config.project_root)?;
        let persistence = MemoryPersistence::open_project(&self.config.project_root)?;
        let mut records = Vec::new();

        for item in queue.list()? {
            if !matches!(item.state, ReviewState::Accepted | ReviewState::Edited) {
                continue;
            }
            if !matches!(item.candidate.category, LessonCategory::CandidateSkill)
                && !matches!(
                    item.candidate.suggested_action,
                    SuggestedAction::CreateSkillDraft | SuggestedAction::UpdateSkillDraft
                )
            {
                continue;
            }

            let related_memories = persistence
                .search(item.candidate.summary())?
                .into_iter()
                .take(5)
                .map(|result| result.memory_id)
                .collect();
            let source_agents = self.source_agents_for_session(item.session_id.as_str())?;
            let name = slug(item.candidate.summary());
            let draft = SkillDraft {
                id: SkillDraftId::new(format!("skill-{}", item.id)),
                name: name.clone(),
                description: format!(
                    "Suggested disabled workflow draft from review item {}.",
                    item.id
                ),
                trigger_conditions: vec![format!(
                    "When work resembles: {}",
                    item.candidate.summary()
                )],
                workflow_steps: vec![
                    "Review the source session evidence.".to_string(),
                    "Apply the workflow only after a human accepts the draft.".to_string(),
                    "Keep project-specific constraints explicit in the skill body.".to_string(),
                ],
                constraints: vec![
                    "disabled: true until explicitly installed".to_string(),
                    "local project evidence only".to_string(),
                ],
                verification_steps: vec![
                    "Run the relevant project tests.".to_string(),
                    "Inspect the generated SKILL.md before installing it.".to_string(),
                ],
                related_memories,
                source_agents,
                last_reviewed_at: item.updated_at.clone(),
                body_markdown: String::new(),
                disabled: true,
                cooldown_key: Some(name),
                evidence: item.candidate.evidence().to_vec(),
            };
            let draft = SkillDraft {
                body_markdown: render_skill_markdown(&draft),
                ..draft
            };
            records.push(self.write_draft(&draft)?);
            persistence.record_skill_draft_created(&draft)?;
        }

        Ok(records)
    }

    pub fn activate(
        &self,
        draft_id: &SkillDraftId,
    ) -> Result<Option<ActiveSkillRecord>, SkillDraftError> {
        let Some(record) = self.get(draft_id)? else {
            return Ok(None);
        };
        let persistence = MemoryPersistence::open_project(&self.config.project_root)?;
        let active = SkillDraft {
            disabled: false,
            body_markdown: record
                .draft
                .body_markdown
                .replace("disabled: true", "disabled: false"),
            ..record.draft
        };
        let source_memory_ids: Vec<String> = active
            .related_memories
            .iter()
            .map(ToString::to_string)
            .collect();
        self.connection()?
            .execute(
                r#"
            INSERT INTO skill_records
            (skill_id, draft_json, status, source_memory_ids_json, created_at, updated_at)
            VALUES(?1, ?2, 'active', ?3, ?4, ?4)
            ON CONFLICT(skill_id) DO UPDATE SET
                draft_json = excluded.draft_json,
                status = 'active',
                source_memory_ids_json = excluded.source_memory_ids_json,
                updated_at = excluded.updated_at
            "#,
                params![
                    active.id.as_str(),
                    serde_json::to_string(&active).map_err(SkillDraftError::SerializeDraft)?,
                    serde_json::to_string(&source_memory_ids)
                        .map_err(SkillDraftError::SerializeSources)?,
                    time::OffsetDateTime::now_utc().to_string()
                ],
            )
            .map_err(SkillDraftError::Sqlite)?;
        persistence.record_custom_audit(
            AuditEventKind::SkillActivated,
            "cli",
            active.id.as_str(),
            &serde_json::json!({ "name": active.name }),
        )?;
        Ok(Some(ActiveSkillRecord {
            skill: active,
            status: "active".to_string(),
            source_memory_ids,
        }))
    }

    pub fn retire(&self, draft_id: &SkillDraftId, reason: &str) -> Result<bool, SkillDraftError> {
        let changed = self.connection()?.execute(
            "UPDATE skill_records SET status = 'retired', updated_at = ?2 WHERE skill_id = ?1 AND status != 'retired'",
            params![draft_id.as_str(), time::OffsetDateTime::now_utc().to_string()],
        ).map_err(SkillDraftError::Sqlite)?;
        if changed > 0 {
            let persistence = MemoryPersistence::open_project(&self.config.project_root)?;
            persistence.record_custom_audit(
                AuditEventKind::SkillRetired,
                "cli",
                draft_id.as_str(),
                &serde_json::json!({ "reason": reason }),
            )?;
        }
        Ok(changed > 0)
    }

    pub fn active(&self) -> Result<Vec<ActiveSkillRecord>, SkillDraftError> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare("SELECT draft_json, status, source_memory_ids_json FROM skill_records WHERE status = 'active' ORDER BY skill_id")
            .map_err(SkillDraftError::Sqlite)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(SkillDraftError::Sqlite)?;
        let mut records = Vec::new();
        for row in rows {
            let (draft_json, status, sources_json) = row.map_err(SkillDraftError::Sqlite)?;
            records.push(ActiveSkillRecord {
                skill: serde_json::from_str(&draft_json)
                    .map_err(SkillDraftError::DeserializeActiveSkill)?,
                status,
                source_memory_ids: serde_json::from_str(&sources_json)
                    .map_err(SkillDraftError::DeserializeSources)?,
            });
        }
        Ok(records)
    }

    pub fn refresh_from_memory(&self) -> Result<usize, SkillDraftError> {
        let mut refreshed = 0;
        for active in self.active()? {
            if active.source_memory_ids.is_empty() {
                continue;
            }
            refreshed += 1;
        }
        Ok(refreshed)
    }

    pub fn list(&self) -> Result<Vec<SkillDraftRecord>, SkillDraftError> {
        let root = self.root();
        if !root.exists() {
            return Ok(Vec::new());
        }
        let mut records = Vec::new();
        for entry in fs::read_dir(&root).map_err(|source| SkillDraftError::ReadDraftDir {
            path: root.clone(),
            source,
        })? {
            let entry = entry.map_err(|source| SkillDraftError::ReadDraftDir {
                path: root.clone(),
                source,
            })?;
            let metadata_path = entry.path().join("draft.json");
            if !metadata_path.exists() {
                continue;
            }
            records.push(self.read_record(&metadata_path)?);
        }
        records.sort_by(|left, right| left.draft.id.cmp(&right.draft.id));
        Ok(records)
    }

    pub fn get(
        &self,
        draft_id: &SkillDraftId,
    ) -> Result<Option<SkillDraftRecord>, SkillDraftError> {
        let metadata_path = self.root().join(draft_id.as_str()).join("draft.json");
        if !metadata_path.exists() {
            return Ok(None);
        }
        Ok(Some(self.read_record(&metadata_path)?))
    }

    fn write_draft(&self, draft: &SkillDraft) -> Result<SkillDraftRecord, SkillDraftError> {
        let draft_dir = self.root().join(draft.id.as_str());
        fs::create_dir_all(&draft_dir).map_err(|source| SkillDraftError::CreateDraftDir {
            path: draft_dir.clone(),
            source,
        })?;
        let draft_path = draft_dir.join("SKILL.md");
        let metadata_path = draft_dir.join("draft.json");
        fs::write(&draft_path, &draft.body_markdown).map_err(|source| {
            SkillDraftError::WriteDraft {
                path: draft_path.clone(),
                source,
            }
        })?;
        let metadata_json =
            serde_json::to_string_pretty(draft).map_err(SkillDraftError::SerializeDraft)?;
        fs::write(&metadata_path, metadata_json).map_err(|source| SkillDraftError::WriteDraft {
            path: metadata_path.clone(),
            source,
        })?;

        Ok(SkillDraftRecord {
            draft: draft.clone(),
            draft_path,
            metadata_path,
        })
    }

    fn read_record(&self, metadata_path: &Path) -> Result<SkillDraftRecord, SkillDraftError> {
        let json =
            fs::read_to_string(metadata_path).map_err(|source| SkillDraftError::ReadDraft {
                path: metadata_path.to_path_buf(),
                source,
            })?;
        let draft = serde_json::from_str::<SkillDraft>(&json).map_err(|source| {
            SkillDraftError::ParseDraft {
                path: metadata_path.to_path_buf(),
                source,
            }
        })?;
        let draft_path = metadata_path
            .parent()
            .map(|path| path.join("SKILL.md"))
            .unwrap_or_else(|| self.root().join("SKILL.md"));
        Ok(SkillDraftRecord {
            draft,
            draft_path,
            metadata_path: metadata_path.to_path_buf(),
        })
    }

    fn source_agents_for_session(&self, session_id: &str) -> Result<Vec<String>, SkillDraftError> {
        let metadata_path = self
            .config
            .project_root
            .join(".localmind")
            .join("sessions")
            .join(session_id)
            .join("metadata.json");
        if !metadata_path.exists() {
            return Ok(Vec::new());
        }
        let json =
            fs::read_to_string(&metadata_path).map_err(|source| SkillDraftError::ReadDraft {
                path: metadata_path.clone(),
                source,
            })?;
        let imported = serde_json::from_str::<ImportedSession>(&json).map_err(|source| {
            SkillDraftError::ParseImportedSession {
                path: metadata_path,
                source,
            }
        })?;
        Ok(vec![format!("{:?}", imported.session.source)])
    }

    fn root(&self) -> PathBuf {
        self.config
            .project_root
            .join(".localmind")
            .join("skill-drafts")
    }

    fn connection(&self) -> Result<Connection, SkillDraftError> {
        let state_dir = self.config.project_root.join(".localmind");
        fs::create_dir_all(&state_dir).map_err(|source| SkillDraftError::CreateDraftDir {
            path: state_dir.clone(),
            source,
        })?;
        let connection = crate::schema::open_database(&state_dir.join("localmind.sqlite"))
            .map_err(SkillDraftError::Sqlite)?;
        crate::schema::migrate(&connection).map_err(SkillDraftError::Schema)?;
        Ok(connection)
    }
}

fn render_skill_markdown(draft: &SkillDraft) -> String {
    let mut output = String::new();
    output.push_str("---\n");
    output.push_str("disabled: true\n");
    output.push_str(&format!("name: {}\n", draft.name));
    if let Some(cooldown_key) = &draft.cooldown_key {
        output.push_str(&format!("cooldown_key: {cooldown_key}\n"));
    }
    if let Some(last_reviewed_at) = &draft.last_reviewed_at {
        output.push_str(&format!("last_reviewed_at: {last_reviewed_at}\n"));
    }
    output.push_str("---\n\n");
    output.push_str(&format!("# {}\n\n", draft.name));
    output.push_str(&format!("{}\n\n", draft.description));
    push_section(&mut output, "Trigger Conditions", &draft.trigger_conditions);
    push_section(&mut output, "Workflow Steps", &draft.workflow_steps);
    push_section(&mut output, "Constraints", &draft.constraints);
    push_section(&mut output, "Verification Steps", &draft.verification_steps);
    if !draft.related_memories.is_empty() {
        output.push_str("## Related Memories\n\n");
        for memory_id in &draft.related_memories {
            output.push_str(&format!("- {memory_id}\n"));
        }
        output.push('\n');
    }
    if !draft.source_agents.is_empty() {
        output.push_str("## Source Agents\n\n");
        for agent in &draft.source_agents {
            output.push_str(&format!("- {agent}\n"));
        }
        output.push('\n');
    }
    output
}

fn push_section(output: &mut String, title: &str, items: &[String]) {
    output.push_str(&format!("## {title}\n\n"));
    for item in items {
        output.push_str(&format!("- {item}\n"));
    }
    output.push('\n');
}

fn slug(input: &str) -> String {
    let mut slug = String::new();
    let mut previous_dash = false;
    for character in input.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
            previous_dash = false;
        } else if !previous_dash {
            slug.push('-');
            previous_dash = true;
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "suggested-skill".to_string()
    } else {
        slug
    }
}

#[derive(Debug, Error)]
pub enum SkillDraftError {
    #[error(transparent)]
    Config(#[from] StoreConfigError),
    #[error(transparent)]
    ReviewQueue(#[from] ReviewQueueError),
    #[error(transparent)]
    MemoryPersistence(#[from] crate::MemoryPersistenceError),
    #[error("failed to create skill draft directory {path:?}: {source}")]
    CreateDraftDir {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to read skill draft directory {path:?}: {source}")]
    ReadDraftDir {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to read skill draft {path:?}: {source}")]
    ReadDraft {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to write skill draft {path:?}: {source}")]
    WriteDraft {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to serialize skill draft: {0}")]
    SerializeDraft(serde_json::Error),
    #[error("failed to serialize skill source ids: {0}")]
    SerializeSources(serde_json::Error),
    #[error("failed to deserialize active skill: {0}")]
    DeserializeActiveSkill(serde_json::Error),
    #[error("failed to deserialize skill source ids: {0}")]
    DeserializeSources(serde_json::Error),
    #[error("failed to parse skill draft {path:?}: {source}")]
    ParseDraft {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("failed to parse imported session {path:?}: {source}")]
    ParseImportedSession {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error(transparent)]
    Schema(#[from] crate::SchemaError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}
