use crate::{ProjectConfig, Redaction, Redactor, StoreConfigError};
use localmind_core::{
    EvidenceKind, EvidenceRef, SessionId, SessionOutcome, SessionRecord, SessionSource,
};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum TranscriptImportFormat {
    PlainText,
    JsonLines,
    Markdown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ImportedSession {
    pub session: SessionRecord,
    pub format: TranscriptImportFormat,
    pub redactions: Vec<Redaction>,
    pub redacted_transcript_path: PathBuf,
    pub metadata_path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ImportReport {
    pub session_id: SessionId,
    pub source: SessionSource,
    pub format: TranscriptImportFormat,
    pub redactions: Vec<Redaction>,
    pub redacted_transcript_path: PathBuf,
    pub metadata_path: PathBuf,
}

pub struct TranscriptImporter;

impl TranscriptImporter {
    pub fn import_file(
        project_root: impl AsRef<Path>,
        transcript_path: impl AsRef<Path>,
        source: SessionSource,
        format: TranscriptImportFormat,
    ) -> Result<ImportReport, ImportError> {
        let config = ProjectConfig::discover(project_root).map_err(ImportError::Config)?;
        let input_path = transcript_path.as_ref().to_path_buf();
        let raw_text =
            fs::read_to_string(&input_path).map_err(|source| ImportError::ReadTranscript {
                path: input_path.clone(),
                source,
            })?;

        Self::import_text(&config, &raw_text, source, format)
    }

    pub fn import_text(
        config: &ProjectConfig,
        raw_text: &str,
        source: SessionSource,
        format: TranscriptImportFormat,
    ) -> Result<ImportReport, ImportError> {
        let redactor = Redactor::new(config.config.learning.excluded_paths.clone());
        let report = redactor.redact(raw_text);
        let session_id = SessionId::new(generate_session_id(&source, &report.redacted_text));
        let session_dir = config
            .project_root
            .join(".localmind")
            .join("sessions")
            .join(session_id.as_str());
        fs::create_dir_all(&session_dir).map_err(|source| ImportError::CreateSessionDirectory {
            path: session_dir.clone(),
            source,
        })?;

        let redacted_transcript_path = session_dir.join("transcript.redacted.txt");
        fs::write(&redacted_transcript_path, &report.redacted_text).map_err(|source| {
            ImportError::WriteTranscript {
                path: redacted_transcript_path.clone(),
                source,
            }
        })?;

        let evidence = EvidenceRef::new(EvidenceKind::Transcript, "redacted transcript").redacted();
        let mut session =
            SessionRecord::new(session_id.clone(), source.clone(), SessionOutcome::Unknown);
        session.transcript = Some(evidence.clone());
        session.evidence.push(evidence);
        session
            .metadata
            .insert("import_format".to_string(), format!("{format:?}"));
        session.metadata.insert(
            "redacted_transcript_path".to_string(),
            redacted_transcript_path.to_string_lossy().to_string(),
        );

        let metadata_path = session_dir.join("metadata.json");
        let imported = ImportedSession {
            session: session.clone(),
            format,
            redactions: report.redactions.clone(),
            redacted_transcript_path: redacted_transcript_path.clone(),
            metadata_path: metadata_path.clone(),
        };
        let metadata_json = serde_json::to_string_pretty(&imported)
            .map_err(|source| ImportError::SerializeMetadata { source })?;
        fs::write(&metadata_path, metadata_json).map_err(|source| ImportError::WriteMetadata {
            path: metadata_path.clone(),
            source,
        })?;

        Ok(ImportReport {
            session_id,
            source,
            format: imported.format,
            redactions: imported.redactions,
            redacted_transcript_path,
            metadata_path,
        })
    }
}

fn generate_session_id(source: &SessionSource, redacted_text: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in format!("{source:?}\n{redacted_text}").as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }

    format!("session-{hash:016x}")
}

#[derive(Debug, Error)]
pub enum ImportError {
    #[error(transparent)]
    Config(#[from] StoreConfigError),
    #[error("failed to read transcript {path:?}: {source}")]
    ReadTranscript {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to create session directory {path:?}: {source}")]
    CreateSessionDirectory {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to write redacted transcript {path:?}: {source}")]
    WriteTranscript {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to serialize import metadata: {source}")]
    SerializeMetadata { source: serde_json::Error },
    #[error("failed to write import metadata {path:?}: {source}")]
    WriteMetadata {
        path: PathBuf,
        source: std::io::Error,
    },
}
