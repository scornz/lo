//! Whether a pull request can merge, why not, and what approve and ship does (design §9, "Merge
//! status", "Merging", "Approve and ship"; §11, "Acting on what you saw").
//!
//! The GitHub layer fills a [`MergeSnapshot`] from GraphQL and REST. Where GitHub reports a
//! decision (`mergeable`, `mergeStateStatus`, `reviewDecision`), the decision wins, and the rest
//! of the snapshot says why.

mod action;
mod blockers;
mod github;
mod reviews;
mod ship;

pub use action::{MergeAction, NotPossible, merge_action};
pub use blockers::{Blocker, blockers};
pub use github::{
    CheckConclusion, CheckStatus, MergeMethod, MergeStateStatus, MergeableState, PullRequestState,
    ReviewDecision, ReviewState, StatusState, UnknownValue,
};
pub use reviews::MissingApprovals;
pub use ship::{Approval, ApproveAndShip, Plan, Ship, Stop, approve_and_ship};

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
    /// (`latestOpinionatedReviews`), one per reviewer. A later entry for the same reviewer replaces
    /// an earlier one.
    pub reviews: Vec<Review>,
    /// Reviews requested and not yet given (`reviewRequests`). GitHub drops a user's request when
    /// they review.
    pub review_requests: Vec<ReviewRequest>,
    /// What the base branch's rules require of reviews and conversations.
    pub rules: ReviewRules,
    /// The checks the base branch's rules require, one per name, as they stand on the head. A
    /// required check that hasn't reported is [`CheckState::Missing`].
    pub required_checks: Vec<RequiredCheck>,
    /// Review threads not yet resolved (`reviewThreads` with `isResolved` false).
    pub unresolved_threads: u32,
    /// The base branch's rules require the head to be up to date with the base, and the base has
    /// commits the head doesn't. The caller works it out from the base's rules (strict required
    /// status checks) and the local copy, because `mergeStateStatus` says `BEHIND` only when nothing
    /// else blocks the merge. It's ignored on a branch that requires a merge queue, since the queue
    /// tests each pull request on the latest base itself.
    pub out_of_date: bool,
    /// Other rules on the base branch the pull request doesn't meet yet, such as required
    /// deployments or signed commits.
    pub unmet_rules: Vec<UnmetRule>,
    /// The base branch's merge queue.
    pub queue: MergeQueue,
    /// Auto-merge, when it's on (`autoMergeRequest`).
    pub auto_merge: Option<AutoMerge>,
    /// The merge methods the repository allows.
    pub methods: AllowedMethods,
    /// The method to pick first when several are allowed: the one you last used in the repository,
    /// which rozed remembers, else GitHub's `Repository.viewerDefaultMergeMethod`.
    pub preferred_method: Option<MergeMethod>,
    /// The account rozed acts as.
    pub viewer: Viewer,
    /// The head commit GitHub has now (`headRefOid`).
    pub head: String,
    /// The head of the revision on screen: what you looked at, so what approving approves.
    pub head_on_screen: String,
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
}

/// A check the base branch's rules require.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequiredCheck {
    /// The check run's `name`, or the commit status's `context`.
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
    /// It's requested, queued, running, or waiting, such as on a deployment's approval.
    Running,
    /// It hasn't reported on the head.
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
    /// The pull request's position in it (`mergeQueueEntry.position`), when it's in it
    /// (`isInMergeQueue`).
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
/// to squash and rebase when the base requires linear history.
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

    /// The method to pick first: the preferred one when it's allowed, else the first allowed.
    pub fn choose(&self, preferred: Option<MergeMethod>) -> Option<MergeMethod> {
        preferred
            .filter(|&method| self.allows(method))
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
    /// Whether GitHub lets it submit an approving review here.
    pub can_approve: bool,
    /// Whether its approval counts toward the base branch's rules: it has write access
    /// (`Repository.viewerPermission` is `WRITE`, `MAINTAIN` or `ADMIN`).
    pub approval_counts: bool,
    /// Whether it can merge into the base branch, or add to its merge queue: write access, and the
    /// base's push restrictions don't keep it out.
    pub can_merge: bool,
    /// Whether it can merge without meeting the base branch's rules (`viewerCanMergeAsAdmin`), for
    /// the merge panel's admin override. Nothing here plans one: approve and ship never bypasses
    /// rules.
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
            CheckStatus::Waiting,
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
            methods.choose(Some(MergeMethod::Squash)),
            Some(MergeMethod::Squash)
        );
        assert_eq!(
            methods.choose(Some(MergeMethod::Rebase)),
            Some(MergeMethod::Merge)
        );
        assert_eq!(methods.choose(None), Some(MergeMethod::Merge));
        assert_eq!(AllowedMethods::default().choose(None), None);
    }
}
