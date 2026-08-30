//! Machine-global leases for a configured embedding endpoint.
//!
//! This module owns bookkeeping, not the server process. A process owner such
//! as LocalPilot may reserve an unreachable endpoint, start it through its own
//! effect boundary, and then register the exact server PID. Other products may
//! only join a reachable endpoint whose active marker still names that live
//! PID. An unmarked/user-managed listener remains usable but is never leased or
//! stopped through this contract.

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use thiserror::Error;

const OWNER_FILE: &str = "started-by-localpilot";
const LOCK_FILE: &str = "lease-state.lock";
const OWNER_SCHEMA: u32 = 1;
const CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
static LEASE_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Compare two configured endpoint URLs by their normalized loopback socket.
/// Process owners use this to validate an external runtime pidfile without
/// reimplementing the registry's endpoint parser or localhost normalization.
pub fn endpoints_match(left: &str, right: &str) -> Result<bool, LeaseError> {
    Ok(EndpointIdentity::parse(left)?.socket == EndpointIdentity::parse(right)?.socket)
}

/// The result of asking to join an already-running endpoint.
#[derive(Debug)]
pub enum JoinOutcome {
    /// The endpoint is reachable and its exact live owner permits this lease.
    Acquired(EmbeddingLease),
    /// Nothing is listening at the configured endpoint.
    Unreachable,
    /// A listener exists, but there is no valid matching ownership marker.
    UserManaged,
    /// The matching owner is between last-lease release and process teardown.
    Stopping,
}

/// The owner-side result before deciding whether a process must be started.
#[derive(Debug)]
pub enum OwnerPreparation {
    /// The registered owned endpoint was already running.
    Acquired(EmbeddingLease),
    /// A legacy marker exists beside a reachable endpoint. Only the process
    /// owner may verify the external PID state and migrate it.
    Legacy(LegacyOwnerPermit),
    /// No listener exists; this permit serializes the start/claim transaction.
    Start(OwnerStartPermit),
    /// A reachable listener is not the exact registered owned process.
    UserManaged,
    /// Another owner is already tearing the registered process down.
    Stopping,
}

/// Outcome of an explicit owner release.
#[derive(Debug)]
pub enum ReleaseOutcome {
    /// This lease did not match a current ownership record.
    Unowned,
    /// At least one other live lease still protects the server.
    OthersRemain,
    /// This was the last live lease. The caller must stop its process and then
    /// complete the token with the observed stop result.
    StopOwned(OwnerStopToken),
}

/// Result of an owner-side reaper checking whether outstanding client leases
/// have drained after the original owner process released its own lease.
#[derive(Debug)]
pub enum StopPreparation {
    /// At least one live client still protects the endpoint.
    Waiting,
    /// No live leases remain and teardown is atomically reserved.
    Ready(OwnerStopToken),
    /// The exact owner record no longer exists or its server process died.
    Unowned,
    /// Another owner/reaper already reserved teardown.
    Stopping,
}

/// Canonical machine-global lease registry.
#[derive(Clone, Debug)]
pub struct EmbeddingLeaseRegistry {
    root: PathBuf,
}

impl EmbeddingLeaseRegistry {
    /// Resolve the shared registry beside the machine-global LocalX runtime
    /// state. Returns `None` only when the process has no resolvable user home.
    #[must_use]
    pub fn machine_default() -> Option<Self> {
        std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
            .map(|home| Self::at(home.join(".local-llm").join("embed-leases")))
    }

    /// Construct a registry rooted at an explicit machine-state directory.
    /// This is also the hermetic seam used by contract tests and embedded hosts.
    #[must_use]
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Join a reachable, actively owned endpoint without starting anything.
    pub fn join_existing(&self, base_url: &str) -> Result<JoinOutcome, LeaseError> {
        let endpoint = EndpointIdentity::parse(base_url)?;
        let locked = self.lock()?;
        locked.prune_stale_leases()?;
        if !endpoint.reachable() {
            return Ok(JoinOutcome::Unreachable);
        }
        match locked.read_owner()? {
            Marker::Current(owner) if owner.matches(&endpoint) && owner.server_is_live() => {
                if owner.phase == OwnerPhase::Stopping {
                    Ok(JoinOutcome::Stopping)
                } else {
                    locked.create_lease(owner).map(JoinOutcome::Acquired)
                }
            }
            Marker::Missing | Marker::Legacy | Marker::Invalid | Marker::Current(_) => {
                Ok(JoinOutcome::UserManaged)
            }
        }
    }

    /// Serialize an owner decision around an endpoint probe and possible start.
    pub fn prepare_owner(&self, base_url: &str) -> Result<OwnerPreparation, LeaseError> {
        let endpoint = EndpointIdentity::parse(base_url)?;
        let locked = self.lock()?;
        locked.prune_stale_leases()?;
        if endpoint.reachable() {
            return match locked.read_owner()? {
                Marker::Current(owner) if owner.matches(&endpoint) && owner.server_is_live() => {
                    if owner.phase == OwnerPhase::Stopping {
                        Ok(OwnerPreparation::Stopping)
                    } else {
                        locked.create_lease(owner).map(OwnerPreparation::Acquired)
                    }
                }
                Marker::Legacy => Ok(OwnerPreparation::Legacy(LegacyOwnerPermit {
                    locked,
                    endpoint,
                })),
                Marker::Missing | Marker::Invalid | Marker::Current(_) => {
                    Ok(OwnerPreparation::UserManaged)
                }
            };
        }
        Ok(OwnerPreparation::Start(OwnerStartPermit {
            locked,
            endpoint,
        }))
    }

    /// Atomically claim teardown after an exact owner's remaining client leases
    /// drain. This gives a process-owning reaper a neutral polling seam without
    /// granting process-control capability to the registry or its clients.
    pub fn prepare_stop_when_unleased(
        &self,
        base_url: &str,
        owner: &str,
        server_pid: u32,
    ) -> Result<StopPreparation, LeaseError> {
        validate_owner(owner)?;
        let endpoint = EndpointIdentity::parse(base_url)?;
        let locked = self.lock()?;
        let remaining = locked.prune_stale_leases()?;
        let Marker::Current(mut current) = locked.read_owner()? else {
            return Ok(StopPreparation::Unowned);
        };
        if !current.matches(&endpoint) || current.owner != owner || current.server_pid != server_pid
        {
            return Ok(StopPreparation::Unowned);
        }
        if !current.server_is_live() {
            remove_if_present(&locked.owner_path())?;
            return Ok(StopPreparation::Unowned);
        }
        if current.phase == OwnerPhase::Stopping {
            return Ok(StopPreparation::Stopping);
        }
        if remaining > 0 {
            return Ok(StopPreparation::Waiting);
        }
        current.phase = OwnerPhase::Stopping;
        locked.write_owner(&current)?;
        Ok(StopPreparation::Ready(OwnerStopToken {
            registry: self.clone(),
            owner: current,
            completed: false,
        }))
    }

    fn lock(&self) -> Result<LockedRegistry, LeaseError> {
        std::fs::create_dir_all(&self.root).map_err(|source| LeaseError::io(&self.root, source))?;
        let lock_path = self.root.join(LOCK_FILE);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|source| LeaseError::io(&lock_path, source))?;
        file.lock_exclusive()
            .map_err(|source| LeaseError::io(&lock_path, source))?;
        Ok(LockedRegistry {
            registry: self.clone(),
            lock_file: file,
        })
    }
}

/// A unique claim held for one process lifetime. Drop removes only this claim;
/// it never starts or stops a process.
#[derive(Debug)]
pub struct EmbeddingLease {
    registry: EmbeddingLeaseRegistry,
    path: PathBuf,
    owner: OwnerRecord,
    released: bool,
}

impl EmbeddingLease {
    /// Release this claim and atomically reserve last-owner teardown if needed.
    pub fn release(mut self) -> Result<ReleaseOutcome, LeaseError> {
        self.released = true;
        let locked = self.registry.lock()?;
        remove_if_present(&self.path)?;
        let remaining = locked.prune_stale_leases()?;
        let Marker::Current(mut current) = locked.read_owner()? else {
            return Ok(ReleaseOutcome::Unowned);
        };
        if current != self.owner || current.phase != OwnerPhase::Active {
            return Ok(ReleaseOutcome::Unowned);
        }
        if remaining > 0 {
            return Ok(ReleaseOutcome::OthersRemain);
        }
        current.phase = OwnerPhase::Stopping;
        locked.write_owner(&current)?;
        Ok(ReleaseOutcome::StopOwned(OwnerStopToken {
            registry: self.registry.clone(),
            owner: current,
            completed: false,
        }))
    }

    /// The normalized loopback socket protected by this lease.
    #[must_use]
    pub fn endpoint(&self) -> String {
        self.owner.endpoint.clone()
    }
}

impl Drop for EmbeddingLease {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        if let Ok(locked) = self.registry.lock() {
            let _ = remove_if_present(&self.path);
            let _ = locked.prune_stale_leases();
        }
    }
}

/// Exclusive permit held while the process owner starts an unreachable server.
#[derive(Debug)]
pub struct OwnerStartPermit {
    locked: LockedRegistry,
    endpoint: EndpointIdentity,
}

impl OwnerStartPermit {
    /// Register the exact live server and acquire the owner's first lease.
    pub fn claim(self, owner: &str, server_pid: u32) -> Result<EmbeddingLease, LeaseError> {
        self.claim_inner(owner, server_pid)
    }

    fn claim_inner(self, owner: &str, server_pid: u32) -> Result<EmbeddingLease, LeaseError> {
        validate_owner(owner)?;
        if !process_is_alive(server_pid) || !self.endpoint.reachable() {
            return Err(LeaseError::OwnerNotLive {
                endpoint: self.endpoint.normalized(),
                server_pid,
            });
        }
        let record = OwnerRecord::new(owner, &self.endpoint, server_pid);
        self.locked.write_owner(&record)?;
        self.locked.create_lease(record)
    }
}

/// Exclusive permit for migrating the old plain-text ownership marker.
#[derive(Debug)]
pub struct LegacyOwnerPermit {
    locked: LockedRegistry,
    endpoint: EndpointIdentity,
}

impl LegacyOwnerPermit {
    /// Replace the legacy marker only after the process owner independently
    /// verified that its runtime state names this endpoint and live server PID.
    pub fn migrate(self, owner: &str, server_pid: u32) -> Result<EmbeddingLease, LeaseError> {
        OwnerStartPermit {
            locked: self.locked,
            endpoint: self.endpoint,
        }
        .claim_inner(owner, server_pid)
    }
}

/// Token proving that the owner atomically moved to `stopping` after its last
/// live lease disappeared.
#[derive(Debug)]
pub struct OwnerStopToken {
    registry: EmbeddingLeaseRegistry,
    owner: OwnerRecord,
    completed: bool,
}

impl OwnerStopToken {
    /// Complete teardown. Success clears the exact marker; failure restores it
    /// to active so a later owner can lease it and retry rather than strand it.
    pub fn complete(mut self, stopped: bool) -> Result<(), LeaseError> {
        self.finish(stopped)?;
        self.completed = true;
        Ok(())
    }

    fn finish(&self, stopped: bool) -> Result<(), LeaseError> {
        let locked = self.registry.lock()?;
        let Marker::Current(current) = locked.read_owner()? else {
            return Ok(());
        };
        if current != self.owner {
            return Ok(());
        }
        if stopped {
            remove_if_present(&locked.owner_path())
        } else {
            let mut active = current;
            active.phase = OwnerPhase::Active;
            locked.write_owner(&active)
        }
    }
}

impl Drop for OwnerStopToken {
    fn drop(&mut self) {
        if !self.completed {
            let _ = self.finish(false);
        }
    }
}

#[derive(Debug)]
struct LockedRegistry {
    registry: EmbeddingLeaseRegistry,
    lock_file: File,
}

impl LockedRegistry {
    fn owner_path(&self) -> PathBuf {
        self.registry.root.join(OWNER_FILE)
    }

    fn read_owner(&self) -> Result<Marker, LeaseError> {
        let path = self.owner_path();
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Marker::Missing)
            }
            Err(source) => return Err(LeaseError::io(&path, source)),
        };
        if raw.trim() == "localpilot" {
            return Ok(Marker::Legacy);
        }
        Ok(serde_json::from_str(&raw)
            .ok()
            .filter(|record: &OwnerRecord| record.schema == OWNER_SCHEMA)
            .map_or(Marker::Invalid, Marker::Current))
    }

    fn write_owner(&self, owner: &OwnerRecord) -> Result<(), LeaseError> {
        let bytes = serde_json::to_vec_pretty(owner).map_err(LeaseError::SerializeOwner)?;
        let target = self.owner_path();
        let temporary = self.registry.root.join(format!(
            ".owner-{}-{}.tmp",
            std::process::id(),
            LEASE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|source| LeaseError::io(&temporary, source))?;
        file.write_all(&bytes)
            .and_then(|()| file.write_all(b"\n"))
            .and_then(|()| file.sync_data())
            .map_err(|source| LeaseError::io(&temporary, source))?;
        if target.exists() {
            std::fs::remove_file(&target).map_err(|source| LeaseError::io(&target, source))?;
        }
        std::fs::rename(&temporary, &target).map_err(|source| LeaseError::io(&target, source))
    }

    fn create_lease(&self, owner: OwnerRecord) -> Result<EmbeddingLease, LeaseError> {
        let pid = std::process::id();
        for _ in 0..32 {
            let sequence = LEASE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = self.registry.root.join(format!("{pid}-{sequence}.lease"));
            match OpenOptions::new().create_new(true).write(true).open(&path) {
                Ok(mut file) => {
                    file.write_all(pid.to_string().as_bytes())
                        .and_then(|()| file.sync_data())
                        .map_err(|source| LeaseError::io(&path, source))?;
                    return Ok(EmbeddingLease {
                        registry: self.registry.clone(),
                        path,
                        owner,
                        released: false,
                    });
                }
                Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(source) => return Err(LeaseError::io(&path, source)),
            }
        }
        Err(LeaseError::LeaseNameExhausted)
    }

    fn prune_stale_leases(&self) -> Result<usize, LeaseError> {
        let mut live = 0;
        for entry in std::fs::read_dir(&self.registry.root)
            .map_err(|source| LeaseError::io(&self.registry.root, source))?
        {
            let entry = entry.map_err(|source| LeaseError::io(&self.registry.root, source))?;
            let path = entry.path();
            if path
                .extension()
                .is_none_or(|extension| extension != "lease")
            {
                continue;
            }
            let alive = std::fs::read_to_string(&path)
                .ok()
                .and_then(|value| value.trim().parse::<u32>().ok())
                .is_some_and(process_is_alive);
            if alive {
                live += 1;
            } else {
                remove_if_present(&path)?;
            }
        }
        Ok(live)
    }
}

impl Drop for LockedRegistry {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.lock_file);
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct OwnerRecord {
    schema: u32,
    owner: String,
    endpoint: String,
    server_pid: u32,
    phase: OwnerPhase,
}

impl OwnerRecord {
    fn new(owner: &str, endpoint: &EndpointIdentity, server_pid: u32) -> Self {
        Self {
            schema: OWNER_SCHEMA,
            owner: owner.to_string(),
            endpoint: endpoint.normalized(),
            server_pid,
            phase: OwnerPhase::Active,
        }
    }

    fn matches(&self, endpoint: &EndpointIdentity) -> bool {
        self.endpoint == endpoint.normalized()
    }

    fn server_is_live(&self) -> bool {
        process_is_alive(self.server_pid)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum OwnerPhase {
    Active,
    Stopping,
}

#[derive(Debug)]
enum Marker {
    Missing,
    Legacy,
    Invalid,
    Current(OwnerRecord),
}

#[derive(Clone, Copy, Debug)]
struct EndpointIdentity {
    socket: SocketAddr,
}

impl EndpointIdentity {
    fn parse(base_url: &str) -> Result<Self, LeaseError> {
        let parsed = url::Url::parse(base_url).map_err(|_| LeaseError::InvalidEndpoint {
            endpoint: base_url.to_string(),
        })?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(LeaseError::InvalidEndpoint {
                endpoint: base_url.to_string(),
            });
        }
        let host = parsed
            .host_str()
            .ok_or_else(|| LeaseError::InvalidEndpoint {
                endpoint: base_url.to_string(),
            })?;
        let ip = if host.eq_ignore_ascii_case("localhost") {
            IpAddr::from([127, 0, 0, 1])
        } else {
            host.parse::<IpAddr>()
                .ok()
                .filter(IpAddr::is_loopback)
                .ok_or_else(|| LeaseError::InvalidEndpoint {
                    endpoint: base_url.to_string(),
                })?
        };
        let port = parsed
            .port_or_known_default()
            .ok_or_else(|| LeaseError::InvalidEndpoint {
                endpoint: base_url.to_string(),
            })?;
        Ok(Self {
            socket: SocketAddr::new(ip, port),
        })
    }

    fn normalized(&self) -> String {
        self.socket.to_string()
    }

    fn reachable(&self) -> bool {
        TcpStream::connect_timeout(&self.socket, CONNECT_TIMEOUT).is_ok()
    }
}

fn validate_owner(owner: &str) -> Result<(), LeaseError> {
    if owner.is_empty()
        || owner.len() > 64
        || !owner
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(LeaseError::InvalidOwner);
    }
    Ok(())
}

fn remove_if_present(path: &Path) -> Result<(), LeaseError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(LeaseError::io(path, source)),
    }
}

#[cfg(windows)]
fn process_is_alive(pid: u32) -> bool {
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output()
        .is_ok_and(|output| {
            String::from_utf8_lossy(&output.stdout).contains(&format!("\",\"{pid}\",\""))
        })
}

#[cfg(not(windows))]
fn process_is_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
        || std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
}

/// Lease registry failures. Endpoint reachability and user-managed listeners
/// are outcomes, not errors.
#[derive(Debug, Error)]
pub enum LeaseError {
    #[error("embedding lease I/O failed at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("embedding lease endpoint must be an explicit loopback HTTP(S) URL: {endpoint}")]
    InvalidEndpoint { endpoint: String },
    #[error("embedding lease owner must contain only 1-64 ASCII letters, digits, '-' or '_'")]
    InvalidOwner,
    #[error("embedding owner process {server_pid} is not live and reachable at {endpoint}")]
    OwnerNotLive { endpoint: String, server_pid: u32 },
    #[error("could not serialize the embedding ownership marker: {0}")]
    SerializeOwner(#[source] serde_json::Error),
    #[error("could not allocate a unique embedding lease filename")]
    LeaseNameExhausted,
}

impl LeaseError {
    fn io(path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::Arc;

    fn start_owned(registry: &EmbeddingLeaseRegistry) -> (TcpListener, EmbeddingLease, String) {
        let probe = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = probe.local_addr().unwrap();
        drop(probe);
        let endpoint = format!("http://{address}");
        let OwnerPreparation::Start(permit) = registry.prepare_owner(&endpoint).unwrap() else {
            panic!("an unused socket must issue a start permit");
        };
        let listener = TcpListener::bind(address).unwrap();
        let lease = permit.claim("test-owner", std::process::id()).unwrap();
        (listener, lease, endpoint)
    }

    #[test]
    fn user_managed_listener_is_never_leased() {
        let root = tempfile::tempdir().unwrap();
        let registry = EmbeddingLeaseRegistry::at(root.path());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());

        assert!(matches!(
            registry.join_existing(&endpoint).unwrap(),
            JoinOutcome::UserManaged
        ));
        assert_eq!(
            std::fs::read_dir(root.path())
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "lease"))
                .count(),
            0
        );
    }

    #[test]
    fn overlapping_guards_are_unique_and_last_release_reserves_stop() {
        let root = tempfile::tempdir().unwrap();
        let registry = EmbeddingLeaseRegistry::at(root.path());
        let (_listener, owner_lease, endpoint) = start_owned(&registry);
        let JoinOutcome::Acquired(joined) = registry.join_existing(&endpoint).unwrap() else {
            panic!("matching owner must permit a join");
        };

        assert!(matches!(
            owner_lease.release().unwrap(),
            ReleaseOutcome::OthersRemain
        ));
        let ReleaseOutcome::StopOwned(stop) = joined.release().unwrap() else {
            panic!("the final live lease must own teardown");
        };
        assert!(matches!(
            registry.join_existing(&endpoint).unwrap(),
            JoinOutcome::Stopping
        ));
        stop.complete(false).unwrap();
        assert!(matches!(
            registry.join_existing(&endpoint).unwrap(),
            JoinOutcome::Acquired(_)
        ));
    }

    #[test]
    fn drop_releases_only_its_unique_claim() {
        let root = tempfile::tempdir().unwrap();
        let registry = EmbeddingLeaseRegistry::at(root.path());
        let (_listener, owner_lease, endpoint) = start_owned(&registry);
        let JoinOutcome::Acquired(joined) = registry.join_existing(&endpoint).unwrap() else {
            panic!("join");
        };
        drop(joined);

        assert!(matches!(
            owner_lease.release().unwrap(),
            ReleaseOutcome::StopOwned(_)
        ));
    }

    #[test]
    fn stale_leases_are_pruned_inside_the_locked_transaction() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path()).unwrap();
        std::fs::write(root.path().join("stale.lease"), "not-a-pid").unwrap();
        let registry = EmbeddingLeaseRegistry::at(root.path());

        let _ = registry.join_existing("http://127.0.0.1:1").unwrap();

        assert!(!root.path().join("stale.lease").exists());
    }

    #[test]
    fn legacy_marker_requires_owner_side_migration() {
        let root = tempfile::tempdir().unwrap();
        let registry = EmbeddingLeaseRegistry::at(root.path());
        std::fs::create_dir_all(root.path()).unwrap();
        std::fs::write(root.path().join(OWNER_FILE), "localpilot\n").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());

        assert!(matches!(
            registry.join_existing(&endpoint).unwrap(),
            JoinOutcome::UserManaged
        ));
        let OwnerPreparation::Legacy(permit) = registry.prepare_owner(&endpoint).unwrap() else {
            panic!("owner must receive the migration permit");
        };
        let lease = permit.migrate("localpilot", std::process::id()).unwrap();
        assert!(matches!(
            registry.join_existing(&endpoint).unwrap(),
            JoinOutcome::Acquired(_)
        ));
        drop(lease);
    }

    #[test]
    fn lock_serializes_concurrent_joiners_without_colliding() {
        let root = tempfile::tempdir().unwrap();
        let registry = Arc::new(EmbeddingLeaseRegistry::at(root.path()));
        let (_listener, owner_lease, endpoint) = start_owned(&registry);
        let handles = (0..8)
            .map(|_| {
                let registry = Arc::clone(&registry);
                let endpoint = endpoint.clone();
                std::thread::spawn(move || registry.join_existing(&endpoint).unwrap())
            })
            .collect::<Vec<_>>();
        let leases = handles
            .into_iter()
            .map(|handle| match handle.join().unwrap() {
                JoinOutcome::Acquired(lease) => lease,
                other => panic!("unexpected join outcome: {other:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(leases.len(), 8);
        drop(leases);
        assert!(matches!(
            owner_lease.release().unwrap(),
            ReleaseOutcome::StopOwned(_)
        ));
    }

    #[test]
    fn owner_reaper_waits_for_clients_then_atomically_claims_stop() {
        let root = tempfile::tempdir().unwrap();
        let registry = EmbeddingLeaseRegistry::at(root.path());
        let (_listener, owner_lease, endpoint) = start_owned(&registry);
        let JoinOutcome::Acquired(client) = registry.join_existing(&endpoint).unwrap() else {
            panic!("client must join");
        };
        assert!(matches!(
            owner_lease.release().unwrap(),
            ReleaseOutcome::OthersRemain
        ));
        assert!(matches!(
            registry
                .prepare_stop_when_unleased(&endpoint, "test-owner", std::process::id())
                .unwrap(),
            StopPreparation::Waiting
        ));
        drop(client);
        let StopPreparation::Ready(stop) = registry
            .prepare_stop_when_unleased(&endpoint, "test-owner", std::process::id())
            .unwrap()
        else {
            panic!("drained clients must hand teardown to the owner reaper");
        };
        assert!(matches!(
            registry.join_existing(&endpoint).unwrap(),
            JoinOutcome::Stopping
        ));
        stop.complete(false).unwrap();
    }

    #[test]
    fn remote_and_unparseable_endpoints_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        let registry = EmbeddingLeaseRegistry::at(root.path());
        assert!(matches!(
            registry.join_existing("https://example.com:8090"),
            Err(LeaseError::InvalidEndpoint { .. })
        ));
        assert!(matches!(
            registry.join_existing("not a url"),
            Err(LeaseError::InvalidEndpoint { .. })
        ));
        assert!(endpoints_match("http://localhost:8090/", "http://127.0.0.1:8090").unwrap());
        assert!(!endpoints_match("http://127.0.0.1:8090", "http://127.0.0.1:8091").unwrap());
    }
}
