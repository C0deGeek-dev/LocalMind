//! Immutable fact references and their identity.
//!
//! An [`EvidenceRef`] is the only fact record in this codebase: a model may
//! *select* the ids it is given, never mint or edit one. That guarantee is only
//! as strong as the id, so identity has a canonical construction —
//! [`stable_evidence_id`] over the producing source, the locator, and the
//! content fingerprint — and a recogniser, [`is_canonical_evidence_id`], for
//! callers that must refuse a fact whose id did not come from it.

use crate::EvidenceId;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Prefix on every canonically constructed evidence id, in the
/// `cgn-`/`cge-` style the code graph already uses.
pub const EVIDENCE_ID_PREFIX: &str = "ev-";

/// Hex characters of digest a canonical evidence id carries: SHA-256 truncated
/// to 128 bits. Short enough to read in a Markdown file, and far past the point
/// where the birthday bound matters for one machine's fact corpus.
pub const EVIDENCE_ID_DIGEST_HEX: usize = 32;

/// Domain separator, so this construction can never collide with another
/// SHA-256 use in the workspace even over identical input bytes.
const EVIDENCE_ID_SCHEME: &str = "localmind.evidence.id.v1";

/// Metadata key holding the producing source an id was derived from. Stored
/// because an id that cannot be recomputed cannot be verified.
pub const EVIDENCE_SOURCE_KEY: &str = "source";

/// Character ceiling on [`EvidenceRef::excerpt`]. An excerpt is the observation
/// a reader or a drafting model needs to recognise the fact, not the output it
/// was taken from; the full output stays wherever the locator points.
pub const MAX_EXCERPT_CHARS: usize = 500;

/// Appended to an excerpt cut at [`MAX_EXCERPT_CHARS`], so a reader can tell a
/// short observation from a long one that was cut.
pub const EXCERPT_TRUNCATION_MARKER: &str = " [truncated]";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EvidenceRef {
    pub id: EvidenceId,
    pub kind: EvidenceKind,
    pub label: String,
    pub uri: Option<String>,
    pub redacted: bool,
    pub content_hash: Option<String>,
    pub metadata: BTreeMap<String, String>,
    /// A bounded, redacted piece of what was observed, so the fact can be read
    /// without following its locator. Not an identity input: the
    /// `content_hash` fingerprints the content the excerpt was cut from.
    ///
    /// Omitted from the serialized form when absent, so a reference written
    /// before this field existed serializes — and therefore identifies its
    /// candidate — exactly as it did. Redaction happens before it is set and
    /// again when the review queue persists it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<String>,
}

impl EvidenceRef {
    /// A reference whose id is derived from its label.
    ///
    /// Two facts with the same label are therefore the same id. That is
    /// tolerable for a reference a human reads and not for one a claim cites —
    /// use [`EvidenceRef::identified`] wherever the id carries weight.
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
            excerpt: None,
        }
    }

    /// A reference with a canonical, collision-resistant identity.
    ///
    /// `source` is the producing context (a session, a run, an import),
    /// `locator` pins *where* the fact was read — it becomes the `uri` — and
    /// `content_hash` fingerprints *what* was read. The three together are the
    /// fact's identity: change any of them and this is a different fact with a
    /// different id, which is what makes staleness detectable rather than
    /// asserted. `source` is kept in metadata so the id can be recomputed from
    /// the record alone (see [`EvidenceRef::canonical_id`]).
    #[must_use]
    pub fn identified(
        kind: EvidenceKind,
        label: impl Into<String>,
        source: impl Into<String>,
        locator: impl Into<String>,
        content_hash: impl Into<String>,
    ) -> Self {
        let source = source.into();
        let locator = locator.into();
        let content_hash = content_hash.into();
        let id = stable_evidence_id(&kind, &source, &locator, &content_hash);

        Self {
            id,
            kind,
            label: label.into(),
            uri: Some(locator),
            redacted: false,
            content_hash: Some(content_hash),
            metadata: BTreeMap::from([(EVIDENCE_SOURCE_KEY.to_string(), source)]),
            excerpt: None,
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

    /// Attaches an excerpt of the observation, cut to [`MAX_EXCERPT_CHARS`]
    /// with [`EXCERPT_TRUNCATION_MARKER`] when it is longer. The caller redacts
    /// first: this type cannot, and the review queue's redaction on write is a
    /// second line, not the first. A blank excerpt is no excerpt.
    #[must_use]
    pub fn with_excerpt(mut self, excerpt: impl AsRef<str>) -> Self {
        self.excerpt = bound_excerpt(excerpt.as_ref());
        self
    }

    /// The producing source this reference's id was derived from, when it has
    /// one. `None` for a label-derived reference.
    #[must_use]
    pub fn source(&self) -> Option<&str> {
        self.metadata.get(EVIDENCE_SOURCE_KEY).map(String::as_str)
    }

    /// The id this reference's own fields produce, or `None` when it does not
    /// carry the identity inputs (a label-derived reference does not).
    #[must_use]
    pub fn canonical_id(&self) -> Option<EvidenceId> {
        let source = self.source()?;
        let locator = self.uri.as_deref()?;
        let content_hash = self.content_hash.as_deref()?;

        Some(stable_evidence_id(
            &self.kind,
            source,
            locator,
            content_hash,
        ))
    }

    /// Whether this reference's id has the shape [`stable_evidence_id`]
    /// produces. Shape only — [`EvidenceRef::identity_is_intact`] is the check
    /// that the id actually belongs to this content.
    #[must_use]
    pub fn has_canonical_id(&self) -> bool {
        is_canonical_evidence_id(&self.id)
    }

    /// Whether this reference's id still matches what its fields produce.
    ///
    /// `false` means the record was altered after it was minted: a rewritten
    /// observation, a re-pointed locator, or an id copied from another fact.
    /// A label-derived reference has no identity to verify and is `false` here,
    /// so a caller that requires verifiable facts refuses it by the same test.
    #[must_use]
    pub fn identity_is_intact(&self) -> bool {
        self.canonical_id()
            .is_some_and(|expected| expected == self.id)
    }
}

/// Trim an excerpt and cut it to [`MAX_EXCERPT_CHARS`], marker included, on a
/// character boundary. `None` for a blank one.
///
/// Public so a store that re-redacts a persisted excerpt can put the result
/// back under the same bound.
#[must_use]
pub fn bound_excerpt(excerpt: &str) -> Option<String> {
    let excerpt = excerpt.trim();
    if excerpt.is_empty() {
        return None;
    }
    if excerpt.chars().count() <= MAX_EXCERPT_CHARS {
        return Some(excerpt.to_string());
    }

    let keep = MAX_EXCERPT_CHARS - EXCERPT_TRUNCATION_MARKER.chars().count();
    let mut cut: String = excerpt.chars().take(keep).collect();
    cut.truncate(cut.trim_end().len());
    cut.push_str(EXCERPT_TRUNCATION_MARKER);
    Some(cut)
}

/// The canonical evidence id: SHA-256 over the kind, producing source, locator
/// and content fingerprint, truncated to [`EVIDENCE_ID_DIGEST_HEX`] hex
/// characters and prefixed with [`EVIDENCE_ID_PREFIX`].
///
/// The kind participates because the same bytes read at the same place mean
/// different things as a `Command` and as its `TestOutput`. Parts are
/// length-framed, so no choice of separator-bearing input can make two
/// different part lists hash alike.
#[must_use]
pub fn stable_evidence_id(
    kind: &EvidenceKind,
    source: &str,
    locator: &str,
    content_hash: &str,
) -> EvidenceId {
    let digest = digest_parts(&[
        EVIDENCE_ID_SCHEME,
        kind.as_str(),
        kind.qualifier(),
        source,
        locator,
        content_hash,
    ]);

    EvidenceId::new(format!("{EVIDENCE_ID_PREFIX}{digest}"))
}

/// Whether an id has the shape [`stable_evidence_id`] produces.
///
/// Callers that require verifiable facts reject anything else: without this, a
/// fact set mixing canonical and label-derived ids satisfies "every cited id was
/// supplied" while still being collidable.
#[must_use]
pub fn is_canonical_evidence_id(id: &EvidenceId) -> bool {
    let Some(digest) = id.as_str().strip_prefix(EVIDENCE_ID_PREFIX) else {
        return false;
    };

    digest.len() == EVIDENCE_ID_DIGEST_HEX
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// SHA-256 over length-framed parts, truncated to [`EVIDENCE_ID_DIGEST_HEX`]
/// hex characters.
///
/// Shared with candidate identity so the workspace has one content-addressing
/// construction rather than one per id type.
pub(crate) fn digest_parts(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(u64::try_from(part.len()).unwrap_or(u64::MAX).to_le_bytes());
        hasher.update(part.as_bytes());
    }

    let digest = hasher.finalize();
    let mut hex = String::with_capacity(EVIDENCE_ID_DIGEST_HEX);
    for byte in digest.iter().take(EVIDENCE_ID_DIGEST_HEX / 2) {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
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

impl EvidenceKind {
    /// The stable tag for this kind, used in identity construction. `Other`
    /// reports `"other"`; its payload is [`EvidenceKind::qualifier`], so a
    /// custom kind named `"transcript"` can never be mistaken for
    /// [`EvidenceKind::Transcript`].
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Transcript => "transcript",
            Self::ToolEvent => "tool_event",
            Self::Command => "command",
            Self::FileDiff => "file_diff",
            Self::TestOutput => "test_output",
            Self::Commit => "commit",
            Self::CodeParse => "code_parse",
            Self::RecoveryEvent => "recovery_event",
            Self::UserCorrection => "user_correction",
            Self::ManualNote => "manual_note",
            Self::Other(_) => "other",
        }
    }

    /// The custom name of an [`EvidenceKind::Other`], empty for every named
    /// variant.
    #[must_use]
    pub fn qualifier(&self) -> &str {
        match self {
            Self::Other(name) => name,
            _ => "",
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{
        is_canonical_evidence_id, stable_evidence_id, EvidenceKind, EvidenceRef,
        EVIDENCE_ID_DIGEST_HEX, EVIDENCE_ID_PREFIX, EXCERPT_TRUNCATION_MARKER, MAX_EXCERPT_CHARS,
    };
    use crate::EvidenceId;

    fn fact() -> EvidenceRef {
        EvidenceRef::identified(
            EvidenceKind::TestOutput,
            "review_queue dedup test failed",
            "session:abc",
            "repo@9f1c2d#tests/review_queue.rs:42",
            "sha256:0f1e2d",
        )
    }

    #[test]
    fn a_canonical_id_is_stable_for_the_same_fact() {
        assert_eq!(fact().id, fact().id);
        assert!(fact().has_canonical_id());
        assert!(fact().identity_is_intact());
    }

    #[test]
    fn every_identity_input_changes_the_id() {
        let base = fact().id;

        let other_kind = EvidenceRef::identified(
            EvidenceKind::Command,
            "review_queue dedup test failed",
            "session:abc",
            "repo@9f1c2d#tests/review_queue.rs:42",
            "sha256:0f1e2d",
        );
        let other_source = EvidenceRef::identified(
            EvidenceKind::TestOutput,
            "review_queue dedup test failed",
            "session:def",
            "repo@9f1c2d#tests/review_queue.rs:42",
            "sha256:0f1e2d",
        );
        let other_locator = EvidenceRef::identified(
            EvidenceKind::TestOutput,
            "review_queue dedup test failed",
            "session:abc",
            "repo@9f1c2d#tests/review_queue.rs:99",
            "sha256:0f1e2d",
        );
        let other_content = EvidenceRef::identified(
            EvidenceKind::TestOutput,
            "review_queue dedup test failed",
            "session:abc",
            "repo@9f1c2d#tests/review_queue.rs:42",
            "sha256:ffffff",
        );

        for (name, changed) in [
            ("kind", other_kind),
            ("source", other_source),
            ("locator", other_locator),
            ("content", other_content),
        ] {
            assert_ne!(base, changed.id, "changing the {name} must change the id");
        }
    }

    #[test]
    fn the_label_is_not_part_of_identity() {
        let relabelled = EvidenceRef::identified(
            EvidenceKind::TestOutput,
            "a completely different sentence",
            "session:abc",
            "repo@9f1c2d#tests/review_queue.rs:42",
            "sha256:0f1e2d",
        );

        // Prose describing a fact is not the fact. Two records of the same
        // observation stay one id however they are worded — the inverse of the
        // label-derived default, where two different facts collapse into one.
        assert_eq!(fact().id, relabelled.id);
    }

    #[test]
    fn a_custom_kind_cannot_impersonate_a_named_one() {
        let named = stable_evidence_id(&EvidenceKind::Transcript, "s", "l", "c");
        let custom = stable_evidence_id(
            &EvidenceKind::Other("transcript".to_string()),
            "s",
            "l",
            "c",
        );

        assert_ne!(named, custom);
    }

    #[test]
    fn parts_are_framed_so_a_shifted_boundary_is_a_different_id() {
        let left = stable_evidence_id(&EvidenceKind::Command, "ab", "c", "d");
        let right = stable_evidence_id(&EvidenceKind::Command, "a", "bc", "d");

        assert_ne!(left, right);
    }

    #[test]
    fn a_mutated_observation_no_longer_matches_its_id() {
        let mut tampered = fact();
        tampered.content_hash = Some("sha256:rewritten".to_string());

        assert!(tampered.has_canonical_id(), "the id still looks canonical");
        assert!(
            !tampered.identity_is_intact(),
            "but it no longer belongs to this content"
        );
    }

    #[test]
    fn an_id_copied_from_another_fact_is_caught() {
        let mut forged = fact();
        forged.id = stable_evidence_id(&EvidenceKind::Commit, "other", "elsewhere", "sha256:aa");

        assert!(forged.has_canonical_id());
        assert!(!forged.identity_is_intact());
    }

    #[test]
    fn a_label_derived_reference_has_no_verifiable_identity() {
        let legacy = EvidenceRef::new(EvidenceKind::Transcript, "redacted transcript");

        assert!(!legacy.has_canonical_id());
        assert!(!legacy.identity_is_intact());
        assert!(legacy.canonical_id().is_none());
        assert!(legacy.source().is_none());
    }

    #[test]
    fn the_recogniser_accepts_only_the_canonical_shape() {
        assert!(is_canonical_evidence_id(&fact().id));

        for rejected in [
            "redacted transcript",
            "ev-",
            "ev-0f1e",
            "ev-0F1E2D3C4B5A69788796A5B4C3D2E1F0",
            "ev-0f1e2d3c4b5a69788796a5b4c3d2e1f00",
            "xx-0f1e2d3c4b5a69788796a5b4c3d2e1f0",
        ] {
            assert!(
                !is_canonical_evidence_id(&EvidenceId::new(rejected)),
                "{rejected} must not pass as canonical"
            );
        }
    }

    #[test]
    fn the_id_carries_the_advertised_shape() {
        let id = fact().id;
        let digest = id
            .as_str()
            .strip_prefix(EVIDENCE_ID_PREFIX)
            .unwrap_or_default();

        assert_eq!(digest.len(), EVIDENCE_ID_DIGEST_HEX);
    }

    #[test]
    fn identified_pins_the_locator_and_keeps_the_source_recomputable() {
        let reference = fact();

        assert_eq!(
            reference.uri.as_deref(),
            Some("repo@9f1c2d#tests/review_queue.rs:42")
        );
        assert_eq!(reference.source(), Some("session:abc"));
        assert_eq!(reference.canonical_id(), Some(reference.id.clone()));
    }

    #[test]
    fn the_excerpt_is_not_part_of_identity() {
        let excerpted = fact().with_excerpt("assertion failed: left == right");

        // The content hash already fingerprints what was read. An excerpt is a
        // view of it, so cutting it differently must not mint a second fact.
        assert_eq!(fact().id, excerpted.id);
        assert!(excerpted.identity_is_intact());
    }

    #[test]
    fn a_reference_without_an_excerpt_serializes_as_it_did_before() {
        let json = serde_json::to_value(fact()).unwrap();

        // Candidate identity hashes the serialized candidate, so a new key on
        // every old reference would have shifted every stored identity.
        assert!(json.get("excerpt").is_none(), "{json}");
    }

    #[test]
    fn a_reference_written_before_the_excerpt_reads_without_one() {
        let old = r#"{"id":"ev-x","kind":"Transcript","label":"l","uri":null,
            "redacted":true,"content_hash":null,"metadata":{}}"#;
        let parsed: EvidenceRef = serde_json::from_str(old).unwrap();

        assert_eq!(parsed.excerpt, None);
        assert_eq!(
            serde_json::to_value(&parsed).unwrap(),
            serde_json::from_str::<serde_json::Value>(old).unwrap(),
            "and writes back byte-for-byte the same shape"
        );
    }

    #[test]
    fn an_excerpt_round_trips() {
        let excerpted = fact().with_excerpt("  exit code 101  ");
        let json = serde_json::to_string(&excerpted).unwrap();
        let back: EvidenceRef = serde_json::from_str(&json).unwrap();

        assert_eq!(back.excerpt.as_deref(), Some("exit code 101"));
        assert_eq!(back, excerpted);
    }

    #[test]
    fn a_long_excerpt_is_cut_on_a_character_boundary_and_says_so() {
        let long = "é".repeat(MAX_EXCERPT_CHARS * 2);
        let excerpt = fact().with_excerpt(&long).excerpt.unwrap();

        assert_eq!(excerpt.chars().count(), MAX_EXCERPT_CHARS);
        assert!(excerpt.ends_with(EXCERPT_TRUNCATION_MARKER));

        let exact = "a".repeat(MAX_EXCERPT_CHARS);
        let kept = fact().with_excerpt(&exact).excerpt.unwrap();
        assert_eq!(kept, exact, "an excerpt at the bound is not cut");
    }

    #[test]
    fn a_blank_excerpt_is_no_excerpt() {
        assert_eq!(
            fact()
                .with_excerpt(
                    "  
 "
                )
                .excerpt,
            None
        );
    }
}
