//! Review queue workflow boundary.

use localmind_core::{ReviewAction, ReviewDecision, ReviewState};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewCapabilities {
    pub accept: bool,
    pub reject: bool,
    pub edit: bool,
    pub merge: bool,
    pub convert_to_skill: bool,
}

impl ReviewCapabilities {
    #[must_use]
    pub fn manual_mvp() -> Self {
        Self {
            accept: true,
            reject: true,
            edit: true,
            merge: true,
            convert_to_skill: true,
        }
    }
}

pub fn decision_closes_item(action: &ReviewAction) -> bool {
    matches!(
        action,
        ReviewAction::Accept
            | ReviewAction::Reject
            | ReviewAction::Edit
            | ReviewAction::MergeInto(_)
            | ReviewAction::ConvertToSkill
            | ReviewAction::Supersede(_)
    )
}

pub fn state_after_decision(decision: &ReviewDecision) -> ReviewState {
    match &decision.action {
        ReviewAction::Accept => ReviewState::Accepted,
        ReviewAction::Reject | ReviewAction::IgnoreSimilar => ReviewState::Rejected,
        ReviewAction::Edit => ReviewState::Edited,
        ReviewAction::MergeInto(_) => ReviewState::Merged,
        ReviewAction::MarkTemporary => ReviewState::Deferred,
        ReviewAction::ConvertToSkill => ReviewState::Accepted,
        ReviewAction::Supersede(_) => ReviewState::Accepted,
    }
}

#[cfg(test)]
mod tests {
    use super::{decision_closes_item, ReviewCapabilities};
    use localmind_core::ReviewAction;

    #[test]
    fn manual_mvp_supports_edit_before_promotion() {
        let capabilities = ReviewCapabilities::manual_mvp();

        assert!(capabilities.edit);
        assert!(decision_closes_item(&ReviewAction::Accept));
    }
}
