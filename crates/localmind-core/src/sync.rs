//! Cross-device sync scoping for accepted memory.
//!
//! These types decide *whether* a memory travels to another device and record
//! *which machine* wrote it, without touching how memory is stored or retrieved.
//! They ride on [`MemoryEntry`](crate::MemoryEntry) as a single defaulted
//! [`SyncMeta`] field, so an entry that predates sync (or a bundle written by an
//! older reader) deserializes with an empty `SyncMeta` and behaves exactly as
//! before — the disposition then falls back to the per-scope default.

use crate::MemoryScope;
use serde::{Deserialize, Serialize};

/// Whether — and how — a memory participates in cross-device sync.
///
/// The disposition is a per-memory override; when unset an entry takes the
/// [per-scope default](SyncDisposition::default_for_scope).
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncDisposition {
    /// Portable knowledge: exported to peers freely.
    Sync,
    /// Machine-specific: never leaves this device (local paths, GPU/driver
    /// quirks, `-ngl 0`-style tips). Excluded from every export.
    MachineLocal,
    /// Portable *but* machine-flavoured: syncs, and carries its origin
    /// environment so the far side can down-weight (never drop) it when the
    /// fingerprint mismatches the destination machine.
    SyncAnnotated,
}

impl SyncDisposition {
    /// The default disposition for a memory of the given scope. `Session` and
    /// `Research` are transient/local and never sync; `Skill` drafts stay on
    /// their authoring machine; durable `Project`/`GlobalUser` knowledge syncs.
    #[must_use]
    pub fn default_for_scope(scope: &MemoryScope) -> Self {
        match scope {
            MemoryScope::GlobalUser | MemoryScope::Project => SyncDisposition::Sync,
            MemoryScope::Session | MemoryScope::Research | MemoryScope::Skill => {
                SyncDisposition::MachineLocal
            }
        }
    }

    /// Does this disposition leave the machine at all?
    #[must_use]
    pub fn syncs(self) -> bool {
        matches!(self, SyncDisposition::Sync | SyncDisposition::SyncAnnotated)
    }

    /// The stored/serialized token for this disposition.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            SyncDisposition::Sync => "sync",
            SyncDisposition::MachineLocal => "machine_local",
            SyncDisposition::SyncAnnotated => "sync_annotated",
        }
    }

    /// Parse a stored token; an unknown token reads as `None` so a
    /// forward-compatible value never mis-classifies an entry.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token.trim() {
            "sync" => Some(SyncDisposition::Sync),
            "machine_local" => Some(SyncDisposition::MachineLocal),
            "sync_annotated" => Some(SyncDisposition::SyncAnnotated),
            _ => None,
        }
    }
}

/// A best-effort fingerprint of the machine that wrote a memory. Captured at
/// write time so the destination device can tell same-machine from
/// cross-machine knowledge. `os`/`arch` are compile-time constants (capture
/// never fails); `device_label` comes from config/enrollment (empty when
/// unknown); `toolchain` is a reserved, optional GPU/toolchain summary.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct EnvFingerprint {
    pub os: String,
    pub arch: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub device_label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toolchain: Option<String>,
}

impl EnvFingerprint {
    /// Capture the current machine's fingerprint. Pure and total — it reads two
    /// compile-time constants and the caller-supplied label, so stamping a
    /// fingerprint can never block or fail a memory write.
    #[must_use]
    pub fn capture(device_label: impl Into<String>) -> Self {
        Self {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            device_label: device_label.into(),
            toolchain: None,
        }
    }

    /// Whether two fingerprints describe the same machine. A same-machine pair
    /// agrees on OS, arch, and a non-empty device label; if either label is
    /// empty the label is not decisive and only OS+arch are compared.
    #[must_use]
    pub fn is_same_machine(&self, other: &Self) -> bool {
        if self.os != other.os || self.arch != other.arch {
            return false;
        }
        if self.device_label.is_empty() || other.device_label.is_empty() {
            return true;
        }
        self.device_label == other.device_label
    }
}

/// A best-effort device label read from the environment (`COMPUTERNAME` on
/// Windows, `HOSTNAME` elsewhere). Empty string when neither is set — never an
/// error. Config/enrollment supplies a stable label; this is only the fallback.
#[must_use]
pub fn host_device_label() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_default()
}

/// Per-memory sync state carried on [`MemoryEntry`](crate::MemoryEntry). Both
/// fields are optional and default-empty, so an entry without them behaves as a
/// pre-sync entry: the disposition falls back to the per-scope default and no
/// origin machine is recorded.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SyncMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disposition: Option<SyncDisposition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_env: Option<EnvFingerprint>,
}

impl SyncMeta {
    /// The disposition this memory actually takes: the explicit per-memory
    /// override if set, otherwise the per-scope default.
    #[must_use]
    pub fn effective_disposition(&self, scope: &MemoryScope) -> SyncDisposition {
        self.disposition
            .unwrap_or_else(|| SyncDisposition::default_for_scope(scope))
    }
}

#[cfg(test)]
mod tests {
    use super::{EnvFingerprint, SyncDisposition, SyncMeta};
    use crate::MemoryScope;

    #[test]
    fn scope_defaults_match_the_contract() {
        assert_eq!(
            SyncDisposition::default_for_scope(&MemoryScope::GlobalUser),
            SyncDisposition::Sync
        );
        assert_eq!(
            SyncDisposition::default_for_scope(&MemoryScope::Project),
            SyncDisposition::Sync
        );
        for local in [
            MemoryScope::Session,
            MemoryScope::Research,
            MemoryScope::Skill,
        ] {
            assert_eq!(
                SyncDisposition::default_for_scope(&local),
                SyncDisposition::MachineLocal
            );
            assert!(!SyncDisposition::default_for_scope(&local).syncs());
        }
    }

    #[test]
    fn disposition_token_round_trips_and_rejects_unknown() {
        for disposition in [
            SyncDisposition::Sync,
            SyncDisposition::MachineLocal,
            SyncDisposition::SyncAnnotated,
        ] {
            assert_eq!(
                SyncDisposition::from_token(disposition.as_str()),
                Some(disposition)
            );
        }
        assert_eq!(SyncDisposition::from_token("last_writer_wins"), None);
    }

    #[test]
    fn effective_disposition_prefers_the_override() {
        let default_meta = SyncMeta::default();
        // No override → per-scope default (Session never syncs).
        assert_eq!(
            default_meta.effective_disposition(&MemoryScope::Session),
            SyncDisposition::MachineLocal
        );
        let pinned = SyncMeta {
            disposition: Some(SyncDisposition::MachineLocal),
            origin_env: None,
        };
        // Override wins even for a normally-syncing scope.
        assert_eq!(
            pinned.effective_disposition(&MemoryScope::Project),
            SyncDisposition::MachineLocal
        );
    }

    #[test]
    fn same_machine_ignores_empty_labels_but_honours_present_ones() {
        let pc = EnvFingerprint {
            os: "windows".to_string(),
            arch: "x86_64".to_string(),
            device_label: "PC".to_string(),
            toolchain: None,
        };
        let same = EnvFingerprint {
            device_label: "PC".to_string(),
            ..pc.clone()
        };
        let laptop = EnvFingerprint {
            device_label: "Laptop".to_string(),
            ..pc.clone()
        };
        let unlabeled = EnvFingerprint {
            device_label: String::new(),
            ..pc.clone()
        };
        let other_arch = EnvFingerprint {
            arch: "aarch64".to_string(),
            ..pc.clone()
        };
        assert!(pc.is_same_machine(&same));
        assert!(!pc.is_same_machine(&laptop));
        assert!(pc.is_same_machine(&unlabeled)); // empty label is not decisive
        assert!(!pc.is_same_machine(&other_arch)); // os/arch always decisive
    }

    #[test]
    fn capture_is_total_and_reads_the_platform_constants() {
        let fingerprint = EnvFingerprint::capture("PC");
        assert_eq!(fingerprint.os, std::env::consts::OS);
        assert_eq!(fingerprint.arch, std::env::consts::ARCH);
        assert_eq!(fingerprint.device_label, "PC");
    }
}
