//! Incremental, signed, encrypted sync payloads.
//!
//! A sync exchange moves an **op-bundle** — a set of create/update/supersede/
//! tombstone ops over accepted memory — from one of the owner's devices to the
//! others. The op-bundle is signed with the existing Ed25519 identity (reusing
//! [`sign_detached`](crate::sign_detached), one signer) and then *sealed to each
//! enrolled device's X25519 key*, so the transport folder only ever holds
//! ciphertext. Fail-closed: an op-bundle that cannot be encrypted to at least
//! one enrolled peer is never produced.
//!
//! Op identity is **content-addressed** (`op_id` = digest of kind+memory+version)
//! and the per-peer cursor records the memory versions a peer has already seen,
//! so re-exports are idempotent and echo loops are avoided without a vector
//! clock — concurrent edits are reconciled by routing to review on import
//! (D-LM-0016), never last-writer-wins, so causal counters are an optimization
//! this layer does not need (see the plan's decision log).

use crate::{
    author_fingerprint, digest_hex, sign_detached, verify_detached, Device, SignatureEnvelope,
    SigningError, VerificationOutcome,
};
use crypto_box::{aead::OsRng, PublicKey as EncryptionPublicKey, SecretKey as EncryptionSecretKey};
use ed25519_dalek::SigningKey;
use localmind_core::MemoryEntry;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

/// Op-bundle format version (independent of the memory-bundle and signature
/// versions).
pub const SYNC_BUNDLE_FORMAT_VERSION: u32 = 1;
/// Encrypted-envelope format version.
pub const ENCRYPTED_BUNDLE_FORMAT_VERSION: u32 = 1;

/// What a sync op does to a memory on the destination device.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpKind {
    /// The destination has never seen this memory.
    Create,
    /// The destination has an older version of this memory.
    Update,
    /// This memory supersedes a prior one (its `supersedes` edge carries which).
    Supersede,
    /// This memory was deleted on the origin (carries no payload).
    Tombstone,
}

/// One change to one memory. `entry` is present for create/update/supersede and
/// absent for a tombstone; `version` is the content version the op advances the
/// peer's cursor to; `origin_device` is provenance only (never a merge tiebreak).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SyncOp {
    pub op_id: String,
    pub kind: OpKind,
    pub memory_id: String,
    pub version: String,
    #[serde(default)]
    pub origin_device: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<MemoryEntry>,
}

impl SyncOp {
    pub(crate) fn new(
        kind: OpKind,
        memory_id: String,
        version: String,
        origin_device: String,
        entry: Option<MemoryEntry>,
    ) -> Self {
        let op_id = digest_hex(format!("{}:{memory_id}:{version}", kind_tag(kind)).as_bytes());
        Self {
            op_id,
            kind,
            memory_id,
            version,
            origin_device,
            entry,
        }
    }
}

fn kind_tag(kind: OpKind) -> &'static str {
    match kind {
        OpKind::Create => "create",
        OpKind::Update => "update",
        OpKind::Supersede => "supersede",
        OpKind::Tombstone => "tombstone",
    }
}

/// A set of ops exported for one peer, ordered deterministically by `op_id`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SyncBundle {
    pub format_version: u32,
    pub ops: Vec<SyncOp>,
}

impl SyncBundle {
    #[must_use]
    pub fn new(mut ops: Vec<SyncOp>) -> Self {
        ops.sort_by(|a, b| a.op_id.cmp(&b.op_id));
        Self {
            format_version: SYNC_BUNDLE_FORMAT_VERSION,
            ops,
        }
    }

    /// Deterministic canonical bytes (ops sorted by id, compact JSON) — the input
    /// to signing and to the content digest, stable across runs and machines.
    ///
    /// # Errors
    /// [`SyncBundleError::Serialize`] if serialization fails.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, SyncBundleError> {
        let mut sorted = self.clone();
        sorted.ops.sort_by(|a, b| a.op_id.cmp(&b.op_id));
        serde_json::to_vec(&sorted).map_err(|e| SyncBundleError::Serialize(e.to_string()))
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }
}

/// An op-bundle plus its Ed25519 signature over its canonical bytes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SignedSyncBundle {
    pub bundle: SyncBundle,
    pub signature: SignatureEnvelope,
}

impl SignedSyncBundle {
    /// Sign an op-bundle with the device's Ed25519 signing key.
    ///
    /// # Errors
    /// [`SyncBundleError`] if the bundle cannot be serialized.
    pub fn sign(bundle: SyncBundle, signing_key: &SigningKey) -> Result<Self, SyncBundleError> {
        let canonical = bundle.canonical_bytes()?;
        let signature = sign_detached(&canonical, signing_key);
        Ok(Self { bundle, signature })
    }

    /// Verify the signature fail-closed and classify trust against `trusted_keys`.
    /// A `format_version` newer than this build understands is rejected.
    #[must_use]
    pub fn verify(&self, trusted_keys: &[[u8; 32]]) -> VerificationOutcome {
        if self.bundle.format_version > SYNC_BUNDLE_FORMAT_VERSION {
            return VerificationOutcome::Rejected {
                reason: crate::RejectReason::UnsupportedBundleVersion,
            };
        }
        let Ok(canonical) = self.bundle.canonical_bytes() else {
            return VerificationOutcome::Rejected {
                reason: crate::RejectReason::BadDigest,
            };
        };
        verify_detached(&canonical, &self.signature, trusted_keys)
    }

    fn to_json(&self) -> Result<Vec<u8>, SyncBundleError> {
        serde_json::to_vec(self).map_err(|e| SyncBundleError::Serialize(e.to_string()))
    }
}

/// One recipient's sealed copy inside an [`EncryptedBundle`]. `fingerprint` is
/// the recipient device's signing fingerprint (which sealed copy is whose);
/// `sealed` is the hex crypto_box sealed box of the signed op-bundle JSON.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SealedCopy {
    pub fingerprint: String,
    pub sealed: String,
}

/// The only thing that lands in the sync folder: a per-recipient set of sealed
/// copies of the signed op-bundle. No plaintext, no memory titles, no signer
/// identity in the clear beyond opaque recipient fingerprints.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EncryptedBundle {
    pub format_version: u32,
    pub recipients: Vec<SealedCopy>,
}

impl EncryptedBundle {
    /// Seal a signed op-bundle to every enrolled device. **Fail-closed**: with no
    /// devices, nothing is written ([`SyncBundleError::NoRecipients`]) — plaintext
    /// never reaches the folder because an unencryptable bundle is never produced.
    ///
    /// # Errors
    /// [`SyncBundleError::NoRecipients`] if `devices` is empty;
    /// [`SyncBundleError`] on a serialization or sealing failure.
    pub fn seal_to_devices(
        signed: &SignedSyncBundle,
        devices: &[Device],
    ) -> Result<Self, SyncBundleError> {
        if devices.is_empty() {
            return Err(SyncBundleError::NoRecipients);
        }
        let plaintext = signed.to_json()?;
        let mut recipients = Vec::with_capacity(devices.len());
        for device in devices {
            let public = EncryptionPublicKey::from_bytes(device.encryption_key);
            let sealed = public
                .seal(&mut OsRng, &plaintext)
                .map_err(|_| SyncBundleError::Encrypt)?;
            recipients.push(SealedCopy {
                fingerprint: device.fingerprint.clone(),
                sealed: to_hex(&sealed),
            });
        }
        Ok(Self {
            format_version: ENCRYPTED_BUNDLE_FORMAT_VERSION,
            recipients,
        })
    }

    /// Open the bundle with this device's X25519 secret key, returning the signed
    /// op-bundle. Tries every sealed copy; a device that was not a recipient
    /// cannot decrypt any of them.
    ///
    /// # Errors
    /// [`SyncBundleError::UnsupportedVersion`] for a newer envelope;
    /// [`SyncBundleError::NotARecipient`] if no sealed copy opens with this key;
    /// [`SyncBundleError::Malformed`] if a decrypted copy is not a valid bundle.
    pub fn open(
        &self,
        device_secret: &EncryptionSecretKey,
    ) -> Result<SignedSyncBundle, SyncBundleError> {
        if self.format_version > ENCRYPTED_BUNDLE_FORMAT_VERSION {
            return Err(SyncBundleError::UnsupportedVersion);
        }
        for copy in &self.recipients {
            let Some(ciphertext) = from_hex(&copy.sealed) else {
                continue;
            };
            if let Ok(plaintext) = device_secret.unseal(&ciphertext) {
                return serde_json::from_slice(&plaintext)
                    .map_err(|e| SyncBundleError::Malformed(e.to_string()));
            }
        }
        Err(SyncBundleError::NotARecipient)
    }

    /// Pretty JSON for writing to the folder.
    ///
    /// # Errors
    /// [`SyncBundleError::Serialize`] on failure.
    pub fn to_pretty_json(&self) -> Result<String, SyncBundleError> {
        serde_json::to_string_pretty(self).map_err(|e| SyncBundleError::Serialize(e.to_string()))
    }

    /// Parse an encrypted bundle from folder JSON.
    ///
    /// # Errors
    /// [`SyncBundleError::Malformed`] on malformed JSON.
    pub fn from_json(text: &str) -> Result<Self, SyncBundleError> {
        serde_json::from_str(text).map_err(|e| SyncBundleError::Malformed(e.to_string()))
    }

    /// An opaque, content-addressed name for the folder artefact — the digest of
    /// the ciphertext, so it leaks no memory title, id, or count.
    #[must_use]
    pub fn content_address(&self) -> String {
        let joined: String = self.recipients.iter().map(|c| c.sealed.as_str()).collect();
        digest_hex(joined.as_bytes())
    }
}

/// The per-peer cursor: the content version of each memory a peer has already
/// seen. Persisted so re-exports send only changes and never echo a memory back.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SyncCursor {
    #[serde(default)]
    pub versions: BTreeMap<String, String>,
}

impl SyncCursor {
    /// Diff `entries` (already filtered to syncable, redacted memory) against
    /// this cursor into the ops a peer needs, and return the advanced cursor to
    /// persist after a successful export. Unchanged memories produce no op.
    #[must_use]
    pub fn diff(
        &self,
        entries: &[MemoryEntry],
        own_device_label: &str,
    ) -> (Vec<SyncOp>, SyncCursor) {
        let mut ops = Vec::new();
        let mut advanced = self.clone();
        for entry in entries {
            let version = memory_version(entry);
            let memory_id = entry.id.to_string();
            let seen = self.versions.get(&memory_id);
            if seen == Some(&version) {
                continue; // peer already has this exact version
            }
            let kind = match (seen, entry.supersedes.is_empty()) {
                (None, _) => OpKind::Create,
                (Some(_), false) => OpKind::Supersede,
                (Some(_), true) => OpKind::Update,
            };
            let origin_device = entry
                .sync_meta
                .origin_env
                .as_ref()
                .map(|env| env.device_label.clone())
                .filter(|label| !label.is_empty())
                .unwrap_or_else(|| own_device_label.to_string());
            advanced.versions.insert(memory_id.clone(), version.clone());
            ops.push(SyncOp::new(
                kind,
                memory_id,
                version,
                origin_device,
                Some(entry.clone()),
            ));
        }
        (ops, advanced)
    }
}

/// The content version of a memory: a digest of its canonical serialization, so
/// any change to a synced field yields a new version.
pub(crate) fn memory_version(entry: &MemoryEntry) -> String {
    match serde_json::to_vec(entry) {
        Ok(bytes) => digest_hex(&bytes)[..16].to_string(),
        // A memory that cannot serialize cannot sync; a stable sentinel keeps the
        // function total (the export path skips such an entry downstream).
        Err(_) => "unserializable".to_string(),
    }
}

fn to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn from_hex(text: &str) -> Option<Vec<u8>> {
    let text = text.trim();
    if text.len() % 2 != 0 {
        return None;
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(text.len() / 2);
    let mut index = 0;
    while index < bytes.len() {
        let high = (bytes[index] as char).to_digit(16)?;
        let low = (bytes[index + 1] as char).to_digit(16)?;
        out.push(((high << 4) | low) as u8);
        index += 2;
    }
    Some(out)
}

/// The fingerprint of a device's *signing* key — how a recipient copy is
/// labelled and how a device is named in diagnostics.
#[must_use]
pub fn device_signing_fingerprint(device: &Device) -> String {
    author_fingerprint(&device.signing_key)
}

/// Errors producing or opening a sync payload. Never carries memory content.
#[derive(Debug, Error)]
pub enum SyncBundleError {
    #[error("failed to serialize sync bundle: {0}")]
    Serialize(String),
    #[error("sync bundle is malformed: {0}")]
    Malformed(String),
    #[error("refusing to write a sync bundle with no enrolled recipient devices")]
    NoRecipients,
    #[error("failed to encrypt a sync bundle to a recipient")]
    Encrypt,
    #[error("this device is not a recipient of the encrypted bundle")]
    NotARecipient,
    #[error("encrypted bundle format is newer than this build supports")]
    UnsupportedVersion,
    #[error(transparent)]
    Signing(#[from] SigningError),
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use localmind_core::{
        Confidence, EnvFingerprint, LessonCategory, MemoryEntryId, MemoryScope, MemoryStatus,
        SyncMeta,
    };

    fn entry(id: &str, body: &str, supersedes: Vec<&str>) -> MemoryEntry {
        MemoryEntry {
            id: MemoryEntryId::new(id),
            scope: MemoryScope::Project,
            body: body.to_string(),
            category: LessonCategory::CodePattern,
            confidence: Confidence::new(0.9).unwrap(),
            source_session: None,
            evidence: Vec::new(),
            tags: Vec::new(),
            related_files: Vec::new(),
            related_entities: Vec::new(),
            created_at: None,
            updated_at: None,
            supersedes: supersedes.into_iter().map(MemoryEntryId::new).collect(),
            contradicts: Vec::new(),
            status: MemoryStatus::Active,
            sync_meta: SyncMeta {
                disposition: None,
                origin_env: Some(EnvFingerprint::capture("PC")),
            },
        }
    }

    fn device(secret: &EncryptionSecretKey, signing: [u8; 32]) -> Device {
        Device {
            label: "peer".to_string(),
            signing_key: signing,
            encryption_key: *secret.public_key().as_bytes(),
            fingerprint: author_fingerprint(&signing),
        }
    }

    #[test]
    fn diff_produces_create_then_update_then_supersede_and_skips_unchanged() {
        let cursor = SyncCursor::default();
        let e1 = entry("m1", "first", vec![]);
        let (ops, cursor) = cursor.diff(&[e1.clone()], "PC");
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].kind, OpKind::Create);
        assert_eq!(ops[0].origin_device, "PC");

        // Re-diffing the same entry produces nothing (idempotent, no echo).
        let (none, cursor) = cursor.diff(&[e1.clone()], "PC");
        assert!(none.is_empty());

        // An edited body is an Update.
        let e1b = entry("m1", "first, edited", vec![]);
        let (ops, cursor) = cursor.diff(&[e1b], "PC");
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].kind, OpKind::Update);

        // A new memory that supersedes another is a Supersede once the peer has
        // seen a prior version of it.
        let s1 = entry("m1", "first, edited", vec!["m0"]);
        let (ops, _cursor) = cursor.diff(&[s1], "PC");
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].kind, OpKind::Supersede);
    }

    #[test]
    fn signed_op_bundle_round_trips_and_detects_tampering() {
        let signing = SigningKey::from_bytes(&[7u8; 32]);
        let trusted = [signing.verifying_key().to_bytes()];
        let (ops, _) = SyncCursor::default().diff(&[entry("m1", "b", vec![])], "PC");
        let signed = SignedSyncBundle::sign(SyncBundle::new(ops), &signing).unwrap();
        assert!(matches!(
            signed.verify(&trusted),
            VerificationOutcome::Verified { .. }
        ));
        // Tamper with an op after signing.
        let mut tampered = signed.clone();
        tampered.bundle.ops[0].memory_id = "m2".to_string();
        assert!(matches!(
            tampered.verify(&trusted),
            VerificationOutcome::Rejected { .. }
        ));
    }

    #[test]
    fn sealed_bundle_opens_only_for_an_enrolled_recipient() {
        let signing = SigningKey::from_bytes(&[9u8; 32]);
        let (ops, _) = SyncCursor::default().diff(&[entry("m1", "secret body", vec![])], "PC");
        let signed = SignedSyncBundle::sign(SyncBundle::new(ops), &signing).unwrap();

        let recipient = EncryptionSecretKey::from([3u8; 32]);
        let outsider = EncryptionSecretKey::from([4u8; 32]);
        let devices = vec![device(&recipient, [1u8; 32])];

        let encrypted = EncryptedBundle::seal_to_devices(&signed, &devices).unwrap();
        // The recipient recovers the exact signed op-bundle.
        let opened = encrypted.open(&recipient).unwrap();
        assert_eq!(opened, signed);
        // A non-enrolled device cannot open it.
        assert!(matches!(
            encrypted.open(&outsider),
            Err(SyncBundleError::NotARecipient)
        ));
    }

    #[test]
    fn an_op_bundle_carries_no_derived_state() {
        // Derived state — vectors, code graph, usage counters — is never part of
        // the sync payload; it is rebuilt locally after import. The op-bundle
        // serializes only MemoryEntry (Markdown source-of-truth) fields, so a
        // serialized bundle must contain none of those column/table markers.
        let (ops, _) = SyncCursor::default().diff(&[entry("m1", "b", vec![])], "PC");
        let json = serde_json::to_string(&SyncBundle::new(ops)).unwrap();
        for marker in [
            "hit_count",
            "last_used_at",
            "vector_blob",
            "vector_index",
            "graph_node",
            "graph_edge",
        ] {
            assert!(
                !json.contains(marker),
                "op-bundle must not carry derived-state field `{marker}`:\n{json}"
            );
        }
    }

    #[test]
    fn encryption_is_fail_closed_with_no_recipients() {
        let signing = SigningKey::from_bytes(&[1u8; 32]);
        let (ops, _) = SyncCursor::default().diff(&[entry("m1", "b", vec![])], "PC");
        let signed = SignedSyncBundle::sign(SyncBundle::new(ops), &signing).unwrap();
        assert!(matches!(
            EncryptedBundle::seal_to_devices(&signed, &[]),
            Err(SyncBundleError::NoRecipients)
        ));
    }

    #[test]
    fn the_folder_artefact_holds_no_plaintext_memory_content() {
        let signing = SigningKey::from_bytes(&[5u8; 32]);
        let secret_body = "prefer ripgrep over grep for speed";
        let (ops, _) = SyncCursor::default().diff(&[entry("m1", secret_body, vec![])], "PC");
        let signed = SignedSyncBundle::sign(SyncBundle::new(ops), &signing).unwrap();
        let recipient = EncryptionSecretKey::from([8u8; 32]);
        let encrypted =
            EncryptedBundle::seal_to_devices(&signed, &[device(&recipient, [2u8; 32])]).unwrap();

        let on_disk = encrypted.to_pretty_json().unwrap();
        assert!(
            !on_disk.contains(secret_body),
            "the folder artefact must not contain plaintext memory content"
        );
        assert!(!on_disk.contains("prefer ripgrep"));
        // The content address is opaque (a hex digest), not a memory title/id.
        let name = encrypted.content_address();
        assert_eq!(name.len(), 64);
        assert!(!name.contains("m1"));
    }
}
