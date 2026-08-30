use crate::EvidenceId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EvidenceRef {
    pub id: EvidenceId,
    pub kind: EvidenceKind,
    pub label: String,
    pub uri: Option<String>,
    pub redacted: bool,
    pub content_hash: Option<String>,
    pub metadata: BTreeMap<String, String>,
}

impl EvidenceRef {
    #[must_use]
    pub fn new(kind: EvidenceKind, label: impl Into<String>) -> Self {
        let label = label.into();

        Self {
            id: EvidenceId::new(label.clone()),
            kind,
            label,
            uri: None,
            redacted: false,
            content_hash: None,
            metadata: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn redacted(mut self) -> Self {
        self.redacted = true;
        self
    }

    /// Pins the evidence to a locator (e.g. `repo@commit`).
    #[must_use]
    pub fn with_uri(mut self, uri: impl Into<String>) -> Self {
        self.uri = Some(uri.into());
        self
    }

    /// Records the content fingerprint the evidence was taken at, for staleness.
    #[must_use]
    pub fn with_content_hash(mut self, content_hash: impl Into<String>) -> Self {
        self.content_hash = Some(content_hash.into());
        self
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum EvidenceKind {
    Transcript,
    ToolEvent,
    Command,
    FileDiff,
    TestOutput,
    Commit,
    /// A source span or import read deterministically from a syntax tree.
    CodeParse,
    RecoveryEvent,
    UserCorrection,
    ManualNote,
    Other(String),
}
