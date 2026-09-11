//! What a chat endpoint will accept as an output constraint, and what that is
//! worth.
//!
//! A local server may take a `response_format` constraint, ignore it, and
//! return HTTP 200. llama.cpp does this on several paths: grammar enforcement
//! goes silently inactive when thinking is enabled, the server fails open when
//! schema-grammar parsing fails, and a schema using `$ref` or `$defs` quietly
//! falls back to unconstrained JSON. In every one of those cases the request
//! succeeds and a caller that trusts the constraint believes it received
//! structured output.
//!
//! So this module never reports that a schema was enforced. It reports what was
//! *asked for* and what the transport did with the ask, and it refuses to build
//! a constraint that is known to fail open. **The contract decides whether to
//! attempt a constraint; it never decides whether to validate.** Validation runs
//! on every reply on every path — constrained, unconstrained and repaired alike.
//!
//! The second thing this module separates is *why* a second request happened.
//! A transport that refuses a constraint costs nothing to retry without it. A
//! reply that arrives and fails the caller's validation costs the one bounded
//! repair pass. Collapsing the two is how a server that rejects every
//! constrained request silently burns a budget meant for malformed content.

use crate::InferenceError;
use serde::Serialize;

/// A JSON-schema output constraint, guaranteed free of the constructs that make
/// a server stop enforcing without saying so.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JsonSchemaConstraint {
    name: String,
    schema: serde_json::Value,
}

impl JsonSchemaConstraint {
    /// Build a constraint from a flat JSON schema.
    ///
    /// # Errors
    /// [`InferenceError::UnenforceableSchema`] when the schema uses `$ref`,
    /// `$defs` or `definitions`. Those are not merely unsupported — a server
    /// that meets them falls back to unconstrained JSON and still answers 200,
    /// so a constraint built from one would be a guarantee that quietly is not
    /// one. Refusing at construction means that constraint cannot be sent by
    /// accident.
    ///
    /// [`InferenceError::EncodeRequest`] when the schema is not valid JSON.
    pub fn new(name: impl Into<String>, schema: &str) -> Result<Self, InferenceError> {
        for construct in ["$ref", "$defs", "\"definitions\""] {
            if schema.contains(construct) {
                return Err(InferenceError::UnenforceableSchema { construct });
            }
        }

        Ok(Self {
            name: name.into(),
            schema: serde_json::from_str(schema).map_err(InferenceError::EncodeRequest)?,
        })
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn schema(&self) -> &serde_json::Value {
        &self.schema
    }
}

/// The output constraint to attempt on a request.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum ChatConstraint {
    /// Send no `response_format`. The reply is prose and must still be parsed
    /// and validated.
    #[default]
    None,
    /// `response_format: {"type": "json_object"}` — the reply should be a JSON
    /// object, with no statement about its shape.
    JsonObject,
    /// `response_format: {"type": "json_schema", ...}` — the reply should match
    /// the schema. Should, not will.
    JsonSchema(JsonSchemaConstraint),
}

/// What became of the constraint on one exchange.
///
/// There is deliberately no `Enforced` variant. Nothing observable at this layer
/// distinguishes a server that applied a grammar from one that ignored it and
/// happened to answer in shape, so a variant claiming enforcement could only
/// ever be a guess presented as a fact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConstraintDisposition {
    /// No constraint was asked for.
    NotRequested,
    /// The constraint was sent and the server answered. The reply still has to
    /// be validated: this says the ask was made, not that it was honoured.
    Requested,
    /// The transport rejected the constraint and the request was retried
    /// without it. The reply is definitely unconstrained.
    RefusedByTransport,
}

impl ConstraintDisposition {
    /// Whether the reply is known to be unconstrained.
    ///
    /// Note what this is not: its negation is not "constrained". A `Requested`
    /// reply may be either, which is the whole reason validation is
    /// unconditional.
    #[must_use]
    pub fn known_unconstrained(self) -> bool {
        matches!(self, Self::NotRequested | Self::RefusedByTransport)
    }
}

/// A reply together with an honest account of how it was obtained.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConstrainedCompletion {
    pub completion: crate::ChatCompletion,
    pub disposition: ConstraintDisposition,
}

/// The one bounded content-repair pass.
///
/// Spent only when a reply *arrived* and failed the caller's validation — a
/// parse failure or a contract violation. A transport that refuses the
/// constraint never touches this: that retry is free and automatic, because
/// being unable to ask for JSON says nothing about the model's ability to
/// produce it.
///
/// One pass, then the caller degrades — deterministic fallback, or review-only
/// output. Parsing failure is not reasoning failure, and neither is evidence of
/// a lesson.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RepairBudget {
    spent: bool,
}

impl RepairBudget {
    #[must_use]
    pub fn new() -> Self {
        Self { spent: false }
    }

    /// Claim the repair pass. `true` the first time, `false` ever after.
    pub fn spend(&mut self) -> bool {
        if self.spent {
            return false;
        }
        self.spent = true;
        true
    }

    #[must_use]
    pub fn is_spent(self) -> bool {
        self.spent
    }
}

/// What an endpoint was observed to do, not what it advertises.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ChatCapabilities {
    /// A `json_object` request was accepted.
    pub json_object: bool,
    /// A `json_schema` request was accepted **and** a reply that was invited to
    /// violate the schema came back conforming anyway.
    ///
    /// The second half is the whole test. A server that accepts the constraint
    /// and ignores it answers the first half exactly like one that enforces it.
    pub json_schema_enforced: bool,
}

impl ChatCapabilities {
    /// The strongest constraint worth attempting for `schema`.
    ///
    /// Purely an optimisation: a weaker answer costs latency and quality, never
    /// correctness, because the reply is validated either way.
    #[must_use]
    pub fn best_constraint(self, schema: Option<JsonSchemaConstraint>) -> ChatConstraint {
        match (self.json_schema_enforced, schema, self.json_object) {
            (true, Some(schema), _) => ChatConstraint::JsonSchema(schema),
            (_, _, true) => ChatConstraint::JsonObject,
            _ => ChatConstraint::None,
        }
    }
}

/// The schema the honesty probe constrains against: one boolean key, nothing
/// else allowed. Small enough that conformance can be checked here rather than
/// pulling in a schema validator for a single probe.
pub(crate) const PROBE_SCHEMA: &str = r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["ok"],
  "properties": { "ok": { "type": "boolean" } }
}"#;

/// The prompt that invites a violation. A server enforcing the schema cannot
/// comply with it; one that has silently stopped enforcing will.
pub(crate) const PROBE_VIOLATION_PROMPT: &str =
    "Reply with a JSON object whose only key is \"nope\" and whose value is the string \"x\". \
     Do not include any other key.";

/// Whether a probe reply conforms to [`PROBE_SCHEMA`].
pub(crate) fn probe_reply_conforms(reply: &str) -> bool {
    let Some(payload) = crate::extract_json_payload(reply) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
        return false;
    };
    let Some(object) = value.as_object() else {
        return false;
    };

    object.len() == 1 && object.get("ok").is_some_and(serde_json::Value::is_boolean)
}

/// The wire shape of `response_format`, flattened here so the request struct
/// stays one type across all three constraints.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum ResponseFormatBody<'a> {
    Simple {
        #[serde(rename = "type")]
        kind: &'static str,
    },
    Schema {
        #[serde(rename = "type")]
        kind: &'static str,
        json_schema: SchemaBody<'a>,
    },
}

#[derive(Debug, Serialize)]
pub(crate) struct SchemaBody<'a> {
    pub(crate) name: &'a str,
    pub(crate) schema: &'a serde_json::Value,
    pub(crate) strict: bool,
}

impl ChatConstraint {
    pub(crate) fn response_format(&self) -> Option<ResponseFormatBody<'_>> {
        match self {
            Self::None => None,
            Self::JsonObject => Some(ResponseFormatBody::Simple {
                kind: "json_object",
            }),
            Self::JsonSchema(constraint) => Some(ResponseFormatBody::Schema {
                kind: "json_schema",
                json_schema: SchemaBody {
                    name: &constraint.name,
                    schema: &constraint.schema,
                    strict: true,
                },
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{
        probe_reply_conforms, ChatCapabilities, ChatConstraint, ConstraintDisposition,
        JsonSchemaConstraint, RepairBudget, PROBE_SCHEMA,
    };
    use crate::InferenceError;

    const FLAT: &str = r#"{"type":"object","properties":{"ok":{"type":"boolean"}}}"#;

    #[test]
    fn a_schema_that_would_silently_fail_open_cannot_be_built() {
        for unenforceable in [
            r##"{"type":"object","properties":{"a":{"$ref":"#/$defs/x"}}}"##,
            r#"{"$defs":{"x":{"type":"string"}},"type":"object"}"#,
            r#"{"definitions":{"x":{"type":"string"}},"type":"object"}"#,
        ] {
            assert!(matches!(
                JsonSchemaConstraint::new("draft", unenforceable),
                Err(InferenceError::UnenforceableSchema { .. })
            ));
        }

        assert!(JsonSchemaConstraint::new("draft", FLAT).is_ok());
    }

    #[test]
    fn a_malformed_schema_is_refused_rather_than_sent() {
        assert!(matches!(
            JsonSchemaConstraint::new("draft", "{not json"),
            Err(InferenceError::EncodeRequest(_))
        ));
    }

    #[test]
    fn the_hindsight_schema_is_constrainable() {
        // The contract this plan actually sends. Flat by construction, so it
        // must survive the same gate every other schema does.
        JsonSchemaConstraint::new("hindsight_draft", localmind_core::HINDSIGHT_DRAFT_SCHEMA)
            .expect("the hindsight schema must be sendable as a constraint");
    }

    #[test]
    fn only_a_refused_or_absent_constraint_is_known_unconstrained() {
        assert!(ConstraintDisposition::NotRequested.known_unconstrained());
        assert!(ConstraintDisposition::RefusedByTransport.known_unconstrained());
        // The important one: asking is not receiving.
        assert!(!ConstraintDisposition::Requested.known_unconstrained());
    }

    #[test]
    fn the_repair_budget_is_spent_exactly_once() {
        let mut budget = RepairBudget::new();
        assert!(!budget.is_spent());
        assert!(budget.spend());
        assert!(budget.is_spent());
        assert!(!budget.spend());
        assert!(!budget.spend());
    }

    #[test]
    fn the_best_constraint_degrades_without_ever_reaching_correctness() {
        let schema = JsonSchemaConstraint::new("draft", FLAT).unwrap();

        let enforcing = ChatCapabilities {
            json_object: true,
            json_schema_enforced: true,
        };
        assert!(matches!(
            enforcing.best_constraint(Some(schema.clone())),
            ChatConstraint::JsonSchema(_)
        ));

        // A server that takes the schema and ignores it is treated as if it had
        // no schema support at all.
        let fails_open = ChatCapabilities {
            json_object: true,
            json_schema_enforced: false,
        };
        assert_eq!(
            fails_open.best_constraint(Some(schema.clone())),
            ChatConstraint::JsonObject
        );

        let bare = ChatCapabilities::default();
        assert_eq!(bare.best_constraint(Some(schema)), ChatConstraint::None);
        assert_eq!(enforcing.best_constraint(None), ChatConstraint::JsonObject);
    }

    #[test]
    fn the_probe_only_accepts_a_reply_that_obeyed_the_schema() {
        assert!(probe_reply_conforms(r#"{"ok": true}"#));
        assert!(probe_reply_conforms("<think>hmm</think>\n{\"ok\": false}"));

        for failed_open in [
            r#"{"nope": "x"}"#,
            r#"{"ok": true, "nope": "x"}"#,
            r#"{"ok": "yes"}"#,
            "{}",
            "sorry, I cannot do that",
        ] {
            assert!(
                !probe_reply_conforms(failed_open),
                "{failed_open} must read as unenforced"
            );
        }
    }

    #[test]
    fn the_probe_schema_is_itself_flat() {
        JsonSchemaConstraint::new("probe", PROBE_SCHEMA)
            .expect("the probe cannot use a schema that fails open");
    }

    #[test]
    fn the_wire_shape_matches_what_an_openai_compatible_server_expects() {
        let schema = JsonSchemaConstraint::new("hindsight_draft", FLAT).unwrap();
        let body =
            serde_json::to_value(ChatConstraint::JsonSchema(schema).response_format()).unwrap();

        assert_eq!(body["type"], "json_schema");
        assert_eq!(body["json_schema"]["name"], "hindsight_draft");
        assert_eq!(body["json_schema"]["strict"], true);
        assert_eq!(body["json_schema"]["schema"]["type"], "object");

        let object = serde_json::to_value(ChatConstraint::JsonObject.response_format()).unwrap();
        assert_eq!(object["type"], "json_object");
        assert!(object.get("json_schema").is_none());

        assert!(ChatConstraint::None.response_format().is_none());
    }
}
