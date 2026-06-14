use crate::{EvidenceRef, SessionId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionSummary {
    pub session_id: SessionId,
    pub title: String,
    pub body: String,
    pub outcome: String,
    pub key_points: Vec<String>,
    #[serde(default)]
    pub digest_sections: Vec<DigestSection>,
    #[serde(default)]
    pub unresolved_risks: Vec<String>,
    #[serde(default)]
    pub stale_or_superseded: Vec<String>,
    pub evidence: Vec<EvidenceRef>,
}

impl SessionSummary {
    #[must_use]
    pub fn new(session_id: SessionId, title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            session_id,
            title: title.into(),
            body: body.into(),
            outcome: String::new(),
            key_points: Vec::new(),
            digest_sections: Vec::new(),
            unresolved_risks: Vec::new(),
            stale_or_superseded: Vec::new(),
            evidence: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DigestSection {
    pub kind: DigestSectionKind,
    pub items: Vec<String>,
}

impl DigestSection {
    #[must_use]
    pub fn new(kind: DigestSectionKind, items: Vec<String>) -> Self {
        Self { kind, items }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DigestSectionKind {
    Goal,
    Constraints,
    Progress,
    Decisions,
    NextSteps,
    CriticalContext,
    RelevantFiles,
    CommandOutcomes,
    Risks,
    StaleOrSuperseded,
}
