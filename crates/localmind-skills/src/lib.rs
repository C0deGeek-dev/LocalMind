//! Skill draft generation and maintenance boundary.

use localmind_core::SkillDraft;

#[must_use]
pub fn draft_is_disabled(draft: &SkillDraft) -> bool {
    draft.disabled
}

#[cfg(test)]
mod tests {
    use super::draft_is_disabled;
    use localmind_core::{SkillDraft, SkillDraftId};

    #[test]
    fn generated_skill_drafts_start_disabled() {
        let draft = SkillDraft {
            id: SkillDraftId::new("skill-1"),
            name: "review-closeout".to_string(),
            description: "Review completed work before promoting lessons.".to_string(),
            body_markdown: "# Review Closeout\n".to_string(),
            disabled: true,
            cooldown_key: Some("review-closeout".to_string()),
            evidence: Vec::new(),
        };

        assert!(draft_is_disabled(&draft));
    }
}
