//! Whether a pull request can merge, why not, and what approve and ship does (design §9, "Merge
//! status", "Merging", "Approve and ship"; §11, "Acting on what you saw").
//!
//! The GitHub layer fills a [`MergeSnapshot`] from GraphQL and REST. Where GitHub reports a
//! decision (`mergeable`, `mergeStateStatus`, `reviewDecision`), the decision wins, and the rest
//! of the snapshot says why.
//!
//! - [`blockers`] lists every reason it can't merge, for the merge panel, each saying whether
//!   waiting will clear it.
//! - [`status`] sums them up for the one-line status in the pull request's header.
//! - [`merge_action`] is what the merge button does: merge with a method, add to the merge queue,
//!   or neither.
//! - [`approve_and_ship`] and [`after_approving`] are approve and ship, `⌘⇧⏎`: the first refuses
//!   or plans the approval, and words the confirmation with what's likely next; the second, from
//!   the pull request read again once approved, merges, joins the queue, turns on auto-merge, or
//!   stops and says why.
//!
//! ```
//! use review::merge::{
//!     AllowedMethods, Approval, ApproveAndShip, Blocker, CheckState, MergeMethod, MergeSnapshot,
//!     MergeStatus, MissingApprovals, RequiredCheck, Review, ReviewRules, Ship, Viewer,
//!     after_approving, approve_and_ship, blockers, status,
//! };
//!
//! // Someone else's pull request: one approval required, and its tests still running.
//! let head = "c0ffee1c0ffee1c0ffee1c0ffee1c0ffee1c0ffe";
//! let snapshot = MergeSnapshot {
//!     mergeable: "MERGEABLE".parse().unwrap(),
//!     merge_state: "BLOCKED".parse().unwrap(),
//!     review_decision: "REVIEW_REQUIRED".parse().ok(),
//!     rules: ReviewRules { required_approvals: 1, ..Default::default() },
//!     required_checks: vec![RequiredCheck {
//!         name: "test".into(),
//!         state: CheckState::of_check_run("IN_PROGRESS".parse().unwrap(), None),
//!         url: Some("https://github.com/rozed-dev/playground/actions/runs/1".into()),
//!     }],
//!     methods: AllowedMethods { merge: false, squash: true, rebase: true },
//!     viewer: Viewer {
//!         login: "alice".into(),
//!         can_approve: true,
//!         approval_counts: true,
//!         can_merge: true,
//!         can_enable_auto_merge: true,
//!         ..Default::default()
//!     },
//!     head: head.into(),
//!     head_on_screen: head.into(),
//!     ..Default::default()
//! };
//!
//! let blockers = blockers(&snapshot);
//! assert!(matches!(
//!     blockers[0],
//!     Blocker::Approvals(MissingApprovals { more: 1, .. }),
//! ));
//! assert!(blockers.iter().all(Blocker::waiting));
//! assert!(matches!(status(&snapshot), MergeStatus::Waiting { auto_merge: false, .. }));
//!
//! // Your approval is the one missing, and the tests will finish: approve, and likely auto-merge.
//! let ApproveAndShip::Plan(plan) = approve_and_ship(&snapshot) else { unreachable!() };
//! assert_eq!(plan.approval, Approval::Approve);
//! // The base branch keeps approvals when someone pushes, GitHub's default, so it warns.
//! assert_eq!(
//!     plan.wording(),
//!     "will likely wait for checks, with auto-merge on. \
//!      While auto-merge is on, a push could merge without a new review",
//! );
//!
//! // Once the approval lands, GitHub's verdict decides: still blocked by the running tests.
//! let mut after = snapshot.clone();
//! after.reviews.push(Review {
//!     author: "alice".into(),
//!     state: "APPROVED".parse().unwrap(),
//!     commit: Some(head.into()),
//!     counts: true,
//!     on_behalf_of: vec![],
//! });
//! after.review_decision = "APPROVED".parse().ok();
//! assert!(matches!(
//!     after_approving(&after, &plan),
//!     Ship::AutoMerge { method: MergeMethod::Squash, .. },
//! ));
//! ```

mod action;
mod blockers;
mod github;
mod reviews;
mod ship;
mod status;

pub use action::{MergeAction, NotPossible, merge_action};
pub use blockers::{Blocker, blockers, may_not_see_every_rule};
pub use github::{
    CheckConclusion, CheckStatus, MergeMethod, MergeStateStatus, MergeableState, PullRequestState,
    ReviewDecision, ReviewState, StatusState, UnknownValue,
};
pub use reviews::MissingApprovals;
pub use ship::{
    AfterPush, Approval, ApproveAndShip, Plan, Ship, Stop, after_approving, after_push,
    approve_and_ship,
};
pub use status::{MergeStatus, status};

/// What GitHub says about a pull request's merge, and the head on screen. The GitHub layer fills
/// it from GraphQL (field names below are `PullRequest`'s unless they say otherwise) and REST,
/// right before rozed shows the merge status or acts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MergeSnapshot {
    /// The pull request's page on GitHub (`url`), linked when GitHub reports a block rozed can't
    /// explain.
    pub url: String,
    /// `state`.
    pub state: PullRequestState,
    /// `isDraft`.
    pub draft: bool,
    /// `mergeable`.
    pub mergeable: MergeableState,
    /// `mergeStateStatus`.
    pub merge_state: MergeStateStatus,
    /// `reviewDecision`: `None` when the base branch's rules don't require reviews.
    pub review_decision: Option<ReviewDecision>,
    /// Each reviewer's latest approval or request for changes, or its dismissal
    /// (`latestOpinionatedReviews`, without `writersOnly`), one per reviewer. A later entry for the
    /// same reviewer replaces an earlier one. It keeps reviews from people without write access,
    /// marked by [`Review::counts`], because approve and ship needs your own latest review even
    /// when it doesn't count.
    pub reviews: Vec<Review>,
    /// Reviews requested and not yet given (`reviewRequests`). GitHub drops a user's request when
    /// they review.
    pub review_requests: Vec<ReviewRequest>,
    /// What the base branch's rules require of reviews and conversations.
    pub rules: ReviewRules,
    /// The checks the base branch's rules require, as they stand on the head: one entry for each
    /// check run and commit status under a required name, since GitHub requires every one of them
    /// to pass, such as a check run and a commit status both named `build`. A required name with
    /// nothing the rule accepts reported under it, such as a check pinned to an app that didn't
    /// run, is one entry, [`CheckState::Missing`].
    pub required_checks: Vec<RequiredCheck>,
    /// Whether something on the head can still report a check: a check suite or check run on it
    /// hasn't completed. A required check that hasn't reported is a wait only while something can
    /// still report it; a running required check counts too.
    pub checks_pending: bool,
    /// Review threads not yet resolved (`reviewThreads` with `isResolved` false).
    pub unresolved_threads: u32,
    /// The base branch's rules require the head to be up to date with the base, and the base has
    /// commits the head doesn't. The caller works it out from the base's rules (strict required
    /// status checks) and the local copy, because `mergeStateStatus` says `BEHIND` only when nothing
    /// else blocks the merge. It's ignored on a branch that requires a merge queue, since the queue
    /// tests each pull request on the latest base itself, and so is GitHub's `BEHIND` there.
    pub out_of_date: bool,
    /// Other rules on the base branch the pull request doesn't meet yet, such as required
    /// deployments or signed commits.
    pub unmet_rules: Vec<UnmetRule>,
    /// Whether rozed sees every rule on the base branch that can block the merge: the caller read
    /// its branch protection and its rulesets' active rules (`GET
    /// /repos/{owner}/{repo}/rules/branches/{branch}`), each one maps to a field here, and no list
    /// was cut short by paging. Rules rozed doesn't model include code scanning, required
    /// workflows, reviewers required by file pattern, and the extra approval for a pull request
    /// Copilot opened. Without this, a `BLOCKED` that rozed explains may be only partly explained
    /// ([`may_not_see_every_rule`]).
    pub sees_every_rule: bool,
    /// The base branch's merge queue.
    pub queue: MergeQueue,
    /// Auto-merge, when it's on (`autoMergeRequest`).
    pub auto_merge: Option<AutoMerge>,
    /// The merge methods the repository allows.
    pub methods: AllowedMethods,
    /// Whether the base branch requires signed commits (`requiresCommitSignatures`, a ruleset's
    /// `required_signatures`). GitHub can't sign the commits a rebase merge makes, so rozed doesn't
    /// offer rebase there.
    pub signed_commits: bool,
    /// The method you last used in the repository, which rozed remembers: the one to pick first.
    pub preferred_method: Option<MergeMethod>,
    /// GitHub's default method for you (`Repository.viewerDefaultMergeMethod`): the one to pick
    /// when you haven't used one here, or the one you used is no longer allowed.
    pub default_method: Option<MergeMethod>,
    /// The account rozed acts as.
    pub viewer: Viewer,
    /// The head commit GitHub has now (`headRefOid`), as a full SHA. Empty when GitHub didn't
    /// report it.
    pub head: String,
    /// The head of the revision on screen, as a full SHA: what you looked at, so what approving
    /// approves. Empty when there's no revision on screen yet.
    pub head_on_screen: String,
}

impl MergeSnapshot {
    /// The merge methods allowed here: the repository's, without rebase when the base branch
    /// requires signed commits.
    fn allowed_methods(&self) -> AllowedMethods {
        AllowedMethods {
            rebase: self.methods.rebase && !self.signed_commits,
            ..self.methods
        }
    }

    /// The method to pick first: the one you last used here, else GitHub's default for you, else
    /// the first allowed, whichever is allowed first.
    fn method(&self) -> Option<MergeMethod> {
        let preferred: Vec<MergeMethod> = [self.preferred_method, self.default_method]
            .into_iter()
            .flatten()
            .collect();
        self.allowed_methods().choose(&preferred)
    }
}

/// A reviewer's latest approval or request for changes, or its dismissal: an entry of
/// `latestOpinionatedReviews`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Review {
    /// The reviewer's login (`author.login`).
    pub author: String,
    /// `state`.
    pub state: ReviewState,
    /// The commit reviewed (`commit.oid`), or `None` when GitHub no longer has it.
    pub commit: Option<String>,
    /// Whether it counts toward the base branch's rules: the reviewer has write access
    /// (`authorCanPushToRepository`). Other reviews neither approve nor block.
    pub counts: bool,
    /// The teams it was made on behalf of as a code owner (`onBehalfOf`), as `org/team`
    /// (`Team.combinedSlug`).
    pub on_behalf_of: Vec<String>,
}

/// A review requested and not yet given: an entry of `reviewRequests`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewRequest {
    /// `requestedReviewer`.
    pub reviewer: Reviewer,
    /// Whether GitHub requested it because the reviewer owns changed files (`asCodeOwner`).
    pub as_code_owner: bool,
}

/// Someone asked to review.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Reviewer {
    /// A user, bot or mannequin, by login.
    User(String),
    /// A team, as `org/team` (`Team.combinedSlug`).
    Team(String),
}

/// What the base branch's rules require of reviews and conversations, from its branch protection
/// rule and the `pull_request` rules of its rulesets. With several, the strictest applies: the
/// highest count, and each requirement any of them sets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReviewRules {
    /// Approving reviews required (`requiredApprovingReviewCount`).
    pub required_approvals: u32,
    /// Whether files with a code owner need one of their owners to approve
    /// (`requireCodeOwnerReview`, `requiresCodeOwnerReviews`).
    pub code_owner_review: bool,
    /// Whether every conversation must be resolved (`requiredReviewThreadResolution`,
    /// `requiresConversationResolution`).
    pub conversation_resolution: bool,
    /// Whether a push dismisses the approvals given before it (`dismissStaleReviewsOnPush`,
    /// `dismissesStaleReviews`). GitHub's default is not to, so a push keeps them.
    pub dismiss_stale_reviews: bool,
    /// Whether the latest push needs approving by someone other than whoever pushed it
    /// (`requireLastPushApproval`, `requireLastPushApproval` on the branch protection rule).
    pub last_push_approval: bool,
}

impl ReviewRules {
    /// Whether a push makes GitHub want a new approval before the pull request merges: the rules
    /// require reviews, and dismiss stale approvals or require the latest push's approval.
    /// Otherwise a push merges with the approvals already given, or with none when none are
    /// required.
    pub fn push_needs_new_approval(&self) -> bool {
        let reviews_required = self.required_approvals > 0 || self.code_owner_review;
        reviews_required && (self.dismiss_stale_reviews || self.last_push_approval)
    }
}

/// A check run or commit status under a name the base branch's rules require, or the name alone
/// when nothing has reported under it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequiredCheck {
    /// The check run's `name`, or the commit status's `context`. Several entries can share it.
    pub name: String,
    /// Where it stands on the head.
    pub state: CheckState,
    /// Its page: the check run's `detailsUrl` (else `url`), or the commit status's `targetUrl`.
    /// `None` for a check that hasn't reported.
    pub url: Option<String>,
}

/// Where a required check stands on the head.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CheckState {
    /// It succeeded, or ended neutral or skipped, which GitHub's rules accept as passing.
    Passed,
    /// It ended any other way. Running it again is up to someone.
    Failed,
    /// It's requested, queued or running.
    Running,
    /// It's waiting on a deployment's protection rules, usually for a reviewer to approve the
    /// deployment (a check run's `WAITING`).
    AwaitingApproval,
    /// It hasn't reported on the head. It's a wait while something on the head can still report
    /// it ([`MergeSnapshot::checks_pending`]), and otherwise blocks until someone acts.
    Missing,
}

impl CheckState {
    /// A check run's state, from its `status` and, once completed, its `conclusion`.
    /// A completed run with no conclusion, which GitHub shouldn't report, counts as failed.
    pub fn of_check_run(status: CheckStatus, conclusion: Option<CheckConclusion>) -> Self {
        use CheckConclusion::{Neutral, Skipped, Success};
        match (status, conclusion) {
            (CheckStatus::Completed, Some(Success | Neutral | Skipped)) => Self::Passed,
            (CheckStatus::Completed, _) => Self::Failed,
            (CheckStatus::Waiting, _) => Self::AwaitingApproval,
            _ => Self::Running,
        }
    }

    /// A commit status's state. `EXPECTED` is a required status not reported yet.
    pub fn of_status(state: StatusState) -> Self {
        match state {
            StatusState::Success => Self::Passed,
            StatusState::Failure | StatusState::Error => Self::Failed,
            StatusState::Pending => Self::Running,
            StatusState::Expected => Self::Missing,
        }
    }
}

/// A rule on the base branch the pull request doesn't meet yet, other than reviews, checks,
/// conversations and being up to date, such as required deployments (`REQUIRED_DEPLOYMENTS`) or
/// signed commits (`REQUIRED_SIGNATURES`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnmetRule {
    /// What the rule requires, as the merge panel shows it.
    pub description: String,
    /// Where to see more, such as the deployment's page.
    pub url: Option<String>,
    /// Whether waiting meets it without anyone acting, such as a deployment that's running.
    pub waiting: bool,
}

/// The base branch's merge queue.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MergeQueue {
    /// Whether the base branch requires one (`isMergeQueueEnabled`). Then pull requests merge only
    /// through it.
    pub required: bool,
    /// The method it merges with (`mergeQueue.configuration.mergeMethod`).
    pub method: Option<MergeMethod>,
    /// Whether the pull request is in it (`isInMergeQueue`). GitHub can say so while
    /// `mergeQueueEntry` is null or can't be read, so this, not `position`, says it's queued.
    pub in_queue: bool,
    /// The pull request's position in it (`mergeQueueEntry.position`), when it's in it and GitHub
    /// says where.
    pub position: Option<u32>,
}

/// Auto-merge, turned on: `autoMergeRequest`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutoMerge {
    /// `mergeMethod`. On a branch with a merge queue, the queue's method applies instead.
    pub method: MergeMethod,
    /// Who turned it on (`enabledBy.login`).
    pub enabled_by: Option<String>,
}

/// The merge methods the repository allows (`mergeCommitAllowed`, `squashMergeAllowed`,
/// `rebaseMergeAllowed`), narrowed by the `allowedMergeMethods` of the base branch's rulesets, and
/// to squash and rebase when the base requires linear history. rozed itself leaves out rebase when
/// the base requires signed commits ([`MergeSnapshot::signed_commits`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AllowedMethods {
    pub merge: bool,
    pub squash: bool,
    pub rebase: bool,
}

impl AllowedMethods {
    /// Whether the method is allowed.
    pub fn allows(&self, method: MergeMethod) -> bool {
        match method {
            MergeMethod::Merge => self.merge,
            MergeMethod::Squash => self.squash,
            MergeMethod::Rebase => self.rebase,
        }
    }

    /// The allowed methods, in the order GitHub's merge button lists them.
    pub fn to_vec(&self) -> Vec<MergeMethod> {
        [MergeMethod::Merge, MergeMethod::Squash, MergeMethod::Rebase]
            .into_iter()
            .filter(|&method| self.allows(method))
            .collect()
    }

    /// The method to pick first: the first of `preferred` that's allowed, else the first allowed.
    pub fn choose(&self, preferred: &[MergeMethod]) -> Option<MergeMethod> {
        (preferred.iter().copied())
            .find(|&method| self.allows(method))
            .or_else(|| self.to_vec().into_iter().next())
    }
}

/// The account rozed acts as, and what GitHub lets it do here.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Viewer {
    /// Its login.
    pub login: String,
    /// The teams it's in, as `org/team` (`Team.combinedSlug`). Only teams asked to review as code
    /// owners matter: its approval counts for them.
    pub teams: Vec<String>,
    /// Whether it opened the pull request (`viewerDidAuthor`). GitHub doesn't let authors approve.
    pub did_author: bool,
    /// Whether GitHub lets it submit an approving review here. GraphQL has no `viewerCanApprove`,
    /// so the GitHub layer works it out: it didn't open the pull request, the pull request is open,
    /// the repository isn't archived, the conversation isn't locked unless it has write access,
    /// and it can read the repository.
    pub can_approve: bool,
    /// Whether its approval counts toward the base branch's rules: it has write access
    /// (`Repository.viewerPermission` is `WRITE`, `MAINTAIN` or `ADMIN`).
    pub approval_counts: bool,
    /// Whether it can merge into the base branch, or add to its merge queue: write access, and the
    /// base's push restrictions don't keep it out.
    pub can_merge: bool,
    /// Whether it can merge without meeting the base branch's rules (`viewerCanMergeAsAdmin`), for
    /// the merge panel's admin override. Approve and ship never relies on it: [`after_approving`]
    /// merges only when GitHub reports the rules are met.
    pub can_merge_as_admin: bool,
    /// Whether it can turn on auto-merge (`viewerCanEnableAutoMerge`): write access, and the
    /// repository allows auto-merge.
    pub can_enable_auto_merge: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_completed_check_run_passes_on_success_neutral_or_skipped() {
        for conclusion in [
            CheckConclusion::Success,
            CheckConclusion::Neutral,
            CheckConclusion::Skipped,
        ] {
            assert_eq!(
                CheckState::of_check_run(CheckStatus::Completed, Some(conclusion)),
                CheckState::Passed,
                "{conclusion:?}"
            );
        }
    }

    #[test]
    fn a_completed_check_run_fails_on_any_other_conclusion() {
        for conclusion in [
            Some(CheckConclusion::Failure),
            Some(CheckConclusion::Cancelled),
            Some(CheckConclusion::TimedOut),
            Some(CheckConclusion::ActionRequired),
            Some(CheckConclusion::StartupFailure),
            Some(CheckConclusion::Stale),
            None,
        ] {
            assert_eq!(
                CheckState::of_check_run(CheckStatus::Completed, conclusion),
                CheckState::Failed,
                "{conclusion:?}"
            );
        }
    }

    #[test]
    fn a_check_run_that_has_not_completed_is_running() {
        for status in [
            CheckStatus::Requested,
            CheckStatus::Queued,
            CheckStatus::Pending,
            CheckStatus::InProgress,
        ] {
            assert_eq!(
                CheckState::of_check_run(status, None),
                CheckState::Running,
                "{status:?}"
            );
        }
    }

    #[test]
    fn a_check_run_waiting_on_a_deployment_awaits_approval() {
        assert_eq!(
            CheckState::of_check_run(CheckStatus::Waiting, None),
            CheckState::AwaitingApproval
        );
    }

    #[test]
    fn a_commit_status_maps_to_a_check_state() {
        assert_eq!(
            CheckState::of_status(StatusState::Success),
            CheckState::Passed
        );
        assert_eq!(
            CheckState::of_status(StatusState::Failure),
            CheckState::Failed
        );
        assert_eq!(
            CheckState::of_status(StatusState::Error),
            CheckState::Failed
        );
        assert_eq!(
            CheckState::of_status(StatusState::Pending),
            CheckState::Running
        );
        assert_eq!(
            CheckState::of_status(StatusState::Expected),
            CheckState::Missing
        );
    }

    #[test]
    fn allowed_methods_are_listed_in_the_merge_buttons_order() {
        let methods = AllowedMethods {
            merge: true,
            squash: false,
            rebase: true,
        };

        assert_eq!(methods.to_vec(), [MergeMethod::Merge, MergeMethod::Rebase]);
        assert!(!methods.allows(MergeMethod::Squash));
        assert_eq!(AllowedMethods::default().to_vec(), []);
    }

    #[test]
    fn the_preferred_method_is_chosen_when_allowed() {
        let methods = AllowedMethods {
            merge: true,
            squash: true,
            rebase: false,
        };

        assert_eq!(
            methods.choose(&[MergeMethod::Squash]),
            Some(MergeMethod::Squash)
        );
        assert_eq!(
            methods.choose(&[MergeMethod::Rebase, MergeMethod::Squash]),
            Some(MergeMethod::Squash)
        );
        assert_eq!(
            methods.choose(&[MergeMethod::Rebase]),
            Some(MergeMethod::Merge)
        );
        assert_eq!(methods.choose(&[]), Some(MergeMethod::Merge));
        assert_eq!(AllowedMethods::default().choose(&[]), None);
    }

    #[test]
    fn rebase_is_not_offered_when_the_base_requires_signed_commits() {
        let snapshot = MergeSnapshot {
            methods: AllowedMethods {
                merge: false,
                squash: true,
                rebase: true,
            },
            signed_commits: true,
            ..Default::default()
        };

        assert_eq!(snapshot.allowed_methods().to_vec(), [MergeMethod::Squash]);
    }

    #[test]
    fn the_method_is_the_remembered_one_then_githubs_default_for_you() {
        let snapshot = MergeSnapshot {
            methods: AllowedMethods {
                merge: true,
                squash: true,
                rebase: false,
            },
            preferred_method: Some(MergeMethod::Rebase),
            default_method: Some(MergeMethod::Squash),
            ..Default::default()
        };
        let remembered_allowed = MergeSnapshot {
            preferred_method: Some(MergeMethod::Merge),
            ..snapshot.clone()
        };

        assert_eq!(snapshot.method(), Some(MergeMethod::Squash));
        assert_eq!(remembered_allowed.method(), Some(MergeMethod::Merge));
    }
}
