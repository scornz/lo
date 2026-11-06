//! Approve and ship, `⌘⇧⏎` (design §9, "Approve and ship"; §11, "Acting on what you saw").

use super::reviews;
use super::{
    Blocker, MergeMethod, MergeSnapshot, MergeStateStatus, PullRequestState, Review,
    ReviewDecision, ReviewState, Reviewer, blockers,
};

/// What approve and ship does: the plan its one confirmation lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApproveAndShip {
    /// Nothing: someone pushed since the revision on screen. rozed shows the new revision, with
    /// the changes since the one you saw.
    HeadMoved { on_screen: String, head: String },
    /// Nothing: it's merged.
    Merged,
    /// Nothing: it's closed.
    Closed,
    /// Nothing: GitHub doesn't let you approve it, and it isn't yours.
    CannotApprove,
    /// Approve unless that's skipped, then ship.
    Plan(Plan),
}

/// Approve and ship's steps, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// The commit every step acts on: the head on screen, which is GitHub's head too. Approving
    /// approves it, and merging, joining the queue or turning on auto-merge expects it, so GitHub
    /// refuses if someone pushes in between.
    pub head: String,
    /// The first step.
    pub approval: Approval,
    /// The step after it.
    pub then: Ship,
}

/// Whether approve and ship approves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Approval {
    /// Approve the head on screen.
    Approve,
    /// Skip it: it's your pull request, which GitHub doesn't let you approve.
    OwnPullRequest,
    /// Skip it: your latest review already approves the head on screen.
    AlreadyApproved,
}

/// What approve and ship does after approving.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ship {
    /// Merge it with this method.
    Merge(MergeMethod),
    /// Add it to the base branch's merge queue.
    Enqueue,
    /// Turn on auto-merge with this method, so GitHub merges it once what it's waiting on clears.
    /// If GitHub turns out to be waiting on nothing, it refuses ("clean status"), and merging now
    /// does what this meant.
    AutoMerge(MergeMethod),
    /// Turn on auto-merge on a branch with a merge queue (GitHub's "Merge when ready"), so GitHub
    /// adds it to the queue once what it's waiting on clears. The repository's auto-merge setting
    /// doesn't apply there.
    EnqueueWhenReady,
    /// Nothing: auto-merge is already on, so GitHub merges it, or adds it to the queue, once
    /// ready.
    AutoMergeAlreadyOn,
    /// Nothing: it's in the merge queue already.
    AlreadyQueued,
    /// Stop after approving, and show why.
    Stop(Stop),
}

/// Why approve and ship stops after approving.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stop {
    /// Something waiting won't fix blocks it. These are every blocker left after your approval,
    /// those waiting won't fix first, as [`blockers`](super::blockers) orders them.
    Blocked(Vec<Blocker>),
    /// Only things waiting will fix block it, but you can't turn on auto-merge: the repository
    /// doesn't allow it, or you lack write access. These are the blockers.
    NoAutoMerge(Vec<Blocker>),
    /// Nothing blocks it, but you can't merge into the base branch.
    NoPermission,
    /// The repository allows no merge method.
    NoMethod,
}

/// The plan for approve and ship, from a snapshot read right before it runs.
///
/// - It does nothing to a merged or closed pull request, or when GitHub's head isn't the head on
///   screen, so it never approves or merges what you didn't see.
/// - It approves the head on screen. It skips that on your own pull request, and when your latest
///   review approves that head already. An approval of an older revision is given again, so
///   GitHub records that you approved what you saw. When GitHub doesn't let you approve someone
///   else's pull request, it does nothing.
/// - Then, from what would block the merge after your approval: nothing, and it merges with the
///   preferred method, or joins the queue on a branch that requires one; only things waiting will
///   fix, and it turns on auto-merge; anything else, and it stops after approving, with the
///   blockers. It does nothing more when the pull request is in the queue already or has
///   auto-merge on.
pub fn approve_and_ship(snapshot: &MergeSnapshot) -> ApproveAndShip {
    match snapshot.state {
        PullRequestState::Merged => return ApproveAndShip::Merged,
        PullRequestState::Closed => return ApproveAndShip::Closed,
        PullRequestState::Open => {}
    }
    if snapshot.head != snapshot.head_on_screen {
        return ApproveAndShip::HeadMoved {
            on_screen: snapshot.head_on_screen.clone(),
            head: snapshot.head.clone(),
        };
    }
    let viewer = &snapshot.viewer;
    let yours = reviews::latest(snapshot)
        .into_iter()
        .find(|review| reviews::same_login(&review.author, &viewer.login));
    let approval = if viewer.did_author {
        Approval::OwnPullRequest
    } else if yours.is_some_and(|review| {
        review.state == ReviewState::Approved
            && review.commit.as_deref() == Some(snapshot.head_on_screen.as_str())
    }) {
        Approval::AlreadyApproved
    } else if viewer.can_approve {
        Approval::Approve
    } else {
        return ApproveAndShip::CannotApprove;
    };
    let after = match approval {
        Approval::Approve => with_your_approval(snapshot),
        Approval::OwnPullRequest | Approval::AlreadyApproved => snapshot.clone(),
    };
    ApproveAndShip::Plan(Plan {
        head: snapshot.head_on_screen.clone(),
        approval,
        then: ship(&after),
    })
}

/// What to do once approved, from the snapshot as it would be.
fn ship(snapshot: &MergeSnapshot) -> Ship {
    if snapshot.queue.position.is_some() {
        return Ship::AlreadyQueued;
    }
    let blockers = blockers(snapshot);
    if blockers.iter().any(|blocker| !blocker.waiting()) {
        return Ship::Stop(Stop::Blocked(blockers));
    }
    if snapshot.auto_merge.is_some() {
        return Ship::AutoMergeAlreadyOn;
    }
    let viewer = &snapshot.viewer;
    let method = snapshot.methods.choose(snapshot.preferred_method);
    if blockers.is_empty() {
        if !viewer.can_merge {
            Ship::Stop(Stop::NoPermission)
        } else if snapshot.queue.required {
            Ship::Enqueue
        } else {
            method.map_or(Ship::Stop(Stop::NoMethod), Ship::Merge)
        }
    } else if snapshot.queue.required && (viewer.can_merge || viewer.can_enable_auto_merge) {
        Ship::EnqueueWhenReady
    } else if !snapshot.queue.required && viewer.can_enable_auto_merge {
        method.map_or(Ship::Stop(Stop::NoMethod), Ship::AutoMerge)
    } else {
        Ship::Stop(Stop::NoAutoMerge(blockers))
    }
}

/// The snapshot as GitHub would report it after you approve the head on screen.
///
/// - Your approval replaces your latest review, and GitHub drops the request for your review.
/// - It covers the code-owner requests for you and for your teams, when it counts.
/// - The review decision follows from the reviews: changes requested while anyone with write
///   access still requests them; else review required while approvals are missing, or while GitHub
///   required a review rozed couldn't attribute, which rozed assumes your approval doesn't give;
///   else approved.
/// - A `BLOCKED` merge state that the snapshot explained becomes `CLEAN`, so what still blocks the
///   merge comes from the other fields. One it didn't explain stays.
fn with_your_approval(snapshot: &MergeSnapshot) -> MergeSnapshot {
    let viewer = &snapshot.viewer;
    let yours = |login: &str| reviews::same_login(login, &viewer.login);
    let mut after = snapshot.clone();
    after.reviews.retain(|review| !yours(&review.author));
    after.reviews.push(Review {
        author: viewer.login.clone(),
        state: ReviewState::Approved,
        commit: Some(snapshot.head_on_screen.clone()),
        counts: viewer.approval_counts,
        on_behalf_of: if viewer.approval_counts {
            viewer.teams.clone()
        } else {
            Vec::new()
        },
    });
    after
        .review_requests
        .retain(|request| !matches!(&request.reviewer, Reviewer::User(login) if yours(login)));

    let unattributed = snapshot.review_decision == Some(ReviewDecision::ReviewRequired)
        && reviews::missing_approvals(snapshot).is_empty();
    after.review_decision = snapshot.review_decision.map(|_| {
        if !reviews::changes_requested_by(&after).is_empty() {
            ReviewDecision::ChangesRequested
        } else if unattributed || !reviews::missing_approvals(&after).is_empty() {
            ReviewDecision::ReviewRequired
        } else {
            ReviewDecision::Approved
        }
    });

    let unexplained =
        (blockers(snapshot).iter()).any(|blocker| matches!(blocker, Blocker::Unexplained { .. }));
    if snapshot.merge_state == MergeStateStatus::Blocked && !unexplained {
        after.merge_state = MergeStateStatus::Clean;
    }
    after
}

#[cfg(test)]
mod tests {
    use super::super::{
        AllowedMethods, AutoMerge, CheckState, MergeQueue, MergeStateStatus, MergeableState,
        MissingApprovals, PullRequestState, RequiredCheck, Review, ReviewDecision, ReviewRequest,
        ReviewRules, ReviewState, Reviewer, Viewer,
    };
    use super::*;

    const HEAD: &str = "c0ffee1";
    const URL: &str = "https://github.com/rozed-dev/playground/pull/12";

    /// Bob's pull request, waiting only on one approval, which you, Alice, can give.
    fn reviewing() -> MergeSnapshot {
        MergeSnapshot {
            url: URL.to_string(),
            mergeable: MergeableState::Mergeable,
            merge_state: MergeStateStatus::Blocked,
            review_decision: Some(ReviewDecision::ReviewRequired),
            rules: ReviewRules {
                required_approvals: 1,
                ..Default::default()
            },
            methods: AllowedMethods {
                merge: true,
                squash: true,
                rebase: true,
            },
            preferred_method: Some(MergeMethod::Squash),
            viewer: Viewer {
                login: "alice".to_string(),
                teams: vec!["rozed-dev/github".to_string()],
                can_approve: true,
                approval_counts: true,
                can_merge: true,
                can_enable_auto_merge: true,
                ..Default::default()
            },
            head: HEAD.to_string(),
            head_on_screen: HEAD.to_string(),
            ..Default::default()
        }
    }

    /// Two approvals required, so another one is still missing after yours.
    fn needing_another_approval() -> MergeSnapshot {
        MergeSnapshot {
            rules: ReviewRules {
                required_approvals: 2,
                ..Default::default()
            },
            ..reviewing()
        }
    }

    fn with_queue(snapshot: MergeSnapshot) -> MergeSnapshot {
        MergeSnapshot {
            queue: MergeQueue {
                required: true,
                method: Some(MergeMethod::Rebase),
                position: None,
            },
            ..snapshot
        }
    }

    fn review(author: &str, state: ReviewState, commit: &str) -> Review {
        Review {
            author: author.to_string(),
            state,
            commit: Some(commit.to_string()),
            counts: true,
            on_behalf_of: Vec::new(),
        }
    }

    fn check(name: &str, state: CheckState) -> RequiredCheck {
        RequiredCheck {
            name: name.to_string(),
            state,
            url: Some(format!(
                "https://github.com/rozed-dev/playground/runs/{name}"
            )),
        }
    }

    fn code_owner(slug: &str) -> ReviewRequest {
        ReviewRequest {
            reviewer: Reviewer::Team(slug.to_string()),
            as_code_owner: true,
        }
    }

    fn plan(approval: Approval, then: Ship) -> ApproveAndShip {
        ApproveAndShip::Plan(Plan {
            head: HEAD.to_string(),
            approval,
            then,
        })
    }

    fn approvals(more: u32) -> Blocker {
        Blocker::Approvals(MissingApprovals {
            more,
            code_owners: Vec::new(),
        })
    }

    #[test]
    fn it_approves_and_merges_when_your_approval_is_all_that_is_missing() {
        assert_eq!(
            approve_and_ship(&reviewing()),
            plan(Approval::Approve, Ship::Merge(MergeMethod::Squash))
        );
    }

    #[test]
    fn it_approves_and_merges_with_the_first_allowed_method_without_a_preference() {
        let snapshot = MergeSnapshot {
            methods: AllowedMethods {
                merge: false,
                squash: true,
                rebase: true,
            },
            preferred_method: None,
            ..reviewing()
        };

        assert_eq!(
            approve_and_ship(&snapshot),
            plan(Approval::Approve, Ship::Merge(MergeMethod::Squash))
        );
    }

    #[test]
    fn it_approves_and_joins_the_queue_on_a_branch_that_requires_one() {
        assert_eq!(
            approve_and_ship(&with_queue(reviewing())),
            plan(Approval::Approve, Ship::Enqueue)
        );
    }

    #[test]
    fn it_does_nothing_when_the_head_moved_since_what_is_on_screen() {
        let snapshot = MergeSnapshot {
            head_on_screen: "5ca1ab1".to_string(),
            ..reviewing()
        };

        assert_eq!(
            approve_and_ship(&snapshot),
            ApproveAndShip::HeadMoved {
                on_screen: "5ca1ab1".to_string(),
                head: HEAD.to_string(),
            }
        );
    }

    #[test]
    fn it_does_nothing_to_a_merged_or_closed_pull_request() {
        let merged = MergeSnapshot {
            state: PullRequestState::Merged,
            ..reviewing()
        };
        let closed = MergeSnapshot {
            state: PullRequestState::Closed,
            head_on_screen: "5ca1ab1".to_string(),
            ..reviewing()
        };

        assert_eq!(approve_and_ship(&merged), ApproveAndShip::Merged);
        assert_eq!(approve_and_ship(&closed), ApproveAndShip::Closed);
    }

    #[test]
    fn it_does_nothing_when_you_cannot_approve_someone_elses_pull_request() {
        let mut snapshot = reviewing();
        snapshot.viewer.can_approve = false;

        assert_eq!(approve_and_ship(&snapshot), ApproveAndShip::CannotApprove);
    }

    #[test]
    fn it_skips_approving_your_own_pull_request_and_merges() {
        let mut snapshot = MergeSnapshot {
            merge_state: MergeStateStatus::Clean,
            review_decision: Some(ReviewDecision::Approved),
            reviews: vec![review("bob", ReviewState::Approved, HEAD)],
            ..reviewing()
        };
        snapshot.viewer.did_author = true;
        snapshot.viewer.can_approve = false;

        assert_eq!(
            approve_and_ship(&snapshot),
            plan(Approval::OwnPullRequest, Ship::Merge(MergeMethod::Squash))
        );
    }

    #[test]
    fn your_own_pull_request_waiting_on_an_approval_gets_auto_merge() {
        let mut snapshot = reviewing();
        snapshot.viewer.did_author = true;

        assert_eq!(
            approve_and_ship(&snapshot),
            plan(
                Approval::OwnPullRequest,
                Ship::AutoMerge(MergeMethod::Squash)
            )
        );
    }

    #[test]
    fn it_skips_approving_when_you_approved_the_head_on_screen() {
        let snapshot = MergeSnapshot {
            merge_state: MergeStateStatus::Clean,
            review_decision: Some(ReviewDecision::Approved),
            reviews: vec![review("alice", ReviewState::Approved, HEAD)],
            ..reviewing()
        };

        assert_eq!(
            approve_and_ship(&snapshot),
            plan(Approval::AlreadyApproved, Ship::Merge(MergeMethod::Squash))
        );
    }

    #[test]
    fn it_approves_again_when_you_approved_an_older_revision() {
        let snapshot = MergeSnapshot {
            merge_state: MergeStateStatus::Clean,
            review_decision: Some(ReviewDecision::Approved),
            reviews: vec![review("alice", ReviewState::Approved, "0ldhead")],
            ..reviewing()
        };

        assert_eq!(
            approve_and_ship(&snapshot),
            plan(Approval::Approve, Ship::Merge(MergeMethod::Squash))
        );
    }

    #[test]
    fn it_turns_on_auto_merge_when_another_approval_is_still_needed() {
        assert_eq!(
            approve_and_ship(&needing_another_approval()),
            plan(Approval::Approve, Ship::AutoMerge(MergeMethod::Squash))
        );
    }

    #[test]
    fn your_approval_and_one_already_given_meet_the_count() {
        let snapshot = MergeSnapshot {
            reviews: vec![review("carol", ReviewState::Approved, HEAD)],
            ..needing_another_approval()
        };

        assert_eq!(
            approve_and_ship(&snapshot),
            plan(Approval::Approve, Ship::Merge(MergeMethod::Squash))
        );
    }

    #[test]
    fn it_joins_the_queue_when_ready_on_a_branch_that_requires_one() {
        assert_eq!(
            approve_and_ship(&with_queue(needing_another_approval())),
            plan(Approval::Approve, Ship::EnqueueWhenReady)
        );
    }

    #[test]
    fn your_approval_covers_your_teams_code_owner_review() {
        let snapshot = MergeSnapshot {
            rules: ReviewRules {
                required_approvals: 1,
                code_owner_review: true,
                ..Default::default()
            },
            review_requests: vec![code_owner("rozed-dev/github")],
            ..reviewing()
        };

        assert_eq!(
            approve_and_ship(&snapshot),
            plan(Approval::Approve, Ship::Merge(MergeMethod::Squash))
        );
    }

    #[test]
    fn another_teams_code_owner_review_is_still_waited_on() {
        let snapshot = MergeSnapshot {
            rules: ReviewRules {
                required_approvals: 1,
                code_owner_review: true,
                ..Default::default()
            },
            review_requests: vec![code_owner("rozed-dev/github"), code_owner("rozed-dev/ui")],
            ..reviewing()
        };

        assert_eq!(
            approve_and_ship(&snapshot),
            plan(Approval::Approve, Ship::AutoMerge(MergeMethod::Squash))
        );
    }

    #[test]
    fn your_approval_replaces_your_request_for_changes() {
        let snapshot = MergeSnapshot {
            review_decision: Some(ReviewDecision::ChangesRequested),
            reviews: vec![review("alice", ReviewState::ChangesRequested, "0ldhead")],
            ..reviewing()
        };

        assert_eq!(
            approve_and_ship(&snapshot),
            plan(Approval::Approve, Ship::Merge(MergeMethod::Squash))
        );
    }

    #[test]
    fn it_stops_after_approving_when_someone_else_requested_changes() {
        let snapshot = MergeSnapshot {
            review_decision: Some(ReviewDecision::ChangesRequested),
            reviews: vec![review("carol", ReviewState::ChangesRequested, "0ldhead")],
            ..needing_another_approval()
        };

        assert_eq!(
            approve_and_ship(&snapshot),
            plan(
                Approval::Approve,
                Ship::Stop(Stop::Blocked(vec![
                    Blocker::ChangesRequested {
                        by: vec!["carol".to_string()]
                    },
                    approvals(1),
                ]))
            )
        );
    }

    #[test]
    fn it_stops_after_approving_when_a_required_check_failed() {
        let snapshot = MergeSnapshot {
            required_checks: vec![
                check("build", CheckState::Failed),
                check("test", CheckState::Running),
            ],
            ..reviewing()
        };

        assert_eq!(
            approve_and_ship(&snapshot),
            plan(
                Approval::Approve,
                Ship::Stop(Stop::Blocked(vec![
                    Blocker::Check(check("build", CheckState::Failed)),
                    Blocker::Check(check("test", CheckState::Running)),
                ]))
            )
        );
    }

    #[test]
    fn it_stops_after_approving_on_conflicts_a_draft_or_an_out_of_date_branch() {
        let conflicting = MergeSnapshot {
            mergeable: MergeableState::Conflicting,
            merge_state: MergeStateStatus::Dirty,
            ..reviewing()
        };
        let draft = MergeSnapshot {
            draft: true,
            ..reviewing()
        };
        let behind = MergeSnapshot {
            out_of_date: true,
            ..reviewing()
        };

        for (snapshot, blocker) in [
            (conflicting, Blocker::Conflicts),
            (draft, Blocker::Draft),
            (behind, Blocker::OutOfDate),
        ] {
            assert_eq!(
                approve_and_ship(&snapshot),
                plan(Approval::Approve, Ship::Stop(Stop::Blocked(vec![blocker])))
            );
        }
    }

    #[test]
    fn it_turns_on_auto_merge_while_required_checks_run() {
        let snapshot = MergeSnapshot {
            required_checks: vec![
                check("build", CheckState::Passed),
                check("test", CheckState::Running),
                check("e2e", CheckState::Missing),
            ],
            ..reviewing()
        };

        assert_eq!(
            approve_and_ship(&snapshot),
            plan(Approval::Approve, Ship::AutoMerge(MergeMethod::Squash))
        );
    }

    #[test]
    fn it_turns_on_auto_merge_while_github_works_out_mergeability() {
        let snapshot = MergeSnapshot {
            mergeable: MergeableState::Unknown,
            merge_state: MergeStateStatus::Unknown,
            ..reviewing()
        };

        assert_eq!(
            approve_and_ship(&snapshot),
            plan(Approval::Approve, Ship::AutoMerge(MergeMethod::Squash))
        );
    }

    #[test]
    fn it_stops_after_approving_when_you_cannot_turn_on_auto_merge() {
        let mut snapshot = needing_another_approval();
        snapshot.viewer.can_enable_auto_merge = false;

        assert_eq!(
            approve_and_ship(&snapshot),
            plan(
                Approval::Approve,
                Ship::Stop(Stop::NoAutoMerge(vec![approvals(1)]))
            )
        );
    }

    #[test]
    fn a_queue_joins_when_ready_whatever_the_repositorys_auto_merge_setting() {
        let mut snapshot = with_queue(needing_another_approval());
        snapshot.viewer.can_enable_auto_merge = false;

        assert_eq!(
            approve_and_ship(&snapshot),
            plan(Approval::Approve, Ship::EnqueueWhenReady)
        );
    }

    #[test]
    fn it_does_nothing_more_when_auto_merge_is_already_on() {
        let auto_merge = Some(AutoMerge {
            method: MergeMethod::Merge,
            enabled_by: Some("bob".to_string()),
        });
        let waiting = MergeSnapshot {
            auto_merge: auto_merge.clone(),
            ..needing_another_approval()
        };
        let ready = MergeSnapshot {
            auto_merge,
            ..reviewing()
        };

        assert_eq!(
            approve_and_ship(&waiting),
            plan(Approval::Approve, Ship::AutoMergeAlreadyOn)
        );
        assert_eq!(
            approve_and_ship(&ready),
            plan(Approval::Approve, Ship::AutoMergeAlreadyOn)
        );
    }

    #[test]
    fn it_does_nothing_more_when_the_pull_request_is_already_queued() {
        let mut snapshot = MergeSnapshot {
            merge_state: MergeStateStatus::Clean,
            review_decision: Some(ReviewDecision::Approved),
            reviews: vec![review("carol", ReviewState::Approved, HEAD)],
            ..with_queue(reviewing())
        };
        snapshot.queue.position = Some(3);

        assert_eq!(
            approve_and_ship(&snapshot),
            plan(Approval::Approve, Ship::AlreadyQueued)
        );
    }

    #[test]
    fn it_stops_after_approving_when_you_cannot_merge() {
        let mut snapshot = reviewing();
        snapshot.viewer.can_merge = false;

        assert_eq!(
            approve_and_ship(&snapshot),
            plan(Approval::Approve, Ship::Stop(Stop::NoPermission))
        );
    }

    #[test]
    fn it_stops_after_approving_when_no_method_is_allowed() {
        let snapshot = MergeSnapshot {
            methods: AllowedMethods::default(),
            ..reviewing()
        };

        assert_eq!(
            approve_and_ship(&snapshot),
            plan(Approval::Approve, Ship::Stop(Stop::NoMethod))
        );
    }

    #[test]
    fn an_approval_without_write_access_leaves_the_approval_missing() {
        let mut snapshot = reviewing();
        snapshot.viewer.approval_counts = false;
        snapshot.viewer.can_merge = false;
        snapshot.viewer.can_enable_auto_merge = false;

        assert_eq!(
            approve_and_ship(&snapshot),
            plan(
                Approval::Approve,
                Ship::Stop(Stop::NoAutoMerge(vec![approvals(1)]))
            )
        );
    }

    #[test]
    fn a_block_rozed_cannot_explain_still_stops_it() {
        let snapshot = MergeSnapshot {
            review_decision: Some(ReviewDecision::Approved),
            reviews: vec![review("carol", ReviewState::Approved, HEAD)],
            ..reviewing()
        };

        assert_eq!(
            approve_and_ship(&snapshot),
            plan(
                Approval::Approve,
                Ship::Stop(Stop::Blocked(vec![Blocker::Unexplained {
                    url: URL.to_string()
                }]))
            )
        );
    }

    #[test]
    fn a_review_github_requires_for_a_reason_rozed_cannot_see_is_still_waited_on() {
        let snapshot = MergeSnapshot {
            reviews: vec![review("carol", ReviewState::Approved, HEAD)],
            ..reviewing()
        };

        assert_eq!(
            approve_and_ship(&snapshot),
            plan(Approval::Approve, Ship::AutoMerge(MergeMethod::Squash))
        );
    }
}
