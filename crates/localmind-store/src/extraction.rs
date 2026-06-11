use crate::{ImportedSession, ProjectConfig, ReviewQueue, ReviewQueueError, StoreConfigError};
use localmind_core::{
    CandidateDestination, CandidateLesson, Confidence, ContractError, EvidenceKind, EvidenceRef,
    LessonCategory, LessonId, SessionId, SessionSummary, SuggestedAction, ValidationStatus,
};
use serde::Serialize;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtractionInput {
    pub session_id: SessionId,
    pub transcript: String,
    pub transcript_evidence: EvidenceRef,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ExtractionOutput {
    pub summary: SessionSummary,
    pub candidates: Vec<CandidateLesson>,
}

pub trait SessionExtractor {
    fn extract(&self, input: ExtractionInput) -> Result<ExtractionOutput, CloseoutError>;
}

pub struct DeterministicExtractor;

impl SessionExtractor for DeterministicExtractor {
    fn extract(&self, input: ExtractionInput) -> Result<ExtractionOutput, CloseoutError> {
        let summary = summarize_transcript(&input);
        let candidates = extract_candidates(&input)?;

        Ok(ExtractionOutput {
            summary,
            candidates,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CloseoutReport {
    pub session_id: SessionId,
    pub summary_path: PathBuf,
    pub candidates_path: PathBuf,
    pub candidate_count: usize,
    pub enqueued_count: usize,
}

pub struct CloseoutProcessor;

impl CloseoutProcessor {
    pub fn closeout_project_session(
        project_root: impl AsRef<Path>,
        session_id: &SessionId,
        extractor: &impl SessionExtractor,
    ) -> Result<CloseoutReport, CloseoutError> {
        let config = ProjectConfig::discover(project_root).map_err(CloseoutError::Config)?;
        let session_dir = config
            .project_root
            .join(".localmind")
            .join("sessions")
            .join(session_id.as_str());
        let metadata_path = session_dir.join("metadata.json");
        let transcript_path = session_dir.join("transcript.redacted.txt");

        let metadata =
            fs::read_to_string(&metadata_path).map_err(|source| CloseoutError::ReadMetadata {
                path: metadata_path.clone(),
                source,
            })?;
        let imported = serde_json::from_str::<ImportedSession>(&metadata).map_err(|source| {
            CloseoutError::ParseMetadata {
                path: metadata_path.clone(),
                source,
            }
        })?;
        let transcript = fs::read_to_string(&transcript_path).map_err(|source| {
            CloseoutError::ReadTranscript {
                path: transcript_path.clone(),
                source,
            }
        })?;

        let evidence = EvidenceRef::new(EvidenceKind::Transcript, "redacted transcript").redacted();
        let input = ExtractionInput {
            session_id: imported.session.id,
            transcript,
            transcript_evidence: evidence,
        };
        let output = extractor.extract(input)?;
        validate_candidates(&output.candidates)?;

        let summary_path = session_dir.join("summary.json");
        let candidates_path = session_dir.join("candidates.json");
        let summary_json = serde_json::to_string_pretty(&output.summary)
            .map_err(|source| CloseoutError::SerializeSummary { source })?;
        let candidates_json = serde_json::to_string_pretty(&output.candidates)
            .map_err(|source| CloseoutError::SerializeCandidates { source })?;

        fs::write(&summary_path, summary_json).map_err(|source| CloseoutError::WriteSummary {
            path: summary_path.clone(),
            source,
        })?;
        fs::write(&candidates_path, candidates_json).map_err(|source| {
            CloseoutError::WriteCandidates {
                path: candidates_path.clone(),
                source,
            }
        })?;
        let queue = ReviewQueue::open_project(&config.project_root)?;
        let enqueued_count = queue.enqueue_candidates(session_id, &output.candidates)?;

        Ok(CloseoutReport {
            session_id: session_id.clone(),
            summary_path,
            candidates_path,
            candidate_count: output.candidates.len(),
            enqueued_count,
        })
    }
}

fn summarize_transcript(input: &ExtractionInput) -> SessionSummary {
    let first_line = input
        .transcript
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("Imported session");
    let mut summary = SessionSummary::new(
        input.session_id.clone(),
        format!("Session {}", input.session_id),
        first_line,
    );
    summary.outcome = "unknown".to_string();
    summary.key_points = input
        .transcript
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(5)
        .map(ToString::to_string)
        .collect();
    summary.evidence.push(input.transcript_evidence.clone());
    summary
}

/// Most candidates a single heuristic family may contribute per session,
/// so one noisy signal cannot flood the review queue.
const MAX_CANDIDATES_PER_FAMILY: usize = 5;

fn extract_candidates(input: &ExtractionInput) -> Result<Vec<CandidateLesson>, CloseoutError> {
    let mut seen = BTreeSet::new();
    let mut candidates = Vec::new();
    let lines: Vec<&str> = input.transcript.lines().map(str::trim).collect();

    // Family 1: explicit "Lesson:" markers — the author already decided this
    // is worth keeping, so these carry the highest confidence.
    for line in &lines {
        let Some(summary) = lesson_summary(line) else {
            continue;
        };
        if summary.is_empty() || !seen.insert(summary.to_ascii_lowercase()) {
            continue;
        }

        let mut candidate = CandidateLesson::new(
            LessonId::new(candidate_id(&input.session_id, summary)),
            summary,
            LessonCategory::Process,
            Confidence::new(0.8)?,
            SuggestedAction::PromoteToMemory,
        )
        .with_evidence(input.transcript_evidence.clone());
        candidate.suggested_destination = CandidateDestination::ProjectMemory;
        candidates.push(candidate);
    }

    // Family 2: skill/workflow keyword lines.
    for line in &lines {
        let lower = line.to_ascii_lowercase();
        if !(lower.contains("skill") || lower.contains("workflow")) {
            continue;
        }

        let summary = line.trim_start_matches("- ").trim();
        if summary.is_empty() || !seen.insert(summary.to_ascii_lowercase()) {
            continue;
        }

        let mut candidate = CandidateLesson::new(
            LessonId::new(candidate_id(&input.session_id, summary)),
            summary,
            LessonCategory::CandidateSkill,
            Confidence::new(0.65)?,
            SuggestedAction::CreateSkillDraft,
        )
        .with_evidence(input.transcript_evidence.clone());
        candidate.suggested_destination = CandidateDestination::SkillDraft;
        candidates.push(candidate);
    }

    // Family 3: failure→resolution pairs — a debugging recipe worth keeping.
    for summary in failure_resolution_summaries(&lines)
        .into_iter()
        .take(MAX_CANDIDATES_PER_FAMILY)
    {
        if !seen.insert(summary.to_ascii_lowercase()) {
            continue;
        }
        let mut candidate = CandidateLesson::new(
            LessonId::new(candidate_id(&input.session_id, &summary)),
            &summary,
            LessonCategory::DebuggingRecipe,
            Confidence::new(0.6)?,
            SuggestedAction::PromoteToMemory,
        )
        .with_evidence(input.transcript_evidence.clone());
        candidate.suggested_destination = CandidateDestination::ProjectMemory;
        candidates.push(candidate);
    }

    // Family 4: commands repeated within one session — workflow material.
    for summary in repeated_command_summaries(&lines)
        .into_iter()
        .take(MAX_CANDIDATES_PER_FAMILY)
    {
        if !seen.insert(summary.to_ascii_lowercase()) {
            continue;
        }
        let mut candidate = CandidateLesson::new(
            LessonId::new(candidate_id(&input.session_id, &summary)),
            &summary,
            LessonCategory::CandidateSkill,
            Confidence::new(0.55)?,
            SuggestedAction::CreateSkillDraft,
        )
        .with_evidence(input.transcript_evidence.clone());
        candidate.suggested_destination = CandidateDestination::SkillDraft;
        candidates.push(candidate);
    }

    // Family 5: explicit user corrections — durable preference signals.
    for summary in user_correction_summaries(&lines)
        .into_iter()
        .take(MAX_CANDIDATES_PER_FAMILY)
    {
        if !seen.insert(summary.to_ascii_lowercase()) {
            continue;
        }
        let mut candidate = CandidateLesson::new(
            LessonId::new(candidate_id(&input.session_id, &summary)),
            &summary,
            LessonCategory::UserPreference,
            Confidence::new(0.6)?,
            SuggestedAction::PromoteToMemory,
        )
        .with_evidence(input.transcript_evidence.clone());
        candidate.suggested_destination = CandidateDestination::ProjectMemory;
        candidates.push(candidate);
    }

    Ok(candidates)
}

/// How many lines after a failure line a resolution may appear and still be
/// treated as the answer to that failure.
const RESOLUTION_WINDOW: usize = 30;
/// Keep extracted line fragments readable inside a one-line summary.
const FRAGMENT_MAX_CHARS: usize = 120;

fn failure_resolution_summaries(lines: &[&str]) -> Vec<String> {
    let mut summaries = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        if !is_failure_line(lines[index]) {
            index += 1;
            continue;
        }
        let window_end = lines.len().min(index + 1 + RESOLUTION_WINDOW);
        let Some(resolution_offset) = lines[index + 1..window_end]
            .iter()
            .position(|line| is_resolution_line(line))
        else {
            index += 1;
            continue;
        };
        let resolution_index = index + 1 + resolution_offset;
        summaries.push(format!(
            "When \"{}\" occurred, the session resolved it with: \"{}\"",
            truncate_fragment(strip_speaker(lines[index])),
            truncate_fragment(strip_speaker(lines[resolution_index])),
        ));
        // Continue past the resolution so one fix is not paired with several
        // preceding failure lines.
        index = resolution_index + 1;
    }
    summaries
}

fn is_failure_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    if lower.contains("0 errors") || lower.contains("no errors") {
        return false;
    }
    [
        "error",
        "failed",
        "failure",
        "exception",
        "panicked",
        "traceback",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn is_resolution_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    [
        "fixed",
        "fix was",
        "fix:",
        "resolved",
        "works now",
        "passing",
        "passed",
        "succeeded",
        "solution",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn repeated_command_summaries(lines: &[&str]) -> Vec<String> {
    let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    let mut order = Vec::new();
    for line in lines {
        let Some(command) = command_text(line) else {
            continue;
        };
        // Very short commands repeat for trivial reasons (ls, cd).
        if command.len() < 8 {
            continue;
        }
        let entry = counts.entry(command).or_insert(0);
        *entry += 1;
        if *entry == 2 {
            order.push(command);
        }
    }
    order
        .into_iter()
        .map(|command| {
            format!(
                "Command repeated during this session (candidate for a reusable workflow): {}",
                truncate_fragment(command)
            )
        })
        .collect()
}

fn command_text(line: &str) -> Option<&str> {
    for prefix in ["$ ", "> ", "PS> ", "❯ "] {
        if let Some(rest) = line.strip_prefix(prefix) {
            let rest = rest.trim();
            if !rest.is_empty() {
                return Some(rest);
            }
        }
    }
    None
}

fn user_correction_summaries(lines: &[&str]) -> Vec<String> {
    lines
        .iter()
        .filter_map(|line| {
            let lower = line.to_ascii_lowercase();
            let rest = lower.strip_prefix("user:")?;
            let text = line[line.len() - rest.len()..].trim();
            let rest = rest.trim_start();
            let is_correction = [
                "no,",
                "no ",
                "actually",
                "instead",
                "don't",
                "do not",
                "stop ",
                "that's wrong",
                "that is wrong",
                "not what",
            ]
            .iter()
            .any(|marker| rest.starts_with(marker));
            if !is_correction || text.len() < 12 {
                return None;
            }
            Some(format!("User correction: {}", truncate_fragment(text)))
        })
        .collect()
}

fn strip_speaker(line: &str) -> &str {
    let lower = line.to_ascii_lowercase();
    for speaker in ["user:", "assistant:", "system:", "tool:"] {
        if lower.starts_with(speaker) {
            return line[speaker.len()..].trim_start();
        }
    }
    line
}

fn truncate_fragment(text: &str) -> String {
    if text.chars().count() <= FRAGMENT_MAX_CHARS {
        return text.to_string();
    }
    let truncated: String = text.chars().take(FRAGMENT_MAX_CHARS).collect();
    format!("{truncated}…")
}

fn lesson_summary(line: &str) -> Option<&str> {
    line.strip_prefix("Lesson:")
        .or_else(|| line.strip_prefix("lesson:"))
        .or_else(|| line.split_once("Lesson:").map(|(_, rest)| rest))
        .or_else(|| line.split_once("lesson:").map(|(_, rest)| rest))
        .map(str::trim)
}

fn candidate_id(session_id: &SessionId, summary: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in format!("{session_id}\n{summary}").as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }

    format!("lesson-{hash:016x}")
}

fn validate_candidates(candidates: &[CandidateLesson]) -> Result<(), CloseoutError> {
    let mut seen = BTreeSet::new();

    for candidate in candidates {
        if candidate.summary().trim().is_empty() {
            return Err(CloseoutError::InvalidCandidate {
                reason: "summary is required".to_string(),
            });
        }

        if candidate.confidence.value() < 0.5 {
            return Err(CloseoutError::InvalidCandidate {
                reason: "confidence is below 0.5".to_string(),
            });
        }

        if !seen.insert(candidate.summary().to_ascii_lowercase()) {
            return Err(CloseoutError::InvalidCandidate {
                reason: "duplicate candidate summary".to_string(),
            });
        }

        if matches!(
            candidate.validation_status,
            ValidationStatus::Malformed | ValidationStatus::MissingRequiredField
        ) {
            return Err(CloseoutError::InvalidCandidate {
                reason: format!(
                    "candidate validation failed: {:?}",
                    candidate.validation_status
                ),
            });
        }
    }

    Ok(())
}

#[derive(Debug, Error)]
pub enum CloseoutError {
    #[error(transparent)]
    Config(#[from] StoreConfigError),
    #[error("failed to read import metadata {path:?}: {source}")]
    ReadMetadata {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse import metadata {path:?}: {source}")]
    ParseMetadata {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("failed to read redacted transcript {path:?}: {source}")]
    ReadTranscript {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid candidate lesson: {reason}")]
    InvalidCandidate { reason: String },
    #[error(transparent)]
    Contract(#[from] ContractError),
    #[error("failed to serialize session summary: {source}")]
    SerializeSummary { source: serde_json::Error },
    #[error("failed to serialize candidate lessons: {source}")]
    SerializeCandidates { source: serde_json::Error },
    #[error("failed to write session summary {path:?}: {source}")]
    WriteSummary {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to write candidate lessons {path:?}: {source}")]
    WriteCandidates {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(transparent)]
    ReviewQueue(#[from] ReviewQueueError),
}
