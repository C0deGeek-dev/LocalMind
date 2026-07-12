//! `localmind sync` device-enrollment commands.
//!
//! A device publishes a **card** (its label + public signing/encryption keys)
//! that the owner carries out-of-band to another of their machines. Enrollment
//! is refused unless the fingerprint the user read off the other machine matches
//! the card, so a swapped card cannot be enrolled. Enrolled devices are the
//! encryption targets and trusted signers for sync; revoking one removes both.

use anyhow::{Context, Result};
use localmind_store::{DeviceCard, KeyStore, ProjectConfig, SigningError, SyncEngine};
use std::io::Read as _;
use std::path::{Path, PathBuf};

use crate::store_root;

/// Resolve the sync folder: the explicit `--folder`, else `[sync] folder` from
/// config. Errors when neither is set.
fn resolve_folder(root: &Path, folder: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(folder) = folder {
        return Ok(folder);
    }
    let config = ProjectConfig::discover(root).context("reading project config")?;
    config.sync_folder().map(Path::to_path_buf).context(
        "no sync folder configured — pass --folder <path> or set [sync] folder in .localmind.toml",
    )
}

/// Open the machine-wide key store for the resolved project root, or `None`
/// (message already printed) when no project store is found.
fn open_store(project: &Path) -> Result<Option<(PathBuf, KeyStore)>> {
    let Some(root) = store_root::resolve_or_report(project) else {
        return Ok(None);
    };
    let store = KeyStore::open(&root)?;
    Ok(Some((root, store)))
}

/// A hex view of an enrolled device (the in-memory `Device` holds raw key
/// bytes, not hex) — used for both the text listing and the `--json` output.
struct DeviceView {
    label: String,
    fingerprint: String,
    signing_key: String,
    encryption_key: String,
}

fn to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// `localmind sync device-card` — print this machine's shareable device card.
pub fn device_card(project: &Path, label: Option<String>, json: bool) -> Result<()> {
    let Some((root, store)) = open_store(project)? else {
        return Ok(());
    };
    let label = label.unwrap_or_else(|| {
        ProjectConfig::discover(&root)
            .map(|config| config.sync_device_label())
            .unwrap_or_default()
    });
    let card = store.own_device_card(&label)?;

    if json {
        println!("{}", card.to_pretty_json()?);
        return Ok(());
    }
    println!("Device card for '{}':", card.label);
    println!("  fingerprint:    {}", card.fingerprint());
    println!("  signing key:    {}", card.signing_key);
    println!("  encryption key: {}", card.encryption_key);
    println!();
    println!("Share this card with your other machine, then on that machine run:");
    println!(
        "  localmind sync enroll --card <this-card.json> --confirm-fingerprint {}",
        card.fingerprint()
    );
    println!("Confirm the fingerprint matches on both machines before enrolling.");
    Ok(())
}

/// `localmind sync enroll` — enroll a peer device from its card after confirming
/// the out-of-band fingerprint.
pub fn enroll(
    project: &Path,
    card_path: Option<PathBuf>,
    confirm_fingerprint: &str,
    json: bool,
) -> Result<()> {
    let Some((_root, store)) = open_store(project)? else {
        return Ok(());
    };
    let card_json = match &card_path {
        Some(path) if path != Path::new("-") => std::fs::read_to_string(path)
            .with_context(|| format!("reading device card {}", path.display()))?,
        _ => {
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .context("reading device card from stdin")?;
            buf
        }
    };
    let card = DeviceCard::from_json(&card_json)?;

    match store.enroll_device(&card, confirm_fingerprint) {
        Ok(()) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "enrolled": card.label,
                        "fingerprint": card.fingerprint(),
                    }))?
                );
            } else {
                println!(
                    "Enrolled device '{}' ({}). It is now an encryption target and a \
                     trusted signer for sync.",
                    card.label,
                    card.fingerprint()
                );
            }
            Ok(())
        }
        Err(SigningError::FingerprintMismatch { expected, actual }) => {
            // Fail closed with a clear, secret-free diagnostic and a non-zero exit.
            Err(anyhow::anyhow!(
                "device NOT enrolled: the confirmed fingerprint ({expected}) does not match \
                 the card's fingerprint ({actual}). Re-check the fingerprint on both machines."
            ))
        }
        Err(other) => Err(other.into()),
    }
}

/// `localmind sync devices` — list this machine's identity and enrolled peers.
pub fn devices(project: &Path, json: bool) -> Result<()> {
    let Some((root, store)) = open_store(project)? else {
        return Ok(());
    };
    let enrolled: Vec<DeviceView> = store
        .enrolled_devices()?
        .into_iter()
        .map(|device| DeviceView {
            label: device.label,
            fingerprint: device.fingerprint,
            signing_key: to_hex(&device.signing_key),
            encryption_key: to_hex(&device.encryption_key),
        })
        .collect();
    let own_label = ProjectConfig::discover(&root)
        .map(|config| config.sync_device_label())
        .unwrap_or_default();
    let own_fingerprint = store
        .public_key()?
        .map(|key| localmind_store::author_fingerprint(&key));

    if json {
        let enrolled_json: Vec<serde_json::Value> = enrolled
            .iter()
            .map(|device| {
                serde_json::json!({
                    "label": device.label,
                    "fingerprint": device.fingerprint,
                    "signing_key": device.signing_key,
                    "encryption_key": device.encryption_key,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "this_device": { "label": own_label, "fingerprint": own_fingerprint },
                "enrolled": enrolled_json,
            }))?
        );
        return Ok(());
    }
    match &own_fingerprint {
        Some(fingerprint) => println!("This device: '{own_label}' ({fingerprint})"),
        None => println!("This device: no identity yet (run `localmind sync device-card`)"),
    }
    if enrolled.is_empty() {
        println!("No enrolled devices.");
    } else {
        println!("Enrolled devices ({}):", enrolled.len());
        for device in &enrolled {
            println!("  - '{}'  {}", device.label, device.fingerprint);
        }
    }
    Ok(())
}

/// `localmind sync run` — export this device's snapshot and import peers'.
pub fn run(project: &Path, folder: Option<PathBuf>, json: bool) -> Result<()> {
    let Some(root) = store_root::resolve_or_report(project) else {
        return Ok(());
    };
    let folder = resolve_folder(&root, folder)?;
    let report = SyncEngine::open(&root).run(&folder)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    if report.exported {
        println!(
            "Exported {} memor{} to {}.",
            report.exported_ops,
            if report.exported_ops == 1 { "y" } else { "ies" },
            folder.display()
        );
    } else {
        println!(
            "Nothing exported: no enrolled devices to encrypt to (run `localmind sync enroll` \
             first)."
        );
    }
    println!(
        "Imported {} new review candidate{} from {} peer{} ({} conflict{}, {} proposed \
         deletion{} flagged, {} from unknown signers rejected, {} skipped).",
        report.imported_candidates,
        plural(report.imported_candidates),
        report.peers_scanned,
        plural(report.peers_scanned),
        report.conflicts,
        plural(report.conflicts),
        report.tombstones_flagged,
        plural(report.tombstones_flagged),
        report.rejected_unknown_signer,
        report.skipped_files,
    );
    if report.imported_candidates > 0 || report.tombstones_flagged > 0 {
        println!("Review them with `localmind review list`.");
    }
    Ok(())
}

/// `localmind sync status` — show folder, peers, cursors, pending review.
pub fn status(project: &Path, folder: Option<PathBuf>, json: bool) -> Result<()> {
    let Some(root) = store_root::resolve_or_report(project) else {
        return Ok(());
    };
    let folder = resolve_folder(&root, folder)?;
    let status = SyncEngine::open(&root).status(&folder)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&status)?);
        return Ok(());
    }
    println!("Sync folder:      {}", status.folder);
    match &status.own_fingerprint {
        Some(fingerprint) => println!("This device:      {fingerprint}"),
        None => println!("This device:      no identity yet"),
    }
    println!("Enrolled devices: {}", status.enrolled_devices);
    println!("Peer bundles:     {}", status.peer_bundles);
    println!("Tracked peers:    {}", status.tracked_peers);
    println!("Pending review:   {}", status.pending_review);
    Ok(())
}

fn plural(count: usize) -> &'static str {
    if count == 1 {
        ""
    } else {
        "s"
    }
}

/// `localmind sync revoke` — revoke an enrolled device by fingerprint or label.
pub fn revoke(project: &Path, device: &str, json: bool) -> Result<()> {
    let Some((_root, store)) = open_store(project)? else {
        return Ok(());
    };
    let removed = store.revoke_device(device)?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "selector": device,
                "revoked": removed,
            }))?
        );
        return Ok(());
    }
    if removed {
        println!(
            "Revoked device '{device}'. Future exports stop encrypting to it and its \
             signature is no longer trusted for sync."
        );
    } else {
        println!("No enrolled device matched '{device}' (nothing revoked).");
    }
    Ok(())
}
