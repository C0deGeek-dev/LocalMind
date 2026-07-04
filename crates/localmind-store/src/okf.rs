//! Open Knowledge Format (OKF v0.1) profile over the canonical Markdown memory
//! format.
//!
//! Google Cloud's OKF (v0.1, 2026-06) represents organizational knowledge as a
//! directory of Markdown files with YAML front matter, requiring exactly one
//! field — `type` — and reserving `title`, `description`, `resource`, `tags`,
//! and `timestamp`. Producers may add arbitrary further fields.
//!
//! LocalMind already stores accepted memory as Markdown + front matter with a
//! richer schema. This module is a thin *profile* over
//! [`MarkdownMemoryFormat`](crate::MarkdownMemoryFormat), **not** a second
//! serializer:
//!
//! - **Export** ([`OkfFormat::to_okf`]) is the canonical serialization with the
//!   OKF reader-facing keys (`type`/`title`/`description`/`resource`/`timestamp`
//!   plus `okf_version`) prepended. Every native key is still present, so an
//!   OKF consumer reads the reserved fields while a LocalMind re-import reads the
//!   native ones. This is why the export is lossless *by construction*: the OKF
//!   keys are additive, and re-import goes through the existing parser (the OKF
//!   keys are ignored as unknown), reusing the canonical model serde rather than
//!   a second field map.
//! - **Import** ([`OkfFormat::from_okf`]) recognises a LocalMind-origin file by
//!   its native keys and delegates to the canonical parser (lossless). A
//!   *foreign* OKF file — one without the native keys — is synthesized from its
//!   reserved fields into a low-trust [`MemoryEntry`] destined for review.
//!
//! The foreign reader accepts the flat OKF front-matter shapes the canonical
//! block-list reader does not — inline-flow sequences (`tags: [a, b]`) and
//! double-quoted scalars — without touching the canonical reader, so its blast
//! radius is this module only.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use localmind_core::{
    Confidence, LessonCategory, MemoryEntry, MemoryEntryId, MemoryScope, MemoryStatus,
};
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::markdown::{
    parse_category, split_front_matter, MarkdownMemoryFormat, MarkdownParseError,
};

/// The OKF specification version this profile targets. Emitted into every
/// exported concept so a drift in the format is detectable at import time.
pub const OKF_VERSION: &str = "0.1";

/// Conservative confidence assigned to a *foreign* OKF concept on import: it
/// arrives unsigned and unreviewed, so it enters the review queue low-trust.
const FOREIGN_IMPORT_CONFIDENCE: f32 = 0.4;

const MAX_TITLE_LEN: usize = 120;
const MAX_DESCRIPTION_LEN: usize = 200;

/// The reserved OKF front-matter field names (v0.1). `type` is the only required
/// one; the rest are optional. Used to skip reserved keys when scanning for the
/// producer-extension (native) markers.
const RESERVED_KEYS: [&str; 6] = [
    "type",
    "title",
    "description",
    "resource",
    "tags",
    "timestamp",
];

/// The native front-matter keys whose joint presence marks a file as
/// LocalMind-origin (and therefore losslessly re-importable through the
/// canonical parser). These mirror the required fields of
/// [`MarkdownMemoryFormat::parse`](crate::MarkdownMemoryFormat).
const NATIVE_MARKERS: [&str; 4] = ["id", "scope", "category", "confidence"];

/// An OKF (Open Knowledge Format) view over a [`MemoryEntry`].
pub struct OkfFormat;

impl OkfFormat {
    /// Serialize a memory entry as an OKF v0.1 concept document. The output is
    /// the canonical Markdown serialization with the OKF reader-facing keys
    /// prepended, so it is both a conformant OKF concept and a lossless
    /// LocalMind memory file.
    #[must_use]
    pub fn to_okf(entry: &MemoryEntry) -> String {
        let native = MarkdownMemoryFormat::serialize(entry);
        // The canonical serialization always opens with a `---` fence; splice the
        // OKF keys directly after it so the native block follows unchanged.
        let inner = native.strip_prefix("---\n").unwrap_or(native.as_str());

        let mut out = String::from("---\n");
        out.push_str(&format!("okf_version: \"{OKF_VERSION}\"\n"));
        out.push_str(&format!("type: {}\n", scalar(&okf_type(&entry.category))));
        out.push_str(&format!("title: {}\n", scalar(&derive_title(entry))));
        if let Some(description) = derive_description(entry) {
            out.push_str(&format!("description: {}\n", scalar(&description)));
        }
        if let Some(resource) = derive_resource(entry) {
            out.push_str(&format!("resource: {}\n", scalar(&resource)));
        }
        if let Some(timestamp) = entry.updated_at.or(entry.created_at) {
            if let Ok(formatted) = timestamp.format(&Rfc3339) {
                out.push_str(&format!("timestamp: {formatted}\n"));
            }
        }
        out.push_str(inner);
        out
    }

    /// Parse an OKF v0.1 concept document into a [`MemoryEntry`].
    ///
    /// A LocalMind-origin document (one carrying the native keys) round-trips
    /// losslessly through the canonical parser. A foreign OKF document is
    /// synthesized from its reserved fields into a low-trust, review-bound entry
    /// (`status = Active`, a conservative confidence, `scope = Project`).
    ///
    /// # Errors
    /// [`OkfParseError::MissingType`] when a foreign document omits the required
    /// `type` field, or a propagated [`MarkdownParseError`] when the front-matter
    /// delimiters are missing or a native document is malformed.
    pub fn from_okf(text: &str) -> Result<MemoryEntry, OkfParseError> {
        let (front_matter, body) = split_front_matter(text)?;

        if has_native_markers(&front_matter) {
            // LocalMind-origin: the native keys are the source of truth and the
            // OKF keys are ignored as unknown — a lossless round-trip that reuses
            // the canonical model serde rather than a second field map.
            return MarkdownMemoryFormat::parse(text).map_err(OkfParseError::from);
        }

        let reserved = OkfReserved::read(&front_matter);
        let type_value = reserved.scalar("type").ok_or(OkfParseError::MissingType)?;
        let category = parse_category(type_value);
        let confidence = Confidence::new(FOREIGN_IMPORT_CONFIDENCE).map_err(|_| {
            OkfParseError::InvalidField {
                field: "confidence",
            }
        })?;
        let updated_at = reserved
            .scalar("timestamp")
            .and_then(|value| OffsetDateTime::parse(value, &Rfc3339).ok());
        let related_files = reserved
            .scalar("resource")
            .map(|value| vec![value.to_string()])
            .unwrap_or_default();

        Ok(MemoryEntry {
            id: MemoryEntryId::new(foreign_id(
                reserved.scalar("title").unwrap_or_default(),
                &body,
            )),
            scope: MemoryScope::Project,
            body,
            category,
            confidence,
            source_session: None,
            evidence: Vec::new(),
            tags: reserved.tags,
            related_files,
            related_entities: Vec::new(),
            created_at: None,
            updated_at,
            supersedes: Vec::new(),
            contradicts: Vec::new(),
            status: MemoryStatus::Active,
        })
    }
}

/// The reserved OKF fields recovered from a foreign concept's front matter.
struct OkfReserved {
    scalars: BTreeMap<String, String>,
    tags: Vec<String>,
}

impl OkfReserved {
    /// Read the reserved fields from front-matter lines, accepting inline-flow
    /// (`tags: [a, b]`) and block (`tags:` then `  - a`) sequences and quoted
    /// scalars. Non-reserved (producer/native) keys are ignored here.
    fn read(lines: &[&str]) -> Self {
        let mut scalars = BTreeMap::new();
        let mut tags = Vec::new();
        let mut cursor = 0;
        while cursor < lines.len() {
            let (key, value) = split_key_value(lines[cursor]);
            if key == "tags" {
                if value.is_empty() {
                    // Block form: consume the following `  - item` lines.
                    while cursor + 1 < lines.len() {
                        let Some(item) = block_item(lines[cursor + 1]) else {
                            break;
                        };
                        tags.push(unquote(item));
                        cursor += 1;
                    }
                } else {
                    tags = parse_flow_sequence(value);
                }
            } else if RESERVED_KEYS.contains(&key) {
                scalars.insert(key.to_string(), unquote(value));
            }
            cursor += 1;
        }
        Self { scalars, tags }
    }

    fn scalar(&self, key: &str) -> Option<&str> {
        self.scalars.get(key).map(String::as_str)
    }
}

/// True when every native required key is present, marking a LocalMind-origin
/// document that the canonical parser owns.
fn has_native_markers(lines: &[&str]) -> bool {
    NATIVE_MARKERS
        .iter()
        .all(|marker| lines.iter().any(|line| split_key_value(line).0 == *marker))
}

/// The OKF `type` token for a category: the variant name, or the inner string of
/// an `Other(..)` category. On import the inverse is
/// [`parse_category`](crate::markdown::parse_category), which maps a known name
/// back to its variant and anything else to `Other`.
fn okf_type(category: &LessonCategory) -> String {
    match category {
        LessonCategory::Other(inner) => inner.clone(),
        other => format!("{other:?}"),
    }
}

/// The OKF `title`: the first Markdown heading in the body, else the first
/// non-empty line, else the category token — always non-empty and bounded.
fn derive_title(entry: &MemoryEntry) -> String {
    let first = entry
        .body
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    let text = first.strip_prefix("# ").unwrap_or(first).trim();
    let title = if text.is_empty() {
        okf_type(&entry.category)
    } else {
        text.to_string()
    };
    truncate(&title, MAX_TITLE_LEN)
}

/// The OKF `description`: the first body line after the title line, bounded.
/// `None` for a single-line body (OKF `description` is optional).
fn derive_description(entry: &MemoryEntry) -> Option<String> {
    let mut content = entry.body.lines().map(str::trim).filter(|l| !l.is_empty());
    content.next()?; // the title line
    let next = content.next()?;
    let text = next.strip_prefix("# ").unwrap_or(next).trim();
    if text.is_empty() {
        None
    } else {
        Some(truncate(text, MAX_DESCRIPTION_LEN))
    }
}

/// The OKF `resource`: the primary related file, else the first evidence URI.
fn derive_resource(entry: &MemoryEntry) -> Option<String> {
    if let Some(file) = entry.related_files.first() {
        return Some(file.clone());
    }
    entry
        .evidence
        .iter()
        .find_map(|reference| reference.uri.clone())
}

/// A deterministic, stable id for a foreign concept, derived from its title and
/// body so re-importing the same concept yields the same id (byte-stable
/// re-export).
fn foreign_id(title: &str, body: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(title.as_bytes());
    hasher.update(b"\n");
    hasher.update(body.as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::from("okf-");
    for byte in digest.iter().take(8) {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Split a `key: value` front-matter line at the first colon. A bare `key:`
/// yields an empty value (the marker for a list/block that follows).
fn split_key_value(line: &str) -> (&str, &str) {
    match line.split_once(':') {
        Some((key, value)) => (key.trim(), value.trim()),
        None => (line.trim(), ""),
    }
}

/// The scalar of a `  - <value>` block-list line, or `None` when the line is not
/// a block item.
fn block_item(line: &str) -> Option<&str> {
    line.trim_start().strip_prefix("- ").map(str::trim)
}

/// Parse an inline-flow YAML sequence `[a, b, c]` into its unquoted items. A
/// value that is not bracketed is treated as a single-item list.
fn parse_flow_sequence(value: &str) -> Vec<String> {
    let trimmed = value.trim();
    let inner = trimmed
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(trimmed);
    inner
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(unquote)
        .collect()
}

/// Remove a surrounding single- or double-quote pair, collapsing YAML `''`
/// escapes inside single quotes.
fn unquote(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2 && value.starts_with('\'') && value.ends_with('\'') {
        value[1..value.len() - 1].replace("''", "'")
    } else if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        value[1..value.len() - 1].to_string()
    } else {
        value.to_string()
    }
}

/// Emit a scalar for OKF front matter, single-quoting (with `''` escaping) when
/// it contains characters that would break a bare YAML scalar.
fn scalar(value: &str) -> String {
    if value
        .chars()
        .any(|character| matches!(character, ':' | '#' | '\n' | '\'' | '"' | '[' | ']'))
    {
        format!("'{}'", value.replace('\'', "''"))
    } else {
        value.to_string()
    }
}

/// Truncate to a character budget without splitting a UTF-8 boundary.
fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        value.to_string()
    } else {
        value.chars().take(max).collect()
    }
}

/// An error parsing an OKF concept document.
#[derive(Debug, Error)]
pub enum OkfParseError {
    /// A foreign OKF concept omitted the one required field, `type`.
    #[error("OKF concept is missing the required `type` field")]
    MissingType,
    /// A reserved field held an unusable value.
    #[error("OKF concept has an invalid {field}")]
    InvalidField { field: &'static str },
    /// A propagated canonical-parser error (missing delimiters, malformed native
    /// document).
    #[error(transparent)]
    Native(#[from] MarkdownParseError),
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::{OkfFormat, OkfParseError, OKF_VERSION};
    use localmind_core::{
        Confidence, EvidenceKind, EvidenceRef, LessonCategory, MemoryEntry, MemoryEntryId,
        MemoryScope, MemoryStatus, SessionId,
    };
    use time::OffsetDateTime;

    fn rich_entry() -> MemoryEntry {
        MemoryEntry {
            id: MemoryEntryId::new("mem-1"),
            scope: MemoryScope::GlobalUser,
            body: "# Guard clauses\nPrefer a guard clause: it keeps the happy path flat."
                .to_string(),
            category: LessonCategory::DebuggingRecipe,
            confidence: Confidence::new(0.812).unwrap(),
            source_session: Some(SessionId::new("session-7")),
            evidence: vec![EvidenceRef::new(EvidenceKind::Transcript, "redacted").redacted()],
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
    fn export_carries_okf_reserved_keys_and_the_native_block() {
        let text = OkfFormat::to_okf(&rich_entry());
        // OKF reader-facing keys.
        assert!(text.contains(&format!("okf_version: \"{OKF_VERSION}\"")));
        assert!(text.contains("type: DebuggingRecipe"));
        assert!(text.contains("title: Guard clauses"));
        assert!(text.contains("resource: src/parser.rs"));
        // Native block still present, so LocalMind re-import is lossless.
        assert!(text.contains("id: mem-1"));
        assert!(text.contains("scope: GlobalUser"));
        assert!(text.contains("category: DebuggingRecipe"));
    }

    #[test]
    fn native_origin_round_trips_every_field() {
        let original = rich_entry();
        let text = OkfFormat::to_okf(&original);
        let parsed = OkfFormat::from_okf(&text).unwrap();

        assert_eq!(parsed.id, original.id);
        assert_eq!(parsed.scope, original.scope);
        assert_eq!(parsed.category, original.category);
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
        assert_eq!(parsed.evidence.len(), 1);
    }

    #[test]
    fn other_category_survives_the_native_round_trip() {
        let mut entry = rich_entry();
        entry.category = LessonCategory::Other("custom: tricky\"name".to_string());
        let parsed = OkfFormat::from_okf(&OkfFormat::to_okf(&entry)).unwrap();
        assert_eq!(parsed.category, entry.category);
    }

    #[test]
    fn foreign_concept_becomes_a_low_trust_review_entry() {
        // Only the required `type`, plus inline-flow tags and a quoted scalar —
        // shapes the canonical block reader does not accept.
        let text = "---\ntype: BigQuery Table\ntitle: \"Orders\"\ntags: [sales, revenue]\ntimestamp: 2026-05-28T14:30:00Z\n---\n\nOne row per completed customer order.\n";
        let parsed = OkfFormat::from_okf(text).unwrap();

        assert_eq!(
            parsed.category,
            LessonCategory::Other("BigQuery Table".to_string())
        );
        assert_eq!(parsed.scope, MemoryScope::Project);
        assert_eq!(
            parsed.tags,
            vec!["sales".to_string(), "revenue".to_string()]
        );
        assert_eq!(parsed.related_files, Vec::<String>::new());
        assert!((parsed.confidence.value() - 0.4).abs() < 0.001);
        assert_eq!(parsed.status, MemoryStatus::Active);
        assert!(parsed.updated_at.is_some());
        assert_eq!(parsed.body, "One row per completed customer order.");
        assert!(parsed.id.to_string().starts_with("okf-"));
    }

    #[test]
    fn foreign_id_is_deterministic() {
        let text = "---\ntype: Metric\n---\n\nWeekly active users.\n";
        let first = OkfFormat::from_okf(text).unwrap();
        let second = OkfFormat::from_okf(text).unwrap();
        assert_eq!(first.id, second.id);
    }

    #[test]
    fn foreign_concept_without_type_is_rejected() {
        let text = "---\ntitle: Untyped\n---\n\nBody.\n";
        assert!(matches!(
            OkfFormat::from_okf(text),
            Err(OkfParseError::MissingType)
        ));
    }

    #[test]
    fn missing_front_matter_is_rejected() {
        assert!(matches!(
            OkfFormat::from_okf("no front matter here"),
            Err(OkfParseError::Native(_))
        ));
    }
}
