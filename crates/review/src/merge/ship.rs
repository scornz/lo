//! Approve and ship, `⌘⇧⏎` (design §9, "Approve and ship"; §11, "Acting on what you saw").
//!
//! It runs in two steps, so what it does after approving rests on GitHub's verdict, not rozed's
//! prediction:
//!
//! 1. [`approve_and_ship`], on the snapshot read right before it runs, refuses or returns a
//!    [`Plan`]: the head to approve, whether to approve it, and what rozed expects to happen next,
//!    which only words the confirmation.
//! 2. Once the approval lands, the caller reads the pull request again and calls
//!    [`after_approving`] with that snapshot and the plan. It merges or joins the queue only when
//!    GitHub reports the pull request can merge. Otherwise it turns on auto-merge, or stops.

use super::reviews;
use super::{
    Blocker, CheckState, MergeMethod, MergeSnapshot, MergeStateStatus, PullRequestState, Review,
    ReviewDecision, ReviewState, Reviewer, blockers,
};

/// Approve and ship's first step: nothing, and why, or the plan its one confirmation lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApproveAndShip {
    /// Nothing: someone pushed since the revision on screen. rozed shows the new revision, with
    /// the changes since the one you saw.
    HeadMoved { on_screen: String, head: String },
    /// Nothing: GitHub's head or the head on screen is missing, so nothing would make GitHub check
    /// that the approval and the merge act on what you saw.
    HeadUnknown,
    /// Nothing: it's merged.
    Merged,
    /// Nothing: it's closed.
    Closed,
    /// Nothing: it's a draft. Its author hasn't said it's ready, so there's nothing to ship.
    /// Approving a draft is left to `⌘⏎`.
    Draft,
    /// Nothing: GitHub doesn't let you approve it, and it isn't yours.
    CannotApprove,
    /// Approve unless that's skipped, then call [`after_approving`].
    Plan(Plan),
}

/// What approve and ship approves, and what rozed expects to happen next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// The head on screen, which is GitHub's head too. Approving approves it. Merging, joining the
    /// queue or turning on auto-merge expects it, so GitHub refuses if someone pushes in between,
    /// and [`after_approving`] stops if GitHub's head is no longer it.
    pub head: String,
    /// The first step.
    pub approval: Approval,
    /// What rozed expects [`after_approving`] to return, from the snapshot as it expects GitHub to
    /// report it once you've approved. It only words the confirmation ([`Plan::wording`]): GitHub
    /// can still block the merge for a rule rozed can't see, and then approve and ship turns on
    /// auto-merge instead of merging.
    pub likely: Ship,
    /// Whether the confirmation warns that a push before it merges could merge without a new
    /// review. GitHub keeps auto-merge on when someone with write access pushes, so it warns when
    /// the next step may be auto-merge and the base branch's rules don't want a new approval after
    /// a push ([`ReviewRules::push_needs_new_approval`]). The next step may be auto-merge when it's
    /// likely, and also when a merge is likely only because rozed expects GitHub to stop blocking
    /// once you've approved. While rozed runs, [`after_push`] turns auto-merge off on such a push.
    ///
    /// [`ReviewRules::push_needs_new_approval`]: super::ReviewRules::push_needs_new_approval
    pub stale_approval_warning: bool,
}

impl Plan {
    /// What the confirmation says follows the approval, from [`likely`](Self::likely), such as
    /// "will likely merge" or "will likely wait for checks, with auto-merge on", and then the
    /// stale-approval warning when there is one.
    pub fn wording(&self) -> String {
        let next = self.next();
        if self.stale_approval_warning {
            format!("{next}. While auto-merge is on, a push could merge without a new review")
        } else {
            next
        }
    }

    fn next(&self) -> String {
        match &self.likely {
            Ship::Merge(_) => "will likely merge".to_string(),
            Ship::Enqueue => "will likely join the merge queue".to_string(),
            Ship::AutoMerge { waiting_on, .. } => {
                format!(
                    "will likely wait for {}, with auto-merge on",
                    waits(waiting_on)
                )
            }
            Ship::EnqueueWhenReady { waiting_on } => format!(
                "will likely wait for {}, then join the merge queue",
                waits(waiting_on)
            ),
            Ship::AutoMergeAlreadyOn { waiting_on } if waiting_on.is_empty() => {
                "will likely merge: auto-merge is on".to_string()
            }
            Ship::AutoMergeAlreadyOn { waiting_on } => format!(
                "will likely wait for {}, with auto-merge on already",
                waits(waiting_on)
            ),
            Ship::AlreadyQueued => "is in the merge queue already".to_string(),
            Ship::Stop(stop) => format!(
                "will likely stop: {}",
                match stop {
                    Stop::HeadMoved { .. } => "someone pushed after the approval",
                    Stop::HeadUnknown => "GitHub didn't report the head",
                    Stop::Blocked { .. } => "something waiting won't fix blocks it",
                    Stop::NoAutoMerge(_) => "you can't turn on auto-merge",
                    Stop::NoPermission => "you can't merge into the base branch",
                    Stop::NoMethod => "the repository allows no merge method",
                }
            ),
        }
    }
}

/// What waiting blockers wait for, as a list for the confirmation: "checks", "an approval and
/// checks".
fn waits(blockers: &[Blocker]) -> String {
    let mut kinds: Vec<&str> = Vec::new();
    for blocker in blockers {
        let kind = match blocker {
            Blocker::Approvals(missing) if missing.more == 1 && missing.code_owners.is_empty() => {
                "an approval"
            }
            Blocker::Approvals(_) => "approvals",
            Blocker::Check(check) if check.state == CheckState::AwaitingApproval => {
                "a deployment's approval"
            }
            Blocker::Check(_) => "checks",
            Blocker::Rule(_) => "other rules",
            Blocker::CheckingMergeability => "GitHub to check mergeability",
            _ => continue,
        };
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    match kinds.as_slice() {
        [] => "GitHub".to_string(),
        [one] => one.to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
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
    /// Merge it with this method. [`after_approving`] returns it only when GitHub reports the pull
    /// request can merge (`CLEAN`, `HAS_HOOKS` or `UNSTABLE`) and nothing rozed sees blocks it, so it
    /// never relies on an admin's bypass.
    Merge(MergeMethod),
    /// Add it to the base branch's merge queue, on the same terms as [`Ship::Merge`].
    Enqueue,
    /// Turn on auto-merge with this method, so GitHub merges it once what it's waiting on clears:
    /// `waiting_on`, every blocker left, each one waiting clears. A `BLOCKED` can also hide a rule
    /// rozed can't see, which auto-merge waits for too.
    ///
    /// If GitHub refuses because the pull request can merge already ("clean status" or "unstable
    /// status"), read it again and call [`after_approving`] again. Never merge on any other
    /// refusal: for an admin, that merge could skip a rule.
    AutoMerge {
        method: MergeMethod,
        waiting_on: Vec<Blocker>,
    },
    /// Turn on auto-merge on a branch with a merge queue (GitHub's "Merge when ready"), so GitHub
    /// adds it to the queue once `waiting_on` clears. Refusals are handled as for
    /// [`Ship::AutoMerge`].
    EnqueueWhenReady { waiting_on: Vec<Blocker> },
    /// Nothing: auto-merge is on already, so GitHub merges it, or adds it to the queue, once
    /// `waiting_on` clears, or straight away when it's empty.
    AutoMergeAlreadyOn { waiting_on: Vec<Blocker> },
    /// Nothing: it's in the merge queue already.
    AlreadyQueued,
    /// Stop, and show why.
    Stop(Stop),
}

/// Why approve and ship stops.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stop {
    /// GitHub's head isn't the one approved: someone pushed after the approval. rozed shows the new
    /// revision.
    HeadMoved { approved: String, head: String },
    /// GitHub reported no head, so rozed can't tell that it's still the one approved.
    HeadUnknown,
    /// Something waiting won't fix blocks it. These are every blocker, those waiting won't fix
    /// first, as [`blockers`](super::blockers) orders them. With `auto_merge`, auto-merge is on, so
    /// GitHub merges it if they all clear while it stays on.
    Blocked {
        blockers: Vec<Blocker>,
        auto_merge: bool,
    },
    /// Only things waiting will fix block it, but you can't turn on auto-merge: the repository
    /// doesn't allow it, or you lack write access. These are the blockers.
    NoAutoMerge(Vec<Blocker>),
    /// GitHub lets it merge, but you can't merge into the base branch.
    NoPermission,
    /// The repository allows no merge method.
    NoMethod,
}

/// Approve and ship's first step, from a snapshot read right before it runs.
///
/// - It does nothing to a merged or closed pull request, or to a draft. It does nothing when
///   GitHub's head or the head on screen is missing, or GitHub's head isn't the head on screen, so
///   it never approves what you didn't see. Heads are full commit SHAs, compared ignoring case.
/// - It approves the head on screen. It skips that on your own pull request, and when your latest
///   review approves that head already. An approval of an older revision is given again, so
///   GitHub records that you approved what you saw. When GitHub doesn't let you approve someone
///   else's pull request, it does nothing.
/// - [`Plan::likely`] is what [`after_approving`] would return if GitHub reported what rozed
///   expects once you've approved. It's only a prediction, for the confirmation's wording.
pub fn approve_and_ship(snapshot: &MergeSnapshot) -> ApproveAndShip {
    match snapshot.state {
        PullRequestState::Merged => return ApproveAndShip::Merged,
        PullRequestState::Closed => return ApproveAndShip::Closed,
        PullRequestState::Open => {}
    }
    if snapshot.draft || snapshot.merge_state == MergeStateStatus::Draft {
        return ApproveAndShip::Draft;
    }
    if snapshot.head.is_empty() || snapshot.head_on_screen.is_empty() {
        return ApproveAndShip::HeadUnknown;
    }
    if !same_commit(&snapshot.head, &snapshot.head_on_screen) {
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
            && (review.commit.as_deref())
                .is_some_and(|commit| same_commit(commit, &snapshot.head_on_screen))
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
    let likely = ship(&after);
    let may_auto_merge = match &likely {
        Ship::AutoMerge { .. }
        | Ship::EnqueueWhenReady { .. }
        | Ship::AutoMergeAlreadyOn { .. } => true,
        Ship::Merge(_) | Ship::Enqueue => !github_lets_it_merge(snapshot),
        Ship::AlreadyQueued | Ship::Stop(_) => false,
    };
    ApproveAndShip::Plan(Plan {
        head: snapshot.head_on_screen.clone(),
        approval,
        likely,
        stale_approval_warning: may_auto_merge && !snapshot.rules.push_needs_new_approval(),
    })
}

/// Approve and ship's second step: what to do once approved, from the pull request read again
/// after the approval. When [`Plan::approval`] skips approving, the snapshot [`approve_and_ship`]
/// read will do.
///
/// - It stops if GitHub reports no head, or a head that isn't the plan's.
/// - It does nothing more when the pull request is in the queue already.
/// - It stops, with the blockers, when something waiting won't fix blocks the merge.
/// - It does nothing more when auto-merge is on.
/// - It merges with the preferred method, or joins the queue on a branch that requires one, only
///   when GitHub reports the pull request can merge (`CLEAN`, `HAS_HOOKS` or `UNSTABLE`, or `BEHIND`
///   on a queue branch) and nothing rozed sees blocks it. Those are the states in which GitHub's rules are met, so approve
///   and ship never merges past a rule, even for an admin GitHub would let bypass one.
/// - Otherwise only things waiting will fix block it, and it turns on auto-merge, or merge when
///   ready on a queue branch. When rozed's explanation of a `BLOCKED` is only partial, auto-merge
///   waits for what it can't see too.
pub fn after_approving(snapshot: &MergeSnapshot, plan: &Plan) -> Ship {
    if snapshot.head.is_empty() {
        return Ship::Stop(Stop::HeadUnknown);
    }
    if !same_commit(&snapshot.head, &plan.head) {
        return Ship::Stop(Stop::HeadMoved {
            approved: plan.head.clone(),
            head: snapshot.head.clone(),
        });
    }
    ship(snapshot)
}

/// What to do about a push to a pull request on which approve and ship approved [`Plan::head`]
/// and then turned on auto-merge, or merge when ready.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AfterPush {
    /// Turn auto-merge off, and tell the user: `head` was pushed after they approved `approved`,
    /// and it would merge with their approval.
    TurnOffAutoMerge { approved: String, head: String },
    /// Nothing.
    Nothing,
}

/// What to do when the poller sees a pull request on which approve and ship turned on auto-merge,
/// or merge when ready, with this plan.
///
/// GitHub keeps auto-merge on when someone with write access pushes. Unless the base branch's
/// rules want a new approval after a push ([`ReviewRules::push_needs_new_approval`]), the new head
/// then merges with the approvals already given, yours included. So when GitHub's head isn't the
/// one you approved, and auto-merge is still on, rozed turns it off and says so. It leaves it on
/// when you approved nothing (your own pull request), when someone else has turned it on since,
/// and once the pull request is merged or closed.
///
/// [`ReviewRules::push_needs_new_approval`]: super::ReviewRules::push_needs_new_approval
pub fn after_push(snapshot: &MergeSnapshot, plan: &Plan) -> AfterPush {
    let yours = |login: &str| reviews::same_login(login, &snapshot.viewer.login);
    let turn_off = snapshot.state == PullRequestState::Open
        && plan.approval != Approval::OwnPullRequest
        && !snapshot.rules.push_needs_new_approval()
        && (snapshot.auto_merge.as_ref())
            .is_some_and(|auto_merge| auto_merge.enabled_by.as_deref().is_none_or(yours))
        && !snapshot.head.is_empty()
        && !same_commit(&snapshot.head, &plan.head);
    if turn_off {
        AfterPush::TurnOffAutoMerge {
            approved: plan.head.clone(),
            head: snapshot.head.clone(),
        }
    } else {
        AfterPush::Nothing
    }
}

/// Whether two full commit SHAs name the same commit. Git writes them in lowercase, but nothing
/// here depends on it.
fn same_commit(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// What to do once approved, from the snapshot as GitHub reports it, or as rozed expects it to.
fn ship(snapshot: &MergeSnapshot) -> Ship {
    if snapshot.queue.position.is_some() {
        return Ship::AlreadyQueued;
    }
    let blockers = blockers(snapshot);
    let auto_merge = snapshot.auto_merge.is_some();
    if blockers.iter().any(|blocker| !blocker.waiting()) {
        return Ship::Stop(Stop::Blocked {
            blockers,
            auto_merge,
        });
    }
    if auto_merge {
        return Ship::AutoMergeAlreadyOn {
            waiting_on: blockers,
        };
    }
    let viewer = &snapshot.viewer;
    let method = snapshot.methods.choose(snapshot.preferred_method);
    if blockers.is_empty() && github_lets_it_merge(snapshot) {
        if !viewer.can_merge {
            Ship::Stop(Stop::NoPermission)
        } else if snapshot.queue.required {
            Ship::Enqueue
        } else {
            method.map_or(Ship::Stop(Stop::NoMethod), Ship::Merge)
        }
    } else if snapshot.queue.required && (viewer.can_merge || viewer.can_enable_auto_merge) {
        Ship::EnqueueWhenReady {
            waiting_on: blockers,
        }
    } else if !snapshot.queue.required && viewer.can_enable_auto_merge {
        match method {
            Some(method) => Ship::AutoMerge {
                method,
                waiting_on: blockers,
            },
            None => Ship::Stop(Stop::NoMethod),
        }
    } else {
        Ship::Stop(Stop::NoAutoMerge(blockers))
    }
}

/// Whether GitHub reports the pull request can merge: the merge states `gh pr merge` also merges
/// on. In each, the base branch's rules are met, so merging never relies on an admin's bypass. On
/// a branch that requires a merge queue, `BEHIND` lets it join the queue too: GitHub reports it
/// only when nothing else blocks, and the queue tests each pull request on the latest base.
fn github_lets_it_merge(snapshot: &MergeSnapshot) -> bool {
    match snapshot.merge_state {
        MergeStateStatus::Clean | MergeStateStatus::HasHooks | MergeStateStatus::Unstable => true,
        MergeStateStatus::Behind => snapshot.queue.required,
        MergeStateStatus::Blocked
        | MergeStateStatus::Dirty
        | MergeStateStatus::Draft
        | MergeStateStatus::Unknown => false,
    }
}

/// The snapshot as rozed expects GitHub to report it after you approve the head on screen. It's
/// only for [`Plan::likely`].
///
/// - Your approval replaces your latest review, and GitHub drops the request for your review.
/// - It covers the code-owner requests for you and for your teams, when it counts.
/// - GitHub's review decision stands unless your approval is provably what changes it. Approved
///   stays approved, and no decision stays none. Changes requested stays, unless you were the only
///   one rozed sees requesting them. Then, as from review required, it's review required while
///   approvals are missing, or while GitHub required a review rozed couldn't attribute, which rozed
///   assumes your approval doesn't give; else approved.
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

    let requesters = reviews::changes_requested_by(snapshot);
    let only_you_requested = !requesters.is_empty() && requesters.iter().all(|login| yours(login));
    let unattributed = snapshot.review_decision == Some(ReviewDecision::ReviewRequired)
        && reviews::missing_approvals(snapshot).is_empty();
    after.review_decision = snapshot.review_decision.map(|decision| match decision {
        ReviewDecision::Approved => ReviewDecision::Approved,
        ReviewDecision::ChangesRequested if !only_you_requested => decision,
        ReviewDecision::ChangesRequested | ReviewDecision::ReviewRequired => {
            if unattributed || !reviews::missing_approvals(&after).is_empty() {
                ReviewDecision::ReviewRequired
            } else {
                ReviewDecision::Approved
            }
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

    const HEAD: &str = "c0ffee1c0ffee1c0ffee1c0ffee1c0ffee1c0ffe";
    const OLD_HEAD: &str = "0d1d4ead0d1d4ead0d1d4ead0d1d4ead0d1d4ead";
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
                dismiss_stale_reviews: true,
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
                ..reviewing().rules
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

    /// An admin who could merge past the base branch's rules.
    fn as_admin(mut snapshot: MergeSnapshot) -> MergeSnapshot {
        snapshot.viewer.can_merge_as_admin = true;
        snapshot
    }

    /// The pull request as GitHub reports it once your approval of the head has landed, with
    /// GitHub's review decision and merge state then.
    fn approved(snapshot: MergeSnapshot, decision: &str, merge_state: &str) -> MergeSnapshot {
        let mut after = snapshot;
        let login = after.viewer.login.clone();
        after
            .reviews
            .push(review(&login, ReviewState::Approved, HEAD));
        after.review_decision = decision.parse().ok();
        after.merge_state = merge_state.parse().unwrap();
        after
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

    fn plan(approval: Approval, likely: Ship) -> ApproveAndShip {
        ApproveAndShip::Plan(Plan {
            head: HEAD.to_string(),
            approval,
            likely,
            stale_approval_warning: false,
        })
    }

    /// The base branch's rules keep approvals when someone pushes.
    fn keeping_stale_approvals(mut snapshot: MergeSnapshot) -> MergeSnapshot {
        snapshot.rules.dismiss_stale_reviews = false;
        snapshot
    }

    /// Auto-merge, turned on by `by`.
    fn auto_merge_on(by: Option<&str>) -> Option<AutoMerge> {
        Some(AutoMerge {
            method: MergeMethod::Squash,
            enabled_by: by.map(str::to_string),
        })
    }

    /// The plan for a snapshot that gets one.
    fn the_plan(snapshot: &MergeSnapshot) -> Plan {
        match approve_and_ship(snapshot) {
            ApproveAndShip::Plan(plan) => plan,
            other => panic!("expected a plan, got {other:?}"),
        }
    }

    fn approvals(more: u32) -> Blocker {
        Blocker::Approvals(MissingApprovals {
            more,
            code_owners: Vec::new(),
        })
    }

    fn auto_merge(waiting_on: Vec<Blocker>) -> Ship {
        Ship::AutoMerge {
            method: MergeMethod::Squash,
            waiting_on,
        }
    }

    fn blocked(blockers: Vec<Blocker>) -> Ship {
        Ship::Stop(Stop::Blocked {
            blockers,
            auto_merge: false,
        })
    }

    // The first step: what it approves, and what it expects to happen next.

    #[test]
    fn it_approves_and_expects_to_merge_when_your_approval_is_all_that_is_missing() {
        assert_eq!(
            approve_and_ship(&reviewing()),
            plan(Approval::Approve, Ship::Merge(MergeMethod::Squash))
        );
    }

    #[test]
    fn it_expects_the_first_allowed_method_without_a_preference() {
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
    fn it_expects_to_join_the_queue_on_a_branch_that_requires_one() {
        assert_eq!(
            approve_and_ship(&with_queue(reviewing())),
            plan(Approval::Approve, Ship::Enqueue)
        );
    }

    #[test]
    fn it_does_nothing_when_the_head_moved_since_what_is_on_screen() {
        let snapshot = MergeSnapshot {
            head_on_screen: OLD_HEAD.to_string(),
            ..reviewing()
        };

        assert_eq!(
            approve_and_ship(&snapshot),
            ApproveAndShip::HeadMoved {
                on_screen: OLD_HEAD.to_string(),
                head: HEAD.to_string(),
            }
        );
    }

    #[test]
    fn it_does_nothing_without_a_head_to_approve() {
        for (head, on_screen) in [("", HEAD), (HEAD, ""), ("", "")] {
            let snapshot = MergeSnapshot {
                head: head.to_string(),
                head_on_screen: on_screen.to_string(),
                ..reviewing()
            };

            assert_eq!(
                approve_and_ship(&snapshot),
                ApproveAndShip::HeadUnknown,
                "{head:?} {on_screen:?}"
            );
        }
    }

    #[test]
    fn heads_are_the_same_whatever_their_case() {
        let snapshot = MergeSnapshot {
            head: HEAD.to_uppercase(),
            merge_state: MergeStateStatus::Clean,
            review_decision: Some(ReviewDecision::Approved),
            reviews: vec![review("alice", ReviewState::Approved, &HEAD.to_uppercase())],
            ..reviewing()
        };
        let plan = the_plan(&snapshot);
        let after = MergeSnapshot {
            head: HEAD.to_uppercase(),
            ..snapshot
        };

        assert_eq!(plan.approval, Approval::AlreadyApproved);
        assert_eq!(
            after_approving(&after, &plan),
            Ship::Merge(MergeMethod::Squash)
        );
    }

    #[test]
    fn it_does_nothing_to_a_draft() {
        let draft = MergeSnapshot {
            draft: true,
            ..reviewing()
        };
        let draft_as_rest_reports_it = MergeSnapshot {
            merge_state: MergeStateStatus::Draft,
            ..reviewing()
        };

        assert_eq!(approve_and_ship(&draft), ApproveAndShip::Draft);
        assert_eq!(
            approve_and_ship(&draft_as_rest_reports_it),
            ApproveAndShip::Draft
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
            head_on_screen: OLD_HEAD.to_string(),
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
    fn it_skips_approving_your_own_pull_request_and_expects_to_merge() {
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
    fn your_own_pull_request_waiting_on_an_approval_expects_auto_merge() {
        let mut snapshot = reviewing();
        snapshot.viewer.did_author = true;

        assert_eq!(
            approve_and_ship(&snapshot),
            plan(Approval::OwnPullRequest, auto_merge(vec![approvals(1)]))
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
            reviews: vec![review("alice", ReviewState::Approved, OLD_HEAD)],
            ..reviewing()
        };

        assert_eq!(
            approve_and_ship(&snapshot),
            plan(Approval::Approve, Ship::Merge(MergeMethod::Squash))
        );
    }

    #[test]
    fn it_expects_auto_merge_when_another_approval_is_still_needed() {
        assert_eq!(
            approve_and_ship(&needing_another_approval()),
            plan(Approval::Approve, auto_merge(vec![approvals(1)]))
        );
    }

    #[test]
    fn your_approval_and_one_already_given_are_expected_to_meet_the_count() {
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
    fn it_expects_to_join_the_queue_when_ready_on_a_branch_that_requires_one() {
        assert_eq!(
            approve_and_ship(&with_queue(needing_another_approval())),
            plan(
                Approval::Approve,
                Ship::EnqueueWhenReady {
                    waiting_on: vec![approvals(1)]
                }
            )
        );
    }

    #[test]
    fn your_approval_is_expected_to_cover_your_teams_code_owner_review() {
        let snapshot = MergeSnapshot {
            rules: ReviewRules {
                required_approvals: 1,
                code_owner_review: true,
                ..reviewing().rules
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
    fn another_teams_code_owner_review_is_expected_to_be_waited_on() {
        let snapshot = MergeSnapshot {
            rules: ReviewRules {
                required_approvals: 1,
                code_owner_review: true,
                ..reviewing().rules
            },
            review_requests: vec![code_owner("rozed-dev/github"), code_owner("rozed-dev/ui")],
            ..reviewing()
        };

        assert_eq!(
            approve_and_ship(&snapshot),
            plan(
                Approval::Approve,
                auto_merge(vec![Blocker::Approvals(MissingApprovals {
                    more: 0,
                    code_owners: vec![Reviewer::Team("rozed-dev/ui".to_string())],
                })])
            )
        );
    }

    #[test]
    fn your_approval_is_expected_to_replace_your_request_for_changes() {
        let snapshot = MergeSnapshot {
            review_decision: Some(ReviewDecision::ChangesRequested),
            reviews: vec![review("alice", ReviewState::ChangesRequested, OLD_HEAD)],
            ..reviewing()
        };

        assert_eq!(
            approve_and_ship(&snapshot),
            plan(Approval::Approve, Ship::Merge(MergeMethod::Squash))
        );
    }

    #[test]
    fn it_expects_to_stop_when_someone_else_requested_changes() {
        let snapshot = MergeSnapshot {
            review_decision: Some(ReviewDecision::ChangesRequested),
            reviews: vec![review("carol", ReviewState::ChangesRequested, OLD_HEAD)],
            ..needing_another_approval()
        };

        assert_eq!(
            approve_and_ship(&snapshot),
            plan(
                Approval::Approve,
                blocked(vec![
                    Blocker::ChangesRequested {
                        by: vec!["carol".to_string()]
                    },
                    approvals(1),
                ])
            )
        );
    }

    #[test]
    fn changes_requested_by_someone_rozed_cannot_name_are_expected_to_stay() {
        // Carol's request counts for GitHub, but rozed reads her as without write access: she
        // lost it after reviewing, or her review is past the page fetched.
        let snapshot = MergeSnapshot {
            review_decision: Some(ReviewDecision::ChangesRequested),
            reviews: vec![Review {
                counts: false,
                ..review("carol", ReviewState::ChangesRequested, HEAD)
            }],
            ..reviewing()
        };

        assert_eq!(
            approve_and_ship(&snapshot),
            plan(
                Approval::Approve,
                blocked(vec![Blocker::ChangesRequested { by: vec![] }])
            )
        );
    }

    #[test]
    fn changes_requested_by_you_and_someone_else_are_expected_to_stay() {
        let snapshot = MergeSnapshot {
            review_decision: Some(ReviewDecision::ChangesRequested),
            reviews: vec![
                review("alice", ReviewState::ChangesRequested, OLD_HEAD),
                review("carol", ReviewState::ChangesRequested, OLD_HEAD),
            ],
            ..reviewing()
        };

        assert_eq!(
            approve_and_ship(&snapshot),
            plan(
                Approval::Approve,
                blocked(vec![Blocker::ChangesRequested {
                    by: vec!["carol".to_string()]
                }])
            )
        );
    }

    #[test]
    fn github_approving_is_expected_to_stand_after_your_approval() {
        // Erin and Grace own the same files. Erin approved, which covers them, but GitHub keeps
        // Grace's request, so rozed alone would read her approval as missing.
        let snapshot = MergeSnapshot {
            merge_state: MergeStateStatus::Clean,
            review_decision: Some(ReviewDecision::Approved),
            rules: ReviewRules {
                required_approvals: 1,
                code_owner_review: true,
                ..reviewing().rules
            },
            reviews: vec![review("erin", ReviewState::Approved, HEAD)],
            review_requests: vec![ReviewRequest {
                reviewer: Reviewer::User("grace".to_string()),
                as_code_owner: true,
            }],
            ..reviewing()
        };
        let mut without_auto_merge = snapshot.clone();
        without_auto_merge.viewer.can_enable_auto_merge = false;

        assert_eq!(
            approve_and_ship(&snapshot),
            plan(Approval::Approve, Ship::Merge(MergeMethod::Squash))
        );
        assert_eq!(
            approve_and_ship(&without_auto_merge),
            plan(Approval::Approve, Ship::Merge(MergeMethod::Squash))
        );
    }

    #[test]
    fn it_expects_to_stop_when_a_required_check_failed() {
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
                blocked(vec![
                    Blocker::Check(check("build", CheckState::Failed)),
                    Blocker::Check(check("test", CheckState::Running)),
                ])
            )
        );
    }

    #[test]
    fn it_expects_to_stop_on_conflicts_or_an_out_of_date_branch() {
        let conflicting = MergeSnapshot {
            mergeable: MergeableState::Conflicting,
            merge_state: MergeStateStatus::Dirty,
            ..reviewing()
        };
        let behind = MergeSnapshot {
            out_of_date: true,
            ..reviewing()
        };

        for (snapshot, blocker) in [
            (conflicting, Blocker::Conflicts),
            (behind, Blocker::OutOfDate),
        ] {
            assert_eq!(
                approve_and_ship(&snapshot),
                plan(Approval::Approve, blocked(vec![blocker]))
            );
        }
    }

    #[test]
    fn it_expects_auto_merge_while_required_checks_run() {
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
            plan(
                Approval::Approve,
                auto_merge(vec![
                    Blocker::Check(check("test", CheckState::Running)),
                    Blocker::Check(check("e2e", CheckState::Missing)),
                ])
            )
        );
    }

    #[test]
    fn a_check_nothing_will_report_is_expected_to_stop_it() {
        let snapshot = MergeSnapshot {
            required_checks: vec![check("e2e", CheckState::Missing)],
            ..reviewing()
        };

        assert_eq!(
            approve_and_ship(&snapshot),
            plan(
                Approval::Approve,
                blocked(vec![Blocker::NotReported(check(
                    "e2e",
                    CheckState::Missing
                ))])
            )
        );
    }

    #[test]
    fn it_expects_auto_merge_while_github_works_out_mergeability() {
        let snapshot = MergeSnapshot {
            mergeable: MergeableState::Unknown,
            merge_state: MergeStateStatus::Unknown,
            ..reviewing()
        };

        assert_eq!(
            approve_and_ship(&snapshot),
            plan(
                Approval::Approve,
                auto_merge(vec![Blocker::CheckingMergeability])
            )
        );
    }

    #[test]
    fn it_expects_to_stop_when_you_cannot_turn_on_auto_merge() {
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
            plan(
                Approval::Approve,
                Ship::EnqueueWhenReady {
                    waiting_on: vec![approvals(1)]
                }
            )
        );
    }

    #[test]
    fn it_expects_nothing_more_when_auto_merge_is_already_on() {
        let on = Some(AutoMerge {
            method: MergeMethod::Merge,
            enabled_by: Some("bob".to_string()),
        });
        let waiting = MergeSnapshot {
            auto_merge: on.clone(),
            ..needing_another_approval()
        };
        let ready = MergeSnapshot {
            auto_merge: on,
            ..reviewing()
        };

        assert_eq!(
            approve_and_ship(&waiting),
            plan(
                Approval::Approve,
                Ship::AutoMergeAlreadyOn {
                    waiting_on: vec![approvals(1)]
                }
            )
        );
        assert_eq!(
            approve_and_ship(&ready),
            plan(
                Approval::Approve,
                Ship::AutoMergeAlreadyOn { waiting_on: vec![] }
            )
        );
    }

    #[test]
    fn it_expects_nothing_more_when_the_pull_request_is_already_queued() {
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
    fn it_expects_to_stop_when_you_cannot_merge() {
        let mut snapshot = reviewing();
        snapshot.viewer.can_merge = false;

        assert_eq!(
            approve_and_ship(&snapshot),
            plan(Approval::Approve, Ship::Stop(Stop::NoPermission))
        );
    }

    #[test]
    fn it_expects_to_stop_when_no_method_is_allowed() {
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
    fn an_approval_without_write_access_is_expected_to_leave_the_approval_missing() {
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
    fn a_block_rozed_cannot_explain_is_expected_to_stop_it() {
        let snapshot = MergeSnapshot {
            review_decision: Some(ReviewDecision::Approved),
            reviews: vec![review("carol", ReviewState::Approved, HEAD)],
            ..reviewing()
        };

        assert_eq!(
            approve_and_ship(&snapshot),
            plan(
                Approval::Approve,
                blocked(vec![Blocker::Unexplained {
                    url: URL.to_string()
                }])
            )
        );
    }

    #[test]
    fn a_review_github_requires_for_a_reason_rozed_cannot_see_is_expected_to_be_waited_on() {
        let snapshot = MergeSnapshot {
            reviews: vec![review("carol", ReviewState::Approved, HEAD)],
            ..reviewing()
        };

        assert_eq!(
            approve_and_ship(&snapshot),
            plan(Approval::Approve, auto_merge(vec![approvals(0)]))
        );
    }

    // The confirmation's wording.

    #[test]
    fn the_confirmation_words_what_is_likely_to_happen() {
        let checks_running = MergeSnapshot {
            required_checks: vec![check("test", CheckState::Running)],
            ..reviewing()
        };
        let approval_checks_and_mergeability = MergeSnapshot {
            mergeable: MergeableState::Unknown,
            required_checks: vec![check("test", CheckState::Running)],
            ..needing_another_approval()
        };
        let auto_merge_on = MergeSnapshot {
            auto_merge: Some(AutoMerge {
                method: MergeMethod::Squash,
                enabled_by: None,
            }),
            ..reviewing()
        };
        let deployment = MergeSnapshot {
            required_checks: vec![check("deploy", CheckState::AwaitingApproval)],
            ..reviewing()
        };
        let mut cannot_merge = reviewing();
        cannot_merge.viewer.can_merge = false;

        for (snapshot, wording) in [
            (reviewing(), "will likely merge"),
            (with_queue(reviewing()), "will likely join the merge queue"),
            (
                checks_running,
                "will likely wait for checks, with auto-merge on",
            ),
            (
                approval_checks_and_mergeability,
                "will likely wait for an approval, checks and GitHub to check mergeability, with \
                 auto-merge on",
            ),
            (
                with_queue(needing_another_approval()),
                "will likely wait for an approval, then join the merge queue",
            ),
            (
                deployment,
                "will likely wait for a deployment's approval, with auto-merge on",
            ),
            (auto_merge_on, "will likely merge: auto-merge is on"),
            (
                MergeSnapshot {
                    out_of_date: true,
                    ..reviewing()
                },
                "will likely stop: something waiting won't fix blocks it",
            ),
            (
                cannot_merge,
                "will likely stop: you can't merge into the base branch",
            ),
        ] {
            assert_eq!(the_plan(&snapshot).wording(), wording);
        }
    }

    // Pushes while auto-merge is on.

    #[test]
    fn the_confirmation_warns_when_a_later_push_could_merge_with_your_approval() {
        let waiting = keeping_stale_approvals(needing_another_approval());
        let plan = the_plan(&waiting);

        assert_eq!(plan.likely, auto_merge(vec![approvals(1)]));
        assert!(plan.stale_approval_warning);
        assert_eq!(
            plan.wording(),
            "will likely wait for an approval, with auto-merge on. While auto-merge is on, a push \
             could merge without a new review"
        );
    }

    #[test]
    fn the_confirmation_warns_whenever_the_next_step_may_be_auto_merge() {
        let mut queue = keeping_stale_approvals(with_queue(needing_another_approval()));
        queue.viewer.can_enable_auto_merge = false;
        let already_on = MergeSnapshot {
            auto_merge: auto_merge_on(Some("bob")),
            ..keeping_stale_approvals(needing_another_approval())
        };
        // Expected to merge, but only if GitHub agrees once you've approved: otherwise auto-merge.
        let merge_if_github_agrees = keeping_stale_approvals(reviewing());
        let no_reviews_required = MergeSnapshot {
            review_decision: None,
            rules: ReviewRules {
                dismiss_stale_reviews: true,
                ..Default::default()
            },
            required_checks: vec![check("test", CheckState::Running)],
            ..reviewing()
        };

        for snapshot in [
            queue,
            already_on,
            merge_if_github_agrees,
            no_reviews_required,
        ] {
            assert!(
                the_plan(&snapshot).stale_approval_warning,
                "{:?}",
                the_plan(&snapshot).likely
            );
        }
    }

    #[test]
    fn the_confirmation_does_not_warn_when_a_push_needs_a_new_approval_or_it_will_not_wait() {
        let dismissing = needing_another_approval();
        let latest_push_approved = MergeSnapshot {
            rules: ReviewRules {
                last_push_approval: true,
                ..keeping_stale_approvals(needing_another_approval()).rules
            },
            ..needing_another_approval()
        };
        let clean = MergeSnapshot {
            merge_state: MergeStateStatus::Clean,
            review_decision: Some(ReviewDecision::Approved),
            reviews: vec![review("carol", ReviewState::Approved, HEAD)],
            ..keeping_stale_approvals(reviewing())
        };
        let stopping = MergeSnapshot {
            out_of_date: true,
            ..keeping_stale_approvals(reviewing())
        };

        for snapshot in [dismissing, latest_push_approved, clean, stopping] {
            let plan = the_plan(&snapshot);
            assert!(!plan.stale_approval_warning, "{:?}", plan.likely);
            assert!(!plan.wording().contains("push"), "{}", plan.wording());
        }
    }

    #[test]
    fn a_push_after_you_approved_turns_off_the_auto_merge_rozed_turned_on() {
        let plan = the_plan(&keeping_stale_approvals(needing_another_approval()));
        let pushed = "5ca1ab1e5ca1ab1e5ca1ab1e5ca1ab1e5ca1ab1e";
        for enabled_by in [Some("alice"), Some("ALICE"), None] {
            let snapshot = MergeSnapshot {
                head: pushed.to_string(),
                auto_merge: auto_merge_on(enabled_by),
                ..keeping_stale_approvals(needing_another_approval())
            };

            assert_eq!(
                after_push(&snapshot, &plan),
                AfterPush::TurnOffAutoMerge {
                    approved: HEAD.to_string(),
                    head: pushed.to_string(),
                },
                "{enabled_by:?}"
            );
        }
    }

    #[test]
    fn a_push_leaves_auto_merge_alone_otherwise() {
        let waiting = keeping_stale_approvals(needing_another_approval());
        let plan = the_plan(&waiting);
        let pushed = |snapshot: MergeSnapshot| MergeSnapshot {
            head: "5ca1ab1e5ca1ab1e5ca1ab1e5ca1ab1e5ca1ab1e".to_string(),
            auto_merge: auto_merge_on(Some("alice")),
            ..snapshot
        };
        let mut own = plan.clone();
        own.approval = Approval::OwnPullRequest;

        let cases = [
            (
                "same head",
                plan.clone(),
                MergeSnapshot {
                    auto_merge: auto_merge_on(Some("alice")),
                    ..waiting.clone()
                },
            ),
            (
                "no head",
                plan.clone(),
                MergeSnapshot {
                    head: String::new(),
                    ..pushed(waiting.clone())
                },
            ),
            (
                "auto-merge off",
                plan.clone(),
                MergeSnapshot {
                    auto_merge: None,
                    ..pushed(waiting.clone())
                },
            ),
            (
                "someone else turned it on",
                plan.clone(),
                MergeSnapshot {
                    auto_merge: auto_merge_on(Some("bob")),
                    ..pushed(waiting.clone())
                },
            ),
            (
                "stale approvals dismissed",
                plan.clone(),
                pushed(needing_another_approval()),
            ),
            (
                "merged",
                plan.clone(),
                MergeSnapshot {
                    state: PullRequestState::Merged,
                    ..pushed(waiting.clone())
                },
            ),
            ("your own pull request", own, pushed(waiting.clone())),
        ];
        for (case, plan, snapshot) in cases {
            assert_eq!(after_push(&snapshot, &plan), AfterPush::Nothing, "{case}");
        }
    }

    // The second step: what GitHub reports after the approval decides.

    #[test]
    fn it_merges_once_github_reports_the_pull_request_can_merge() {
        let plan = the_plan(&reviewing());

        for merge_state in ["CLEAN", "HAS_HOOKS", "UNSTABLE"] {
            let after = approved(reviewing(), "APPROVED", merge_state);

            assert_eq!(
                after_approving(&after, &plan),
                Ship::Merge(MergeMethod::Squash),
                "{merge_state}"
            );
        }
    }

    #[test]
    fn it_joins_the_queue_once_github_reports_the_pull_request_can_merge() {
        let snapshot = with_queue(reviewing());
        let after = approved(snapshot.clone(), "APPROVED", "CLEAN");

        assert_eq!(after_approving(&after, &the_plan(&snapshot)), Ship::Enqueue);
    }

    #[test]
    fn it_joins_the_queue_when_github_reports_only_that_the_head_is_behind() {
        let snapshot = with_queue(reviewing());
        let after = approved(snapshot.clone(), "APPROVED", "BEHIND");

        assert_eq!(after_approving(&after, &the_plan(&snapshot)), Ship::Enqueue);
    }

    #[test]
    fn it_turns_on_auto_merge_when_github_still_blocks_for_a_rule_rozed_cannot_see() {
        // Copilot opened it, or a ruleset requires a reviewer for these files: GitHub still needs
        // a review after yours, and nothing in the snapshot says why.
        let snapshot = reviewing();
        let plan = the_plan(&snapshot);
        let after = approved(snapshot, "REVIEW_REQUIRED", "BLOCKED");

        assert_eq!(plan.likely, Ship::Merge(MergeMethod::Squash));
        assert_eq!(
            after_approving(&after, &plan),
            auto_merge(vec![approvals(0)])
        );
    }

    #[test]
    fn it_never_merges_past_a_rule_for_an_admin() {
        let snapshot = as_admin(reviewing());
        let plan = the_plan(&snapshot);
        let review_still_required = approved(snapshot.clone(), "REVIEW_REQUIRED", "BLOCKED");
        let blocked_without_a_reason = approved(snapshot, "APPROVED", "BLOCKED");

        assert_eq!(
            after_approving(&review_still_required, &plan),
            auto_merge(vec![approvals(0)])
        );
        assert_eq!(
            after_approving(&blocked_without_a_reason, &plan),
            blocked(vec![Blocker::Unexplained {
                url: URL.to_string()
            }])
        );
    }

    #[test]
    fn it_turns_on_merge_when_ready_when_github_still_blocks_a_queue_branch() {
        let snapshot = as_admin(with_queue(reviewing()));
        let plan = the_plan(&snapshot);
        let after = approved(snapshot, "REVIEW_REQUIRED", "BLOCKED");

        assert_eq!(plan.likely, Ship::Enqueue);
        assert_eq!(
            after_approving(&after, &plan),
            Ship::EnqueueWhenReady {
                waiting_on: vec![approvals(0)]
            }
        );
    }

    #[test]
    fn a_code_owner_who_only_commented_is_waited_on_after_your_approval() {
        // Erin owns the files and left a comment. GitHub dropped her request, and a comment isn't
        // an opinionated review, so the snapshot can't name the owner GitHub still needs.
        let snapshot = MergeSnapshot {
            rules: ReviewRules {
                required_approvals: 1,
                code_owner_review: true,
                ..reviewing().rules
            },
            ..reviewing()
        };
        let plan = the_plan(&snapshot);
        let after = approved(snapshot, "REVIEW_REQUIRED", "BLOCKED");

        assert_eq!(plan.likely, Ship::Merge(MergeMethod::Squash));
        assert_eq!(
            after_approving(&after, &plan),
            auto_merge(vec![approvals(0)])
        );
    }

    #[test]
    fn your_approval_meeting_the_count_still_waits_when_github_wants_another_review() {
        // Two approvals required and Carol's given, so yours is expected to meet the count. But
        // you pushed the latest commit, and the rules require its approval by someone else.
        let snapshot = MergeSnapshot {
            reviews: vec![review("carol", ReviewState::Approved, OLD_HEAD)],
            ..needing_another_approval()
        };
        let plan = the_plan(&snapshot);
        let after = approved(snapshot, "REVIEW_REQUIRED", "BLOCKED");

        assert_eq!(plan.likely, Ship::Merge(MergeMethod::Squash));
        assert_eq!(
            after_approving(&after, &plan),
            auto_merge(vec![approvals(0)])
        );
    }

    #[test]
    fn it_stops_when_someone_pushed_after_you_approved() {
        let plan = the_plan(&reviewing());
        let after = MergeSnapshot {
            head: "5ca1ab1e5ca1ab1e5ca1ab1e5ca1ab1e5ca1ab1e".to_string(),
            ..approved(reviewing(), "APPROVED", "CLEAN")
        };

        assert_eq!(
            after_approving(&after, &plan),
            Ship::Stop(Stop::HeadMoved {
                approved: HEAD.to_string(),
                head: "5ca1ab1e5ca1ab1e5ca1ab1e5ca1ab1e5ca1ab1e".to_string(),
            })
        );
    }

    #[test]
    fn it_stops_when_github_reports_no_head_after_approving() {
        let plan = the_plan(&reviewing());
        let after = MergeSnapshot {
            head: String::new(),
            ..approved(reviewing(), "APPROVED", "CLEAN")
        };

        assert_eq!(
            after_approving(&after, &plan),
            Ship::Stop(Stop::HeadUnknown)
        );
    }

    #[test]
    fn it_stops_when_something_waiting_wont_fix_blocks_it_after_approving() {
        let plan = the_plan(&reviewing());
        let mut after = approved(reviewing(), "CHANGES_REQUESTED", "BLOCKED");
        after
            .reviews
            .push(review("carol", ReviewState::ChangesRequested, HEAD));
        after.auto_merge = Some(AutoMerge {
            method: MergeMethod::Squash,
            enabled_by: Some("bob".to_string()),
        });

        assert_eq!(
            after_approving(&after, &plan),
            Ship::Stop(Stop::Blocked {
                blockers: vec![Blocker::ChangesRequested {
                    by: vec!["carol".to_string()]
                }],
                auto_merge: true,
            })
        );
    }

    #[test]
    fn it_does_nothing_more_once_approved_when_auto_merge_is_on_or_it_is_queued() {
        let plan = the_plan(&needing_another_approval());
        let mut auto_merge_on = approved(needing_another_approval(), "REVIEW_REQUIRED", "BLOCKED");
        auto_merge_on.auto_merge = Some(AutoMerge {
            method: MergeMethod::Squash,
            enabled_by: Some("bob".to_string()),
        });
        let mut queued = with_queue(approved(reviewing(), "APPROVED", "CLEAN"));
        queued.queue.position = Some(1);

        assert_eq!(
            after_approving(&auto_merge_on, &plan),
            Ship::AutoMergeAlreadyOn {
                waiting_on: vec![approvals(1)]
            }
        );
        assert_eq!(after_approving(&queued, &plan), Ship::AlreadyQueued);
    }

    #[test]
    fn a_read_after_github_refuses_auto_merge_on_a_clean_pull_request_merges() {
        // rozed read UNKNOWN just before GitHub finished working out mergeability, so it turned on
        // auto-merge, and GitHub refused with "clean status". The read after that refusal merges.
        let plan = the_plan(&reviewing());
        let stale = MergeSnapshot {
            mergeable: MergeableState::Unknown,
            ..approved(reviewing(), "APPROVED", "UNKNOWN")
        };
        let fresh = approved(reviewing(), "APPROVED", "CLEAN");

        assert_eq!(
            after_approving(&stale, &plan),
            auto_merge(vec![Blocker::CheckingMergeability])
        );
        assert_eq!(
            after_approving(&fresh, &plan),
            Ship::Merge(MergeMethod::Squash)
        );
    }

    #[test]
    fn it_stops_once_approved_when_you_cannot_merge_or_turn_on_auto_merge() {
        let plan = the_plan(&reviewing());
        let mut clean = approved(reviewing(), "APPROVED", "CLEAN");
        clean.viewer.can_merge = false;
        let mut waiting = approved(reviewing(), "REVIEW_REQUIRED", "BLOCKED");
        waiting.viewer.can_enable_auto_merge = false;

        assert_eq!(
            after_approving(&clean, &plan),
            Ship::Stop(Stop::NoPermission)
        );
        assert_eq!(
            after_approving(&waiting, &plan),
            Ship::Stop(Stop::NoAutoMerge(vec![approvals(0)]))
        );
    }
}
