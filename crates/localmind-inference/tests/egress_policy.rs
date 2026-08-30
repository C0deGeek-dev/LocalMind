//! `local_only` means what its name says: transcript- and document-derived text
//! may only reach a model server on this machine.
//!
//! The setting historically constrained storage scope alone, so a configuration
//! reading `local_only = true` would still ship text to any reachable `https://`
//! host. These tests pin the other half of the promise, and most of them are
//! negative cases — the value of a guard is entirely in what it refuses.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use localmind_inference::{EgressPolicy, EmbeddingEndpoint, InferenceError};

fn refusal(base_url: &str) -> InferenceError {
    EmbeddingEndpoint::new(base_url, "any-model", None, 5)
        .expect_err("a non-loopback endpoint must be refused under the default policy")
}

#[test]
fn loopback_endpoints_are_accepted() {
    for base_url in [
        "http://127.0.0.1:8080",
        "http://localhost:11435",
        "http://LOCALHOST:11435",
        "http://[::1]:8080",
        "http://127.5.6.7:8080",
        "https://localhost",
        "http://anything.localhost:9000",
    ] {
        assert!(
            EmbeddingEndpoint::new(base_url, "any-model", None, 5).is_ok(),
            "{base_url} names this machine and must be permitted"
        );
    }
}

#[test]
fn a_non_loopback_embedding_endpoint_is_refused_by_default() {
    // The headline case: `local_only = true` is the only configurable state, so
    // the default policy is the one every real caller gets.
    let error = refusal("https://api.example.com/v1");
    assert!(
        matches!(&error, InferenceError::RemoteEndpointRefused { host, .. } if host == "api.example.com"),
        "expected a refusal naming the host, got {error:?}"
    );
    // The message has to tell someone what to do about it.
    let rendered = error.to_string();
    assert!(rendered.contains("api.example.com"), "{rendered}");
    assert!(rendered.contains("localhost"), "{rendered}");
}

#[test]
fn bind_any_addresses_are_not_loopback() {
    // `0.0.0.0` and `[::]` mean "listen on every interface". As a *destination*
    // they route to a real one, so treating them as loopback would be a hole.
    for base_url in ["http://0.0.0.0:8080", "http://[::]:8080"] {
        assert!(
            matches!(
                refusal(base_url),
                InferenceError::RemoteEndpointRefused { .. }
            ),
            "{base_url} is a bind-any address, not a loopback destination"
        );
    }
}

#[test]
fn a_hostname_is_refused_even_if_it_would_resolve_to_loopback() {
    // No name resolution happens, deliberately. A name that resolves to
    // 127.0.0.1 at check time can resolve elsewhere at connect time, and the gap
    // between the two lookups is where DNS rebinding lives.
    for base_url in [
        "http://my-nas.local:8080",
        "http://host.docker.internal:8080",
        "http://127.0.0.1.example.com:8080",
        "http://localhost.example.com:8080",
    ] {
        assert!(
            matches!(
                refusal(base_url),
                InferenceError::RemoteEndpointRefused { .. }
            ),
            "{base_url} is not a literal loopback address and must not be trusted to resolve to one"
        );
    }
}

#[test]
fn credentials_in_the_authority_do_not_smuggle_a_remote_host() {
    // `http://127.0.0.1@evil.example.com/` connects to evil.example.com. Reading
    // the host as "everything before the colon" would get this backwards.
    let error = refusal("http://127.0.0.1@evil.example.com/v1");
    assert!(
        matches!(&error, InferenceError::RemoteEndpointRefused { host, .. } if host == "evil.example.com"),
        "the host is what follows the last '@', got {error:?}"
    );
}

#[test]
fn an_unparseable_authority_is_refused_rather_than_permitted() {
    // Fail closed: a URL the check cannot understand is not a URL it may allow.
    for base_url in ["http://", "https://", "http:///v1", "http://:8080"] {
        assert!(
            matches!(
                refusal(base_url),
                InferenceError::RemoteEndpointRefused { .. }
            ),
            "{base_url} is not understood, so it must be refused"
        );
    }
}

#[test]
fn a_non_http_endpoint_still_fails_on_its_scheme() {
    // The pre-existing scheme check keeps its own error; the egress guard does
    // not swallow it into a less specific one.
    assert!(matches!(
        EmbeddingEndpoint::new("ftp://127.0.0.1", "any-model", None, 5),
        Err(InferenceError::InvalidEndpoint { .. })
    ));
}

#[test]
fn the_unrestricted_policy_exists_but_is_not_reachable_from_configuration() {
    // The seam is here so a future opt-in is a policy change rather than a
    // redesign. Nothing in config construction can select it today.
    assert!(
        EmbeddingEndpoint::with_policy(
            "https://api.example.com/v1",
            "any-model",
            None,
            5,
            EgressPolicy::Unrestricted,
        )
        .is_ok(),
        "an explicit opt-in is permitted"
    );
    assert_eq!(EgressPolicy::default(), EgressPolicy::LoopbackOnly);
}
