//! The one-line merge status in a pull request's header (design §9, "Merge status").

use super::{Blocker, MergeSnapshot, PullRequestState, blockers, may_not_see_every_rule};

/// Where a pull request's merge stands, for the one-line status in its tab's header. It's about
/// the pull request, not you: a pull request you can't merge can still be ready.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MergeStatus {
    /// It's merged.
    Merged,
    /// It's closed.
    Closed,
    /// It's in the merge queue, at this position (`mergeQueueEntry.position`).
    Queued { position: u32 },
    /// Nothing blocks it: it can merge, or join the merge queue.
    Ready,
    /// Only things waiting will fix block it. With `auto_merge`, auto-merge is on, so it merges,
    /// or joins the queue, once they clear. With `partial`, rozed may not see every rule behind
    /// GitHub's block ([`may_not_see_every_rule`]), so these may be only part of why.
    Waiting {
        blockers: Vec<Blocker>,
        auto_merge: bool,
        partial: bool,
    },
    /// Something waiting won't fix blocks it. The first blocker is the one to name, as
    /// [`blockers`](super::blockers) orders them. With `auto_merge`, auto-merge is on, and it merges
    /// once everything clears. With `partial`, as for [`MergeStatus::Waiting`].
    Blocked {
        blockers: Vec<Blocker>,
        auto_merge: bool,
        partial: bool,
    },
}

/// Where the pull request's merge stands.
pub fn status(snapshot: &MergeSnapshot) -> MergeStatus {
    match snapshot.state {
        PullRequestState::Merged => return MergeStatus::Merged,
        PullRequestState::Closed => return MergeStatus::Closed,
        PullRequestState::Open => {}
    }
    if let Some(position) = snapshot.queue.position {
        return MergeStatus::Queued { position };
    }
    let blockers = blockers(snapshot);
    let auto_merge = snapshot.auto_merge.is_some();
    let partial = may_not_see_every_rule(snapshot);
    if blockers.is_empty() {
        MergeStatus::Ready
    } else if blockers.iter().all(Blocker::waiting) {
        MergeStatus::Waiting {
            blockers,
            auto_merge,
            partial,
        }
    } else {
        MergeStatus::Blocked {
            blockers,
            auto_merge,
            partial,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{
        AutoMerge, CheckState, MergeMethod, MergeQueue, MergeStateStatus, MergeableState,
        RequiredCheck,
    };
    use super::*;

    fn ready() -> MergeSnapshot {
        MergeSnapshot {
            mergeable: MergeableState::Mergeable,
            merge_state: MergeStateStatus::Clean,
            ..Default::default()
        }
    }

    fn checks(states: &[(&str, CheckState)]) -> MergeSnapshot {
        MergeSnapshot {
            merge_state: MergeStateStatus::Blocked,
            required_checks: states
                .iter()
                .map(|&(name, state)| RequiredCheck {
                    name: name.to_string(),
                    state,
                    url: None,
                })
                .collect(),
            sees_every_rule: true,
            ..ready()
        }
    }

    fn with_auto_merge(snapshot: MergeSnapshot) -> MergeSnapshot {
        MergeSnapshot {
            auto_merge: Some(AutoMerge {
                method: MergeMethod::Squash,
                enabled_by: Some("bob".to_string()),
            }),
            ..snapshot
        }
    }

    fn blocker(name: &str, state: CheckState) -> Blocker {
        Blocker::Check(RequiredCheck {
            name: name.to_string(),
            state,
            url: None,
        })
    }

    #[test]
    fn a_merged_or_closed_pull_request_says_so() {
        let merged = MergeSnapshot {
            state: PullRequestState::Merged,
            ..ready()
        };
        let closed = MergeSnapshot {
            state: PullRequestState::Closed,
            ..ready()
        };

        assert_eq!(status(&merged), MergeStatus::Merged);
        assert_eq!(status(&closed), MergeStatus::Closed);
    }

    #[test]
    fn a_queued_pull_request_gives_its_position() {
        let snapshot = MergeSnapshot {
            queue: MergeQueue {
                required: true,
                method: None,
                position: Some(4),
            },
            ..ready()
        };

        assert_eq!(status(&snapshot), MergeStatus::Queued { position: 4 });
    }

    #[test]
    fn a_pull_request_nothing_blocks_is_ready_whoever_is_looking() {
        let mut snapshot = ready();
        snapshot.viewer.can_merge = false;

        assert_eq!(status(&snapshot), MergeStatus::Ready);
    }

    #[test]
    fn a_pull_request_only_waiting_is_waiting() {
        let snapshot = checks(&[("test", CheckState::Running)]);

        assert_eq!(
            status(&snapshot),
            MergeStatus::Waiting {
                blockers: vec![blocker("test", CheckState::Running)],
                auto_merge: false,
                partial: false,
            }
        );
    }

    #[test]
    fn a_waiting_pull_request_says_when_rozed_may_not_see_every_rule() {
        let snapshot = MergeSnapshot {
            sees_every_rule: false,
            ..checks(&[("test", CheckState::Running)])
        };

        assert_eq!(
            status(&snapshot),
            MergeStatus::Waiting {
                blockers: vec![blocker("test", CheckState::Running)],
                auto_merge: false,
                partial: true,
            }
        );
    }

    #[test]
    fn a_waiting_pull_request_says_whether_auto_merge_is_on() {
        let snapshot = with_auto_merge(checks(&[("test", CheckState::Running)]));

        assert_eq!(
            status(&snapshot),
            MergeStatus::Waiting {
                blockers: vec![blocker("test", CheckState::Running)],
                auto_merge: true,
                partial: false,
            }
        );
    }

    #[test]
    fn a_pull_request_waiting_on_a_check_nothing_will_report_is_blocked() {
        let snapshot = checks(&[("test", CheckState::Missing)]);

        assert_eq!(
            status(&snapshot),
            MergeStatus::Blocked {
                blockers: vec![Blocker::NotReported(RequiredCheck {
                    name: "test".to_string(),
                    state: CheckState::Missing,
                    url: None,
                })],
                auto_merge: false,
                partial: false,
            }
        );
    }

    #[test]
    fn a_pull_request_something_else_blocks_is_blocked() {
        let snapshot = with_auto_merge(checks(&[
            ("test", CheckState::Running),
            ("build", CheckState::Failed),
        ]));

        assert_eq!(
            status(&snapshot),
            MergeStatus::Blocked {
                blockers: vec![
                    blocker("build", CheckState::Failed),
                    blocker("test", CheckState::Running),
                ],
                auto_merge: true,
                partial: false,
            }
        );
    }
}
