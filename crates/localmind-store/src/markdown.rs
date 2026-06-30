use localmind_core::{
    Confidence, EpistemicStatus, EvidenceId, EvidenceKind, EvidenceRef, LessonCategory,
    MemoryEntry, MemoryEntryId, MemoryScope, MemoryStatus, SessionId,
};
use thiserror::Error;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

pub struct MarkdownMemoryFormat;

impl MarkdownMemoryFormat {
    #[must_use]
    pub fn serialize(entry: &MemoryEntry) -> String {
        let mut output = String::new();

        output.push_str("---\n");
        output.push_str(&format!("id: {}\n", entry.id));
        output.push_str(&format!("scope: {:?}\n", entry.scope));
        output.push_str(&format!("category: {:?}\n", entry.category));
        // Derived from category — recorded in front matter so the memory's
        // epistemic status is legible in the human-readable source of truth.
        output.push_str(&format!(
            "epistemic_status: {}\n",
            EpistemicStatus::from_category(&entry.category).as_str()
        ));
        output.push_str(&format!("confidence: {:.3}\n", entry.confidence.value()));
        push_optional_id(&mut output, "source_session", entry.source_session.as_ref());
        push_optional_time(&mut output, "created_at", entry.created_at);
        push_optional_time(&mut output, "updated_at", entry.updated_at);
        push_string_list(&mut output, "tags", &entry.tags);
        push_string_list(&mut output, "related_files", &entry.related_files);
        push_string_list(&mut output, "related_entities", &entry.related_entities);
        push_id_list(&mut output, "supersedes", &entry.supersedes);
        push_id_list(&mut output, "contradicts", &entry.contradicts);
        push_evidence(&mut output, &entry.evidence);
        output.push_str("---\n\n");
        output.push_str(entry.body.trim());
        output.push('\n');

        output
    }

    /// Parse a memory Markdown file back into a [`MemoryEntry`], the inverse of
    /// [`serialize`](Self::serialize). The Markdown file is the canonical source
    /// of truth for accepted memory, so this is how a *portable export* recovers
    /// the full structured entry (body, scope, category, confidence, edges,
    /// evidence, tags, related files/entities) without a second serialization of
    /// the lesson. Round-trips losslessly over every field the serializer emits;
    /// fields the serializer never writes (an evidence `content_hash`/`metadata`)
    /// come back empty, and `status` is always `Active` (only active memory is
    /// serialized to a file).
    ///
    /// # Errors
    /// [`MarkdownParseError`] when the front matter is missing, a required field
    /// (`id`, `scope`, `category`, `confidence`) is absent, or a field's value is
    /// malformed.
    pub fn parse(text: &str) -> Result<MemoryEntry, MarkdownParseError> {
        let (front_matter, body) = split_front_matter(text)?;
        let mut id: Option<String> = None;
        let mut scope: Option<MemoryScope> = None;
        let mut category: Option<LessonCategory> = None;
        let mut confidence: Option<Confidence> = None;
        let mut source_session: Option<SessionId> = None;
        let mut created_at: Option<OffsetDateTime> = None;
        let mut updated_at: Option<OffsetDateTime> = None;
        let mut tags: Vec<String> = Vec::new();
        let mut related_files: Vec<String> = Vec::new();
        let mut related_entities: Vec<String> = Vec::new();
        let mut supersedes: Vec<MemoryEntryId> = Vec::new();
        let mut contradicts: Vec<MemoryEntryId> = Vec::new();
        let mut evidence: Vec<EvidenceRef> = Vec::new();

        let mut cursor = 0;
        while cursor < front_matter.len() {
            let line = front_matter[cursor];
            let (key, value) = split_key_value(line);
            match key {
                "id" => id = Some(value.to_string()),
                "scope" => scope = Some(parse_scope(value)?),
                "category" => category = Some(parse_category(value)),
                // Derived from category on serialize; ignored on parse.
                "epistemic_status" => {}
                "confidence" => {
                    let parsed = value.trim().parse::<f32>().map_err(|_| {
                        MarkdownParseError::InvalidField {
                            field: "confidence",
                            value: value.to_string(),
                        }
                    })?;
                    confidence = Some(Confidence::new(parsed).map_err(|_| {
                        MarkdownParseError::InvalidField {
                            field: "confidence",
                            value: value.to_string(),
                        }
                    })?);
                }
                "source_session" => source_session = Some(SessionId::new(value)),
                "created_at" => created_at = Some(parse_time("created_at", value)?),
                "updated_at" => updated_at = Some(parse_time("updated_at", value)?),
                "tags" => collect_scalar_list(&front_matter, &mut cursor, &mut tags),
                "related_files" => {
                    collect_scalar_list(&front_matter, &mut cursor, &mut related_files);
                }
                "related_entities" => {
                    collect_scalar_list(&front_matter, &mut cursor, &mut related_entities);
                }
                "supersedes" => collect_id_list(&front_matter, &mut cursor, &mut supersedes),
                "contradicts" => collect_id_list(&front_matter, &mut cursor, &mut contradicts),
                "evidence" => {
                    collect_evidence(&front_matter, &mut cursor, &mut evidence);
                }
                // Unknown keys are skipped so a forward-compatible field never
                // fails an older parser.
                _ => {}
            }
            cursor += 1;
        }

        Ok(MemoryEntry {
            id: MemoryEntryId::new(id.ok_or(MarkdownParseError::MissingField { field: "id" })?),
            scope: scope.ok_or(MarkdownParseError::MissingField { field: "scope" })?,
            body,
            category: category.ok_or(MarkdownParseError::MissingField { field: "category" })?,
            confidence: confidence.ok_or(MarkdownParseError::MissingField {
                field: "confidence",
            })?,
            source_session,
            evidence,
            tags,
            related_files,
            related_entities,
            created_at,
            updated_at,
            supersedes,
            contradicts,
            status: MemoryStatus::Active,
        })
    }
}

/// Split a Markdown memory file into its front-matter lines (between the opening
/// and closing `---`) and the trimmed body. Errors when either delimiter is
/// absent.
fn split_front_matter(text: &str) -> Result<(Vec<&str>, String), MarkdownParseError> {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return Err(MarkdownParseError::MissingFrontMatter);
    }
    let mut front_matter = Vec::new();
    let mut body_lines = Vec::new();
    let mut in_body = false;
    for line in lines {
        if !in_body {
            if line.trim() == "---" {
                in_body = true;
                continue;
            }
            front_matter.push(line);
        } else {
            body_lines.push(line);
        }
    }
    if !in_body {
        return Err(MarkdownParseError::MissingFrontMatter);
    }
    Ok((front_matter, body_lines.join("\n").trim().to_string()))
}

/// Split a `key: value` line at the first colon. A bare `key:` returns an empty
/// value (the marker for a list/block that follows on indented lines).
fn split_key_value(line: &str) -> (&str, &str) {
    match line.split_once(':') {
        Some((key, value)) => (key.trim(), value.trim()),
        None => (line.trim(), ""),
    }
}

/// Collect the `  - <scalar>` items that follow a list key, un-escaping each, and
/// advance `cursor` to the last consumed item line.
fn collect_scalar_list(lines: &[&str], cursor: &mut usize, out: &mut Vec<String>) {
    while *cursor + 1 < lines.len() {
        let Some(item) = list_item(lines[*cursor + 1]) else {
            break;
        };
        out.push(unescape_yaml_scalar(item));
        *cursor += 1;
    }
}

/// Like [`collect_scalar_list`] but yields [`MemoryEntryId`]s (ids are never
/// escaped on serialize).
fn collect_id_list(lines: &[&str], cursor: &mut usize, out: &mut Vec<MemoryEntryId>) {
    while *cursor + 1 < lines.len() {
        let Some(item) = list_item(lines[*cursor + 1]) else {
            break;
        };
        out.push(MemoryEntryId::new(unescape_yaml_scalar(item)));
        *cursor += 1;
    }
}

/// Collect the evidence block: a sequence of `- id:` items each followed by
/// indented `kind`/`label`/`redacted`/`uri` fields. Advances `cursor` past the
/// block.
fn collect_evidence(lines: &[&str], cursor: &mut usize, out: &mut Vec<EvidenceRef>) {
    while *cursor + 1 < lines.len() {
        let next = lines[*cursor + 1];
        let Some(item) = list_item(next) else {
            break;
        };
        let (key, evidence_id) = split_key_value(item);
        if key != "id" {
            break;
        }
        let evidence_id = evidence_id.to_string();
        *cursor += 1;
        let mut kind = EvidenceKind::Other(String::new());
        let mut label = String::new();
        let mut redacted = false;
        let mut uri: Option<String> = None;
        while *cursor + 1 < lines.len() {
            let field_line = lines[*cursor + 1];
            // An evidence field is indented deeper than the `- id:` line and is
            // not itself a new list item.
            if list_item(field_line).is_some() || !field_line.starts_with("    ") {
                break;
            }
            let (field_key, field_value) = split_key_value(field_line);
            match field_key {
                "kind" => kind = parse_evidence_kind(field_value),
                "label" => label = unescape_yaml_scalar(field_value),
                "redacted" => redacted = field_value.trim() == "true",
                "uri" => uri = Some(unescape_yaml_scalar(field_value)),
                _ => {}
            }
            *cursor += 1;
        }
        let mut reference = EvidenceRef::new(kind, label);
        // Preserve the serialized id rather than re-deriving it from the label.
        if !evidence_id.is_empty() {
            reference.id = EvidenceId::new(evidence_id);
        }
        if redacted {
            reference = reference.redacted();
        }
        if let Some(uri) = uri {
            reference = reference.with_uri(uri);
        }
        out.push(reference);
    }
}

/// The scalar of a `  - <value>` list item line, or `None` when the line is not a
/// list item.
fn list_item(line: &str) -> Option<&str> {
    line.trim_start().strip_prefix("- ").map(str::trim)
}

/// Reverse [`escape_yaml_scalar`]: a single-quoted value is unwrapped and its
/// doubled `''` collapsed back to `'`.
fn unescape_yaml_scalar(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2 && value.starts_with('\'') && value.ends_with('\'') {
        value[1..value.len() - 1].replace("''", "'")
    } else {
        value.to_string()
    }
}

fn parse_scope(value: &str) -> Result<MemoryScope, MarkdownParseError> {
    match value.trim() {
        "GlobalUser" => Ok(MemoryScope::GlobalUser),
        "Project" => Ok(MemoryScope::Project),
        "Session" => Ok(MemoryScope::Session),
        "Skill" => Ok(MemoryScope::Skill),
        "Research" => Ok(MemoryScope::Research),
        other => Err(MarkdownParseError::InvalidField {
            field: "scope",
            value: other.to_string(),
        }),
    }
}

/// Map a category's `{:?}` form back to a [`LessonCategory`]. An unknown name or
/// the `Other("…")` form recovers the inner string, so a custom category survives
/// the round-trip. Shared with the freshness pass, which reads the stored
/// `memory_index.category` string and needs the typed category for the
/// quality-classifier gate.
pub(crate) fn parse_category(value: &str) -> LessonCategory {
    match value.trim() {
        "UserPreference" => LessonCategory::UserPreference,
        "ProjectConvention" => LessonCategory::ProjectConvention,
        "ArchitectureRule" => LessonCategory::ArchitectureRule,
        "CodePattern" => LessonCategory::CodePattern,
        "DebuggingRecipe" => LessonCategory::DebuggingRecipe,
        "ToolingNote" => LessonCategory::ToolingNote,
        "TestingStrategy" => LessonCategory::TestingStrategy,
        "DeploymentRule" => LessonCategory::DeploymentRule,
        "AntiPattern" => LessonCategory::AntiPattern,
        "SecurityWarning" => LessonCategory::SecurityWarning,
        "DocumentationUpdate" => LessonCategory::DocumentationUpdate,
        "CandidateSkill" => LessonCategory::CandidateSkill,
        "Process" => LessonCategory::Process,
        "ToolUse" => LessonCategory::ToolUse,
        other => {
            LessonCategory::Other(parse_debug_other(other).unwrap_or_else(|| other.to_string()))
        }
    }
}

fn parse_evidence_kind(value: &str) -> EvidenceKind {
    match value.trim() {
        "Transcript" => EvidenceKind::Transcript,
        "ToolEvent" => EvidenceKind::ToolEvent,
        "Command" => EvidenceKind::Command,
        "FileDiff" => EvidenceKind::FileDiff,
        "TestOutput" => EvidenceKind::TestOutput,
        "Commit" => EvidenceKind::Commit,
        "CodeParse" => EvidenceKind::CodeParse,
        "RecoveryEvent" => EvidenceKind::RecoveryEvent,
        "UserCorrection" => EvidenceKind::UserCorrection,
        "ManualNote" => EvidenceKind::ManualNote,
        other => EvidenceKind::Other(parse_debug_other(other).unwrap_or_else(|| other.to_string())),
    }
}

/// Recover the inner string of a `{:?}`-formatted `Other("…")` tuple variant,
/// reversing the `Debug` escaping of `"` and `\`. Returns `None` when the value
/// is not in that shape.
fn parse_debug_other(value: &str) -> Option<String> {
    let inner = value.strip_prefix("Other(\"")?.strip_suffix("\")")?;
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(character) = chars.next() {
        if character == '\\' {
            match chars.next() {
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(character);
        }
    }
    Some(out)
}

fn parse_time(field: &'static str, value: &str) -> Result<OffsetDateTime, MarkdownParseError> {
    OffsetDateTime::parse(value.trim(), &Rfc3339).map_err(|_| MarkdownParseError::InvalidField {
        field,
        value: value.to_string(),
    })
}

#[derive(Debug, Error)]
pub enum MarkdownParseError {
    #[error("memory markdown is missing its `---` front-matter delimiters")]
    MissingFrontMatter,
    #[error("memory markdown front matter is missing required field: {field}")]
    MissingField { field: &'static str },
    #[error("memory markdown has an invalid {field}: {value}")]
    InvalidField { field: &'static str, value: String },
}

fn push_optional_id<T: std::fmt::Display>(output: &mut String, key: &str, value: Option<&T>) {
    if let Some(value) = value {
        output.push_str(&format!("{key}: {value}\n"));
    }
}

fn push_optional_time(output: &mut String, key: &str, value: Option<time::OffsetDateTime>) {
    if let Some(value) = value {
        let formatted = value.format(&Rfc3339).unwrap_or_else(|_| value.to_string());
        output.push_str(&format!("{key}: {formatted}\n"));
    }
}

fn push_string_list(output: &mut String, key: &str, values: &[String]) {
    if values.is_empty() {
        return;
    }

    output.push_str(&format!("{key}:\n"));
    for value in values {
        output.push_str(&format!("  - {}\n", escape_yaml_scalar(value)));
    }
}

fn push_id_list(output: &mut String, key: &str, values: &[MemoryEntryId]) {
    if values.is_empty() {
        return;
    }

    output.push_str(&format!("{key}:\n"));
    for value in values {
        output.push_str(&format!("  - {value}\n"));
    }
}

fn push_evidence(output: &mut String, evidence: &[EvidenceRef]) {
    if evidence.is_empty() {
        return;
    }

    output.push_str("evidence:\n");
    for item in evidence {
        output.push_str(&format!("  - id: {}\n", item.id));
        output.push_str(&format!("    kind: {:?}\n", item.kind));
        output.push_str(&format!("    label: {}\n", escape_yaml_scalar(&item.label)));
        output.push_str(&format!("    redacted: {}\n", item.redacted));
        if let Some(uri) = &item.uri {
            output.push_str(&format!("    uri: {}\n", escape_yaml_scalar(uri)));
        }
    }
}

fn escape_yaml_scalar(value: &str) -> String {
    if value
        .chars()
        .any(|character| matches!(character, ':' | '#' | '\n' | '\'' | '"'))
    {
        format!("'{escaped}'", escaped = value.replace('\'', "''"))
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::{MarkdownMemoryFormat, MarkdownParseError};
    use localmind_core::{
        Confidence, EvidenceKind, EvidenceRef, LessonCategory, MemoryEntry, MemoryEntryId,
        MemoryScope, MemoryStatus, SessionId,
    };
    use time::OffsetDateTime;

    fn rich_entry() -> MemoryEntry {
        MemoryEntry {
            id: MemoryEntryId::new("mem-1"),
            scope: MemoryScope::GlobalUser,
            body: "Prefer a guard clause: it keeps the happy path flat.".to_string(),
            category: LessonCategory::Other("custom: tricky\"name".to_string()),
            confidence: Confidence::new(0.812).unwrap(),
            source_session: Some(SessionId::new("session-7")),
            evidence: vec![
                EvidenceRef::new(EvidenceKind::Transcript, "redacted transcript").redacted(),
                EvidenceRef::new(EvidenceKind::Other("note".to_string()), "a manual: note")
                    .with_uri("repo@abc123"),
            ],
            tags: vec!["accepted".to_string(), "rule-cue".to_string()],
            related_files: vec!["src/parser.rs".to_string()],
            related_entities: vec!["Tokenizer".to_string()],
            created_at: Some(OffsetDateTime::from_unix_timestamp(1_782_900_000).unwrap()),
            updated_at: None,
            supersedes: vec![MemoryEntryId::new("mem-old")],
            contradicts: vec![MemoryEntryId::new("mem-other")],
            status: MemoryStatus::Active,
        }
    }

    #[test]
    fn serialize_then_parse_round_trips_every_persisted_field() {
        let original = rich_entry();
        let text = MarkdownMemoryFormat::serialize(&original);
        let parsed = MarkdownMemoryFormat::parse(&text).unwrap();

        assert_eq!(parsed.id, original.id);
        assert_eq!(parsed.scope, original.scope);
        assert_eq!(parsed.category, original.category, "Other(..) survives");
        assert_eq!(parsed.body, original.body.trim());
        assert!((parsed.confidence.value() - 0.812).abs() < 0.001);
        assert_eq!(parsed.source_session, original.source_session);
        assert_eq!(parsed.created_at, original.created_at);
        assert_eq!(parsed.tags, original.tags);
        assert_eq!(parsed.related_files, original.related_files);
        assert_eq!(parsed.related_entities, original.related_entities);
        assert_eq!(parsed.supersedes, original.supersedes);
        assert_eq!(parsed.contradicts, original.contradicts);
        assert_eq!(parsed.status, MemoryStatus::Active);
        // Evidence: kind/label/redacted/uri survive (content_hash/metadata are
        // never serialized, so they come back empty by design).
        assert_eq!(parsed.evidence.len(), 2);
        assert_eq!(parsed.evidence[0].kind, EvidenceKind::Transcript);
        assert!(parsed.evidence[0].redacted);
        assert_eq!(
            parsed.evidence[1].kind,
            EvidenceKind::Other("note".to_string())
        );
        assert_eq!(parsed.evidence[1].label, "a manual: note");
        assert_eq!(parsed.evidence[1].uri.as_deref(), Some("repo@abc123"));
    }

    #[test]
    fn parse_minimal_entry_with_no_optional_fields() {
        let text = MarkdownMemoryFormat::serialize(&MemoryEntry {
            id: MemoryEntryId::new("mem-min"),
            scope: MemoryScope::Project,
            body: "minimal".to_string(),
            category: LessonCategory::Process,
            confidence: Confidence::new(0.5).unwrap(),
            source_session: None,
            evidence: Vec::new(),
            tags: Vec::new(),
            related_files: Vec::new(),
            related_entities: Vec::new(),
            created_at: None,
            updated_at: None,
            supersedes: Vec::new(),
            contradicts: Vec::new(),
            status: MemoryStatus::Active,
        });
        let parsed = MarkdownMemoryFormat::parse(&text).unwrap();
        assert_eq!(parsed.id.as_str(), "mem-min");
        assert_eq!(parsed.scope, MemoryScope::Project);
        assert!(parsed.evidence.is_empty());
        assert!(parsed.tags.is_empty());
    }

    #[test]
    fn missing_front_matter_is_an_error() {
        assert!(matches!(
            MarkdownMemoryFormat::parse("no front matter here"),
            Err(MarkdownParseError::MissingFrontMatter)
        ));
    }
}
