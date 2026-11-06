//! Why a pull request can't merge (design §9, "Merge status").

use super::reviews::{self, MissingApprovals};
use super::{
    CheckState, MergeSnapshot, MergeStateStatus, MergeableState, PullRequestState, RequiredCheck,
    ReviewDecision, UnmetRule,
};

/// A reason a pull request can't merge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Blocker {
    /// It's merged.
    Merged,
    /// It's closed.
    Closed,
    /// It's a draft, until its author marks it ready for review.
    Draft,
    /// It conflicts with the base branch.
    Conflicts,
    /// The base branch's rules require it to be up to date with the base, and it isn't. rozed
    /// doesn't offer GitHub's "Update branch", which adds a commit: the author updates it.
    OutOfDate,
    /// Reviewers with write access whose latest review requests changes, by login. Each one's
    /// approval, or the review's dismissal, clears theirs.
    ChangesRequested { by: Vec<String> },
    /// Approvals the base branch's rules require and it doesn't have yet.
    Approvals(MissingApprovals),
    /// A required check that hasn't passed: it failed, is running, or hasn't reported. Never
    /// [`CheckState::Passed`].
    Check(RequiredCheck),
    /// Conversations still to resolve, when the base branch's rules require every one resolved.
    Conversations { unresolved: u32 },
    /// Another rule on the base branch.
    Rule(UnmetRule),
    /// GitHub is still working out whether it merges cleanly.
    CheckingMergeability,
    /// GitHub says it's blocked, and nothing in the snapshot explains why. The merge panel says so
    /// and links to the pull request on GitHub.
    Unexplained { url: String },
}

impl Blocker {
    /// Whether waiting clears it without anyone here acting: others' approvals, a required check
    /// that's running or hasn't reported, GitHub working out mergeability, or a rule that says
    /// waiting meets it.
    pub fn waiting(&self) -> bool {
        match self {
            Self::Approvals(_) => true,
            Self::Check(check) => matches!(check.state, CheckState::Running | CheckState::Missing),
            Self::Rule(rule) => rule.waiting,
            Self::CheckingMergeability => true,
            Self::Merged
            | Self::Closed
            | Self::Draft
            | Self::Conflicts
            | Self::OutOfDate
            | Self::ChangesRequested { .. }
            | Self::Conversations { .. }
            | Self::Unexplained { .. } => false,
        }
    }
}

/// Every reason the pull request can't merge, or join its base branch's merge queue, now. Empty
/// when GitHub's rules let it; whether you may is [`merge_action`](super::merge_action)'s concern.
///
/// Blockers waiting won't fix come first, then those it will ([`Blocker::waiting`]), each kind in
/// the order of [`Blocker`]'s variants. Checks keep the snapshot's order, with running checks
/// before those that haven't reported.
///
/// - A merged or closed pull request has only [`Blocker::Merged`] or [`Blocker::Closed`].
/// - A draft is `isDraft`, or `mergeStateStatus` `DRAFT`, which REST still reports.
/// - It conflicts when `mergeable` is `CONFLICTING` or `mergeStateStatus` is `DIRTY`.
/// - It's out of date when `mergeStateStatus` is `BEHIND`, or the caller says so.
/// - Reviews block it only when `reviewDecision` says so. With `CHANGES_REQUESTED`, the
///   requesters are listed, and the missing approvals when any are; with `REVIEW_REQUIRED`, the
///   missing approvals always are.
/// - GitHub is still working out mergeability when `mergeable` or `mergeStateStatus` is `UNKNOWN`.
/// - When `mergeStateStatus` is `BLOCKED` and nothing else is listed but GitHub working out
///   mergeability, the block is [`Blocker::Unexplained`].
pub fn blockers(snapshot: &MergeSnapshot) -> Vec<Blocker> {
    match snapshot.state {
        PullRequestState::Merged => return vec![Blocker::Merged],
        PullRequestState::Closed => return vec![Blocker::Closed],
        PullRequestState::Open => {}
    }
    let merge_state = snapshot.merge_state;
    let checks = |state: CheckState| {
        (snapshot.required_checks.iter())
            .filter(move |check| check.state == state)
            .map(|check| Blocker::Check(check.clone()))
    };
    let rules = |waiting: bool| {
        (snapshot.unmet_rules.iter())
            .filter(move |rule| rule.waiting == waiting)
            .map(|rule| Blocker::Rule(rule.clone()))
    };

    let mut blockers = Vec::new();
    if snapshot.draft || merge_state == MergeStateStatus::Draft {
        blockers.push(Blocker::Draft);
    }
    if snapshot.mergeable == MergeableState::Conflicting || merge_state == MergeStateStatus::Dirty {
        blockers.push(Blocker::Conflicts);
    }
    if merge_state == MergeStateStatus::Behind || (snapshot.out_of_date && !snapshot.queue.required)
    {
        blockers.push(Blocker::OutOfDate);
    }
    let decision = snapshot.review_decision;
    if decision == Some(ReviewDecision::ChangesRequested) {
        blockers.push(Blocker::ChangesRequested {
            by: reviews::changes_requested_by(snapshot),
        });
    }
    blockers.extend(checks(CheckState::Failed));
    if snapshot.rules.conversation_resolution && snapshot.unresolved_threads > 0 {
        blockers.push(Blocker::Conversations {
            unresolved: snapshot.unresolved_threads,
        });
    }
    blockers.extend(rules(false));
    let missing = reviews::missing_approvals(snapshot);
    let approvals_missing = match decision {
        Some(ReviewDecision::ReviewRequired) => true,
        Some(ReviewDecision::ChangesRequested) => !missing.is_empty(),
        Some(ReviewDecision::Approved) | None => false,
    };
    if approvals_missing {
        blockers.push(Blocker::Approvals(missing));
    }
    blockers.extend(checks(CheckState::Running));
    blockers.extend(checks(CheckState::Missing));
    blockers.extend(rules(true));

    if merge_state == MergeStateStatus::Blocked && blockers.is_empty() {
        blockers.push(Blocker::Unexplained {
            url: snapshot.url.clone(),
        });
    }
    if snapshot.mergeable == MergeableState::Unknown || merge_state == MergeStateStatus::Unknown {
        blockers.push(Blocker::CheckingMergeability);
    }
    blockers
}

#[cfg(test)]
mod tests {
    use super::super::{MergeQueue, ReviewRules};
    use super::*;

    const URL: &str = "https://github.com/rozed-dev/playground/pull/7";

    fn ready() -> MergeSnapshot {
        MergeSnapshot {
            url: URL.to_string(),
            mergeable: MergeableState::Mergeable,
            merge_state: MergeStateStatus::Clean,
            ..Default::default()
        }
    }

    fn blocked() -> MergeSnapshot {
        MergeSnapshot {
            merge_state: MergeStateStatus::Blocked,
            ..ready()
        }
    }

    fn check(name: &str, state: CheckState) -> RequiredCheck {
        RequiredCheck {
            name: name.to_string(),
            state,
            url: (state != CheckState::Missing)
                .then(|| format!("https://github.com/rozed-dev/playground/runs/{name}")),
        }
    }

    fn rule(description: &str, waiting: bool) -> UnmetRule {
        UnmetRule {
            description: description.to_string(),
            url: None,
            waiting,
        }
    }

    #[test]
    fn a_pull_request_that_can_merge_has_no_blockers() {
        for merge_state in [
            MergeStateStatus::Clean,
            MergeStateStatus::HasHooks,
            MergeStateStatus::Unstable,
        ] {
            let snapshot = MergeSnapshot {
                merge_state,
                ..ready()
            };

            assert_eq!(blockers(&snapshot), [], "{merge_state:?}");
        }
    }

    #[test]
    fn a_merged_pull_request_is_blocked_only_by_being_merged() {
        let snapshot = MergeSnapshot {
            state: PullRequestState::Merged,
            mergeable: MergeableState::Unknown,
            merge_state: MergeStateStatus::Unknown,
            ..ready()
        };

        assert_eq!(blockers(&snapshot), [Blocker::Merged]);
    }

    #[test]
    fn a_closed_pull_request_is_blocked_only_by_being_closed() {
        let snapshot = MergeSnapshot {
            state: PullRequestState::Closed,
            draft: true,
            mergeable: MergeableState::Conflicting,
            ..ready()
        };

        assert_eq!(blockers(&snapshot), [Blocker::Closed]);
    }

    #[test]
    fn a_draft_is_blocked_until_it_is_marked_ready() {
        let snapshot = MergeSnapshot {
            draft: true,
            ..blocked()
        };

        assert_eq!(blockers(&snapshot), [Blocker::Draft]);
    }

    #[test]
    fn the_draft_merge_state_rest_reports_is_a_draft() {
        let snapshot = MergeSnapshot {
            merge_state: MergeStateStatus::Draft,
            ..ready()
        };

        assert_eq!(blockers(&snapshot), [Blocker::Draft]);
    }

    #[test]
    fn a_pull_request_github_says_conflicts_is_blocked_by_conflicts() {
        let conflicting = MergeSnapshot {
            mergeable: MergeableState::Conflicting,
            ..ready()
        };
        let dirty = MergeSnapshot {
            merge_state: MergeStateStatus::Dirty,
            ..ready()
        };

        assert_eq!(blockers(&conflicting), [Blocker::Conflicts]);
        assert_eq!(blockers(&dirty), [Blocker::Conflicts]);
    }

    #[test]
    fn a_pull_request_github_says_is_behind_is_out_of_date() {
        let snapshot = MergeSnapshot {
            merge_state: MergeStateStatus::Behind,
            ..ready()
        };

        assert_eq!(blockers(&snapshot), [Blocker::OutOfDate]);
    }

    #[test]
    fn the_caller_can_say_it_is_out_of_date_when_github_reports_another_block() {
        let snapshot = MergeSnapshot {
            out_of_date: true,
            required_checks: vec![check("build", CheckState::Failed)],
            ..blocked()
        };

        assert_eq!(
            blockers(&snapshot),
            [
                Blocker::OutOfDate,
                Blocker::Check(check("build", CheckState::Failed)),
            ]
        );
    }

    #[test]
    fn being_behind_does_not_block_joining_a_merge_queue() {
        let snapshot = MergeSnapshot {
            out_of_date: true,
            queue: MergeQueue {
                required: true,
                ..Default::default()
            },
            ..ready()
        };

        assert_eq!(blockers(&snapshot), []);
    }

    #[test]
    fn each_required_check_that_has_not_passed_blocks_it() {
        let snapshot = MergeSnapshot {
            required_checks: vec![
                check("build", CheckState::Failed),
                check("lint", CheckState::Passed),
                check("test", CheckState::Running),
                check("deploy-preview", CheckState::Missing),
                check("docs", CheckState::Failed),
            ],
            ..blocked()
        };

        assert_eq!(
            blockers(&snapshot),
            [
                Blocker::Check(check("build", CheckState::Failed)),
                Blocker::Check(check("docs", CheckState::Failed)),
                Blocker::Check(check("test", CheckState::Running)),
                Blocker::Check(check("deploy-preview", CheckState::Missing)),
            ]
        );
    }

    #[test]
    fn unresolved_conversations_block_it_when_the_rules_require_resolving_them() {
        let snapshot = MergeSnapshot {
            unresolved_threads: 3,
            rules: ReviewRules {
                conversation_resolution: true,
                ..Default::default()
            },
            ..blocked()
        };

        assert_eq!(
            blockers(&snapshot),
            [Blocker::Conversations { unresolved: 3 }]
        );
    }

    #[test]
    fn unresolved_conversations_do_not_block_it_otherwise() {
        let snapshot = MergeSnapshot {
            unresolved_threads: 3,
            ..ready()
        };

        assert_eq!(blockers(&snapshot), []);
    }

    #[test]
    fn resolved_conversations_do_not_block_it() {
        let snapshot = MergeSnapshot {
            rules: ReviewRules {
                conversation_resolution: true,
                ..Default::default()
            },
            ..ready()
        };

        assert_eq!(blockers(&snapshot), []);
    }

    #[test]
    fn unmet_rules_block_it() {
        let snapshot = MergeSnapshot {
            unmet_rules: vec![
                rule("Deployment to staging must succeed", true),
                rule("Commits must be signed", false),
            ],
            ..blocked()
        };

        assert_eq!(
            blockers(&snapshot),
            [
                Blocker::Rule(rule("Commits must be signed", false)),
                Blocker::Rule(rule("Deployment to staging must succeed", true)),
            ]
        );
    }

    #[test]
    fn github_still_working_out_mergeability_is_a_blocker() {
        let unknown = MergeSnapshot {
            mergeable: MergeableState::Unknown,
            merge_state: MergeStateStatus::Unknown,
            ..ready()
        };
        let unknown_state = MergeSnapshot {
            merge_state: MergeStateStatus::Unknown,
            ..ready()
        };

        assert_eq!(blockers(&unknown), [Blocker::CheckingMergeability]);
        assert_eq!(blockers(&unknown_state), [Blocker::CheckingMergeability]);
    }

    #[test]
    fn a_block_nothing_explains_links_to_the_pull_request() {
        assert_eq!(
            blockers(&blocked()),
            [Blocker::Unexplained {
                url: URL.to_string()
            }]
        );
    }

    #[test]
    fn a_block_is_unexplained_while_github_works_out_mergeability() {
        let snapshot = MergeSnapshot {
            mergeable: MergeableState::Unknown,
            ..blocked()
        };

        assert_eq!(
            blockers(&snapshot),
            [
                Blocker::Unexplained {
                    url: URL.to_string()
                },
                Blocker::CheckingMergeability,
            ]
        );
    }

    #[test]
    fn a_block_a_waiting_blocker_explains_is_not_unexplained() {
        let snapshot = MergeSnapshot {
            required_checks: vec![check("test", CheckState::Running)],
            ..blocked()
        };

        assert_eq!(
            blockers(&snapshot),
            [Blocker::Check(check("test", CheckState::Running))]
        );
    }

    #[test]
    fn blockers_waiting_will_not_fix_come_first() {
        let snapshot = MergeSnapshot {
            draft: true,
            mergeable: MergeableState::Unknown,
            merge_state: MergeStateStatus::Unknown,
            required_checks: vec![
                check("test", CheckState::Running),
                check("build", CheckState::Failed),
            ],
            unresolved_threads: 1,
            rules: ReviewRules {
                conversation_resolution: true,
                ..Default::default()
            },
            unmet_rules: vec![rule("Deployment to staging must succeed", true)],
            ..ready()
        };

        assert_eq!(
            blockers(&snapshot),
            [
                Blocker::Draft,
                Blocker::Check(check("build", CheckState::Failed)),
                Blocker::Conversations { unresolved: 1 },
                Blocker::Check(check("test", CheckState::Running)),
                Blocker::Rule(rule("Deployment to staging must succeed", true)),
                Blocker::CheckingMergeability,
            ]
        );
    }

    #[test]
    fn running_and_missing_checks_are_waits() {
        assert!(Blocker::Check(check("test", CheckState::Running)).waiting());
        assert!(Blocker::Check(check("test", CheckState::Missing)).waiting());
        assert!(!Blocker::Check(check("test", CheckState::Failed)).waiting());
    }

    #[test]
    fn a_rule_is_a_wait_when_it_says_so() {
        assert!(Blocker::Rule(rule("Deployment to staging must succeed", true)).waiting());
        assert!(!Blocker::Rule(rule("Commits must be signed", false)).waiting());
    }

    #[test]
    fn github_working_out_mergeability_is_a_wait() {
        assert!(Blocker::CheckingMergeability.waiting());
    }

    #[test]
    fn blockers_someone_must_act_on_are_not_waits() {
        for blocker in [
            Blocker::Merged,
            Blocker::Closed,
            Blocker::Draft,
            Blocker::Conflicts,
            Blocker::OutOfDate,
            Blocker::Conversations { unresolved: 1 },
            Blocker::Unexplained {
                url: URL.to_string(),
            },
        ] {
            assert!(!blocker.waiting(), "{blocker:?}");
        }
    }
}
