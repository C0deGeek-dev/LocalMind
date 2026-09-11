//! Three servers, one constraint, and the difference between them.
//!
//! The dangerous case is not the server that rejects a schema — that fails
//! loudly and is retried unconstrained. It is the server that accepts the
//! schema, ignores it, and answers HTTP 200. Both look identical from the call
//! site, so capability has to be established by inviting a violation and
//! watching what comes back, not by observing that a request succeeded.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

use localmind_inference::{
    ChatConstraint, ChatEndpoint, ChatMessage, ConstraintDisposition, JsonSchemaConstraint,
};

/// How a fixture server treats a `json_schema` request.
#[derive(Clone, Copy)]
enum SchemaBehaviour {
    /// Enforces it: refuses to emit the invited violation.
    Enforces,
    /// Accepts it and ignores it — HTTP 200 with unconstrained output. This is
    /// llama.cpp with thinking enabled, or a schema it could not compile.
    FailsOpen,
    /// Rejects it outright at sampler init.
    Rejects,
}

fn fixture_server(behaviour: SchemaBehaviour, max_requests: usize) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();

    thread::spawn(move || {
        for _ in 0..max_requests {
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            let mut request = Vec::new();
            let mut buffer = [0_u8; 4096];
            loop {
                match stream.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => {
                        request.extend_from_slice(&buffer[..read]);
                        if request_complete(&request) {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }

            let text = String::from_utf8_lossy(&request).to_string();
            let asked_for_schema = text.contains("json_schema");

            let response = if asked_for_schema && matches!(behaviour, SchemaBehaviour::Rejects) {
                // What a real llama.cpp build returns when the grammar cannot
                // accept the model's opening token.
                let body = "{\"error\":{\"message\":\"Failed to initialize samplers\"}}";
                format!(
                    "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
            } else {
                let content = if asked_for_schema && matches!(behaviour, SchemaBehaviour::Enforces)
                {
                    // Constrained: the invited violation is unreachable.
                    "{\\\"ok\\\": true}"
                } else {
                    // Unconstrained: the model does as it was asked.
                    "{\\\"nope\\\": \\\"x\\\"}"
                };
                let body =
                    format!("{{\"choices\":[{{\"message\":{{\"content\":\"{content}\"}}}}]}}");
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
            };
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });

    format!("http://{address}")
}

fn request_complete(request: &[u8]) -> bool {
    let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    let headers = String::from_utf8_lossy(&request[..header_end]);
    let mut content_length = 0_usize;
    for line in headers.lines() {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }
    }
    request.len() >= header_end + 4 + content_length
}

fn endpoint(base: &str) -> ChatEndpoint {
    ChatEndpoint::new(base, "test-model", None, 5).unwrap()
}

#[test]
fn a_server_that_enforces_the_schema_is_reported_as_capable() {
    let base = fixture_server(SchemaBehaviour::Enforces, 2);
    let capabilities = endpoint(&base).probe_capabilities();

    assert!(capabilities.json_object);
    assert!(capabilities.json_schema_enforced);
}

#[test]
fn a_server_that_accepts_the_schema_and_ignores_it_is_not_reported_as_capable() {
    let base = fixture_server(SchemaBehaviour::FailsOpen, 2);
    let capabilities = endpoint(&base).probe_capabilities();

    // The request succeeded. That is exactly the trap: a probe that asked
    // "did the call work?" would report full schema support here and every
    // caller downstream would skip validation on unconstrained output.
    assert!(capabilities.json_object);
    assert!(
        !capabilities.json_schema_enforced,
        "a 200 that ignored the schema is not schema support"
    );
}

#[test]
fn a_server_that_rejects_the_schema_is_not_reported_as_capable_either() {
    let base = fixture_server(SchemaBehaviour::Rejects, 2);
    let capabilities = endpoint(&base).probe_capabilities();

    assert!(capabilities.json_object);
    assert!(!capabilities.json_schema_enforced);
}

#[test]
fn a_rejected_constraint_is_retried_unconstrained_and_says_so() {
    let base = fixture_server(SchemaBehaviour::Rejects, 2);
    let schema = JsonSchemaConstraint::new(
        "probe",
        r#"{"type":"object","properties":{"ok":{"type":"boolean"}}}"#,
    )
    .unwrap();

    let completed = endpoint(&base)
        .complete_constrained(
            &[ChatMessage::user("anything")],
            &ChatConstraint::JsonSchema(schema),
        )
        .unwrap();

    assert_eq!(
        completed.disposition,
        ConstraintDisposition::RefusedByTransport
    );
    assert!(completed.disposition.known_unconstrained());
    assert!(!completed.completion.content.is_empty());
}

#[test]
fn a_constraint_that_was_accepted_is_still_only_reported_as_requested() {
    let base = fixture_server(SchemaBehaviour::FailsOpen, 1);
    let schema = JsonSchemaConstraint::new(
        "probe",
        r#"{"type":"object","properties":{"ok":{"type":"boolean"}}}"#,
    )
    .unwrap();

    let completed = endpoint(&base)
        .complete_constrained(
            &[ChatMessage::user("anything")],
            &ChatConstraint::JsonSchema(schema),
        )
        .unwrap();

    // The server ignored the schema and the reply proves it, yet the call
    // reports only that the constraint was asked for. There is no disposition
    // that claims enforcement, which is why the caller must validate.
    assert_eq!(completed.disposition, ConstraintDisposition::Requested);
    assert!(!completed.disposition.known_unconstrained());
    assert!(completed.completion.content.contains("nope"));
}
