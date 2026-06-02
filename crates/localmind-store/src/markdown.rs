use localmind_core::{EvidenceRef, MemoryEntry, MemoryEntryId};
use time::format_description::well_known::Rfc3339;

pub struct MarkdownMemoryFormat;

impl MarkdownMemoryFormat {
    #[must_use]
    pub fn serialize(entry: &MemoryEntry) -> String {
        let mut output = String::new();

        output.push_str("---\n");
        output.push_str(&format!("id: {}\n", entry.id));
        output.push_str(&format!("scope: {:?}\n", entry.scope));
        output.push_str(&format!("category: {:?}\n", entry.category));
        output.push_str(&format!("confidence: {:.3}\n", entry.confidence.value()));
        push_optional_id(&mut output, "source_session", entry.source_session.as_ref());
        push_optional_time(&mut output, "created_at", entry.created_at);
        push_optional_time(&mut output, "updated_at", entry.updated_at);
        push_string_list(&mut output, "tags", &entry.tags);
        push_string_list(&mut output, "related_files", &entry.related_files);
        push_string_list(&mut output, "related_entities", &entry.related_entities);
        push_id_list(&mut output, "supersedes", &entry.supersedes);
        push_id_list(&mut output, "contradicts", &entry.contradicts);
        push_evidence(&mut output, &entry.evidence);
        output.push_str("---\n\n");
        output.push_str(entry.body.trim());
        output.push('\n');

        output
    }
}

fn push_optional_id<T: std::fmt::Display>(output: &mut String, key: &str, value: Option<&T>) {
    if let Some(value) = value {
        output.push_str(&format!("{key}: {value}\n"));
    }
}

fn push_optional_time(output: &mut String, key: &str, value: Option<time::OffsetDateTime>) {
    if let Some(value) = value {
        let formatted = value.format(&Rfc3339).unwrap_or_else(|_| value.to_string());
        output.push_str(&format!("{key}: {formatted}\n"));
    }
}

fn push_string_list(output: &mut String, key: &str, values: &[String]) {
    if values.is_empty() {
        return;
    }

    output.push_str(&format!("{key}:\n"));
    for value in values {
        output.push_str(&format!("  - {}\n", escape_yaml_scalar(value)));
    }
}

fn push_id_list(output: &mut String, key: &str, values: &[MemoryEntryId]) {
    if values.is_empty() {
        return;
    }

    output.push_str(&format!("{key}:\n"));
    for value in values {
        output.push_str(&format!("  - {value}\n"));
    }
}

fn push_evidence(output: &mut String, evidence: &[EvidenceRef]) {
    if evidence.is_empty() {
        return;
    }

    output.push_str("evidence:\n");
    for item in evidence {
        output.push_str(&format!("  - id: {}\n", item.id));
        output.push_str(&format!("    kind: {:?}\n", item.kind));
        output.push_str(&format!("    label: {}\n", escape_yaml_scalar(&item.label)));
        output.push_str(&format!("    redacted: {}\n", item.redacted));
        if let Some(uri) = &item.uri {
            output.push_str(&format!("    uri: {}\n", escape_yaml_scalar(uri)));
        }
    }
}

fn escape_yaml_scalar(value: &str) -> String {
    if value
        .chars()
        .any(|character| matches!(character, ':' | '#' | '\n' | '\'' | '"'))
    {
        format!("'{escaped}'", escaped = value.replace('\'', "''"))
    } else {
        value.to_string()
    }
}
