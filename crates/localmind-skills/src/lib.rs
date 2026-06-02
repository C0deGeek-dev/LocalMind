//! Skill draft generation and maintenance boundary.

use localmind_core::SkillDraft;

#[must_use]
pub fn draft_is_disabled(draft: &SkillDraft) -> bool {
    draft.disabled
}

#[cfg(test)]
mod tests {
    use super::draft_is_disabled;
    use localmind_core::{MemoryEntryId, SkillDraft, SkillDraftId};

    #[test]
    fn generated_skill_drafts_start_disabled() {
        let draft = SkillDraft {
            id: SkillDraftId::new("skill-1"),
            name: "review-closeout".to_string(),
            description: "Review completed work before promoting lessons.".to_string(),
            trigger_conditions: vec!["After a coding session closes".to_string()],
            workflow_steps: vec!["Inspect extracted lessons".to_string()],
            constraints: vec!["Do not install automatically".to_string()],
            verification_steps: vec!["Confirm tests pass".to_string()],
            related_memories: vec![MemoryEntryId::new("memory-1")],
            source_agents: vec!["generic".to_string()],
            last_reviewed_at: Some("2026-06-03T00:00:00Z".to_string()),
            body_markdown: "# Review Closeout\n".to_string(),
            disabled: true,
            cooldown_key: Some("review-closeout".to_string()),
            evidence: Vec::new(),
        };

        assert!(draft_is_disabled(&draft));
    }
}
