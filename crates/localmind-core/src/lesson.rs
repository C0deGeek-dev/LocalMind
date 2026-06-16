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
    /// The full tool-use lesson, present when `category` is
    /// [`LessonCategory::ToolUse`]. `None` for every other category.
    #[serde(default)]
    pub tool_use: Option<ToolUseLesson>,
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
            tool_use: None,
        }
    }

    #[must_use]
    pub fn with_evidence(mut self, evidence: EvidenceRef) -> Self {
        self.evidence.push(evidence);
        self
    }

    /// Attach the full tool-use lesson (for a `LessonCategory::ToolUse` candidate).
    #[must_use]
    pub fn with_tool_use(mut self, lesson: ToolUseLesson) -> Self {
        self.tool_use = Some(lesson);
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
    /// A reusable, verified tool-use pattern (the full lesson lives in a
    /// [`ToolUseLesson`]). Promotion-eligible only from a verified trajectory.
    ToolUse,
    Other(String),
}

/// A reusable, verified tool-use pattern learned from a trajectory (research
/// §10). Carried alongside a `CandidateLesson` whose category is
/// [`LessonCategory::ToolUse`]; it is its own struct, never flattened into an
/// id-bearing envelope.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ToolUseLesson {
    /// Situations this lesson applies to (retrieval cues).
    pub context_cues: Vec<String>,
    /// The tool this lesson is about.
    pub tool: String,
    /// The tool-contract version it was learned against; a bump can invalidate
    /// it (see `invalidation`).
    pub tool_version: u32,
    /// Conditions that must hold before the action sequence.
    pub preconditions: Vec<String>,
    /// The ordered steps that worked.
    pub action_sequence: Vec<String>,
    /// What a correct run should observe.
    pub expected_observations: Vec<String>,
    /// How the outcome was verified.
    pub verification: String,
    /// Observed failures paired with the recovery that worked — never a
    /// standalone "do this".
    pub failure_recovery: Vec<FailureRecovery>,
    pub confidence: Confidence,
    /// Where this lesson came from (a session / evidence reference).
    pub provenance: String,
    /// When the lesson was last confirmed against a verified trajectory.
    pub last_verified: Option<String>,
    /// When the lesson stops being trusted.
    pub invalidation: InvalidationRule,
    /// How widely the lesson applies.
    pub scope: LessonScope,
}

/// An observed failure and the recovery that resolved it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FailureRecovery {
    pub failure: String,
    pub recovery: String,
}

/// When a tool-use lesson stops being trusted.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum InvalidationRule {
    /// Invalidated when the tool's contract version bumps.
    OnToolVersionBump,
    /// Only a human review invalidates it.
    Manual,
    /// Never auto-invalidated.
    Never,
}

/// How widely a tool-use lesson applies.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum LessonScope {
    Global,
    Project,
    /// Applies only to a named model (off by default until a measured per-model
    /// failure justifies it).
    Model(String),
}

impl ToolUseLesson {
    /// Whether a tool-version bump has made this lesson stale: it is invalidated
    /// on a bump and the current version is newer than the one it learned.
    #[must_use]
    pub fn is_stale_for(&self, current_tool_version: u32) -> bool {
        matches!(self.invalidation, InvalidationRule::OnToolVersionBump)
            && current_tool_version > self.tool_version
    }
}

/// A completed tool-use trajectory a host offers for promotion. The host fills
/// `verified` from its verifier's verdict — a fact keyed by the event log, not
/// re-parsed prose — and `degraded_or_looping` from recovery state.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolUseTrajectory {
    pub id: LessonId,
    pub summary: String,
    /// The source trajectory's verifier verdict was `Verified`.
    pub verified: bool,
    /// The trajectory ended degraded or in a tool loop.
    pub degraded_or_looping: bool,
    pub lesson: ToolUseLesson,
    pub evidence: EvidenceRef,
}

/// Promote a tool-use trajectory to a candidate lesson — **only** when it was
/// verified and not degraded or looping. A failed or unverified trajectory
/// yields no standalone lesson (it stays episodic); a failure survives only as
/// the `failure_recovery` of a verified lesson, never as its own "do this".
/// The candidate still flows through the normal review gate.
#[must_use]
pub fn promote_tool_use(trajectory: &ToolUseTrajectory) -> Option<CandidateLesson> {
    if !trajectory.verified || trajectory.degraded_or_looping {
        return None;
    }
    Some(
        CandidateLesson::new(
            trajectory.id.clone(),
            trajectory.summary.clone(),
            LessonCategory::ToolUse,
            trajectory.lesson.confidence,
            SuggestedAction::PromoteToMemory,
        )
        .with_evidence(trajectory.evidence.clone())
        .with_tool_use(trajectory.lesson.clone()),
    )
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SuggestedAction {
    PromoteToMemory,
    CreateSkillDraft,
    UpdateSkillDraft,
    UpdateDocumentation,
    KeepForSession,
    Ignore,
    /// Merge this candidate's evidence into an existing related memory rather
    /// than creating a near-duplicate. The reviewer selects the target memory;
    /// this is a suggestion only, never a direct write.
    MergeIntoExisting,
    /// Replace prior accepted guidance that this candidate corrects or makes
    /// stale. The reviewer selects the target memory to supersede.
    SupersedeExisting,
    /// This candidate bundles multiple distinct facts; the reviewer should split
    /// it into separate memories before promotion.
    Split,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ValidationStatus {
    Valid,
    LowConfidence,
    Duplicate,
    MissingRequiredField,
    Malformed,
}

#[cfg(test)]
mod tool_use_tests {
    use super::*;
    use crate::{EvidenceKind, EvidenceRef, LessonId};

    fn sample() -> ToolUseLesson {
        ToolUseLesson {
            context_cues: vec!["overwrite an existing config file".to_string()],
            tool: "write_file".to_string(),
            tool_version: 1,
            preconditions: vec!["the path was read this session".to_string()],
            action_sequence: vec!["read_file".to_string(), "write_file".to_string()],
            expected_observations: vec!["the file exists with the new content".to_string()],
            verification: "read back confirms the content".to_string(),
            failure_recovery: vec![FailureRecovery {
                failure: "write rejected: not read first".to_string(),
                recovery: "read_file, then retry write_file".to_string(),
            }],
            confidence: Confidence::new(0.8).unwrap(),
            provenance: "session:abc#evt:42".to_string(),
            last_verified: Some("2026-06-16T00:00:00Z".to_string()),
            invalidation: InvalidationRule::OnToolVersionBump,
            scope: LessonScope::Project,
        }
    }

    #[test]
    fn a_tool_use_lesson_roundtrips_through_json() {
        let lesson = sample();
        let json = serde_json::to_string(&lesson).unwrap();
        // The lesson is its own object, not flattened into an id-bearing envelope.
        assert!(json.contains("action_sequence"));
        let back: ToolUseLesson = serde_json::from_str(&json).unwrap();
        assert_eq!(lesson, back);
    }

    #[test]
    fn a_tool_version_bump_makes_an_on_bump_lesson_stale() {
        let lesson = sample();
        assert!(!lesson.is_stale_for(1), "same version is not stale");
        assert!(
            lesson.is_stale_for(2),
            "a newer tool version invalidates it"
        );
    }

    #[test]
    fn a_never_invalidated_lesson_survives_a_version_bump() {
        let mut lesson = sample();
        lesson.invalidation = InvalidationRule::Never;
        assert!(!lesson.is_stale_for(99));
    }

    fn trajectory(verified: bool, degraded: bool) -> ToolUseTrajectory {
        ToolUseTrajectory {
            id: LessonId::new("lesson-1"),
            summary: "read before overwriting a config file".to_string(),
            verified,
            degraded_or_looping: degraded,
            lesson: sample(),
            evidence: EvidenceRef::new(EvidenceKind::Transcript, "redacted").redacted(),
        }
    }

    #[test]
    fn a_verified_trajectory_yields_a_tool_use_candidate() {
        let candidate = promote_tool_use(&trajectory(true, false)).expect("verified -> candidate");
        assert_eq!(candidate.category, LessonCategory::ToolUse);
        assert!(candidate.tool_use.is_some());
        // The failure→recovery the lesson carries is preserved, never standalone.
        assert!(!candidate.tool_use.unwrap().failure_recovery.is_empty());
    }

    #[test]
    fn an_unverified_trajectory_stays_episodic() {
        assert!(promote_tool_use(&trajectory(false, false)).is_none());
    }

    #[test]
    fn a_degraded_or_looping_trajectory_is_not_promoted() {
        assert!(promote_tool_use(&trajectory(true, true)).is_none());
    }
}
