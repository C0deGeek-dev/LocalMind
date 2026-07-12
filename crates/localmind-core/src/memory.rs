use crate::{Confidence, EvidenceRef, LessonCategory, MemoryEntryId, SessionId};
use crate::{SyncDisposition, SyncMeta};
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
    pub related_files: Vec<String>,
    pub related_entities: Vec<String>,
    pub created_at: Option<OffsetDateTime>,
    pub updated_at: Option<OffsetDateTime>,
    pub supersedes: Vec<MemoryEntryId>,
    pub contradicts: Vec<MemoryEntryId>,
    pub status: MemoryStatus,
    /// Cross-device sync state (disposition override + origin machine). Defaults
    /// empty so an entry that predates sync — or one deserialized from an older
    /// bundle — keeps its exact prior meaning.
    #[serde(default)]
    pub sync_meta: SyncMeta,
}

impl MemoryEntry {
    /// The sync disposition this memory actually takes: the per-memory override
    /// if set, otherwise the per-scope default.
    #[must_use]
    pub fn effective_disposition(&self) -> SyncDisposition {
        self.sync_meta.effective_disposition(&self.scope)
    }

    /// Whether this memory participates in cross-device sync at all.
    #[must_use]
    pub fn syncs(&self) -> bool {
        self.effective_disposition().syncs()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
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

/// How a memory should be trusted at retrieval time: is it something observed,
/// a guess, an established fact, a decision the user made, or a procedure to
/// follow? Deterministically classified from the lesson category so the agent
/// can say *what kind* of knowledge it is using.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EpistemicStatus {
    /// Something seen in the project (a tooling note, a doc update).
    Observation,
    /// An inferred or tentative claim not yet confirmed.
    Hypothesis,
    /// An established, low-volatility truth (e.g. a security warning).
    Fact,
    /// A choice the user or project made (a preference, convention, rule).
    Decision,
    /// A repeatable how-to (a pattern, recipe, strategy, workflow, skill).
    Procedure,
}

impl EpistemicStatus {
    /// Deterministic mapping from a lesson category to its epistemic status.
    /// Total over every category, so every accepted memory has a status.
    #[must_use]
    pub fn from_category(category: &LessonCategory) -> Self {
        match category {
            LessonCategory::UserPreference
            | LessonCategory::ProjectConvention
            | LessonCategory::ArchitectureRule
            | LessonCategory::DeploymentRule
            | LessonCategory::AntiPattern => EpistemicStatus::Decision,
            LessonCategory::CodePattern
            | LessonCategory::DebuggingRecipe
            | LessonCategory::TestingStrategy
            | LessonCategory::CandidateSkill
            | LessonCategory::Process
            | LessonCategory::ToolUse => EpistemicStatus::Procedure,
            LessonCategory::SecurityWarning => EpistemicStatus::Fact,
            LessonCategory::ToolingNote
            | LessonCategory::DocumentationUpdate
            | LessonCategory::Other(_) => EpistemicStatus::Observation,
        }
    }

    /// The stored/serialized token for this status.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            EpistemicStatus::Observation => "observation",
            EpistemicStatus::Hypothesis => "hypothesis",
            EpistemicStatus::Fact => "fact",
            EpistemicStatus::Decision => "decision",
            EpistemicStatus::Procedure => "procedure",
        }
    }

    /// Parse a stored token back to a status; unknown tokens read as
    /// `Observation` (the most conservative trust level).
    #[must_use]
    pub fn from_token(token: &str) -> Self {
        match token {
            "hypothesis" => EpistemicStatus::Hypothesis,
            "fact" => EpistemicStatus::Fact,
            "decision" => EpistemicStatus::Decision,
            "procedure" => EpistemicStatus::Procedure,
            _ => EpistemicStatus::Observation,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::EpistemicStatus;
    use crate::LessonCategory;

    #[test]
    fn every_category_classifies_and_round_trips() {
        let categories = [
            LessonCategory::UserPreference,
            LessonCategory::ProjectConvention,
            LessonCategory::ArchitectureRule,
            LessonCategory::CodePattern,
            LessonCategory::DebuggingRecipe,
            LessonCategory::ToolingNote,
            LessonCategory::TestingStrategy,
            LessonCategory::DeploymentRule,
            LessonCategory::AntiPattern,
            LessonCategory::SecurityWarning,
            LessonCategory::DocumentationUpdate,
            LessonCategory::CandidateSkill,
            LessonCategory::Process,
            LessonCategory::ToolUse,
            LessonCategory::Other("custom".to_string()),
        ];
        for category in &categories {
            let status = EpistemicStatus::from_category(category);
            // The stored token round-trips back to the same status.
            assert_eq!(EpistemicStatus::from_token(status.as_str()), status);
        }
    }

    #[test]
    fn categories_map_to_their_expected_status() {
        assert_eq!(
            EpistemicStatus::from_category(&LessonCategory::SecurityWarning),
            EpistemicStatus::Fact
        );
        assert_eq!(
            EpistemicStatus::from_category(&LessonCategory::UserPreference),
            EpistemicStatus::Decision
        );
        assert_eq!(
            EpistemicStatus::from_category(&LessonCategory::DebuggingRecipe),
            EpistemicStatus::Procedure
        );
        assert_eq!(
            EpistemicStatus::from_category(&LessonCategory::ToolingNote),
            EpistemicStatus::Observation
        );
    }
}
