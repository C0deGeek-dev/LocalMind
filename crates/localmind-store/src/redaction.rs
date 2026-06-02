use regex::Regex;
use serde::Serialize;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Redaction {
    pub kind: String,
    pub replacements: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RedactionReport {
    pub redacted_text: String,
    pub redactions: Vec<Redaction>,
}

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

        apply_regex(
            &mut text,
            &mut redactions,
            "private_key",
            r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----",
        );
        apply_regex(
            &mut text,
            &mut redactions,
            "openai_api_key",
            r"\bsk-[A-Za-z0-9][A-Za-z0-9_-]{16,}\b",
        );
        apply_regex(
            &mut text,
            &mut redactions,
            "github_token",
            r"\bgh[pousr]_[A-Za-z0-9_]{20,}\b",
        );
        apply_regex(
            &mut text,
            &mut redactions,
            "bearer_token",
            r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]{16,}",
        );
        apply_regex(
            &mut text,
            &mut redactions,
            "password_assignment",
            r#"(?i)\b(password|passwd|pwd)\s*[:=]\s*['"]?[^'"\s;\[]{4,}"#,
        );
        apply_regex(
            &mut text,
            &mut redactions,
            "token_assignment",
            r#"(?i)\b(api[_-]?key|token|secret)\s*[:=]\s*['"]?[^'"\s;\[]{16,}"#,
        );
        apply_regex(
            &mut text,
            &mut redactions,
            "connection_string_password",
            r"(?i)(password|pwd)=([^;\s]+)",
        );

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

        RedactionReport {
            redacted_text: text,
            redactions,
        }
    }
}

fn apply_regex(text: &mut String, redactions: &mut Vec<Redaction>, kind: &str, pattern: &str) {
    let Ok(regex) = Regex::new(pattern) else {
        return;
    };
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

#[cfg(test)]
mod tests {
    use super::Redactor;

    #[test]
    fn specific_token_rules_do_not_get_overwritten_by_generic_assignments() {
        let report =
            Redactor::new(Vec::new()).redact("token = sk-proj-abcdefghijklmnopqrstuvwxyz123456");

        assert!(report.redacted_text.contains("[REDACTED:openai_api_key]"));
        assert!(!report.redacted_text.contains("sk-proj"));
    }
}
