//! Sign a memory bundle on export and verify it on import (fail-closed).
//!
//! A portable memory bundle is made tamper-evident and attributable by a content
//! digest plus an Ed25519 signature over its canonical bytes. On
//! import, verification recomputes the digest, checks the signature, and
//! validates the schema/version, then classifies the result:
//!
//! - `Trusted`   — a valid signature by a *known* key (your own, or one you have
//!   added to the local trust store).
//! - `Untrusted` — a valid signature by an *unknown* key (heavier review).
//! - `Rejected`  — a bad digest, bad signature, malformed key/signature, or an
//!   unsupported schema/version.
//!
//! A verified signature attests the **author/integrity**, never the *content* —
//! imported memory is still review-gated downstream. Trust is local: a local
//! keypair and a manual trust list, no PKI or network.
//!
//! Key storage follows the BYOK pattern (ADR-0042): a `0600` owner-only file
//! under the per-user home (beside the machine-wide global store). The private
//! key never leaves this module's audited write call — it is never serialized
//! into a bundle, logged, or `Debug`-printed.

use crate::{MemoryBundle, ProjectConfig, StoreConfigError, MEMORY_BUNDLE_FORMAT_VERSION};
use crypto_box::SecretKey as EncryptionSecretKey;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Signature-envelope schema version (independent of the bundle format version).
pub const SIGNATURE_SCHEMA_VERSION: u32 = 1;
const SIGNATURE_ALG: &str = "ed25519";
const DIGEST_ALG: &str = "sha256";
const SIGNING_KEY_FILE: &str = "signing.json";
const TRUSTED_KEYS_FILE: &str = "trusted.json";
/// The per-device X25519 encryption key, stored beside the Ed25519 signing key.
/// Separate from the signing key: signing proves authorship, this key receives
/// encrypted sync bundles.
const DEVICE_KEY_FILE: &str = "device.json";

/// The tamper-evidence + attribution envelope attached to a bundle. Carries only
/// public material (digest, signature, public key, fingerprint) — never a secret.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SignatureEnvelope {
    /// Signature algorithm (`ed25519`).
    pub alg: String,
    /// Digest algorithm (`sha256`).
    pub digest_alg: String,
    /// Envelope schema version.
    pub schema_version: u32,
    /// Hex SHA-256 of the bundle's canonical bytes.
    pub digest: String,
    /// Hex Ed25519 signature (64 bytes) over the canonical bytes.
    pub signature: String,
    /// Hex Ed25519 public key (32 bytes).
    pub public_key: String,
    /// Key-bound author fingerprint (first 16 hex of `sha256(public_key)`).
    pub author: String,
}

/// A bundle plus its signature envelope — the on-disk signed pack.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SignedBundle {
    /// The portable bundle.
    pub bundle: MemoryBundle,
    /// The signature over its canonical bytes.
    pub signature: SignatureEnvelope,
}

impl SignedBundle {
    /// Pretty JSON for writing the signed pack to a file.
    ///
    /// # Errors
    /// [`SigningError::Serialize`] if serialization fails.
    pub fn to_pretty_json(&self) -> Result<String, SigningError> {
        serde_json::to_string_pretty(self).map_err(|e| SigningError::Serialize(e.to_string()))
    }

    /// Parse a signed pack from JSON.
    ///
    /// # Errors
    /// [`SigningError::Malformed`] on malformed JSON.
    pub fn from_json(text: &str) -> Result<Self, SigningError> {
        serde_json::from_str(text).map_err(|e| SigningError::Malformed(e.to_string()))
    }
}

/// How much a verified bundle is trusted: a verified *author/integrity* signal,
/// not a statement about the *content* (which stays review-gated).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrustClass {
    /// Valid signature by a known/accepted key (your own or trusted).
    Trusted,
    /// Valid signature by an unknown key — import is allowed but flagged heavier.
    Untrusted,
}

/// Why a bundle failed verification (fail-closed: any doubt rejects).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RejectReason {
    /// The signature envelope's alg/digest_alg/schema is unsupported.
    UnsupportedSchema,
    /// The bundle's `format_version` is newer than this build supports.
    UnsupportedBundleVersion,
    /// The public key is missing or not a valid Ed25519 key.
    MalformedKey,
    /// The signature is missing or not 64 bytes.
    MalformedSignature,
    /// The stated author fingerprint does not match the public key.
    AuthorMismatch,
    /// The recomputed digest does not match the envelope's digest.
    BadDigest,
    /// The signature does not verify over the canonical bytes.
    BadSignature,
}

impl RejectReason {
    /// A short, secret-free label for diagnostics.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            RejectReason::UnsupportedSchema => "unsupported signature schema",
            RejectReason::UnsupportedBundleVersion => "unsupported bundle version",
            RejectReason::MalformedKey => "malformed public key",
            RejectReason::MalformedSignature => "malformed signature",
            RejectReason::AuthorMismatch => "author does not match public key",
            RejectReason::BadDigest => "content digest mismatch",
            RejectReason::BadSignature => "signature does not verify",
        }
    }
}

/// The outcome of verifying a signed bundle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VerificationOutcome {
    /// The signature and digest are valid; `class` says whether the key is known.
    Verified {
        /// Whether the signing key is trusted.
        class: TrustClass,
        /// The author fingerprint of the signing key.
        author: String,
    },
    /// Verification failed; the bundle must not reach the store.
    Rejected {
        /// Why it was rejected.
        reason: RejectReason,
    },
}

impl VerificationOutcome {
    /// Whether the bundle is safe to *route into the review queue* (a `Verified`
    /// result of either trust class). A `Rejected` bundle never proceeds.
    #[must_use]
    pub fn may_proceed(&self) -> bool {
        matches!(self, VerificationOutcome::Verified { .. })
    }
}

/// SHA-256 hex digest of `bytes`.
#[must_use]
pub fn digest_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    to_hex(&hasher.finalize())
}

/// A short, key-bound author fingerprint: first 16 hex chars of
/// `sha256(public_key)`. Deterministic from the key, so an author cannot be
/// spoofed with a different key.
#[must_use]
pub fn author_fingerprint(public_key: &[u8]) -> String {
    digest_hex(public_key)[..16].to_string()
}

/// Sign `bundle` with `signing_key`, producing a signed pack. The signature and
/// digest are computed over the bundle's deterministic canonical bytes.
///
/// # Errors
/// [`SigningError::Serialize`] if the bundle cannot be serialized to its
/// canonical form.
pub fn sign_bundle(
    bundle: &MemoryBundle,
    signing_key: &SigningKey,
) -> Result<SignedBundle, SigningError> {
    let canonical = bundle
        .canonical_bytes()
        .map_err(|e| SigningError::Serialize(e.to_string()))?;
    Ok(SignedBundle {
        bundle: bundle.clone(),
        signature: sign_detached(&canonical, signing_key),
    })
}

/// Sign arbitrary canonical bytes, producing the shared [`SignatureEnvelope`].
/// This is the one signer both the portable memory bundle and the sync op-bundle
/// use, so there is no second signing path to keep in step.
#[must_use]
pub fn sign_detached(canonical: &[u8], signing_key: &SigningKey) -> SignatureEnvelope {
    let signature = signing_key.sign(canonical);
    let public_key_bytes = signing_key.verifying_key().to_bytes();
    SignatureEnvelope {
        alg: SIGNATURE_ALG.to_string(),
        digest_alg: DIGEST_ALG.to_string(),
        schema_version: SIGNATURE_SCHEMA_VERSION,
        digest: digest_hex(canonical),
        signature: to_hex(&signature.to_bytes()),
        public_key: to_hex(&public_key_bytes),
        author: author_fingerprint(&public_key_bytes),
    }
}

/// Verify a [`SignatureEnvelope`] over arbitrary canonical bytes, fail-closed —
/// the algorithm/schema, key, author binding, digest, and signature checks
/// shared by every signed artefact (a caller adds any payload-specific version
/// check of its own). `trusted_keys` classify a valid signature as
/// [`TrustClass::Trusted`]; any other valid key is `Untrusted`.
#[must_use]
pub fn verify_detached(
    canonical: &[u8],
    envelope: &SignatureEnvelope,
    trusted_keys: &[[u8; 32]],
) -> VerificationOutcome {
    if envelope.alg != SIGNATURE_ALG
        || envelope.digest_alg != DIGEST_ALG
        || envelope.schema_version > SIGNATURE_SCHEMA_VERSION
    {
        return reject(RejectReason::UnsupportedSchema);
    }
    let Some(public_key_bytes) = from_hex_array::<32>(&envelope.public_key) else {
        return reject(RejectReason::MalformedKey);
    };
    let Ok(verifying_key) = VerifyingKey::from_bytes(&public_key_bytes) else {
        return reject(RejectReason::MalformedKey);
    };
    if author_fingerprint(&public_key_bytes) != envelope.author {
        return reject(RejectReason::AuthorMismatch);
    }
    if digest_hex(canonical) != envelope.digest {
        return reject(RejectReason::BadDigest);
    }
    let Some(signature_bytes) = from_hex_array::<64>(&envelope.signature) else {
        return reject(RejectReason::MalformedSignature);
    };
    let signature = Signature::from_bytes(&signature_bytes);
    if verifying_key.verify(canonical, &signature).is_err() {
        return reject(RejectReason::BadSignature);
    }
    let class = if trusted_keys.iter().any(|key| key == &public_key_bytes) {
        TrustClass::Trusted
    } else {
        TrustClass::Untrusted
    };
    VerificationOutcome::Verified {
        class,
        author: envelope.author.clone(),
    }
}

/// Verify a signed bundle, fail-closed. `trusted_keys` are the raw 32-byte public
/// keys that classify a valid signature as [`TrustClass::Trusted`] (the caller
/// includes its own public key here); any other valid key is `Untrusted`.
#[must_use]
pub fn verify_signed(signed: &SignedBundle, trusted_keys: &[[u8; 32]]) -> VerificationOutcome {
    // The bundle-version check is specific to the portable memory bundle; the
    // rest of the fail-closed verification (schema/key/author/digest/signature/
    // trust) is the shared detached path over the bundle's canonical bytes.
    if signed.bundle.format_version > MEMORY_BUNDLE_FORMAT_VERSION {
        return reject(RejectReason::UnsupportedBundleVersion);
    }
    let Ok(canonical) = signed.bundle.canonical_bytes() else {
        return reject(RejectReason::BadDigest);
    };
    verify_detached(&canonical, &signed.signature, trusted_keys)
}

fn reject(reason: RejectReason) -> VerificationOutcome {
    VerificationOutcome::Rejected { reason }
}

/// The local signing keypair store + trust list, kept under the per-user home
/// beside the machine-wide global memory store (ADR-0042 `0600` file tier).
pub struct KeyStore {
    keys_dir: PathBuf,
}

/// The on-disk signing-key file. The private key is the secret — protected by the
/// file's owner-only mode and per-user location, never logged or shared.
#[derive(Serialize, Deserialize)]
struct StoredKeyPair {
    private_key: String,
    public_key: String,
}

/// The on-disk X25519 device-key file. Same protection posture as the signing
/// key — the secret is owner-only and never leaves this module.
#[derive(Serialize, Deserialize)]
struct StoredDeviceKey {
    private_key: String,
    public_key: String,
}

#[derive(Default, Serialize, Deserialize)]
struct TrustedKeys {
    #[serde(default)]
    trusted: Vec<TrustedKey>,
}

#[derive(Clone, Serialize, Deserialize)]
struct TrustedKey {
    public_key: String,
    #[serde(default)]
    label: String,
    /// The device's X25519 encryption public key (hex). Present for an enrolled
    /// sync *device*; absent for a legacy signer-only trusted key, which stays
    /// trusted for verification but is not an encryption target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    encryption_key: Option<String>,
}

/// The shareable public identity of a device: its label and its two public keys
/// (Ed25519 signing + X25519 encryption). Carried out-of-band to another of the
/// owner's machines to enroll it; contains **no** secret material. The
/// fingerprint (of the signing key) is what the two machines compare by hand.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeviceCard {
    pub label: String,
    /// Hex Ed25519 signing public key (32 bytes).
    pub signing_key: String,
    /// Hex X25519 encryption public key (32 bytes).
    pub encryption_key: String,
}

impl DeviceCard {
    /// The key-bound fingerprint the two machines verify out-of-band (first 16
    /// hex of `sha256(signing_key)`), matching bundle-author fingerprints.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        from_hex_array::<32>(&self.signing_key)
            .map(|key| author_fingerprint(&key))
            .unwrap_or_default()
    }

    /// The signing/encryption keys as raw bytes, or an error if either is not
    /// 32 hex bytes.
    fn key_bytes(&self) -> Result<([u8; 32], [u8; 32]), SigningError> {
        let signing = from_hex_array::<32>(&self.signing_key).ok_or_else(|| {
            SigningError::Malformed("device signing key is not 32 hex bytes".into())
        })?;
        let encryption = from_hex_array::<32>(&self.encryption_key).ok_or_else(|| {
            SigningError::Malformed("device encryption key is not 32 hex bytes".into())
        })?;
        Ok((signing, encryption))
    }

    /// Pretty JSON for sharing the card between machines.
    ///
    /// # Errors
    /// [`SigningError::Serialize`] if serialization fails.
    pub fn to_pretty_json(&self) -> Result<String, SigningError> {
        serde_json::to_string_pretty(self).map_err(|e| SigningError::Serialize(e.to_string()))
    }

    /// Parse a device card from JSON.
    ///
    /// # Errors
    /// [`SigningError::Malformed`] on malformed JSON.
    pub fn from_json(text: &str) -> Result<Self, SigningError> {
        serde_json::from_str(text).map_err(|e| SigningError::Malformed(e.to_string()))
    }
}

/// An enrolled peer device, resolved from the registry: its label, both public
/// keys, and the fingerprint. This is what sync encrypts to and trusts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Device {
    pub label: String,
    pub signing_key: [u8; 32],
    pub encryption_key: [u8; 32],
    pub fingerprint: String,
}

impl KeyStore {
    /// Open the key store for a project. Keys live beside the machine-wide global
    /// memory store (`<global-root-parent>/keys/`) so one identity is shared
    /// across every project on the machine; when no global root resolves, they
    /// fall back under the project's `.localmind/keys/`.
    ///
    /// # Errors
    /// [`SigningError::Config`] if the project config cannot be read.
    pub fn open(project_root: impl AsRef<Path>) -> Result<Self, SigningError> {
        let config = ProjectConfig::discover(project_root)?;
        Ok(Self::from_config(&config))
    }

    /// Like [`open`](Self::open), from an already-discovered config.
    #[must_use]
    pub fn from_config(config: &ProjectConfig) -> Self {
        let keys_dir = config
            .global_memory_root()
            .and_then(|root| root.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| config.project_root.join(".localmind"))
            .join("keys");
        Self { keys_dir }
    }

    /// The signing-key file path.
    #[must_use]
    pub fn signing_key_path(&self) -> PathBuf {
        self.keys_dir.join(SIGNING_KEY_FILE)
    }

    /// Load the local signing key, generating and persisting a fresh keypair the
    /// first time. The private key is written to a `0600` owner-only file.
    ///
    /// # Errors
    /// [`SigningError`] if the store cannot be read/written or randomness fails.
    pub fn load_or_generate(&self) -> Result<SigningKey, SigningError> {
        if let Some(key) = self.load_signing_key()? {
            return Ok(key);
        }
        let mut seed = [0u8; 32];
        getrandom::getrandom(&mut seed).map_err(|e| SigningError::Random(e.to_string()))?;
        let signing_key = SigningKey::from_bytes(&seed);
        let stored = StoredKeyPair {
            private_key: to_hex(&signing_key.to_bytes()),
            public_key: to_hex(&signing_key.verifying_key().to_bytes()),
        };
        let body = serde_json::to_vec_pretty(&stored)
            .map_err(|e| SigningError::Serialize(e.to_string()))?;
        write_owner_only(&self.signing_key_path(), &body)?;
        Ok(signing_key)
    }

    /// Load the local signing key if one exists, without generating.
    ///
    /// # Errors
    /// [`SigningError`] if the file exists but cannot be read or is malformed.
    pub fn load_signing_key(&self) -> Result<Option<SigningKey>, SigningError> {
        let path = self.signing_key_path();
        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(SigningError::Read { path, source }),
        };
        let stored: StoredKeyPair =
            serde_json::from_str(&content).map_err(|e| SigningError::Malformed(e.to_string()))?;
        let seed = from_hex_array::<32>(&stored.private_key).ok_or_else(|| {
            SigningError::Malformed("private key is not 32 hex bytes".to_string())
        })?;
        Ok(Some(SigningKey::from_bytes(&seed)))
    }

    /// The local public key, when a keypair has been generated.
    ///
    /// # Errors
    /// [`SigningError`] if the key file cannot be read or is malformed.
    pub fn public_key(&self) -> Result<Option<[u8; 32]>, SigningError> {
        Ok(self
            .load_signing_key()?
            .map(|key| key.verifying_key().to_bytes()))
    }

    /// The public keys that classify a bundle as `Trusted`: the local key (if any)
    /// plus every key in the trust list.
    ///
    /// # Errors
    /// [`SigningError`] if either store cannot be read.
    pub fn trusted_keys(&self) -> Result<Vec<[u8; 32]>, SigningError> {
        let mut keys = Vec::new();
        if let Some(own) = self.public_key()? {
            keys.push(own);
        }
        for entry in self.load_trusted()?.trusted {
            if let Some(key) = from_hex_array::<32>(&entry.public_key) {
                if !keys.contains(&key) {
                    keys.push(key);
                }
            }
        }
        Ok(keys)
    }

    /// Add a foreign public key to the trust list (idempotent on the key).
    ///
    /// # Errors
    /// [`SigningError`] if the key is malformed or the store cannot be written.
    pub fn add_trusted(&self, public_key: &[u8; 32], label: &str) -> Result<(), SigningError> {
        let mut trusted = self.load_trusted()?;
        let hex = to_hex(public_key);
        if !trusted.trusted.iter().any(|entry| entry.public_key == hex) {
            trusted.trusted.push(TrustedKey {
                public_key: hex,
                label: label.to_string(),
                encryption_key: None,
            });
        }
        self.write_trusted(&trusted)
    }

    fn load_trusted(&self) -> Result<TrustedKeys, SigningError> {
        let path = self.keys_dir.join(TRUSTED_KEYS_FILE);
        match fs::read_to_string(&path) {
            Ok(content) => {
                serde_json::from_str(&content).map_err(|e| SigningError::Malformed(e.to_string()))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(TrustedKeys::default())
            }
            Err(source) => Err(SigningError::Read { path, source }),
        }
    }

    fn write_trusted(&self, trusted: &TrustedKeys) -> Result<(), SigningError> {
        let body = serde_json::to_vec_pretty(trusted)
            .map_err(|e| SigningError::Serialize(e.to_string()))?;
        write_owner_only(&self.keys_dir.join(TRUSTED_KEYS_FILE), &body)
    }

    /// The X25519 device-key file path.
    #[must_use]
    pub fn device_key_path(&self) -> PathBuf {
        self.keys_dir.join(DEVICE_KEY_FILE)
    }

    /// Load this device's X25519 secret key, generating and persisting a fresh
    /// keypair (owner-only `0600`) the first time. Independent of the signing
    /// key so an existing signing identity keeps working.
    ///
    /// # Errors
    /// [`SigningError`] if the store cannot be read/written or randomness fails.
    pub fn load_or_generate_device_key(&self) -> Result<EncryptionSecretKey, SigningError> {
        if let Some(key) = self.load_device_key()? {
            return Ok(key);
        }
        let mut seed = [0u8; 32];
        getrandom::getrandom(&mut seed).map_err(|e| SigningError::Random(e.to_string()))?;
        let secret = EncryptionSecretKey::from(seed);
        let stored = StoredDeviceKey {
            private_key: to_hex(&secret.to_bytes()),
            public_key: to_hex(secret.public_key().as_bytes()),
        };
        let body = serde_json::to_vec_pretty(&stored)
            .map_err(|e| SigningError::Serialize(e.to_string()))?;
        write_owner_only(&self.device_key_path(), &body)?;
        Ok(secret)
    }

    /// Load this device's X25519 secret key if one exists, without generating.
    ///
    /// # Errors
    /// [`SigningError`] if the file exists but cannot be read or is malformed.
    pub fn load_device_key(&self) -> Result<Option<EncryptionSecretKey>, SigningError> {
        let path = self.device_key_path();
        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(SigningError::Read { path, source }),
        };
        let stored: StoredDeviceKey =
            serde_json::from_str(&content).map_err(|e| SigningError::Malformed(e.to_string()))?;
        let seed = from_hex_array::<32>(&stored.private_key).ok_or_else(|| {
            SigningError::Malformed("device private key is not 32 hex bytes".to_string())
        })?;
        Ok(Some(EncryptionSecretKey::from(seed)))
    }

    /// This device's X25519 encryption public key, when a device key exists.
    ///
    /// # Errors
    /// [`SigningError`] if the key file cannot be read or is malformed.
    pub fn device_public_key(&self) -> Result<Option<[u8; 32]>, SigningError> {
        Ok(self
            .load_device_key()?
            .map(|secret| *secret.public_key().as_bytes()))
    }

    /// Build this machine's shareable device card, generating the signing and
    /// device keys if they do not yet exist. The card carries only public keys.
    ///
    /// # Errors
    /// [`SigningError`] if a key cannot be generated, read, or written.
    pub fn own_device_card(&self, label: &str) -> Result<DeviceCard, SigningError> {
        let signing = self.load_or_generate()?;
        let device = self.load_or_generate_device_key()?;
        Ok(DeviceCard {
            label: label.to_string(),
            signing_key: to_hex(&signing.verifying_key().to_bytes()),
            encryption_key: to_hex(device.public_key().as_bytes()),
        })
    }

    /// Enroll a peer device after out-of-band fingerprint verification. The
    /// caller passes the fingerprint the user read off the *other* machine;
    /// enrollment is refused ([`SigningError::FingerprintMismatch`]) unless it
    /// matches the card's key-bound fingerprint, so a swapped or tampered card
    /// cannot be enrolled. Idempotent/upsert on the signing key: re-enrolling
    /// updates the label and encryption key, and a legacy signer-only trusted
    /// key is upgraded in place to a full device.
    ///
    /// # Errors
    /// [`SigningError::FingerprintMismatch`] on a fingerprint mismatch;
    /// [`SigningError::Malformed`] if the card's keys are not valid;
    /// [`SigningError`] if the store cannot be written.
    pub fn enroll_device(
        &self,
        card: &DeviceCard,
        expected_fingerprint: &str,
    ) -> Result<(), SigningError> {
        let (signing, _encryption) = card.key_bytes()?;
        let actual = author_fingerprint(&signing);
        if !fingerprints_match(&actual, expected_fingerprint) {
            return Err(SigningError::FingerprintMismatch {
                expected: expected_fingerprint.trim().to_lowercase(),
                actual,
            });
        }
        let signing_hex = to_hex(&signing);
        let mut trusted = self.load_trusted()?;
        if let Some(entry) = trusted
            .trusted
            .iter_mut()
            .find(|entry| entry.public_key == signing_hex)
        {
            entry.label = card.label.clone();
            entry.encryption_key = Some(card.encryption_key.clone());
        } else {
            trusted.trusted.push(TrustedKey {
                public_key: signing_hex,
                label: card.label.clone(),
                encryption_key: Some(card.encryption_key.clone()),
            });
        }
        self.write_trusted(&trusted)
    }

    /// Every enrolled peer device (registry entries carrying an encryption key).
    /// Legacy signer-only trusted keys are excluded — they are not sync devices.
    ///
    /// # Errors
    /// [`SigningError`] if the store cannot be read.
    pub fn enrolled_devices(&self) -> Result<Vec<Device>, SigningError> {
        let mut devices = Vec::new();
        for entry in self.load_trusted()?.trusted {
            let Some(encryption_hex) = &entry.encryption_key else {
                continue;
            };
            let (Some(signing_key), Some(encryption_key)) = (
                from_hex_array::<32>(&entry.public_key),
                from_hex_array::<32>(encryption_hex),
            ) else {
                continue;
            };
            devices.push(Device {
                label: entry.label.clone(),
                signing_key,
                encryption_key,
                fingerprint: author_fingerprint(&signing_key),
            });
        }
        Ok(devices)
    }

    /// Revoke an enrolled device by its fingerprint or its label: it is removed
    /// from the registry, so later exports stop encrypting to it and its
    /// signature is no longer trusted for sync import. Returns whether a device
    /// was removed. Nothing else is deleted.
    ///
    /// # Errors
    /// [`SigningError`] if the store cannot be read or written.
    pub fn revoke_device(&self, selector: &str) -> Result<bool, SigningError> {
        let selector = selector.trim();
        let mut trusted = self.load_trusted()?;
        let before = trusted.trusted.len();
        trusted.trusted.retain(|entry| {
            // Only enrolled devices (with an encryption key) are revocable here.
            let is_device = entry.encryption_key.is_some();
            let fingerprint = from_hex_array::<32>(&entry.public_key)
                .map(|key| author_fingerprint(&key))
                .unwrap_or_default();
            let matches = fingerprints_match(&fingerprint, selector)
                || entry.label.eq_ignore_ascii_case(selector);
            !(is_device && matches)
        });
        if trusted.trusted.len() == before {
            return Ok(false);
        }
        self.write_trusted(&trusted)?;
        Ok(true)
    }
}

/// Compare two author fingerprints case-insensitively after trimming, so a
/// user-entered fingerprint matches the stored lowercase hex regardless of case
/// or surrounding whitespace.
fn fingerprints_match(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// Write `body` to `path` owner-only (unix `0o600`; other platforms rely on the
/// per-user profile dir ACL — tier-1 parity is behaviour parity, the FS mechanism
/// differs). Mirrors the BYOK credential-file write (ADR-0042).
#[cfg(unix)]
fn write_owner_only(path: &Path, body: &[u8]) -> Result<(), SigningError> {
    use std::io::Write as _;
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| SigningError::Write {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|source| SigningError::Write {
            path: path.to_path_buf(),
            source,
        })?;
    file.write_all(body).map_err(|source| SigningError::Write {
        path: path.to_path_buf(),
        source,
    })?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|source| {
        SigningError::Write {
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn write_owner_only(path: &Path, body: &[u8]) -> Result<(), SigningError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| SigningError::Write {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    fs::write(path, body).map_err(|source| SigningError::Write {
        path: path.to_path_buf(),
        source,
    })
}

fn to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
        out.push(char::from_digit(u32::from(byte & 0x0f), 16).unwrap_or('0'));
    }
    out
}

/// Decode a hex string into exactly `N` bytes, or `None` on bad length/chars.
fn from_hex_array<const N: usize>(text: &str) -> Option<[u8; N]> {
    let text = text.trim();
    if text.len() != N * 2 {
        return None;
    }
    let mut out = [0u8; N];
    let bytes = text.as_bytes();
    for (index, slot) in out.iter_mut().enumerate() {
        let high = (bytes[index * 2] as char).to_digit(16)?;
        let low = (bytes[index * 2 + 1] as char).to_digit(16)?;
        *slot = ((high << 4) | low) as u8;
    }
    Some(out)
}

/// Errors signing, verifying, or storing keys. Never carries a secret value.
#[derive(Debug, Error)]
pub enum SigningError {
    /// The project config is missing or invalid.
    #[error(transparent)]
    Config(#[from] StoreConfigError),
    /// A signed pack could not be serialized.
    #[error("failed to serialize signed bundle: {0}")]
    Serialize(String),
    /// A key or signed pack on disk is malformed.
    #[error("key store is malformed: {0}")]
    Malformed(String),
    /// A key-store file could not be read.
    #[error("failed to read key store {path:?}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    /// A key-store file could not be written.
    #[error("failed to write key store {path:?}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The OS randomness source failed.
    #[error("failed to gather randomness: {0}")]
    Random(String),
    /// A device card's fingerprint did not match the one confirmed out-of-band.
    #[error("device fingerprint mismatch: confirmed {expected}, card is {actual}")]
    FingerprintMismatch { expected: String, actual: String },
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::{BundleMetadata, BundleScope};
    use localmind_core::{
        Confidence, LessonCategory, MemoryEntry, MemoryEntryId, MemoryScope, MemoryStatus,
    };

    fn project(dir: &tempfile::TempDir) -> std::path::PathBuf {
        let root = dir.path().to_path_buf();
        let global = root.join("global-store").join("memory");
        std::fs::write(
            root.join(".localmind.toml"),
            format!(
                "[learning]\nenabled = true\nglobal_memory_root = {:?}\n",
                global.to_string_lossy()
            ),
        )
        .unwrap();
        root
    }

    fn sample_bundle(body: &str) -> MemoryBundle {
        MemoryBundle {
            format_version: MEMORY_BUNDLE_FORMAT_VERSION,
            metadata: BundleMetadata {
                created_by: "tester".to_string(),
                scope_selection: BundleScope::Both,
                entry_count: 1,
                redaction_count: 0,
            },
            entries: vec![MemoryEntry {
                id: MemoryEntryId::new("mem-1"),
                scope: MemoryScope::Project,
                body: body.to_string(),
                category: LessonCategory::Process,
                confidence: Confidence::new(0.8).unwrap(),
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
                sync_meta: localmind_core::SyncMeta::default(),
            }],
        }
    }

    #[test]
    fn keypair_persists_loads_and_never_leaks_the_private_key() {
        let dir = tempfile::tempdir().unwrap();
        let root = project(&dir);
        let store = KeyStore::open(&root).unwrap();

        let key = store.load_or_generate().unwrap();
        // Reloading yields the same identity (same public key).
        let reloaded = store.load_signing_key().unwrap().unwrap();
        assert_eq!(
            key.verifying_key().to_bytes(),
            reloaded.verifying_key().to_bytes(),
            "the keypair persists and reloads"
        );

        // The private key must never appear in a signed bundle's JSON.
        let private_hex = to_hex(&key.to_bytes());
        let signed = sign_bundle(&sample_bundle("a lesson"), &key).unwrap();
        let json = signed.to_pretty_json().unwrap();
        assert!(
            !json.contains(&private_hex),
            "the private key must not be serialized into a bundle"
        );
        // The public key and a key-bound author fingerprint are present.
        assert_eq!(
            signed.signature.public_key,
            to_hex(&key.verifying_key().to_bytes())
        );
        assert_eq!(
            signed.signature.author,
            author_fingerprint(&key.verifying_key().to_bytes())
        );
    }

    #[test]
    fn device_key_persists_reloads_and_is_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let store = KeyStore::open(project(&dir)).unwrap();

        let secret = store.load_or_generate_device_key().unwrap();
        // Idempotent load-or-generate: reloading yields the same public key.
        let reloaded = store.load_device_key().unwrap().unwrap();
        assert_eq!(
            secret.public_key().as_bytes(),
            reloaded.public_key().as_bytes()
        );
        assert_eq!(
            store.device_public_key().unwrap().unwrap(),
            *secret.public_key().as_bytes()
        );
        // The device secret must never appear in the card the store hands out.
        let card = store.own_device_card("this-machine").unwrap();
        let private_hex = to_hex(&secret.to_bytes());
        assert!(!card.to_pretty_json().unwrap().contains(&private_hex));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(store.device_key_path())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "device key must be owner-only");
        }
    }

    /// Two independent stores on one machine stand in for two devices.
    fn two_devices() -> (tempfile::TempDir, tempfile::TempDir, KeyStore, KeyStore) {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let store_a = KeyStore::open(project(&dir_a)).unwrap();
        let store_b = KeyStore::open(project(&dir_b)).unwrap();
        (dir_a, dir_b, store_a, store_b)
    }

    #[test]
    fn enrollment_requires_the_confirmed_fingerprint() {
        let (_a, _b, store_a, store_b) = two_devices();
        let card_b = store_b.own_device_card("laptop").unwrap();

        // A wrong fingerprint is refused and enrolls nothing.
        let wrong = store_a.enroll_device(&card_b, "0000000000000000");
        assert!(matches!(
            wrong,
            Err(SigningError::FingerprintMismatch { .. })
        ));
        assert!(store_a.enrolled_devices().unwrap().is_empty());

        // The out-of-band fingerprint the user reads off the other machine.
        store_a
            .enroll_device(&card_b, &card_b.fingerprint())
            .unwrap();
        let devices = store_a.enrolled_devices().unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].label, "laptop");
        assert_eq!(devices[0].fingerprint, card_b.fingerprint());
        assert_eq!(
            devices[0].encryption_key,
            store_b.device_public_key().unwrap().unwrap()
        );
        // An enrolled device's signing key is trusted for verification.
        assert!(store_a
            .trusted_keys()
            .unwrap()
            .contains(&devices[0].signing_key));
    }

    #[test]
    fn revoking_a_device_stops_encrypting_to_it_and_untrusts_its_signature() {
        let (_a, _b, store_a, store_b) = two_devices();
        let card_b = store_b.own_device_card("laptop").unwrap();
        store_a
            .enroll_device(&card_b, &card_b.fingerprint())
            .unwrap();
        let signing_b = from_hex_array::<32>(&card_b.signing_key).unwrap();
        assert!(store_a.trusted_keys().unwrap().contains(&signing_b));

        // Revoke by fingerprint: the device leaves the encryption target list
        // *and* its signing key leaves the trust set.
        assert!(store_a.revoke_device(&card_b.fingerprint()).unwrap());
        assert!(store_a.enrolled_devices().unwrap().is_empty());
        assert!(!store_a.trusted_keys().unwrap().contains(&signing_b));
        // Revoking again is a no-op.
        assert!(!store_a.revoke_device(&card_b.fingerprint()).unwrap());
    }

    #[test]
    fn re_enrolling_updates_the_label_and_revocation_by_label_works() {
        let (_a, _b, store_a, store_b) = two_devices();
        let mut card_b = store_b.own_device_card("old-name").unwrap();
        store_a
            .enroll_device(&card_b, &card_b.fingerprint())
            .unwrap();
        // Re-enroll the same signing identity under a new label (upsert, no dup).
        card_b.label = "new-name".to_string();
        store_a
            .enroll_device(&card_b, &card_b.fingerprint())
            .unwrap();
        let devices = store_a.enrolled_devices().unwrap();
        assert_eq!(devices.len(), 1, "upsert must not duplicate the device");
        assert_eq!(devices[0].label, "new-name");
        // Revocation also works by label.
        assert!(store_a.revoke_device("new-name").unwrap());
        assert!(store_a.enrolled_devices().unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn the_signing_key_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let root = project(&dir);
        let store = KeyStore::open(&root).unwrap();
        store.load_or_generate().unwrap();
        let mode = std::fs::metadata(store.signing_key_path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "signing key file must be owner-only");
    }

    #[test]
    fn a_signed_bundle_verifies_against_its_own_key() {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let signed = sign_bundle(&sample_bundle("verify me"), &key).unwrap();
        let own = [key.verifying_key().to_bytes()];

        match verify_signed(&signed, &own) {
            VerificationOutcome::Verified { class, author } => {
                assert_eq!(class, TrustClass::Trusted);
                assert_eq!(author, author_fingerprint(&key.verifying_key().to_bytes()));
            }
            other => panic!("expected Verified/Trusted, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_key_is_untrusted_but_valid() {
        let key = SigningKey::from_bytes(&[9u8; 32]);
        let signed = sign_bundle(&sample_bundle("from a stranger"), &key).unwrap();
        // Empty trust list → the (valid) signature is Untrusted, not Rejected.
        assert!(matches!(
            verify_signed(&signed, &[]),
            VerificationOutcome::Verified {
                class: TrustClass::Untrusted,
                ..
            }
        ));
    }

    #[test]
    fn a_tampered_body_is_rejected() {
        let key = SigningKey::from_bytes(&[3u8; 32]);
        let mut signed = sign_bundle(&sample_bundle("the original lesson"), &key).unwrap();
        // Tamper with the content after signing: the recomputed digest no longer
        // matches the signed digest.
        signed.bundle.entries[0].body = "a malicious replacement".to_string();
        let own = [key.verifying_key().to_bytes()];
        assert!(matches!(
            verify_signed(&signed, &own),
            VerificationOutcome::Rejected {
                reason: RejectReason::BadDigest
            }
        ));
    }

    #[test]
    fn a_tampered_signature_is_rejected() {
        let key = SigningKey::from_bytes(&[4u8; 32]);
        let mut signed = sign_bundle(&sample_bundle("trust but verify"), &key).unwrap();
        // Flip the digest back into agreement but corrupt the signature bytes, so
        // the digest matches yet the signature fails to verify.
        let mut sig = from_hex_array::<64>(&signed.signature.signature).unwrap();
        sig[0] ^= 0xff;
        signed.signature.signature = to_hex(&sig);
        assert!(matches!(
            verify_signed(&signed, &[]),
            VerificationOutcome::Rejected {
                reason: RejectReason::BadSignature
            }
        ));
    }

    #[test]
    fn a_spoofed_author_is_rejected() {
        let key = SigningKey::from_bytes(&[5u8; 32]);
        let mut signed = sign_bundle(&sample_bundle("who am i"), &key).unwrap();
        signed.signature.author = "deadbeefdeadbeef".to_string();
        assert!(matches!(
            verify_signed(&signed, &[]),
            VerificationOutcome::Rejected {
                reason: RejectReason::AuthorMismatch
            }
        ));
    }

    #[test]
    fn a_newer_signature_schema_is_rejected() {
        let key = SigningKey::from_bytes(&[6u8; 32]);
        let mut signed = sign_bundle(&sample_bundle("from the future"), &key).unwrap();
        signed.signature.schema_version = SIGNATURE_SCHEMA_VERSION + 1;
        assert!(matches!(
            verify_signed(&signed, &[]),
            VerificationOutcome::Rejected {
                reason: RejectReason::UnsupportedSchema
            }
        ));
    }

    #[test]
    fn adding_a_foreign_key_makes_its_bundles_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let root = project(&dir);
        let store = KeyStore::open(&root).unwrap();
        store.load_or_generate().unwrap();

        let foreign = SigningKey::from_bytes(&[42u8; 32]);
        let signed = sign_bundle(&sample_bundle("a shared lesson"), &foreign).unwrap();
        // Before trusting: Untrusted.
        assert!(matches!(
            verify_signed(&signed, &store.trusted_keys().unwrap()),
            VerificationOutcome::Verified {
                class: TrustClass::Untrusted,
                ..
            }
        ));
        // After adding the foreign key to the trust list: Trusted.
        store
            .add_trusted(&foreign.verifying_key().to_bytes(), "a-colleague")
            .unwrap();
        assert!(matches!(
            verify_signed(&signed, &store.trusted_keys().unwrap()),
            VerificationOutcome::Verified {
                class: TrustClass::Trusted,
                ..
            }
        ));
    }
}
