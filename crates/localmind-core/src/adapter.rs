use crate::{
    ContractResult, EvidenceKind, EvidenceRef, ProjectRef, SessionId, SessionOutcome,
    SessionRecord, SessionSource,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Maps a host-owned session representation into the neutral LocalMind contract.
///
/// Implementations live in host adapter crates. For example, Unshackled may
/// implement this trait for its exported session bundle type, but this crate
/// must not import or name that type directly.
pub trait HostSessionMapper<HostSession> {
    fn map_session(&self, host_session: &HostSession) -> ContractResult<SessionRecord>;
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentSessionInput {
    pub id: String,
    pub project_root_uri: Option<String>,
    pub transcript_label: String,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentSessionAdapter {
    source: SessionSource,
}

impl AgentSessionAdapter {
    #[must_use]
    pub fn generic_transcript() -> Self {
        Self {
            source: SessionSource::GenericTranscript,
        }
    }

    #[must_use]
    pub fn claude_code() -> Self {
        Self {
            source: SessionSource::ClaudeCode,
        }
    }

    #[must_use]
    pub fn open_ai_codex() -> Self {
        Self {
            source: SessionSource::OpenAiCodex,
        }
    }

    #[must_use]
    pub fn unshackled() -> Self {
        Self {
            source: SessionSource::Unshackled,
        }
    }
}

impl HostSessionMapper<AgentSessionInput> for AgentSessionAdapter {
    fn map_session(&self, host_session: &AgentSessionInput) -> ContractResult<SessionRecord> {
        let mut record = SessionRecord::new(
            SessionId::new(host_session.id.clone()),
            self.source.clone(),
            SessionOutcome::Unknown,
        );
        if let Some(root_uri) = &host_session.project_root_uri {
            record.project = Some(ProjectRef {
                name: None,
                root_uri: root_uri.clone(),
            });
        }
        let evidence = EvidenceRef::new(
            EvidenceKind::Transcript,
            host_session.transcript_label.clone(),
        )
        .redacted();
        record.transcript = Some(evidence.clone());
        record.evidence.push(evidence);
        record.metadata = host_session.metadata.clone();
        Ok(record)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostMappingRequirement {
    pub source_field: &'static str,
    pub localmind_field: &'static str,
    pub required: bool,
}
