use crate::{ImportedSession, ProjectConfig, ReviewQueue, ReviewQueueError, StoreConfigError};
use localmind_core::{
    CandidateDestination, CandidateLesson, Confidence, ContractError, DigestSection,
    DigestSectionKind, EvidenceKind, EvidenceRef, LessonCategory, LessonId, ReviewAnnotation,
    SessionId, SessionSummary, SuggestedAction, ValidationStatus,
};
use localmind_inference::{ChatEndpoint, ChatMessage, InferenceCapability, InferenceError};
use serde::Deserialize;
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

pub struct ModelBackedExtractor<'a> {
    chat: Option<&'a ChatEndpoint>,
    fallback: DeterministicExtractor,
}

impl<'a> ModelBackedExtractor<'a> {
    #[must_use]
    pub fn new(capability: &'a InferenceCapability) -> Self {
        Self {
            chat: capability.chat(),
            fallback: DeterministicExtractor,
        }
    }
}

impl SessionExtractor for ModelBackedExtractor<'_> {
    fn extract(&self, input: ExtractionInput) -> Result<ExtractionOutput, CloseoutError> {
        let Some(chat) = self.chat else {
            return self.fallback.extract(input);
        };
        let prompt = format!(
            "Extract durable LocalMind session memory as compact JSON. Return only JSON with fields summary_title, summary_body, key_points, and candidates. Each candidate needs summary, category, confidence, action. Transcript:\n{}",
            input.transcript
        );
        let completion = match chat.complete(&[
            ChatMessage::system("You extract local development lessons. Return valid JSON only."),
            ChatMessage::user(prompt),
        ]) {
            Ok(completion) => completion,
            Err(_source) => return self.fallback.extract(input),
        };
        let parsed: ModelExtraction =
            serde_json::from_str(&completion.content).map_err(CloseoutError::ModelOutput)?;
        parsed.into_output(input)
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

        let mut evidence =
            EvidenceRef::new(EvidenceKind::Transcript, "redacted transcript").redacted();
        evidence
            .metadata
            .insert("range".to_string(), "full_transcript".to_string());
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

    pub fn closeout_project_session_with_configured_inference(
        project_root: impl AsRef<Path>,
        session_id: &SessionId,
    ) -> Result<CloseoutReport, CloseoutError> {
        let config =
            ProjectConfig::discover(project_root.as_ref()).map_err(CloseoutError::Config)?;
        let capability = InferenceCapability::from_settings(config.config.inference.as_ref())
            .map_err(CloseoutError::Inference)?;
        let extractor = ModelBackedExtractor::new(&capability);
        Self::closeout_project_session(project_root, session_id, &extractor)
    }
}

#[derive(Deserialize)]
struct ModelExtraction {
    summary_title: String,
    summary_body: String,
    #[serde(default)]
    key_points: Vec<String>,
    #[serde(default)]
    candidates: Vec<ModelCandidate>,
}

#[derive(Deserialize)]
struct ModelCandidate {
    summary: String,
    #[serde(default = "default_model_category")]
    category: String,
    #[serde(default = "default_model_confidence")]
    confidence: f32,
    #[serde(default = "default_model_action")]
    action: String,
}

impl ModelExtraction {
    fn into_output(self, input: ExtractionInput) -> Result<ExtractionOutput, CloseoutError> {
        let mut summary = SessionSummary::new(
            input.session_id.clone(),
            self.summary_title,
            self.summary_body,
        );
        summary.key_points = self.key_points;
        summary.digest_sections = digest_sections_from_points(&summary.key_points);
        summary.evidence.push(input.transcript_evidence.clone());

        let mut candidates = Vec::new();
        for candidate in self.candidates {
            let category = match candidate.category.as_str() {
                "user_preference" => LessonCategory::UserPreference,
                "project_convention" => LessonCategory::ProjectConvention,
                "architecture_rule" => LessonCategory::ArchitectureRule,
                "code_pattern" => LessonCategory::CodePattern,
                "debugging_recipe" => LessonCategory::DebuggingRecipe,
                "tooling_note" => LessonCategory::ToolingNote,
                "testing_strategy" => LessonCategory::TestingStrategy,
                "deployment_rule" => LessonCategory::DeploymentRule,
                "anti_pattern" => LessonCategory::AntiPattern,
                "security_warning" => LessonCategory::SecurityWarning,
                "documentation_update" => LessonCategory::DocumentationUpdate,
                "candidate_skill" => LessonCategory::CandidateSkill,
                "process" => LessonCategory::Process,
                other => LessonCategory::Other(other.to_string()),
            };
            let action = match candidate.action.as_str() {
                "create_skill_draft" => SuggestedAction::CreateSkillDraft,
                "update_skill_draft" => SuggestedAction::UpdateSkillDraft,
                "update_documentation" => SuggestedAction::UpdateDocumentation,
                "keep_for_session" => SuggestedAction::KeepForSession,
                "ignore" => SuggestedAction::Ignore,
                _ => SuggestedAction::PromoteToMemory,
            };
            let mut lesson = CandidateLesson::new(
                LessonId::new(candidate_id(&input.session_id, &candidate.summary)),
                candidate.summary,
                category,
                Confidence::new(candidate.confidence)?,
                action,
            )
            .with_evidence(input.transcript_evidence.clone());
            if matches!(lesson.suggested_action, SuggestedAction::CreateSkillDraft) {
                lesson.suggested_destination = CandidateDestination::SkillDraft;
            }
            annotate_candidate(&mut lesson)?;
            candidates.push(lesson);
        }

        Ok(ExtractionOutput {
            summary,
            candidates,
        })
    }
}

fn default_model_category() -> String {
    "process".to_string()
}

fn default_model_confidence() -> f32 {
    0.7
}

fn default_model_action() -> String {
    "promote_to_memory".to_string()
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
    summary.digest_sections = digest_sections_for_lines(&lines_for_digest(&input.transcript));
    summary.unresolved_risks = summary
        .key_points
        .iter()
        .filter(|point| {
            let lower = point.to_ascii_lowercase();
            lower.contains("blocked") || lower.contains("risk") || lower.contains("failed")
        })
        .cloned()
        .collect();
    summary.stale_or_superseded = summary
        .key_points
        .iter()
        .filter(|point| {
            let lower = point.to_ascii_lowercase();
            lower.contains("stale") || lower.contains("superseded") || lower.contains("instead")
        })
        .cloned()
        .collect();
    summary.evidence.push(input.transcript_evidence.clone());
    summary
}

fn lines_for_digest(transcript: &str) -> Vec<&str> {
    transcript
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect()
}

fn digest_sections_from_points(points: &[String]) -> Vec<DigestSection> {
    let borrowed: Vec<&str> = points.iter().map(String::as_str).collect();
    digest_sections_for_lines(&borrowed)
}

fn digest_sections_for_lines(lines: &[&str]) -> Vec<DigestSection> {
    let mut progress = Vec::new();
    let mut decisions = Vec::new();
    let mut commands = Vec::new();
    let mut risks = Vec::new();
    let mut next_steps = Vec::new();
    for line in lines {
        let lower = line.to_ascii_lowercase();
        if lower.contains("fixed") || lower.contains("implemented") || lower.contains("passed") {
            progress.push(truncate_fragment(strip_speaker(line)));
        }
        if lower.contains("decided") || lower.contains("prefer") || lower.contains("instead") {
            decisions.push(truncate_fragment(strip_speaker(line)));
        }
        if command_text(line).is_some() || lower.contains("failed") || lower.contains("error") {
            commands.push(truncate_fragment(strip_speaker(line)));
        }
        if lower.contains("blocked") || lower.contains("risk") {
            risks.push(truncate_fragment(strip_speaker(line)));
        }
        if lower.contains("next") || lower.contains("todo") || lower.contains("pending") {
            next_steps.push(truncate_fragment(strip_speaker(line)));
        }
    }
    [
        (DigestSectionKind::Progress, progress),
        (DigestSectionKind::Decisions, decisions),
        (DigestSectionKind::CommandOutcomes, commands),
        (DigestSectionKind::Risks, risks),
        (DigestSectionKind::NextSteps, next_steps),
    ]
    .into_iter()
    .filter(|(_, items)| !items.is_empty())
    .map(|(kind, items)| DigestSection::new(kind, items.into_iter().take(5).collect()))
    .collect()
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
        if !is_admissible_text(summary) || !seen.insert(summary.to_ascii_lowercase()) {
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
        annotate_candidate(&mut candidate)?;
        candidates.push(candidate);
    }

    // Family 2: lines proposing a reusable skill/workflow. The bare keyword
    // "skill"/"workflow" is far too broad — it fires on file paths
    // (`…\skill.rs`), crate names (`localmind-skills`), and any documentation
    // sentence mentioning skills. Require an explicit *intent* phrase, a
    // lesson-like sentence, and a per-family cap so this heuristic cannot flood
    // the queue as it did before (435 file-path/doc-line candidates).
    let mut family2 = 0usize;
    for line in &lines {
        if family2 >= MAX_CANDIDATES_PER_FAMILY {
            break;
        }
        let summary = line.trim_start_matches("- ").trim();
        if !mentions_skill_intent(&summary.to_ascii_lowercase()) || !is_lesson_like(summary) {
            continue;
        }
        if !seen.insert(summary.to_ascii_lowercase()) {
            continue;
        }

        let mut candidate = CandidateLesson::new(
            LessonId::new(candidate_id(&input.session_id, summary)),
            summary,
            LessonCategory::CandidateSkill,
            Confidence::new(0.55)?,
            SuggestedAction::CreateSkillDraft,
        )
        .with_evidence(input.transcript_evidence.clone());
        candidate.suggested_destination = CandidateDestination::SkillDraft;
        annotate_candidate(&mut candidate)?;
        candidates.push(candidate);
        family2 += 1;
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
        annotate_candidate(&mut candidate)?;
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
        annotate_candidate(&mut candidate)?;
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
        annotate_candidate(&mut candidate)?;
        candidates.push(candidate);
    }

    suggest_memory_updates(&mut candidates);
    Ok(candidates)
}

fn annotate_candidate(candidate: &mut CandidateLesson) -> Result<(), CloseoutError> {
    let summary = candidate.summary().to_ascii_lowercase();
    let conflict = summary.contains("instead")
        || summary.contains("do not")
        || summary.contains("don't")
        || summary.contains("wrong");
    candidate.review_annotation = Some(ReviewAnnotation {
        score: Confidence::new(candidate.confidence.value())?,
        duplicate_of: None,
        conflict,
        notes: if conflict {
            "candidate contains a correction or conflict signal".to_string()
        } else {
            "source-grounded deterministic extraction".to_string()
        },
    });
    // Refine a memory-bound candidate into a concrete update suggestion. These
    // are review suggestions only — the reviewer enacts them; extraction never
    // writes accepted memory. Skill/doc/session candidates keep their action.
    if matches!(candidate.suggested_action, SuggestedAction::PromoteToMemory) {
        if conflict {
            candidate.suggested_action = SuggestedAction::SupersedeExisting;
        } else if bundles_multiple_facts(&summary) {
            candidate.suggested_action = SuggestedAction::Split;
        }
    }
    if candidate.evidence().is_empty() {
        candidate.validation_status = ValidationStatus::MissingRequiredField;
    } else if candidate.confidence.value() < 0.6 {
        candidate.validation_status = ValidationStatus::LowConfidence;
    }
    Ok(())
}

/// A summary "bundles" several lessons when it stitches multiple distinct claims
/// together — a semicolon list or two or more conjunctions. Such candidates are
/// better split before promotion than stored as one blurry memory.
fn bundles_multiple_facts(summary: &str) -> bool {
    summary.contains("; ") || summary.matches(" and ").count() >= 2
}

/// Suggest merge/ignore actions for near-duplicate candidates. A later candidate
/// whose summary is contained in (or contains) an earlier one is annotated as a
/// duplicate of that earlier candidate and routed to a merge (when it carries
/// its own evidence) or ignore suggestion — never a direct memory write. Exact
/// duplicates are already collapsed upstream, so this catches the near misses.
fn suggest_memory_updates(candidates: &mut [CandidateLesson]) {
    for index in 0..candidates.len() {
        let summary = normalize_summary(candidates[index].summary());
        let mut duplicate_of = None;
        for earlier in &candidates[..index] {
            if near_duplicate(&summary, &normalize_summary(earlier.summary())) {
                duplicate_of = Some(earlier.id.as_str().to_string());
                break;
            }
        }
        let Some(target) = duplicate_of else {
            continue;
        };
        let adds_evidence = !candidates[index].evidence().is_empty();
        let candidate = &mut candidates[index];
        if let Some(annotation) = candidate.review_annotation.as_mut() {
            annotation.duplicate_of = Some(target.clone());
            annotation.notes = format!("near-duplicate of {target}");
        }
        candidate.suggested_action = if adds_evidence {
            SuggestedAction::MergeIntoExisting
        } else {
            SuggestedAction::Ignore
        };
        if !adds_evidence {
            candidate.validation_status = ValidationStatus::Duplicate;
        }
    }
}

fn normalize_summary(summary: &str) -> String {
    summary
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

/// Two summaries are near-duplicates when one meaningfully contains the other
/// (a short restatement of the same fact), not merely sharing a word.
fn near_duplicate(a: &str, b: &str) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let (shorter, longer) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    shorter.len() >= 12 && longer.contains(shorter)
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
        // The failure line itself must read like a reported problem, not a line
        // of source code that merely contains "error"/"failed" (e.g.
        // `fn deserialize<...> -> Result<...>`). Same gate for the resolution.
        if !is_failure_line(lines[index]) || !is_lesson_like(strip_speaker(lines[index])) {
            index += 1;
            continue;
        }
        let window_end = lines.len().min(index + 1 + RESOLUTION_WINDOW);
        let Some(resolution_offset) = lines[index + 1..window_end]
            .iter()
            .position(|line| is_resolution_line(line) && is_lesson_like(strip_speaker(line)))
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
    let lower = strip_speaker(line).trim().to_ascii_lowercase();
    // Success/summary lines that merely contain the word "failed"/"error".
    if lower.contains("0 errors")
        || lower.contains("no errors")
        || lower.contains("0 failed")
        || lower.contains("0 failures")
        || lower.contains("no failures")
        || lower.starts_with("test result: ok")
    {
        return false;
    }
    // A genuine failure *report* opens with an error token, ends in FAILED, or
    // is a panic — not a documentation sentence that merely mentions "failed"
    // somewhere in its prose (the dominant false-positive over dumped docs).
    const FAILURE_OPENERS: [&str; 6] = [
        "error",
        "failed",
        "failure",
        "exception",
        "panicked",
        "traceback",
    ];
    FAILURE_OPENERS
        .iter()
        .any(|marker| lower.starts_with(marker))
        || lower.ends_with("failed")
        || lower.contains("panicked at")
        || lower.contains("test result: failed")
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
            if !is_correction || !is_lesson_like(text) {
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

/// Fewest space-separated words a summary needs to read as a sentence rather
/// than a fragment.
const MIN_LESSON_WORDS: usize = 5;
/// Smallest fraction of a summary that must be letters or spaces for it to read
/// as prose rather than punctuation/code soup.
const MIN_ALPHA_RATIO: f32 = 0.65;

/// Count the space-separated tokens in `text` that contain at least one letter.
fn alpha_word_count(text: &str) -> usize {
    text.split_whitespace()
        .filter(|word| word.chars().any(char::is_alphabetic))
        .count()
}

/// Lighter gate for *author-declared* lessons (explicit `Lesson:` markers). The
/// author already decided the line is worth keeping, so a short statement is
/// fine — but it must still be prose, not a bare file path, a code/markup line,
/// or a punctuation/sub-token fragment. Shared with distillation, which applies
/// the same admission bar to model-produced insights.
pub(crate) fn is_admissible_text(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() || looks_like_path(trimmed) || looks_like_code_or_markup(trimmed) {
        return false;
    }
    if alpha_word_count(trimmed) < 2 {
        return false;
    }
    let total = trimmed.chars().count();
    let alpha = trimmed
        .chars()
        .filter(|c| c.is_alphabetic() || c.is_whitespace())
        .count();
    total > 0 && (alpha as f32 / total as f32) >= MIN_ALPHA_RATIO
}

/// Stronger gate every *heuristic* candidate summary must pass (skill/workflow
/// proposals, failure→resolution pairs, user corrections). The signal is weaker
/// than an explicit marker, so require a full sentence — this is what rejects
/// the bare file paths and code fragments that flooded the queue before.
///
/// Deliberately conservative: a learning loop that occasionally drops a real
/// lesson is recoverable; one that floods the review queue (the observed
/// failure) is not.
fn is_lesson_like(text: &str) -> bool {
    is_admissible_text(text) && alpha_word_count(text.trim()) >= MIN_LESSON_WORDS
}

/// A bare file path: a single whitespace-free token carrying a path separator
/// (`LocalMind\crates\…\skill.rs`), optionally behind a list bullet.
fn looks_like_path(text: &str) -> bool {
    let token = text.trim_start_matches(['-', '*', ' ']).trim();
    !token.contains(char::is_whitespace) && (token.contains('/') || token.contains('\\'))
}

/// A source-code or markup line rather than prose: markdown structure, a line
/// that ends in code-structural punctuation, or one carrying source signatures.
fn looks_like_code_or_markup(text: &str) -> bool {
    let t = text.trim();
    if t.starts_with('#') || t.starts_with("> ") || t.starts_with('|') || t.starts_with("//") {
        return true;
    }
    // A list bullet whose body is a file/line citation (e.g.
    // "- LocalHub/plans/foo.md:62-93 - …") is dumped doc content, not a lesson.
    if t.starts_with("- ") || t.starts_with("* ") {
        if let Some(rest) = t.get(2..) {
            if (rest.contains(".md:") || rest.contains(".rs:") || rest.contains(".ps1:"))
                || rest.contains("/archive/")
            {
                return true;
            }
        }
    }
    // Markdown bold/emphasis runs and inline file:line citations mark a verbatim
    // documentation line rather than session prose.
    if t.contains("**") || t.contains(".md:") || t.contains("/archive/") {
        return true;
    }
    if t.ends_with(['{', '}', ';', ',', '(', '[', ':']) || t.ends_with("=>") || t.ends_with("\")]")
    {
        return true;
    }
    // Note: a bare `::` is intentionally *not* a marker — a genuine lesson may
    // reference a Rust path ("prefer std::mem::take here"). Real source lines are
    // caught by the keyword/structural markers and the trailing-punctuation check.
    const CODE_MARKERS: [&str; 12] = [
        "fn ", "=>", "impl ", "pub use", "pub fn", "#[", "Result<", "Box<dyn", "{reason}", ":new(",
        "let mut ", "});",
    ];
    CODE_MARKERS.iter().any(|marker| t.contains(marker))
}

/// Whether a (lowercased) line expresses intent to capture a reusable skill or
/// workflow, as opposed to merely containing the word "skill"/"workflow". The
/// phrase set keys on author intent ("create a skill", "reusable workflow")
/// rather than the bare token, which appears in paths, crate names, and docs.
fn mentions_skill_intent(lower: &str) -> bool {
    // Proposal phrases only. Bare noun phrases ("reusable skills", "skill
    // draft") are deliberately excluded — they appear verbatim throughout the
    // ecosystem's own documentation about skills and produced most of the
    // false positives.
    const INTENT: [&str; 9] = [
        "create a skill",
        "create skill for",
        "add a skill",
        "turn this into a skill",
        "turn it into a skill",
        "could be a reusable",
        "would be a reusable",
        "would make a good skill",
        "worth turning into a skill",
    ];
    INTENT.iter().any(|phrase| lower.contains(phrase))
}

/// Extract an explicit lesson marker's text. Only a line that *begins* with
/// `Lesson:` (after any speaker label) is the author declaring a lesson; an
/// embedded `lesson:` substring is almost always source code (e.g.
/// `#[error("invalid candidate lesson: {reason}")]`) or prose mentioning the
/// word, so it must not match.
fn lesson_summary(line: &str) -> Option<&str> {
    let body = strip_speaker(line);
    body.strip_prefix("Lesson:")
        .or_else(|| body.strip_prefix("lesson:"))
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

        if candidate.evidence().is_empty() {
            return Err(CloseoutError::InvalidCandidate {
                reason: "candidate evidence is required".to_string(),
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
    #[error(transparent)]
    Inference(#[from] InferenceError),
    #[error("model extraction output was not valid LocalMind JSON: {0}")]
    ModelOutput(serde_json::Error),
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn extract(transcript: &str) -> Vec<CandidateLesson> {
        let evidence = EvidenceRef::new(EvidenceKind::Transcript, "redacted transcript").redacted();
        let input = ExtractionInput {
            session_id: SessionId::new("session-test"),
            transcript: transcript.to_string(),
            transcript_evidence: evidence,
        };
        DeterministicExtractor
            .extract(input)
            .expect("extraction never fails on text")
            .candidates
    }

    // --- Rejection rule: bare file paths -----------------------------------
    #[test]
    fn rejects_bare_file_paths() {
        for path in [
            r"LocalMind\crates\localmind-core\src\skill.rs",
            "LocalHub/plans/archive/09-localmind-skills.md",
            r"- LocalPilot\external\localmind\crates\localmind-store\src\skill_drafts.rs",
        ] {
            assert!(looks_like_path(path), "should be a path: {path:?}");
            assert!(!is_admissible_text(path), "path admitted: {path:?}");
        }
        // A sentence that merely names a file is not a bare path.
        assert!(!looks_like_path(
            "the off-by-one was in writer.rs at the bounds check"
        ));
    }

    // --- Rejection rule: punctuation / sub-token fragments ------------------
    #[test]
    fn rejects_punctuation_and_subtoken_fragments() {
        for fragment in [":{", ":new(", "{reason}\")]", "=> Ok(())", "});"] {
            assert!(
                !is_admissible_text(fragment),
                "fragment admitted: {fragment:?}"
            );
        }
    }

    // --- Rejection rule: verbatim source / markup lines ---------------------
    #[test]
    fn rejects_source_and_markup_lines() {
        for line in [
            "fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {",
            "use thiserror::Error;",
            "## 13. Possible Architecture",
            "> Do not make tests pass by weakening them",
            "- LocalHub/plans/localmind/archive/LocalMind-Plan.md:161: still open",
            "structured signals from the **event log** of failed tools and recovery",
        ] {
            assert!(
                looks_like_code_or_markup(line),
                "should read as code/markup: {line:?}"
            );
            assert!(!is_lesson_like(line), "code/markup admitted: {line:?}");
        }
    }

    // --- Rejection rule: too short to be a sentence (heuristic gate) --------
    #[test]
    fn heuristic_gate_requires_sentence_length() {
        // Fewer than five words: not enough to be a heuristic lesson…
        assert!(!is_lesson_like("prefer fixtures here"));
        // …but an author-declared marker accepts a short statement.
        assert!(is_admissible_text("prefer fixtures here"));
        assert!(is_admissible_text("Prefer fixtures."));
        // A full sentence passes both.
        assert!(is_lesson_like(
            "exporter changes need the integration suite, not just unit tests"
        ));
    }

    // --- Rejection rule: skill/workflow needs an explicit proposal ----------
    #[test]
    fn skill_family_requires_proposal_intent() {
        // Documentation that merely mentions skills is not a proposal.
        assert!(!mentions_skill_intent(
            "memory, graph-connected knowledge, and reusable skills, and agent context"
        ));
        assert!(!mentions_skill_intent(
            "`crates/localmind-skills` — skill draft generation and maintenance boundary"
        ));
        // An explicit proposal is.
        assert!(mentions_skill_intent(
            "let's create a skill for the release checklist"
        ));
        assert!(mentions_skill_intent(
            "this validate-before-handoff workflow could be a reusable checklist"
        ));
    }

    // --- Rejection rule: explicit marker must open the line -----------------
    #[test]
    fn lesson_marker_must_open_the_line() {
        assert_eq!(
            lesson_summary("Lesson: prefer guard clauses"),
            Some("prefer guard clauses")
        );
        assert_eq!(
            lesson_summary("user: Lesson: prefer guard clauses"),
            Some("prefer guard clauses")
        );
        // An embedded mention is source code, not a declared lesson.
        assert_eq!(
            lesson_summary("#[error(\"invalid candidate lesson: {reason}\")]"),
            None
        );
    }

    // --- Rejection rule: failure lines are reports, not prose mentions ------
    #[test]
    fn failure_line_must_be_a_report() {
        assert!(is_failure_line("error: assertion failed at writer.rs:88"));
        assert!(is_failure_line("tool: test writer::flushes ... FAILED"));
        // Success summaries and documentation that mention "failed" are not.
        assert!(!is_failure_line(
            "test result: ok. 19 passed; 0 failed; 0 ignored"
        ));
        assert!(!is_failure_line(
            "`FAIL` means startup or probing failed for another reason"
        ));
        assert!(!is_failure_line(
            "After attempts_per_step failures, stop or replan depending on config"
        ));
    }

    // --- Family behaviour: junk in, nothing out ----------------------------
    #[test]
    fn pure_dumped_content_yields_no_candidates() {
        // A transcript made only of file paths, source, and documentation —
        // the shape of the dogfood sessions that produced 354 noise candidates.
        let transcript = "\
LocalMind\\crates\\localmind-core\\src\\skill.rs
use thiserror::Error;
fn deserialize<D>(d: D) -> Result<Self, D::Error> {
## Implementation Status
- reusable skills, and agent context.
`crates/localmind-skills` — skill draft generation and maintenance boundary.
:{
{reason}\")]
test result: ok. 19 passed; 0 failed; 0 ignored
";
        assert!(
            extract(transcript).is_empty(),
            "dumped file content must not produce candidates: {:?}",
            extract(transcript)
                .iter()
                .map(|c| c.summary().to_string())
                .collect::<Vec<_>>()
        );
    }

    // --- Family behaviour: real lessons still survive ----------------------
    #[test]
    fn genuine_session_lessons_survive() {
        let transcript = "\
user: the exporter test is failing again
assistant: error: assertion failed at writer.rs:88, the batch flush ordering is wrong
assistant: Fixed: flushing before the clear; the suite is passing now.
user: Lesson: exporter changes need the integration suite, not just unit tests.
assistant: this validate-before-handoff routine could be a reusable checklist.
user: no, don't weaken the test to make it pass, keep the assertion strict.
";
        let candidates = extract(transcript);
        assert!(
            !candidates.is_empty(),
            "a session with genuine lessons must produce candidates"
        );
        // Every surviving candidate reads like prose, never a fragment/path.
        for candidate in &candidates {
            assert!(
                is_admissible_text(candidate.summary()),
                "implausible candidate survived: {:?}",
                candidate.summary()
            );
        }
        // The explicit marker and the failure→fix recipe both survive.
        let summaries: Vec<String> = candidates
            .iter()
            .map(|c| c.summary().to_ascii_lowercase())
            .collect();
        assert!(
            summaries.iter().any(|s| s.contains("integration suite")),
            "explicit lesson lost: {summaries:?}"
        );
        assert!(
            summaries
                .iter()
                .any(|s| s.contains("assertion failed") && s.contains("passing")),
            "failure-to-resolution recipe lost: {summaries:?}"
        );
    }
}
