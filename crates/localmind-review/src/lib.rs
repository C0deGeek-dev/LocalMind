//! Review queue workflow boundary.

use localmind_core::{ReviewAction, ReviewDecision, ReviewState};

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
    use super::{decision_closes_item, state_after_decision};
    use localmind_core::{ReviewAction, ReviewDecision, ReviewItemId, ReviewState};

    fn decision(action: ReviewAction) -> ReviewDecision {
        ReviewDecision {
            item_id: ReviewItemId::new("x"),
            action,
            reviewer: String::new(),
            decided_at: None,
            note: None,
            replacement_summary: None,
            evidence: Vec::new(),
        }
    }

    #[test]
    fn a_terminal_action_closes_the_item_but_deferral_does_not() {
        assert!(decision_closes_item(&ReviewAction::Accept));
        assert!(decision_closes_item(&ReviewAction::ConvertToSkill));
        assert!(!decision_closes_item(&ReviewAction::MarkTemporary));
        assert!(!decision_closes_item(&ReviewAction::IgnoreSimilar));
    }

    #[test]
    fn state_after_decision_is_the_single_source_the_queue_uses() {
        assert_eq!(
            state_after_decision(&decision(ReviewAction::Accept)),
            ReviewState::Accepted
        );
        assert_eq!(
            state_after_decision(&decision(ReviewAction::MarkTemporary)),
            ReviewState::Deferred
        );
        assert_eq!(
            state_after_decision(&decision(ReviewAction::Reject)),
            ReviewState::Rejected
        );
    }
}
