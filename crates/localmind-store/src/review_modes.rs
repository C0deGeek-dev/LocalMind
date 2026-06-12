use crate::{
    MemoryPersistence, MemoryPersistenceError, ProjectConfig, ReviewModeConfig, ReviewQueue,
    ReviewQueueError,
};
use localmind_core::{AuditEventKind, Confidence, ReviewAction, ReviewAnnotation, ReviewDecision};
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewModeReport {
    pub annotated: usize,
    pub accepted: usize,
    pub manual: usize,
}

pub struct ReviewModeProcessor;

impl ReviewModeProcessor {
    pub fn apply_project(
        project_root: impl AsRef<std::path::Path>,
    ) -> Result<ReviewModeReport, ReviewModeError> {
        let config =
            ProjectConfig::discover(project_root.as_ref()).map_err(ReviewModeError::Config)?;
        let queue = ReviewQueue::open_project(&config.project_root)?;
        let persistence = MemoryPersistence::open_project(&config.project_root)?;
        let mut report = ReviewModeReport {
            annotated: 0,
            accepted: 0,
            manual: 0,
        };

        for mut item in queue.list()? {
            if !matches!(item.state, localmind_core::ReviewState::Pending) {
                continue;
            }
            let confidence = item.candidate.confidence.value();
            let duplicate = persistence
                .search(item.candidate.summary())?
                .first()
                .cloned();
            let conflict = item
                .candidate
                .summary()
                .to_ascii_lowercase()
                .contains("contradict");
            item.candidate.review_annotation = Some(ReviewAnnotation {
                score: Confidence::new(confidence)?,
                duplicate_of: duplicate.as_ref().map(|hit| hit.memory_id.to_string()),
                conflict,
                notes: if duplicate.is_some() {
                    "Similar accepted memory found; human review recommended.".to_string()
                } else {
                    "No close duplicate found in accepted memory.".to_string()
                },
            });

            match config.config.review.mode {
                ReviewModeConfig::Manual => {
                    report.manual += 1;
                }
                ReviewModeConfig::Assisted => {
                    queue.replace_candidate(&item.id, &item.candidate)?;
                    persistence.write_mode_audit("assisted", item.id.as_str(), false)?;
                    report.annotated += 1;
                }
                ReviewModeConfig::Trusted => {
                    queue.replace_candidate(&item.id, &item.candidate)?;
                    if !conflict
                        && duplicate.is_none()
                        && confidence >= config.config.review.trusted_threshold
                    {
                        let decided = queue.decide(ReviewDecision {
                            item_id: item.id.clone(),
                            action: ReviewAction::Accept,
                            reviewer: "localmind-trusted".to_string(),
                            decided_at: None,
                            note: Some("trusted mode auto-accepted above threshold".to_string()),
                            replacement_summary: None,
                            evidence: Vec::new(),
                        })?;
                        persistence.record_review_item_audit(&decided)?;
                        persistence.write_mode_audit("trusted", item.id.as_str(), true)?;
                        report.accepted += 1;
                    } else {
                        persistence.write_mode_audit("trusted", item.id.as_str(), false)?;
                        report.manual += 1;
                    }
                }
                ReviewModeConfig::Automatic => {
                    queue.replace_candidate(&item.id, &item.candidate)?;
                    if !conflict && duplicate.is_none() {
                        let decided = queue.decide(ReviewDecision {
                            item_id: item.id.clone(),
                            action: ReviewAction::Accept,
                            reviewer: "localmind-automatic".to_string(),
                            decided_at: None,
                            note: Some("automatic mode auto-accepted".to_string()),
                            replacement_summary: None,
                            evidence: Vec::new(),
                        })?;
                        persistence.record_review_item_audit(&decided)?;
                        persistence.write_mode_audit("automatic", item.id.as_str(), true)?;
                        report.accepted += 1;
                    } else {
                        persistence.write_mode_audit("automatic", item.id.as_str(), false)?;
                        report.manual += 1;
                    }
                }
            }
        }

        Ok(report)
    }
}

trait ReviewModeAudit {
    fn write_mode_audit(
        &self,
        mode: &str,
        item_id: &str,
        auto_accepted: bool,
    ) -> Result<(), MemoryPersistenceError>;
}

impl ReviewModeAudit for MemoryPersistence {
    fn write_mode_audit(
        &self,
        mode: &str,
        item_id: &str,
        auto_accepted: bool,
    ) -> Result<(), MemoryPersistenceError> {
        self.record_custom_audit(
            AuditEventKind::ReviewModeApplied,
            "localmind",
            item_id,
            &serde_json::json!({ "mode": mode, "auto_accepted": auto_accepted }),
        )
    }
}

#[derive(Debug, Error)]
pub enum ReviewModeError {
    #[error(transparent)]
    Config(#[from] crate::StoreConfigError),
    #[error(transparent)]
    Queue(#[from] ReviewQueueError),
    #[error(transparent)]
    Persistence(#[from] MemoryPersistenceError),
    #[error(transparent)]
    Contract(#[from] localmind_core::ContractError),
}
