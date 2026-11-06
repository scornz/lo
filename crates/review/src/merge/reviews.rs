//! What reviews a pull request still needs (design §9, "Merge status").
//!
//! GitHub's `reviewDecision` says whether reviews block the merge. The reviews and review requests
//! say why: who requested changes, how many approvals are missing, and which code owners haven't
//! approved.

use super::{MergeSnapshot, Review, ReviewState, Reviewer};

/// Approvals the base branch's rules require and the pull request doesn't have yet.
///
/// GitHub decides whether approvals are missing. With `more` at 0 and no `code_owners`, GitHub
/// requires a review rozed can't attribute, such as an approval of the latest push by someone other
/// than its pusher, or a code owner whose request was removed; the merge panel says a review is
/// required.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MissingApprovals {
    /// Approvals still needed to reach the required count. Only approvals from reviewers with
    /// write access count.
    pub more: u32,
    /// Code owners asked to review who haven't approved, when the rules require code-owner review.
    /// One owner's approval covers a file, and GitHub keeps the other owners' requests, so this can
    /// name more owners than need to approve.
    pub code_owners: Vec<Reviewer>,
}

impl MissingApprovals {
    /// Whether the reviews and requests show anything missing.
    pub fn is_empty(&self) -> bool {
        self.more == 0 && self.code_owners.is_empty()
    }
}

/// Each reviewer's latest review, in the order reviewers first appear.
pub(super) fn latest(snapshot: &MergeSnapshot) -> Vec<&Review> {
    let mut latest: Vec<&Review> = Vec::new();
    for review in &snapshot.reviews {
        match latest
            .iter_mut()
            .find(|earlier| same_login(&earlier.author, &review.author))
        {
            Some(earlier) => *earlier = review,
            None => latest.push(review),
        }
    }
    latest
}

/// Reviewers with write access whose latest review requests changes.
pub(super) fn changes_requested_by(snapshot: &MergeSnapshot) -> Vec<String> {
    latest(snapshot)
        .into_iter()
        .filter(|review| review.counts && review.state == ReviewState::ChangesRequested)
        .map(|review| review.author.clone())
        .collect()
}

/// The approvals missing, as far as the reviews and requests show: the required count less the
/// approvals from reviewers with write access, and, when the rules require code-owner review, the
/// code owners asked to review that no such approval covers. A user's approval covers their own
/// request; an approval on behalf of a team covers the team's.
pub(super) fn missing_approvals(snapshot: &MergeSnapshot) -> MissingApprovals {
    let approvals: Vec<&Review> = latest(snapshot)
        .into_iter()
        .filter(|review| review.counts && review.state == ReviewState::Approved)
        .collect();
    let approved = u32::try_from(approvals.len()).unwrap_or(u32::MAX);
    let code_owners = if snapshot.rules.code_owner_review {
        (snapshot.review_requests.iter())
            .filter(|request| request.as_code_owner)
            .map(|request| &request.reviewer)
            .filter(|reviewer| !approvals.iter().any(|review| covers(review, reviewer)))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    MissingApprovals {
        more: snapshot.rules.required_approvals.saturating_sub(approved),
        code_owners,
    }
}

/// Whether an approval meets a review request.
fn covers(review: &Review, reviewer: &Reviewer) -> bool {
    match reviewer {
        Reviewer::User(login) => same_login(&review.author, login),
        Reviewer::Team(slug) => (review.on_behalf_of.iter()).any(|team| same_login(team, slug)),
    }
}

/// Whether two GitHub logins or team slugs name the same account. GitHub ignores their case.
pub(super) fn same_login(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

#[cfg(test)]
mod tests {
    use super::super::{
        Blocker, MergeStateStatus, MergeableState, ReviewDecision, ReviewRequest, ReviewRules,
        blockers,
    };
    use super::*;

    fn snapshot(decision: ReviewDecision) -> MergeSnapshot {
        MergeSnapshot {
            mergeable: MergeableState::Mergeable,
            merge_state: MergeStateStatus::Blocked,
            review_decision: Some(decision),
            ..Default::default()
        }
    }

    fn review(author: &str, state: ReviewState) -> Review {
        Review {
            author: author.to_string(),
            state,
            commit: Some("9f2c4e1".to_string()),
            counts: true,
            on_behalf_of: Vec::new(),
        }
    }

    fn without_write_access(review: Review) -> Review {
        Review {
            counts: false,
            ..review
        }
    }

    fn code_owner(reviewer: Reviewer) -> ReviewRequest {
        ReviewRequest {
            reviewer,
            as_code_owner: true,
        }
    }

    fn user(login: &str) -> Reviewer {
        Reviewer::User(login.to_string())
    }

    fn team(slug: &str) -> Reviewer {
        Reviewer::Team(slug.to_string())
    }

    fn requiring(approvals: u32) -> ReviewRules {
        ReviewRules {
            required_approvals: approvals,
            ..Default::default()
        }
    }

    fn requiring_code_owners(approvals: u32) -> ReviewRules {
        ReviewRules {
            code_owner_review: true,
            ..requiring(approvals)
        }
    }

    fn changes_requested(by: &[&str]) -> Blocker {
        Blocker::ChangesRequested {
            by: by.iter().map(|login| login.to_string()).collect(),
        }
    }

    fn approvals(more: u32, code_owners: Vec<Reviewer>) -> Blocker {
        Blocker::Approvals(MissingApprovals { more, code_owners })
    }

    #[test]
    fn requested_changes_block_it_and_name_who_requested_them() {
        let snapshot = MergeSnapshot {
            reviews: vec![
                review("alice", ReviewState::ChangesRequested),
                review("bob", ReviewState::Approved),
                without_write_access(review("carol", ReviewState::ChangesRequested)),
                review("dave", ReviewState::Dismissed),
                review("erin", ReviewState::ChangesRequested),
            ],
            rules: requiring(1),
            ..snapshot(ReviewDecision::ChangesRequested)
        };

        assert_eq!(blockers(&snapshot), [changes_requested(&["alice", "erin"])]);
    }

    #[test]
    fn requested_changes_do_not_block_it_when_the_rules_do_not_require_reviews() {
        let snapshot = MergeSnapshot {
            merge_state: MergeStateStatus::Clean,
            review_decision: None,
            reviews: vec![review("alice", ReviewState::ChangesRequested)],
            ..snapshot(ReviewDecision::ChangesRequested)
        };

        assert_eq!(blockers(&snapshot), []);
    }

    #[test]
    fn a_reviewers_later_review_replaces_their_earlier_one() {
        let snapshot = MergeSnapshot {
            reviews: vec![
                review("alice", ReviewState::ChangesRequested),
                review("bob", ReviewState::ChangesRequested),
                review("alice", ReviewState::Approved),
            ],
            rules: requiring(2),
            ..snapshot(ReviewDecision::ChangesRequested)
        };

        assert_eq!(
            blockers(&snapshot),
            [changes_requested(&["bob"]), approvals(1, vec![])]
        );
    }

    #[test]
    fn missing_approvals_say_how_many_more_are_needed() {
        let snapshot = MergeSnapshot {
            reviews: vec![
                review("alice", ReviewState::Approved),
                without_write_access(review("carol", ReviewState::Approved)),
            ],
            rules: requiring(3),
            ..snapshot(ReviewDecision::ReviewRequired)
        };

        assert_eq!(blockers(&snapshot), [approvals(2, vec![])]);
    }

    #[test]
    fn missing_approvals_name_code_owners_who_have_not_approved() {
        let snapshot = MergeSnapshot {
            reviews: vec![review("alice", ReviewState::Approved)],
            review_requests: vec![
                code_owner(user("erin")),
                ReviewRequest {
                    reviewer: user("frank"),
                    as_code_owner: false,
                },
                code_owner(team("rozed-dev/github")),
            ],
            rules: requiring_code_owners(1),
            ..snapshot(ReviewDecision::ReviewRequired)
        };

        assert_eq!(
            blockers(&snapshot),
            [approvals(0, vec![user("erin"), team("rozed-dev/github")])]
        );
    }

    #[test]
    fn a_code_owner_who_approved_is_not_missing() {
        let snapshot = MergeSnapshot {
            reviews: vec![review("Erin", ReviewState::Approved)],
            review_requests: vec![code_owner(user("erin")), code_owner(user("grace"))],
            rules: requiring_code_owners(1),
            ..snapshot(ReviewDecision::ReviewRequired)
        };

        assert_eq!(blockers(&snapshot), [approvals(0, vec![user("grace")])]);
    }

    #[test]
    fn an_approval_on_behalf_of_a_team_covers_its_request() {
        let snapshot = MergeSnapshot {
            reviews: vec![Review {
                on_behalf_of: vec!["rozed-dev/github".to_string()],
                ..review("heidi", ReviewState::Approved)
            }],
            review_requests: vec![
                code_owner(team("rozed-dev/github")),
                code_owner(team("rozed-dev/ui")),
            ],
            rules: requiring_code_owners(1),
            ..snapshot(ReviewDecision::ReviewRequired)
        };

        assert_eq!(
            blockers(&snapshot),
            [approvals(0, vec![team("rozed-dev/ui")])]
        );
    }

    #[test]
    fn an_approval_without_write_access_covers_no_code_owner() {
        let snapshot = MergeSnapshot {
            reviews: vec![without_write_access(review("erin", ReviewState::Approved))],
            review_requests: vec![code_owner(user("erin"))],
            rules: requiring_code_owners(1),
            ..snapshot(ReviewDecision::ReviewRequired)
        };

        assert_eq!(blockers(&snapshot), [approvals(1, vec![user("erin")])]);
    }

    #[test]
    fn code_owners_are_not_missing_when_the_rules_do_not_require_their_review() {
        let snapshot = MergeSnapshot {
            reviews: vec![review("alice", ReviewState::Approved)],
            review_requests: vec![code_owner(user("erin"))],
            rules: requiring(2),
            ..snapshot(ReviewDecision::ReviewRequired)
        };

        assert_eq!(blockers(&snapshot), [approvals(1, vec![])]);
    }

    #[test]
    fn a_review_github_requires_for_a_reason_rozed_cannot_see_is_still_missing() {
        let snapshot = MergeSnapshot {
            reviews: vec![review("alice", ReviewState::Approved)],
            rules: requiring_code_owners(1),
            ..snapshot(ReviewDecision::ReviewRequired)
        };

        assert_eq!(blockers(&snapshot), [approvals(0, vec![])]);
    }

    #[test]
    fn github_approving_wins_over_the_reviews() {
        let snapshot = MergeSnapshot {
            merge_state: MergeStateStatus::Clean,
            reviews: vec![review("alice", ReviewState::Approved)],
            review_requests: vec![code_owner(user("erin"))],
            rules: requiring_code_owners(2),
            ..snapshot(ReviewDecision::Approved)
        };

        assert_eq!(blockers(&snapshot), []);
    }

    #[test]
    fn requested_changes_list_missing_approvals_only_when_some_are() {
        let snapshot = MergeSnapshot {
            reviews: vec![
                review("alice", ReviewState::ChangesRequested),
                review("bob", ReviewState::Approved),
            ],
            rules: requiring(1),
            ..snapshot(ReviewDecision::ChangesRequested)
        };

        assert_eq!(blockers(&snapshot), [changes_requested(&["alice"])]);
    }

    #[test]
    fn review_blockers_come_in_their_groups() {
        let snapshot = MergeSnapshot {
            draft: true,
            reviews: vec![review("alice", ReviewState::ChangesRequested)],
            rules: requiring(1),
            ..snapshot(ReviewDecision::ChangesRequested)
        };

        assert_eq!(
            blockers(&snapshot),
            [
                Blocker::Draft,
                changes_requested(&["alice"]),
                approvals(1, vec![]),
            ]
        );
    }

    #[test]
    fn missing_approvals_are_a_wait_and_requested_changes_are_not() {
        assert!(approvals(1, vec![user("erin")]).waiting());
        assert!(!changes_requested(&["alice"]).waiting());
    }
}
