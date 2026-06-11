//! Secret redaction applied to transcript text before anything is persisted.
//!
//! The redactor is a data-driven pattern table plus an entropy backstop.
//! Adding a pattern means adding one `PatternRule` row and one corpus entry in
//! the tests below — nothing else.
//!
//! # What is caught
//!
//! - PEM private key blocks, including multi-line bodies.
//! - Labeled provider credentials: AWS access key IDs and secret keys, Slack
//!   tokens, JWTs, OpenAI/Anthropic-style `sk-` keys, GitHub tokens.
//! - `Authorization: Bearer …` values.
//! - Assignment-shaped secrets in code, config, JSON, and `.env` style:
//!   `password=…`, `api_key: …`, `"token": "…"`, `MY_SERVICE_API_KEY=…`.
//! - Credentials embedded in URLs (`scheme://user:pass@host`) and
//!   connection strings (`Password=…;`).
//! - Unlabeled high-entropy tokens of 24+ characters (entropy backstop).
//! - Configured sensitive paths, replaced verbatim.
//!
//! # What is not caught
//!
//! - Secrets split across multiple `redact` calls (each call sees one chunk).
//! - Unlabeled secrets shorter than 24 characters or with low entropy —
//!   hex-only tokens such as git SHAs and UUIDs deliberately stay below the
//!   entropy threshold so diffs and logs survive redaction readably.
//! - Secrets that have been transformed (base64 of a key, ROT13, …).
//!
//! Callers must treat the output as *less likely* to contain secrets, never
//! as guaranteed clean.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct Redaction {
    pub kind: String,
    pub replacements: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct RedactionReport {
    pub redacted_text: String,
    pub redactions: Vec<Redaction>,
}

struct PatternRule {
    kind: &'static str,
    pattern: &'static str,
}

/// Ordered most-specific first: a text span is consumed by the first rule
/// that matches it, so provider-specific kinds must precede the generic
/// assignment rules (pinned by a test below).
const PATTERN_RULES: &[PatternRule] = &[
    PatternRule {
        kind: "private_key",
        pattern: r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----",
    },
    PatternRule {
        kind: "aws_access_key_id",
        pattern: r"\b(?:A3T[A-Z0-9]|AKIA|AGPA|AIDA|AROA|AIPA|ANPA|ANVA|ASIA)[A-Z0-9]{16}\b",
    },
    PatternRule {
        kind: "aws_secret_access_key",
        pattern: r#"(?i)\baws[_-]?(?:secret[_-]?(?:access[_-]?)?key|sk)["']?\s*[:=]\s*["']?[A-Za-z0-9/+=]{40}\b"#,
    },
    PatternRule {
        kind: "slack_token",
        pattern: r"\bxox[baprs]-[A-Za-z0-9-]{10,}\b",
    },
    PatternRule {
        kind: "jwt",
        pattern: r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{5,}\b",
    },
    PatternRule {
        kind: "openai_api_key",
        pattern: r"\bsk-[A-Za-z0-9][A-Za-z0-9_-]{16,}\b",
    },
    PatternRule {
        kind: "github_token",
        pattern: r"\bgh[pousr]_[A-Za-z0-9_]{20,}\b",
    },
    PatternRule {
        kind: "bearer_token",
        pattern: r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]{16,}",
    },
    PatternRule {
        kind: "password_assignment",
        pattern: r#"(?i)\b(?:password|passwd|pwd)["']?\s*[:=]\s*['"]?[^'"\s;\[]{4,}"#,
    },
    PatternRule {
        kind: "token_assignment",
        pattern: r#"(?i)\b(?:api[_-]?key|token|secret|credentials?)["']?\s*[:=]\s*['"]?[^'"\s;\[]{16,}"#,
    },
    PatternRule {
        kind: "env_secret_assignment",
        pattern: r"\b[A-Z][A-Z0-9]*(?:_[A-Z0-9]+)*_(?:KEY|SECRET|TOKEN|PASSWORD|PASSWD|PWD)\s*=\s*\S{6,}",
    },
    PatternRule {
        kind: "url_credentials",
        pattern: r"[a-zA-Z][a-zA-Z0-9+.-]*://[^/\s:@]+:[^@\s/]{3,}@",
    },
    // Connection-string passwords (`…;Password=…;`) are covered by
    // password_assignment: its `[:=]` and `;`-excluding value class match the
    // `Key=Value;` shape, so a dedicated rule would never fire.
];

static COMPILED_RULES: LazyLock<Vec<(&'static str, Regex)>> = LazyLock::new(|| {
    PATTERN_RULES
        .iter()
        .filter_map(|rule| {
            Regex::new(rule.pattern)
                .ok()
                .map(|regex| (rule.kind, regex))
        })
        .collect()
});

/// Minimum length before the entropy backstop considers a token.
const ENTROPY_MIN_LEN: usize = 24;
/// Shannon entropy threshold in bits per character. Random base64/alphanumeric
/// secrets sit well above 4.6; English text, file paths, hex digests (git
/// SHAs, UUIDs — max 4.0 for hex) sit below it.
const ENTROPY_MIN_BITS: f64 = 4.6;

pub struct Redactor {
    sensitive_paths: Vec<String>,
}

impl Redactor {
    #[must_use]
    pub fn new(sensitive_paths: Vec<String>) -> Self {
        Self { sensitive_paths }
    }

    pub fn redact(&self, input: &str) -> RedactionReport {
        let mut text = input.to_string();
        let mut redactions = Vec::new();

        for (kind, regex) in COMPILED_RULES.iter() {
            apply_regex(&mut text, &mut redactions, kind, regex);
        }

        for path in &self.sensitive_paths {
            if path.is_empty() {
                continue;
            }

            let replacements = text.matches(path).count();
            if replacements > 0 {
                text = text.replace(path, "[REDACTED:sensitive_path]");
                redactions.push(Redaction {
                    kind: "sensitive_path".to_string(),
                    replacements,
                });
            }
        }

        apply_entropy_backstop(&mut text, &mut redactions);

        RedactionReport {
            redacted_text: text,
            redactions,
        }
    }
}

fn apply_regex(text: &mut String, redactions: &mut Vec<Redaction>, kind: &str, regex: &Regex) {
    let replacements = regex.find_iter(text).count();

    if replacements > 0 {
        let replacement = format!("[REDACTED:{kind}]");
        *text = regex.replace_all(text, replacement).into_owned();
        redactions.push(Redaction {
            kind: kind.to_string(),
            replacements,
        });
    }
}

/// Catches unlabeled secrets the pattern table cannot know about: any token of
/// `ENTROPY_MIN_LEN`+ secret-alphabet characters whose Shannon entropy says
/// "random", not "language". Runs last so labeled secrets are already gone.
fn apply_entropy_backstop(text: &mut String, redactions: &mut Vec<Redaction>) {
    static TOKEN: LazyLock<Option<Regex>> =
        LazyLock::new(|| Regex::new(&format!(r"[A-Za-z0-9+/=_-]{{{ENTROPY_MIN_LEN},}}")).ok());

    let Some(token_regex) = TOKEN.as_ref() else {
        return;
    };

    let mut replacements = 0;
    let result = token_regex.replace_all(text, |caps: &regex::Captures<'_>| {
        let token = &caps[0];
        if shannon_entropy_bits(token) >= ENTROPY_MIN_BITS {
            replacements += 1;
            "[REDACTED:high_entropy]".to_string()
        } else {
            token.to_string()
        }
    });

    if replacements > 0 {
        *text = result.into_owned();
        redactions.push(Redaction {
            kind: "high_entropy".to_string(),
            replacements,
        });
    }
}

fn shannon_entropy_bits(token: &str) -> f64 {
    let bytes = token.as_bytes();
    if bytes.is_empty() {
        return 0.0;
    }
    let mut counts = [0usize; 256];
    for &b in bytes {
        counts[usize::from(b)] += 1;
    }
    let len = bytes.len() as f64;
    counts
        .iter()
        .filter(|&&count| count > 0)
        .map(|&count| {
            let p = count as f64 / len;
            -p * p.log2()
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::{Redactor, COMPILED_RULES, PATTERN_RULES};

    fn redact(input: &str) -> super::RedactionReport {
        Redactor::new(Vec::new()).redact(input)
    }

    #[test]
    fn every_pattern_rule_compiles() {
        assert_eq!(COMPILED_RULES.len(), PATTERN_RULES.len());
    }

    #[test]
    fn specific_token_rules_do_not_get_overwritten_by_generic_assignments() {
        let report = redact("token = sk-proj-abcdefghijklmnopqrstuvwxyz123456");

        assert!(report.redacted_text.contains("[REDACTED:openai_api_key]"));
        assert!(!report.redacted_text.contains("sk-proj"));
    }

    /// Corpus of secrets that MUST be caught. Each row: input, the kind that
    /// must appear, and a fragment of the secret that must be gone.
    /// The Slack row is concatenated at runtime so the fixture file itself
    /// never contains a token-shaped literal (hosting providers block pushes
    /// containing anything that looks like a live credential).
    #[test]
    fn corpus_secrets_are_caught() {
        let slack_fixture = format!(
            "slack bot xoxb-{}-{}-{}",
            "2444333222111", "0123456789012", "AbCdEfGhIjKlMnOpQrStUvWx"
        );
        let corpus: &[(&str, &str, &str)] = &[
            (
                slack_fixture.as_str(),
                "slack_token",
                "xoxb-2444333222111",
            ),
            (
                "key id AKIAIOSFODNN7EXAMPLE in config",
                "aws_access_key_id",
                "AKIAIOSFODNN7",
            ),
            (
                "aws_secret_access_key = wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
                "aws_secret_access_key",
                "wJalrXUtnFEMI",
            ),
            (
                "jwt eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U here",
                "jwt",
                "eyJhbGciOiJIUzI1NiIs",
            ),
            (
                "openai sk-proj-abcdefghijklmnopqrstuvwxyz123456",
                "openai_api_key",
                "sk-proj-abcdef",
            ),
            (
                "github ghp_AbCdEfGhIjKlMnOpQrStUvWxYz0123456789",
                "github_token",
                "ghp_AbCdEf",
            ),
            (
                "Authorization: Bearer abc123def456ghi789jkl012",
                "bearer_token",
                "abc123def456",
            ),
            (
                "password = hunter2hunter2",
                "password_assignment",
                "hunter2",
            ),
            (
                r#"{"api_key": "0123456789abcdefghij"}"#,
                "token_assignment",
                "0123456789abcdefghij",
            ),
            (
                r#"{"password": "hunter2hunter2"}"#,
                "password_assignment",
                "hunter2",
            ),
            (
                "MY_SERVICE_API_KEY=abc123def456",
                "env_secret_assignment",
                "abc123def456",
            ),
            (
                "DATABASE_URL is postgres://app:s3cretpw@db.internal:5432/app",
                "url_credentials",
                "s3cretpw",
            ),
            (
                "Server=db;Database=x;User Id=sa;Password=Sup3rS3cret!;",
                "password_assignment",
                "Sup3rS3cret",
            ),
            (
                "-----BEGIN RSA PRIVATE KEY-----\nMIIEow\nlines\n-----END RSA PRIVATE KEY-----",
                "private_key",
                "MIIEow",
            ),
        ];

        for (input, expected_kind, must_be_gone) in corpus {
            let report = redact(input);
            assert!(
                report.redactions.iter().any(|r| r.kind == *expected_kind),
                "expected kind {expected_kind} for input {input:?}; got {:?}",
                report.redactions
            );
            assert!(
                !report.redacted_text.contains(must_be_gone),
                "secret fragment {must_be_gone:?} survived in {:?}",
                report.redacted_text
            );
        }
    }

    /// Near-misses that MUST survive untouched: redacting these would make
    /// transcripts unreadable and erode trust in the redactor.
    #[test]
    fn corpus_near_misses_are_not_redacted() {
        let corpus: &[&str] = &[
            "commit 2fd4e1c67a2d28fced849ee1bb76e7391b93eb12 fixed the bug",
            "request id 550e8400-e29b-41d4-a716-446655440000 failed",
            "the password policy requires rotation every 90 days",
            "see src/store/memory_persistence/integration_helpers.rs for details",
            "pwd = abc",
            "token = short",
            "use sklearn and scikit-learn for the model",
            "The quick brown fox jumps over the lazy dog repeatedly today",
        ];

        for input in corpus {
            let report = redact(input);
            assert_eq!(
                report.redacted_text, *input,
                "near-miss was redacted: {:?} -> {:?}",
                input, report.redacted_text
            );
        }
    }

    #[test]
    fn entropy_backstop_catches_unlabeled_random_tokens_only() {
        let secret = "kJ8vQz2xWp9nRt4mYb7cLd3fGh6sNa1e";
        let report = redact(&format!("found {secret} in the dump"));
        assert!(report.redacted_text.contains("[REDACTED:high_entropy]"));
        assert!(!report.redacted_text.contains(secret));

        let hexes = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        let report = redact(&format!("sha {hexes} stays"));
        assert!(report.redacted_text.contains(hexes));
    }

    #[test]
    fn split_line_secrets_are_each_caught() {
        let input = "PASSWORD=firsts3cret\nAPP_API_TOKEN=secondsecretvalue\n";
        let report = redact(input);
        assert!(!report.redacted_text.contains("firsts3cret"));
        assert!(!report.redacted_text.contains("secondsecretvalue"));
    }

    #[test]
    fn sensitive_paths_are_replaced_verbatim() {
        let report = Redactor::new(vec!["/home/user/private".to_string()])
            .redact("logs at /home/user/private/notes.txt");
        assert!(report
            .redacted_text
            .contains("[REDACTED:sensitive_path]/notes.txt"));
    }
}
