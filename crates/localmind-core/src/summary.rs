use crate::{EvidenceRef, SessionId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionSummary {
    pub session_id: SessionId,
    pub title: String,
    pub body: String,
    pub outcome: String,
    pub key_points: Vec<String>,
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
            evidence: Vec::new(),
        }
    }
}
