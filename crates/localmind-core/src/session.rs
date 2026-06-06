use crate::{EvidenceRef, SessionId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use time::OffsetDateTime;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionRecord {
    pub id: SessionId,
    pub source: SessionSource,
    pub project: Option<ProjectRef>,
    pub outcome: SessionOutcome,
    pub started_at: Option<OffsetDateTime>,
    pub ended_at: Option<OffsetDateTime>,
    pub transcript: Option<EvidenceRef>,
    pub evidence: Vec<EvidenceRef>,
    pub tool_events: Vec<ToolEvent>,
    pub command_events: Vec<CommandEvent>,
    pub file_changes: Vec<FileChange>,
    pub test_runs: Vec<TestRun>,
    pub metadata: BTreeMap<String, String>,
}

impl SessionRecord {
    #[must_use]
    pub fn new(id: SessionId, source: SessionSource, outcome: SessionOutcome) -> Self {
        Self {
            id,
            source,
            project: None,
            outcome,
            started_at: None,
            ended_at: None,
            transcript: None,
            evidence: Vec::new(),
            tool_events: Vec::new(),
            command_events: Vec::new(),
            file_changes: Vec::new(),
            test_runs: Vec::new(),
            metadata: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SessionSource {
    GenericTranscript,
    ClaudeCode,
    OpenAiCodex,
    LocalPilot,
    Other(String),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectRef {
    pub name: Option<String>,
    pub root_uri: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SessionOutcome {
    Succeeded,
    Failed,
    Cancelled,
    Partial,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ToolEvent {
    pub name: String,
    pub status: EventStatus,
    pub input_summary: Option<String>,
    pub output_summary: Option<String>,
    pub error_summary: Option<String>,
    pub started_at: Option<OffsetDateTime>,
    pub finished_at: Option<OffsetDateTime>,
    pub evidence: Vec<EvidenceRef>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CommandEvent {
    pub command: String,
    pub cwd_uri: Option<String>,
    pub status: EventStatus,
    pub exit_code: Option<i32>,
    pub output_summary: Option<String>,
    pub started_at: Option<OffsetDateTime>,
    pub finished_at: Option<OffsetDateTime>,
    pub evidence: Vec<EvidenceRef>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FileChange {
    pub path: String,
    pub kind: FileChangeKind,
    pub diff_summary: Option<String>,
    pub additions: Option<u32>,
    pub deletions: Option<u32>,
    pub evidence: Vec<EvidenceRef>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum FileChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Other(String),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TestRun {
    pub command: String,
    pub status: EventStatus,
    pub output_summary: Option<String>,
    pub started_at: Option<OffsetDateTime>,
    pub finished_at: Option<OffsetDateTime>,
    pub evidence: Vec<EvidenceRef>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum EventStatus {
    Succeeded,
    Failed,
    TimedOut,
    Skipped,
    Unknown,
}
