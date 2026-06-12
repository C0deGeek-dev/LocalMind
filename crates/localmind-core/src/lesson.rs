use crate::{ContractError, ContractResult, EvidenceRef, LessonId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CandidateLesson {
    pub id: LessonId,
    summary: String,
    pub rationale: Option<String>,
    pub category: LessonCategory,
    pub confidence: Confidence,
    evidence: Vec<EvidenceRef>,
    pub related_files: Vec<String>,
    pub related_entities: Vec<String>,
    pub suggested_destination: CandidateDestination,
    pub suggested_action: SuggestedAction,
    pub validation_status: ValidationStatus,
    #[serde(default)]
    pub review_annotation: Option<ReviewAnnotation>,
}

impl CandidateLesson {
    #[must_use]
    pub fn new(
        id: LessonId,
        summary: impl Into<String>,
        category: LessonCategory,
        confidence: Confidence,
        suggested_action: SuggestedAction,
    ) -> Self {
        Self {
            id,
            summary: summary.into(),
            rationale: None,
            category,
            confidence,
            evidence: Vec::new(),
            related_files: Vec::new(),
            related_entities: Vec::new(),
            suggested_destination: CandidateDestination::ProjectMemory,
            suggested_action,
            validation_status: ValidationStatus::Valid,
            review_annotation: None,
        }
    }

    #[must_use]
    pub fn with_evidence(mut self, evidence: EvidenceRef) -> Self {
        self.evidence.push(evidence);
        self
    }

    #[must_use]
    pub fn summary(&self) -> &str {
        &self.summary
    }

    #[must_use]
    pub fn evidence(&self) -> &[EvidenceRef] {
        &self.evidence
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ReviewAnnotation {
    pub score: Confidence,
    pub duplicate_of: Option<String>,
    pub conflict: bool,
    pub notes: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum CandidateDestination {
    ProjectMemory,
    GlobalMemory,
    SessionMemory,
    SkillDraft,
    Documentation,
    Ignore,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, PartialOrd, Serialize)]
pub struct Confidence(f32);

impl Confidence {
    pub fn new(value: f32) -> ContractResult<Self> {
        if (0.0..=1.0).contains(&value) {
            Ok(Self(value))
        } else {
            Err(ContractError::InvalidConfidence { value })
        }
    }

    #[must_use]
    pub fn value(self) -> f32 {
        self.0
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum LessonCategory {
    UserPreference,
    ProjectConvention,
    ArchitectureRule,
    CodePattern,
    DebuggingRecipe,
    ToolingNote,
    TestingStrategy,
    DeploymentRule,
    AntiPattern,
    SecurityWarning,
    DocumentationUpdate,
    CandidateSkill,
    Process,
    Other(String),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SuggestedAction {
    PromoteToMemory,
    CreateSkillDraft,
    UpdateSkillDraft,
    UpdateDocumentation,
    KeepForSession,
    Ignore,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ValidationStatus {
    Valid,
    LowConfidence,
    Duplicate,
    MissingRequiredField,
    Malformed,
}
