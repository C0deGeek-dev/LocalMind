//! Storage boundary for LocalMind.
//!
//! The MVP keeps durable memory Markdown-first and uses SQLite for queue, audit,
//! and index state. This crate will own that persistence behavior; subject 01
//! only establishes the dependency direction.

use localmind_core::{LearningAuditEvent, MemoryEntry, ReviewItem};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StoreCapabilities {
    pub markdown_memory: bool,
    pub review_queue: bool,
    pub audit_log: bool,
    pub search_index: bool,
}

impl StoreCapabilities {
    #[must_use]
    pub fn mvp() -> Self {
        Self {
            markdown_memory: true,
            review_queue: true,
            audit_log: true,
            search_index: true,
        }
    }
}

pub type StoreRecordSet = (MemoryEntry, ReviewItem, LearningAuditEvent);

#[cfg(test)]
mod tests {
    use super::StoreCapabilities;

    #[test]
    fn mvp_store_shape_keeps_memory_and_audit_separate() {
        let capabilities = StoreCapabilities::mvp();

        assert!(capabilities.markdown_memory);
        assert!(capabilities.audit_log);
    }
}
