//! CLI integration for the neutral embedding lease contract.

use anyhow::{anyhow, Result};
use localmind_inference::embedding_lease::{
    EmbeddingLease, EmbeddingLeaseRegistry, JoinOutcome, LeaseError,
};
use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Need {
    Required,
    BestEffort,
}

/// Acquire a guard only for a configured, reachable, exactly owned endpoint.
/// A reachable user-managed endpoint remains usable without a lease. This
/// function has no process-discovery or process-control capability.
pub fn acquire(project: &Path, need: Need) -> Result<Option<EmbeddingLease>> {
    let config = match localmind_store::ProjectConfig::discover(project) {
        Ok(config) => config,
        Err(_) if need == Need::BestEffort => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let endpoint = config
        .config
        .inference
        .as_ref()
        .filter(|settings| settings.features.embeddings)
        .and_then(|settings| {
            settings
                .embedding_model
                .as_ref()
                .and_then(|_| settings.embedding_base_url())
        });
    let Some(endpoint) = endpoint else {
        return if need == Need::Required {
            Err(anyhow!(not_configured_message()))
        } else {
            Ok(None)
        };
    };
    let Some(registry) = EmbeddingLeaseRegistry::machine_default() else {
        return unavailable(
            need,
            endpoint,
            "the user home for lease state is unavailable",
        );
    };
    acquire_from(&registry, endpoint, need)
}

fn acquire_from(
    registry: &EmbeddingLeaseRegistry,
    endpoint: &str,
    need: Need,
) -> Result<Option<EmbeddingLease>> {
    match registry.join_existing(endpoint) {
        Ok(JoinOutcome::Acquired(lease)) => Ok(Some(lease)),
        // User-managed endpoints need no lease because LocalMind has no right to
        // stop them. They remain valid configured inference endpoints.
        Ok(JoinOutcome::UserManaged) => Ok(None),
        Ok(JoinOutcome::Unreachable) => unavailable(need, endpoint, "nothing is listening"),
        Ok(JoinOutcome::Stopping) => unavailable(need, endpoint, "the owned server is stopping"),
        Err(error) => unavailable_error(need, endpoint, error),
    }
}

fn unavailable(need: Need, endpoint: &str, reason: &str) -> Result<Option<EmbeddingLease>> {
    let message = unavailable_message(endpoint, reason);
    if need == Need::Required {
        Err(anyhow!(message))
    } else {
        eprintln!("embeddings: {message} Continuing with the lexical/no-vector fallback.");
        Ok(None)
    }
}

fn unavailable_error(
    need: Need,
    endpoint: &str,
    error: LeaseError,
) -> Result<Option<EmbeddingLease>> {
    unavailable(need, endpoint, &error.to_string())
}

fn unavailable_message(endpoint: &str, reason: &str) -> String {
    format!(
        "configured endpoint {endpoint} is unavailable ({reason}). Run `localbox embed-serve`, \
         then verify [inference] embedding_base_url + embedding_model in .localmind.toml and retry."
    )
}

fn not_configured_message() -> &'static str {
    "embeddings are required for this work. Configure [inference] embedding_base_url + \
     embedding_model in .localmind.toml, run `localbox embed-serve`, and retry."
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn a_reachable_user_managed_endpoint_needs_no_lease() {
        let root = tempfile::tempdir().unwrap();
        let registry = EmbeddingLeaseRegistry::at(root.path());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());

        assert!(acquire_from(&registry, &endpoint, Need::Required)
            .unwrap()
            .is_none());
    }

    #[test]
    fn required_and_best_effort_unreachable_outcomes_are_distinct() {
        let root = tempfile::tempdir().unwrap();
        let registry = EmbeddingLeaseRegistry::at(root.path());
        let probe = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", probe.local_addr().unwrap());
        drop(probe);

        assert!(acquire_from(&registry, &endpoint, Need::BestEffort)
            .unwrap()
            .is_none());
        let error = acquire_from(&registry, &endpoint, Need::Required)
            .unwrap_err()
            .to_string();
        assert!(error.contains("`localbox embed-serve`"));
        assert!(error.contains("embedding_base_url + embedding_model"));
    }

    #[test]
    fn configured_but_remote_endpoint_is_never_treated_as_owned() {
        let root = tempfile::tempdir().unwrap();
        let registry = EmbeddingLeaseRegistry::at(root.path());
        let error = acquire_from(&registry, "https://example.com:8090", Need::Required)
            .unwrap_err()
            .to_string();
        assert!(error.contains("explicit loopback"));
        assert!(error.contains("`localbox embed-serve`"));
    }
}
