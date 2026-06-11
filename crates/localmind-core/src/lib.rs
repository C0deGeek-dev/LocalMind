//! Host-neutral learning contracts for LocalMind.
//!
//! This crate owns the shared domain model. Host runtimes such as LocalPilot map
//! their native session and tool records into these contracts at the adapter
//! edge; this crate must not depend on any host-specific type.

mod adapter;
mod audit;
mod context;
mod error;
mod evidence;
mod graph;
mod lesson;
mod memory;
mod review;
mod session;
mod skill;
mod summary;

pub use adapter::{
    AgentSessionAdapter, AgentSessionInput, HostMappingRequirement, HostSessionMapper,
};
pub use audit::{AuditEventKind, LearningAuditEvent};
pub use context::{ContextPack, ContextQuery, ContextSource};
pub use error::{ContractError, ContractResult};
pub use evidence::{EvidenceKind, EvidenceRef};
pub use graph::{
    content_fingerprint, stable_edge_id, stable_node_id, EdgeDerivation, EdgeKind, GraphEdge,
    GraphEndpoint, GraphNode, NodeKind, SourceLocation, TypeShape,
};
pub use lesson::{
    CandidateDestination, CandidateLesson, Confidence, LessonCategory, SuggestedAction,
    ValidationStatus,
};
pub use memory::{MemoryEntry, MemoryScope, MemoryStatus};
pub use review::{ReviewAction, ReviewDecision, ReviewItem, ReviewState};
pub use session::{
    CommandEvent, EventStatus, FileChange, FileChangeKind, ProjectRef, SessionOutcome,
    SessionRecord, SessionSource, TestRun, ToolEvent,
};
pub use skill::SkillDraft;
pub use summary::SessionSummary;

use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};

macro_rules! string_id {
    ($name:ident) => {
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
        pub struct $name(String);

        impl $name {
            #[must_use]
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl Display for $name {
            fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

string_id!(AuditEventId);
string_id!(EvidenceId);
string_id!(GraphEdgeId);
string_id!(GraphNodeId);
string_id!(LessonId);
string_id!(MemoryEntryId);
string_id!(ReviewItemId);
string_id!(SessionId);
string_id!(SkillDraftId);

#[cfg(test)]
mod tests {
    use super::{
        AgentSessionAdapter, AgentSessionInput, CandidateLesson, Confidence, EvidenceKind,
        EvidenceRef, HostSessionMapper, LessonCategory, LessonId, SessionId, SessionOutcome,
        SessionRecord, SessionSource, SessionSummary, SuggestedAction,
    };
    use std::collections::BTreeMap;

    #[test]
    fn neutral_session_contract_serializes_without_host_types(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let session = SessionRecord::new(
            SessionId::new("session-1"),
            SessionSource::LocalPilot,
            SessionOutcome::Succeeded,
        );

        let json = serde_json::to_string(&session)?;

        assert!(json.contains("LocalPilot"));
        assert!(!json.contains("localpilot_store"));
        Ok(())
    }

    #[test]
    fn candidate_lesson_keeps_reviewable_evidence() -> Result<(), Box<dyn std::error::Error>> {
        let evidence = EvidenceRef::new(EvidenceKind::Transcript, "redacted transcript").redacted();
        let lesson = CandidateLesson::new(
            LessonId::new("lesson-1"),
            "Prefer reviewed memory writes over automatic promotion.",
            LessonCategory::Process,
            Confidence::new(0.8)?,
            SuggestedAction::PromoteToMemory,
        )
        .with_evidence(evidence);

        assert_eq!(lesson.evidence().len(), 1);
        assert_eq!(
            lesson.summary(),
            "Prefer reviewed memory writes over automatic promotion."
        );
        Ok(())
    }

    #[test]
    fn session_summary_points_back_to_source_session() {
        let summary = SessionSummary::new(
            SessionId::new("session-1"),
            "Fixed review queue behavior",
            "The session added an explicit review state transition.",
        );

        assert_eq!(summary.session_id.as_str(), "session-1");
    }

    #[test]
    fn localpilot_adapter_maps_to_neutral_session_record() -> Result<(), Box<dyn std::error::Error>>
    {
        let adapter = AgentSessionAdapter::localpilot();
        let input = AgentSessionInput {
            id: "session-1".to_string(),
            project_root_uri: Some("file:///workspace/project".to_string()),
            transcript_label: "localpilot session bundle".to_string(),
            metadata: BTreeMap::from([("host".to_string(), "localpilot".to_string())]),
        };

        let session = adapter.map_session(&input)?;
        let project = session.project.as_ref().ok_or("missing project")?;
        let transcript = session.transcript.as_ref().ok_or("missing transcript")?;

        assert_eq!(session.source, SessionSource::LocalPilot);
        assert_eq!(project.root_uri, "file:///workspace/project");
        assert_eq!(
            session.metadata.get("host").map(String::as_str),
            Some("localpilot")
        );
        assert!(transcript.redacted);
        Ok(())
    }
}
