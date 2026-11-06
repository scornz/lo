//! What the merge button does (design §9, "Merging").

use super::{MergeMethod, MergeSnapshot, PullRequestState, blockers};

/// What merging the pull request does now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MergeAction {
    /// Merge it now, with one of `methods`: those the repository allows, in the order GitHub's
    /// merge button lists them. `default` is the one to pick first. The merge carries the head on
    /// screen, so GitHub refuses it if someone pushed since (design §11).
    Merge {
        methods: Vec<MergeMethod>,
        default: MergeMethod,
    },
    /// Add it to the base branch's merge queue, whose settings choose the method: `method` when
    /// GitHub says which.
    Enqueue { method: Option<MergeMethod> },
    /// Neither, for this reason.
    NotPossible(NotPossible),
}

/// Why the pull request can't merge or join the merge queue now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotPossible {
    /// It's merged.
    Merged,
    /// It's closed.
    Closed,
    /// It's in the merge queue already, at this position.
    Queued { position: u32 },
    /// Something blocks it: see [`blockers`](super::blockers).
    Blocked,
    /// GitHub's rules let it merge, but you can't merge into the base branch.
    NoPermission,
    /// The repository allows no merge method.
    NoMethod,
}

/// What merging the pull request does now: merge it with a method, add it to the merge queue when
/// the base branch requires one, or neither and why. The reasons are checked in [`NotPossible`]'s
/// order, so a blocked pull request you can't merge is [`NotPossible::Blocked`].
pub fn merge_action(snapshot: &MergeSnapshot) -> MergeAction {
    let not_possible = MergeAction::NotPossible;
    match snapshot.state {
        PullRequestState::Merged => return not_possible(NotPossible::Merged),
        PullRequestState::Closed => return not_possible(NotPossible::Closed),
        PullRequestState::Open => {}
    }
    if let Some(position) = snapshot.queue.position {
        return not_possible(NotPossible::Queued { position });
    }
    if !blockers(snapshot).is_empty() {
        return not_possible(NotPossible::Blocked);
    }
    if !snapshot.viewer.can_merge {
        return not_possible(NotPossible::NoPermission);
    }
    if snapshot.queue.required {
        return MergeAction::Enqueue {
            method: snapshot.queue.method,
        };
    }
    match snapshot.methods.choose(snapshot.preferred_method) {
        Some(default) => MergeAction::Merge {
            methods: snapshot.methods.to_vec(),
            default,
        },
        None => not_possible(NotPossible::NoMethod),
    }
}

#[cfg(test)]
mod tests {
    use super::super::{
        AllowedMethods, CheckState, MergeQueue, MergeStateStatus, MergeableState, PullRequestState,
        RequiredCheck, Viewer,
    };
    use super::*;

    const ALL_METHODS: AllowedMethods = AllowedMethods {
        merge: true,
        squash: true,
        rebase: true,
    };

    fn ready() -> MergeSnapshot {
        MergeSnapshot {
            mergeable: MergeableState::Mergeable,
            merge_state: MergeStateStatus::Clean,
            methods: ALL_METHODS,
            preferred_method: Some(MergeMethod::Squash),
            viewer: Viewer {
                login: "alice".to_string(),
                can_approve: true,
                approval_counts: true,
                can_merge: true,
                can_enable_auto_merge: true,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn with_queue(snapshot: MergeSnapshot, position: Option<u32>) -> MergeSnapshot {
        MergeSnapshot {
            queue: MergeQueue {
                required: true,
                method: Some(MergeMethod::Squash),
                position,
            },
            ..snapshot
        }
    }

    fn not_possible(why: NotPossible) -> MergeAction {
        MergeAction::NotPossible(why)
    }

    #[test]
    fn a_pull_request_nothing_blocks_merges_with_the_preferred_method() {
        assert_eq!(
            merge_action(&ready()),
            MergeAction::Merge {
                methods: vec![MergeMethod::Merge, MergeMethod::Squash, MergeMethod::Rebase],
                default: MergeMethod::Squash,
            }
        );
    }

    #[test]
    fn only_the_allowed_methods_are_offered() {
        let snapshot = MergeSnapshot {
            methods: AllowedMethods {
                rebase: false,
                squash: false,
                ..ALL_METHODS
            },
            ..ready()
        };

        assert_eq!(
            merge_action(&snapshot),
            MergeAction::Merge {
                methods: vec![MergeMethod::Merge],
                default: MergeMethod::Merge,
            }
        );
    }

    #[test]
    fn a_base_branch_with_a_merge_queue_adds_it_to_the_queue() {
        assert_eq!(
            merge_action(&with_queue(ready(), None)),
            MergeAction::Enqueue {
                method: Some(MergeMethod::Squash)
            }
        );
    }

    #[test]
    fn the_queue_chooses_the_method_whatever_the_repository_allows() {
        let snapshot = MergeSnapshot {
            methods: AllowedMethods::default(),
            ..with_queue(ready(), None)
        };

        assert_eq!(
            merge_action(&snapshot),
            MergeAction::Enqueue {
                method: Some(MergeMethod::Squash)
            }
        );
    }

    #[test]
    fn a_pull_request_in_the_queue_is_not_added_again() {
        assert_eq!(
            merge_action(&with_queue(ready(), Some(2))),
            not_possible(NotPossible::Queued { position: 2 })
        );
    }

    #[test]
    fn a_blocked_pull_request_neither_merges_nor_joins_the_queue() {
        let running = MergeSnapshot {
            merge_state: MergeStateStatus::Blocked,
            required_checks: vec![RequiredCheck {
                name: "test".to_string(),
                state: CheckState::Running,
                url: None,
            }],
            ..ready()
        };

        assert_eq!(merge_action(&running), not_possible(NotPossible::Blocked));
        assert_eq!(
            merge_action(&with_queue(running, None)),
            not_possible(NotPossible::Blocked)
        );
    }

    #[test]
    fn a_merged_or_closed_pull_request_does_not_merge() {
        let merged = MergeSnapshot {
            state: PullRequestState::Merged,
            ..ready()
        };
        let closed = MergeSnapshot {
            state: PullRequestState::Closed,
            ..ready()
        };

        assert_eq!(merge_action(&merged), not_possible(NotPossible::Merged));
        assert_eq!(merge_action(&closed), not_possible(NotPossible::Closed));
    }

    #[test]
    fn you_need_permission_to_merge() {
        let mut snapshot = ready();
        snapshot.viewer.can_merge = false;

        assert_eq!(
            merge_action(&snapshot),
            not_possible(NotPossible::NoPermission)
        );
        assert_eq!(
            merge_action(&with_queue(snapshot, None)),
            not_possible(NotPossible::NoPermission)
        );
    }

    #[test]
    fn a_blocked_pull_request_you_cannot_merge_is_blocked() {
        let mut snapshot = MergeSnapshot {
            draft: true,
            ..ready()
        };
        snapshot.viewer.can_merge = false;

        assert_eq!(merge_action(&snapshot), not_possible(NotPossible::Blocked));
    }

    #[test]
    fn a_repository_allowing_no_method_cannot_merge() {
        let snapshot = MergeSnapshot {
            methods: AllowedMethods::default(),
            ..ready()
        };

        assert_eq!(merge_action(&snapshot), not_possible(NotPossible::NoMethod));
    }
}
