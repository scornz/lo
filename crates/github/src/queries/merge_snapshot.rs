//! `MergeSnapshot`: what decides whether a pull request can merge (design §9, "Merge status"),
//! read with its base branch's rulesets and mapped into [`review::merge::MergeSnapshot`]
//! ([`MergeFacts::snapshot`]).
//!
//! - **GraphQL** gives GitHub's verdict (`mergeable`, `mergeStateStatus`, `reviewDecision`) and
//!   what explains it: the reviews, review requests, review threads and required checks, the
//!   base branch's classic protection, the merge queue, auto-merge, your pending review, and what
//!   the account may do. The checks are paged ([`super::merge_checks`]). The other lists are read
//!   a page each and counted: a longer one means Lo may not see every reason GitHub blocks the
//!   merge.
//! - **REST** gives the active rules of the base branch's rulesets (`GET
//!   /repos/{owner}/{repo}/rules/branches/{branch}`), which anyone who can read the repository may
//!   ask. They're asked again with their ETag, so an unchanged answer is free.
//! - **The local copy** says whether the head is behind the base, which `mergeStateStatus` says
//!   only when nothing else blocks the merge ([`Local::head_contains_base`]).
//!
//! The open pull request's poller keeps it in the cache and current
//! ([`crate::poll::Poller::merge_facts`]); approve and ship reads it afresh
//! ([`GithubClient::merge_facts`]).

use std::sync::Arc;

use graphql_client::GraphQLQuery;
use http_client::Method;
use http_client::http::StatusCode;
use review::merge::{
    AllowedMethods, AutoMerge, CheckState, MergeMethod, MergeQueue, RequiredCheck, Review,
    ReviewRequest, ReviewRules, Reviewer, Viewer,
};
use review::pending::PendingReview as Pending;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::pending_review::PendingReviewNode;
use super::timeline::github_name;
use super::trunk_rules::path_encoded;
use super::{DateTime, GitObjectID, PAGE_SIZE, Pager, URI, merge_checks, not_found};
use crate::activity::next_link;
use crate::cache::{CacheEntry, schema_shape};
use crate::client::GithubClient;
use crate::error::GithubError;
use crate::poll::CachedRead;
use crate::pulls::{PullRef, RepoRef};
use crate::push_history::stops_the_read;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/schema.graphql",
    query_path = "graphql/merge_snapshot.graphql",
    response_derives = "Debug, Clone, PartialEq, Serialize",
    variables_derives = "Debug, Clone"
)]
pub struct MergeSnapshot;

// The generated types, named from this module, such as `queries::merge_snapshot::Variables`.
pub use self::merge_snapshot::*;

/// The most pages of checks read: 1,000 checks. A head with more has them cut short.
pub const MAX_CHECK_PAGES: usize = 10;

/// What decides whether a pull request can merge, as GitHub reports it to the account it was
/// read as, before Lo adds what's on screen, what it remembers and what the local copy knows
/// ([`Self::snapshot`]). Field docs name the GraphQL field each comes from, `PullRequest`'s
/// unless they say otherwise.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MergeFacts {
    /// The account it was read as.
    pub login: String,
    pub repository: RepositoryFacts,
    pub pull_request: PullRequestFacts,
    /// The base branch's classic branch protection rule, if it has one.
    pub protection: Option<Protection>,
    /// The active rules of the base branch's rulesets.
    pub rules: BranchRules,
    /// The lists longer than the page read of them, by name, such as `reviews`. What explains
    /// GitHub's verdict may be past the page, so Lo may not see every rule.
    pub cut_short: Vec<String>,
    /// The parts GitHub left out with an error, by name. The read is tried again after a wait.
    pub left_out: Vec<String>,
}

/// The repository's merge settings, and the account's access to it: `Repository`'s fields.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RepositoryFacts {
    /// `isArchived`.
    pub archived: bool,
    /// `viewerPermission`: `READ`, `TRIAGE`, `WRITE`, `MAINTAIN` or `ADMIN`. `None` when GitHub
    /// doesn't say.
    pub viewer_permission: Option<String>,
    /// `mergeCommitAllowed`.
    pub merge_commit_allowed: bool,
    /// `squashMergeAllowed`.
    pub squash_merge_allowed: bool,
    /// `rebaseMergeAllowed`.
    pub rebase_merge_allowed: bool,
    /// `viewerDefaultMergeMethod`.
    pub viewer_default_merge_method: Option<String>,
}

/// The pull request, as it bears on merging it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PullRequestFacts {
    /// `url`.
    pub url: String,
    /// `state`.
    pub state: String,
    /// `isDraft`.
    pub draft: bool,
    /// `locked`.
    pub locked: bool,
    /// `mergeable`.
    pub mergeable: String,
    /// `mergeStateStatus`.
    pub merge_state: String,
    /// `reviewDecision`.
    pub review_decision: Option<String>,
    /// `headRefOid`.
    pub head: String,
    /// `baseRefName`.
    pub base: String,
    /// The base branch's tip now (`baseRef.target.oid`), or `None` once the branch is gone.
    pub base_tip: Option<String>,
    /// `author.login`, or `None` for a deleted account.
    pub author: Option<String>,
    /// `viewerDidAuthor`.
    pub viewer_did_author: bool,
    /// `viewerCanMergeAsAdmin`.
    pub viewer_can_merge_as_admin: bool,
    /// `viewerCanEnableAutoMerge`.
    pub viewer_can_enable_auto_merge: bool,
    /// `isMergeQueueEnabled`.
    pub queue_required: bool,
    /// `isInMergeQueue`.
    pub in_queue: bool,
    /// `mergeQueueEntry.position`.
    pub queue_position: Option<u32>,
    /// `mergeQueue.configuration.mergeMethod`, which GitHub shows only to those who can write
    /// to the repository.
    pub queue_method: Option<String>,
    /// `autoMergeRequest`, when auto-merge is on.
    pub auto_merge: Option<AutoMergeFacts>,
    /// `latestOpinionatedReviews`, its first 50.
    pub reviews: Vec<ReviewFacts>,
    /// `reviewRequests`, its first 25. A request for a team the account can't see is left out:
    /// it can't be named, and the account isn't in it.
    pub review_requests: Vec<RequestFacts>,
    /// The teams asked to review that the account is in, child teams included, as `org/team`.
    pub viewer_teams: Vec<String>,
    /// Your pending review (`reviews(states: PENDING, author: $login)`).
    pub pending_review: Option<PendingReviewNode>,
    /// How many of the first 100 `reviewThreads` aren't resolved.
    pub unresolved_threads: u32,
    /// The required check runs and commit statuses on the head
    /// (`statusCheckRollup.contexts` with `isRequired(pullRequestNumber:)`), in GitHub's order.
    pub checks: Vec<CheckFacts>,
    /// Whether something on the head can still report a check: a check run that hasn't
    /// completed, a pending commit status, or a check suite that hasn't completed and either
    /// belongs to a workflow run or has started. GitHub makes a queued suite for every app that
    /// may add checks, and one that never adds any stays queued, so a queued suite counts only
    /// for a workflow.
    pub checks_pending: bool,
}

/// `autoMergeRequest`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AutoMergeFacts {
    /// `mergeMethod`.
    pub method: String,
    /// `enabledBy.login`.
    pub enabled_by: Option<String>,
}

/// An entry of `latestOpinionatedReviews`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReviewFacts {
    /// `author.login`, or `None` for a deleted account.
    pub author: Option<String>,
    /// `state`.
    pub state: String,
    /// `commit.oid`, or `None` once GitHub no longer has the commit.
    pub commit: Option<String>,
    /// `authorCanPushToRepository`.
    pub author_can_push: bool,
    /// `onBehalfOf`'s `combinedSlug`s, its first 10.
    pub on_behalf_of: Vec<String>,
}

/// An entry of `reviewRequests` the account can see.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RequestFacts {
    /// A user's, bot's or mannequin's `login`, or a team's `combinedSlug`.
    pub reviewer: String,
    /// Whether it's a team.
    pub team: bool,
    /// `asCodeOwner`.
    pub as_code_owner: bool,
}

/// A required check run or commit status on the head.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CheckFacts {
    /// A `CheckRun`: its `name`, `status`, `conclusion`, and `detailsUrl` (else `url`).
    CheckRun {
        name: String,
        status: String,
        conclusion: Option<String>,
        url: Option<String>,
    },
    /// A `StatusContext`: its `context`, `state` and `targetUrl`.
    Status {
        context: String,
        state: String,
        url: Option<String>,
    },
}

impl CheckFacts {
    /// The name the base branch's rules require it under.
    pub fn name(&self) -> &str {
        match self {
            Self::CheckRun { name, .. } => name,
            Self::Status { context, .. } => context,
        }
    }

    /// It as `review::merge` takes it. A status or conclusion newer than Lo counts as running,
    /// and a completed run's unknown conclusion as failed, so neither reads as passing.
    fn required(&self) -> RequiredCheck {
        let state = match self {
            Self::CheckRun {
                status, conclusion, ..
            } => match status.parse() {
                Ok(status) => CheckState::of_check_run(
                    status,
                    conclusion.as_deref().and_then(|c| c.parse().ok()),
                ),
                Err(_) => CheckState::Running,
            },
            Self::Status { state, .. } => state
                .parse()
                .map_or(CheckState::Running, CheckState::of_status),
        };
        let url = match self {
            Self::CheckRun { url, .. } | Self::Status { url, .. } => url.clone(),
        };
        RequiredCheck {
            name: self.name().to_string(),
            state,
            url,
        }
    }
}

/// The base branch's classic branch protection rule. Anyone who can read the repository sees
/// part of it (`refUpdateRule`); only those who can administer it see all of it
/// (`branchProtectionRule`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Protection {
    /// Whether all of it was read. Otherwise whether it dismisses stale approvals, wants the
    /// latest push approved by someone else, wants the head up to date, requires deployments or
    /// locks the branch is unknown, and reads as not.
    pub complete: bool,
    /// `requiredApprovingReviewCount`, when it requires approving reviews.
    pub required_approvals: u32,
    /// `requiresCodeOwnerReviews`.
    pub code_owner_review: bool,
    /// `requiresConversationResolution`.
    pub conversation_resolution: bool,
    /// `dismissesStaleReviews`.
    pub dismiss_stale_reviews: bool,
    /// `requireLastPushApproval`.
    pub last_push_approval: bool,
    /// `requiresStrictStatusChecks`, when it requires status checks.
    pub strict: bool,
    /// The names of the checks it requires (`requiredStatusCheckContexts`,
    /// `requiredStatusChecks.context`).
    pub required_checks: Vec<String>,
    /// `requiresLinearHistory`.
    pub linear_history: bool,
    /// `requiresSignatures`, `requiresCommitSignatures`.
    pub signatures: bool,
    /// `requiresDeployments`.
    pub deployments: bool,
    /// `lockBranch`.
    pub lock_branch: bool,
    /// `refUpdateRule.viewerCanPush`: the rule's push restrictions don't keep the account out.
    pub viewer_can_push: bool,
}

/// The active rules of the base branch's rulesets, its organization's included, as `GET
/// /repos/{owner}/{repo}/rules/branches/{branch}` lists them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BranchRules {
    /// The branch they apply to.
    pub branch: String,
    pub rules: Vec<BranchRule>,
    /// The answer's ETag, which the next read sends.
    pub etag: Option<String>,
    /// Why they couldn't be read. Lo then may not see every rule.
    pub unavailable: Option<String>,
    /// Whether there were more than a page of them.
    pub cut_short: bool,
}

/// A rule that applies to the base branch, by its REST `type`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BranchRule {
    /// `pull_request`.
    PullRequest(PullRequestRule),
    /// `required_status_checks`.
    RequiredStatusChecks(StatusChecksRule),
    /// `merge_queue`, with its `merge_method`.
    MergeQueue { method: Option<String> },
    /// `required_linear_history`: no merge commits.
    RequiredLinearHistory,
    /// `required_signatures`. GitHub can't sign a rebase merge's commits, so Lo doesn't offer
    /// it; what else it blocks Lo doesn't model.
    RequiredSignatures,
    /// A rule that never keeps a pull request from merging, such as one against deleting the
    /// branch or force-pushing to it, or one that asks Copilot to review.
    Harmless { kind: String },
    /// Any other rule, which may keep a pull request from merging for a reason Lo doesn't
    /// model, such as required deployments, code scanning or restricted updates, or a rule
    /// newer than Lo.
    Other { kind: String },
}

/// A `pull_request` rule's parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PullRequestRule {
    /// `required_approving_review_count`.
    pub required_approvals: u32,
    /// `require_code_owner_review`.
    pub code_owner_review: bool,
    /// `required_review_thread_resolution`.
    pub conversation_resolution: bool,
    /// `dismiss_stale_reviews_on_push`.
    pub dismiss_stale_reviews: bool,
    /// `require_last_push_approval`.
    pub last_push_approval: bool,
    /// `allowed_merge_methods`, or `None` when it doesn't narrow them.
    pub allowed_methods: Option<Vec<String>>,
    /// Whether it has `required_reviewers`: reviewers required by file pattern, which Lo doesn't
    /// model.
    pub required_reviewers: bool,
    /// `require_extra_approval_for_unattributed_changes`: one more approval for a pull request
    /// Copilot opened, which Lo doesn't model.
    pub extra_approval_for_copilot: bool,
}

/// A `required_status_checks` rule's parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StatusChecksRule {
    /// `strict_required_status_checks_policy`: the head must be up to date with the base.
    pub strict: bool,
    /// The names of the checks it requires (`required_status_checks[].context`).
    pub checks: Vec<String>,
}

/// What GitHub doesn't say: what's on screen, what Lo remembers and what the local copy knows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Local {
    /// The head of the revision on screen, as a full SHA.
    pub head_on_screen: String,
    /// The method you last used in the repository ([`crate::merge_methods::MergeMethods`]).
    pub preferred_method: Option<MergeMethod>,
    /// Whether the head contains the base branch's tip ([`MergeFacts::base_tip`]), from the
    /// local copy (`repo_cache::Repo::is_ancestor(base tip, head)`), or `None` when it can't
    /// tell yet. Asked only when the base branch requires the head to be up to date
    /// ([`MergeFacts::requires_up_to_date`]).
    pub head_contains_base: Option<bool>,
}

impl MergeFacts {
    /// The base branch's tip now, which the head must contain when the branch requires it to be
    /// up to date.
    pub fn base_tip(&self) -> Option<&str> {
        self.pull_request.base_tip.as_deref()
    }

    /// Whether the base branch's rules require the head to be up to date with the base before
    /// it merges: strict required status checks, with at least one check required. Unknown,
    /// and so false, when only part of the branch's protection could be read.
    pub fn requires_up_to_date(&self) -> bool {
        let protection = (self.protection.as_ref())
            .is_some_and(|protection| protection.strict && !protection.required_checks.is_empty());
        protection
            || (self.rules.rules.iter()).any(|rule| {
                matches!(rule, BranchRule::RequiredStatusChecks(checks)
                    if checks.strict && !checks.checks.is_empty())
            })
    }

    /// Whether Lo sees every rule on the base branch that can block the merge
    /// ([`review::merge::MergeSnapshot::sees_every_rule`]): the rulesets' rules were read in
    /// full, the classic protection in full or not there, each rule maps to the snapshot, and no
    /// list was cut short or left out.
    pub fn sees_every_rule(&self) -> bool {
        let protection = self.protection.as_ref().is_none_or(|protection| {
            protection.complete
                && !protection.signatures
                && !protection.deployments
                && !protection.lock_branch
        });
        let by_copilot = (self.pull_request.author.as_deref()).is_some_and(is_copilot);
        let rules = (self.rules.rules.iter()).all(|rule| match rule {
            BranchRule::PullRequest(rule) => {
                !rule.required_reviewers && !(rule.extra_approval_for_copilot && by_copilot)
            }
            BranchRule::RequiredStatusChecks(_)
            | BranchRule::MergeQueue { .. }
            | BranchRule::RequiredLinearHistory
            | BranchRule::Harmless { .. } => true,
            BranchRule::RequiredSignatures | BranchRule::Other { .. } => false,
        });
        protection
            && rules
            && self.rules.unavailable.is_none()
            && !self.rules.cut_short
            && self.cut_short.is_empty()
            && self.left_out.is_empty()
    }

    /// What the base branch's rules require of reviews and conversations: the strictest of its
    /// classic protection and its rulesets' `pull_request` rules.
    pub fn review_rules(&self) -> ReviewRules {
        let mut rules = ReviewRules::default();
        let mut add = |approvals: u32, owners: bool, threads: bool, stale: bool, last: bool| {
            rules.required_approvals = rules.required_approvals.max(approvals);
            rules.code_owner_review |= owners;
            rules.conversation_resolution |= threads;
            rules.dismiss_stale_reviews |= stale;
            rules.last_push_approval |= last;
        };
        if let Some(p) = &self.protection {
            add(
                p.required_approvals,
                p.code_owner_review,
                p.conversation_resolution,
                p.dismiss_stale_reviews,
                p.last_push_approval,
            );
        }
        for rule in &self.rules.rules {
            if let BranchRule::PullRequest(r) = rule {
                add(
                    r.required_approvals,
                    r.code_owner_review,
                    r.conversation_resolution,
                    r.dismiss_stale_reviews,
                    r.last_push_approval,
                );
            }
        }
        rules
    }

    /// The merge methods the repository allows, narrowed by the rulesets' `allowed_merge_methods`
    /// and, when the base requires linear history, to squash and rebase.
    pub fn allowed_methods(&self) -> AllowedMethods {
        let repository = &self.repository;
        let mut methods = AllowedMethods {
            merge: repository.merge_commit_allowed,
            squash: repository.squash_merge_allowed,
            rebase: repository.rebase_merge_allowed,
        };
        let linear = (self.protection.as_ref()).is_some_and(|p| p.linear_history)
            || self
                .rules
                .rules
                .contains(&BranchRule::RequiredLinearHistory);
        if linear {
            methods.merge = false;
        }
        for rule in &self.rules.rules {
            if let BranchRule::PullRequest(PullRequestRule {
                allowed_methods: Some(allowed),
                ..
            }) = rule
            {
                let allows = |method: MergeMethod| {
                    (allowed.iter()).any(|name| name.parse::<MergeMethod>() == Ok(method))
                };
                methods.merge &= allows(MergeMethod::Merge);
                methods.squash &= allows(MergeMethod::Squash);
                methods.rebase &= allows(MergeMethod::Rebase);
            }
        }
        methods
    }

    /// Whether the base branch requires signed commits.
    pub fn signed_commits(&self) -> bool {
        (self.protection.as_ref()).is_some_and(|p| p.signatures)
            || self.rules.rules.contains(&BranchRule::RequiredSignatures)
    }

    /// The base branch's merge queue.
    pub fn queue(&self) -> MergeQueue {
        let pull = &self.pull_request;
        let rule = (self.rules.rules.iter()).find_map(|rule| match rule {
            BranchRule::MergeQueue { method } => Some(method.as_deref()),
            _ => None,
        });
        let method = (pull.queue_method.as_deref())
            .or(rule.flatten())
            .and_then(|method| method.parse().ok());
        MergeQueue {
            required: pull.queue_required || rule.is_some(),
            method,
            in_queue: pull.in_queue,
            position: pull.queue_position,
        }
    }

    /// The account, and what GitHub lets it do here.
    ///
    /// - Its approval counts, and it may merge, with write access (`WRITE`, `MAINTAIN` or
    ///   `ADMIN`); merging also needs the base branch's push restrictions to let it in.
    /// - It may approve when it didn't open the pull request, the pull request is open, the
    ///   repository isn't archived, the conversation isn't locked unless it has write access,
    ///   and it can read the repository.
    pub fn viewer(&self) -> Viewer {
        let pull = &self.pull_request;
        let permission = self.repository.viewer_permission.as_deref();
        let writes = matches!(permission, Some("WRITE" | "MAINTAIN" | "ADMIN"));
        let can_push = (self.protection.as_ref()).is_none_or(|p| p.viewer_can_push);
        let open = pull.state.parse() == Ok(review::merge::PullRequestState::Open);
        Viewer {
            login: self.login.clone(),
            teams: pull.viewer_teams.clone(),
            did_author: pull.viewer_did_author,
            can_approve: !pull.viewer_did_author
                && open
                && !self.repository.archived
                && (!pull.locked || writes)
                && permission.is_some(),
            approval_counts: writes,
            can_merge: writes && can_push,
            can_merge_as_admin: pull.viewer_can_merge_as_admin,
            can_enable_auto_merge: pull.viewer_can_enable_auto_merge,
        }
    }

    /// The checks the base branch's rules require, as they stand on the head: each required
    /// check run and commit status, in GitHub's order, then each required name nothing has
    /// reported under, as [`CheckState::Missing`], in the rules' order.
    pub fn required_checks(&self) -> Vec<RequiredCheck> {
        let mut checks: Vec<RequiredCheck> = (self.pull_request.checks.iter())
            .map(CheckFacts::required)
            .collect();
        let protection = (self.protection.iter()).flat_map(|p| p.required_checks.iter());
        let rules = (self.rules.rules.iter()).flat_map(|rule| match rule {
            BranchRule::RequiredStatusChecks(rule) => rule.checks.as_slice(),
            _ => &[],
        });
        for name in protection.chain(rules) {
            if !checks.iter().any(|check| check.name == *name) {
                checks.push(RequiredCheck {
                    name: name.clone(),
                    state: CheckState::Missing,
                    url: None,
                });
            }
        }
        checks
    }

    /// The snapshot `review::merge` decides with, from these facts and what GitHub doesn't
    /// say. The local copy says whether the head is out of date only when the base branch
    /// requires it to be up to date ([`Self::requires_up_to_date`]). An enum value newer than Lo
    /// reads as GitHub still working it out, which blocks the merge without explaining it.
    pub fn snapshot(&self, local: &Local) -> review::merge::MergeSnapshot {
        let pull = &self.pull_request;
        let reviews = (pull.reviews.iter())
            .filter_map(|review| {
                Some(Review {
                    // GitHub shows a deleted account as `ghost`.
                    author: review.author.clone().unwrap_or_else(|| "ghost".into()),
                    state: review.state.parse().ok()?,
                    commit: review.commit.clone(),
                    counts: review.author_can_push,
                    on_behalf_of: review.on_behalf_of.clone(),
                })
            })
            .collect();
        let review_requests = (pull.review_requests.iter())
            .map(|request| ReviewRequest {
                reviewer: if request.team {
                    Reviewer::Team(request.reviewer.clone())
                } else {
                    Reviewer::User(request.reviewer.clone())
                },
                as_code_owner: request.as_code_owner,
            })
            .collect();
        let auto_merge = pull.auto_merge.as_ref().map(|auto_merge| AutoMerge {
            method: auto_merge.method.parse().unwrap_or_else(|_| {
                log::warn!(
                    "auto-merge's method {:?} isn't one Lo knows",
                    auto_merge.method
                );
                MergeMethod::Merge
            }),
            enabled_by: auto_merge.enabled_by.clone(),
        });
        review::merge::MergeSnapshot {
            url: pull.url.clone(),
            state: pull.state.parse().unwrap_or_default(),
            draft: pull.draft,
            mergeable: pull.mergeable.parse().unwrap_or_default(),
            merge_state: pull.merge_state.parse().unwrap_or_default(),
            review_decision: (pull.review_decision.as_deref()).and_then(|d| d.parse().ok()),
            reviews,
            review_requests,
            rules: self.review_rules(),
            required_checks: self.required_checks(),
            checks_pending: pull.checks_pending,
            unresolved_threads: pull.unresolved_threads,
            out_of_date: self.requires_up_to_date() && local.head_contains_base == Some(false),
            unmet_rules: Vec::new(),
            sees_every_rule: self.sees_every_rule(),
            queue: self.queue(),
            auto_merge,
            methods: self.allowed_methods(),
            signed_commits: self.signed_commits(),
            preferred_method: local.preferred_method,
            default_method: (self.repository.viewer_default_merge_method.as_deref())
                .and_then(|method| method.parse().ok()),
            viewer: self.viewer(),
            head: pull.head.clone(),
            head_on_screen: local.head_on_screen.clone(),
            pending_review: pull.pending_review.as_ref().map(|review| Pending {
                id: review.id.clone(),
                commit: review.commit.clone(),
                comments: review.comments,
            }),
        }
    }
}

/// Whether a pull request's author is Copilot's coding agent, for which a `pull_request` rule
/// can require one more approval.
fn is_copilot(login: &str) -> bool {
    login.eq_ignore_ascii_case("Copilot") || login.starts_with("copilot-swe-agent")
}

impl CacheEntry for MergeFacts {
    const NAME: &'static str = OPERATION_NAME;

    fn shape() -> String {
        format!(
            "{}\n{}\n{}",
            schema_shape::<Self>(),
            QUERY,
            merge_checks::QUERY
        )
    }
}

impl CachedRead for MergeFacts {
    /// What moves GitHub's verdict or what explains it: the pull request's state, draft, head
    /// and base, its review decision and merge state, its checks and queue entry, a review
    /// submitted, a thread resolved or replied to, your pending review, and anything the
    /// timeline shows, such as a review requested, auto-merge turned on or a comment. The base
    /// branch's rules change on the 5-minute refresh, read again for free when they didn't.
    const WATCHED: &'static [&'static str] = &[
        "/state",
        "/isDraft",
        "/headRefOid",
        "/baseRefName",
        "/baseRefOid",
        "/reviewDecision",
        "/mergeStateStatus",
        "/statusCheckRollup",
        "/mergeQueueEntry",
        "/submittedReviews/totalCount",
        "/reviewThreads",
        "/viewerPendingReview",
        "/timelineEvents/filteredCount",
    ];

    /// Whether GitHub gave every part: a part it left out with an error is read again.
    fn is_complete(&self) -> bool {
        self.left_out.is_empty()
    }
}

type PullRequestData = MergeSnapshotRepositoryPullRequest;

/// Lists read a page each: those longer than their page, and those GitHub left items out of.
#[derive(Default)]
struct Lists {
    cut_short: Vec<String>,
    left_out: Vec<String>,
}

impl Lists {
    /// Notes the list `name`, of which GitHub counted `total` items and gave `read`, leaving
    /// out `missing` with an error.
    fn note(&mut self, name: &str, total: i64, read: usize, missing: usize) {
        if usize::try_from(total).unwrap_or(0) > read + missing {
            add(&mut self.cut_short, name);
        }
        if missing > 0 {
            add(&mut self.left_out, name);
        }
    }

    /// Notes that GitHub left the list `name` out altogether.
    fn left_out(&mut self, name: &str) {
        add(&mut self.left_out, name);
    }
}

/// Adds `name` to `names` unless it's there.
fn add(names: &mut Vec<String>, name: &str) {
    if !names.iter().any(|known| known == name) {
        names.push(name.to_string());
    }
}

/// Where the next page of checks starts, and how many there are.
struct NextChecks {
    total: i64,
    cursor: Option<String>,
}

impl MergeFacts {
    /// The facts in GraphQL's answer, read as `login`, and where the next page of checks starts,
    /// if there's one. The base branch's rules are still to add.
    fn from_data(
        repository: MergeSnapshotRepository,
        pull: PullRequestData,
        login: &str,
    ) -> (Self, Option<NextChecks>) {
        let mut lists = Lists::default();
        let protection = protection(&pull);
        let (pull_request, next) = pull_request_facts(pull, login, &mut lists);
        let facts = Self {
            login: login.to_string(),
            repository: RepositoryFacts {
                archived: repository.is_archived,
                viewer_permission: repository.viewer_permission.as_ref().map(github_name),
                merge_commit_allowed: repository.merge_commit_allowed,
                squash_merge_allowed: repository.squash_merge_allowed,
                rebase_merge_allowed: repository.rebase_merge_allowed,
                viewer_default_merge_method: Some(github_name(
                    &repository.viewer_default_merge_method,
                )),
            },
            pull_request,
            protection,
            rules: BranchRules::default(),
            cut_short: lists.cut_short,
            left_out: lists.left_out,
        };
        (facts, next)
    }
}

/// The pull request's facts, and where the next page of its checks starts.
fn pull_request_facts(
    pull: PullRequestData,
    login: &str,
    lists: &mut Lists,
) -> (PullRequestFacts, Option<NextChecks>) {
    let mut facts = PullRequestFacts {
        url: pull.url,
        state: github_name(&pull.state),
        draft: pull.is_draft,
        locked: pull.locked,
        mergeable: github_name(&pull.mergeable),
        merge_state: github_name(&pull.merge_state_status),
        review_decision: pull.review_decision.as_ref().map(github_name),
        head: pull.head_ref_oid,
        base: pull.base_ref_name,
        base_tip: (pull.base_ref.as_ref())
            .and_then(|base| base.target.as_ref())
            .map(|target| target.oid.clone()),
        author: pull.author.map(|author| author.login),
        viewer_did_author: pull.viewer_did_author,
        viewer_can_merge_as_admin: pull.viewer_can_merge_as_admin,
        viewer_can_enable_auto_merge: pull.viewer_can_enable_auto_merge,
        queue_required: pull.is_merge_queue_enabled,
        in_queue: pull.is_in_merge_queue,
        queue_position: (pull.merge_queue_entry)
            .and_then(|entry| u32::try_from(entry.position).ok()),
        queue_method: (pull.merge_queue)
            .and_then(|queue| queue.configuration)
            .and_then(|configuration| configuration.merge_method)
            .map(|method| github_name(&method)),
        auto_merge: pull.auto_merge_request.map(|request| AutoMergeFacts {
            method: github_name(&request.merge_method),
            enabled_by: request.enabled_by.map(|actor| actor.login),
        }),
        ..PullRequestFacts::default()
    };

    match pull.latest_opinionated_reviews {
        Some(reviews) => {
            let nodes = reviews.nodes.unwrap_or_default();
            let missing = nodes.iter().filter(|node| node.is_none()).count();
            let read: Vec<_> = nodes.into_iter().flatten().collect();
            lists.note("reviews", reviews.total_count, read.len(), missing);
            for review in read {
                let teams = review.on_behalf_of;
                let slugs: Vec<String> = (teams.nodes.iter().flatten().flatten())
                    .map(|team| team.combined_slug.clone())
                    .collect();
                lists.note("reviews' teams", teams.total_count, slugs.len(), 0);
                facts.reviews.push(ReviewFacts {
                    author: review.author.map(|author| author.login),
                    state: github_name(&review.state),
                    commit: review.commit.map(|commit| commit.oid),
                    author_can_push: review.author_can_push_to_repository,
                    on_behalf_of: slugs,
                });
            }
        }
        None => lists.left_out("reviews"),
    }

    match pull.review_requests {
        Some(requests) => {
            use MergeSnapshotRepositoryPullRequestReviewRequestsNodesRequestedReviewer as Asked;
            let nodes = requests.nodes.unwrap_or_default();
            let missing = nodes.iter().filter(|node| node.is_none()).count();
            let read: Vec<_> = nodes.into_iter().flatten().collect();
            lists.note("review requests", requests.total_count, read.len(), missing);
            for request in read {
                let (reviewer, team) = match request.requested_reviewer {
                    Some(Asked::User(user)) => (user.login, false),
                    Some(Asked::Bot(bot)) => (bot.login, false),
                    Some(Asked::Mannequin(mannequin)) => (mannequin.login, false),
                    Some(Asked::Team(team)) => {
                        let member = (team.members.nodes.iter().flatten().flatten())
                            .any(|member| member.login.eq_ignore_ascii_case(login));
                        if member && !facts.viewer_teams.contains(&team.combined_slug) {
                            facts.viewer_teams.push(team.combined_slug.clone());
                        }
                        (team.combined_slug, true)
                    }
                    // A team the account can't see, or an enterprise team.
                    Some(Asked::EnterpriseTeam) | None => continue,
                };
                facts.review_requests.push(RequestFacts {
                    reviewer,
                    team,
                    as_code_owner: request.as_code_owner,
                });
            }
        }
        None => lists.left_out("review requests"),
    }

    match pull.viewer_pending_review {
        Some(reviews) => {
            let node = (reviews.nodes.into_iter().flatten().flatten()).next();
            facts.pending_review = node.map(|node| PendingReviewNode {
                id: node.id,
                commit: node.commit.map(|commit| commit.oid),
                comments: usize::try_from(node.comments.total_count).unwrap_or_default(),
            });
        }
        // Read as none, approve and ship would send an approval GitHub refuses.
        None => lists.left_out("your pending review"),
    }

    let threads = pull.review_threads;
    let nodes = threads.nodes.unwrap_or_default();
    let missing = nodes.iter().filter(|node| node.is_none()).count();
    let unresolved = (nodes.iter().flatten())
        .filter(|thread| !thread.is_resolved)
        .count();
    lists.note(
        "review threads",
        threads.total_count,
        nodes.len() - missing,
        missing,
    );
    facts.unresolved_threads = u32::try_from(unresolved).unwrap_or(u32::MAX);

    let suites = (pull.commits.nodes.into_iter().flatten().flatten())
        .next()
        .and_then(|node| node.commit.check_suites);
    if let Some(suites) = suites {
        let nodes: Vec<_> = suites.nodes.into_iter().flatten().flatten().collect();
        // Suites past the page may still be running: wait for them, rather than block on a
        // check one of them will report.
        facts.checks_pending |= usize::try_from(suites.total_count).unwrap_or(0) > nodes.len();
        facts.checks_pending |= (nodes.iter()).any(|suite| {
            let status = github_name(&suite.status);
            let queued = matches!(status.as_str(), "QUEUED" | "REQUESTED");
            status != "COMPLETED" && (!queued || suite.workflow_run.is_some())
        });
    }
    let Some(rollup) = pull.status_check_rollup else {
        return (facts, None);
    };
    let contexts = rollup.contexts;
    let running = (contexts.check_run_counts_by_state.iter().flatten()).any(|count| {
        count.count > 0
            && matches!(
                github_name(&count.state).as_str(),
                "QUEUED" | "IN_PROGRESS" | "PENDING" | "WAITING" | "REQUESTED"
            )
    });
    let pending = (contexts.status_context_counts_by_state.iter().flatten())
        .any(|count| count.count > 0 && github_name(&count.state) == "PENDING");
    facts.checks_pending |= running || pending;
    let nodes = contexts.nodes.unwrap_or_default();
    if nodes.iter().any(Option::is_none) {
        lists.left_out("checks");
    }
    facts.checks = nodes.into_iter().flatten().filter_map(check).collect();
    let next = (contexts.page_info.has_next_page).then_some(NextChecks {
        total: contexts.total_count,
        cursor: contexts.page_info.end_cursor,
    });
    (facts, next)
}

/// A required check run or commit status, or `None` for one that isn't required.
fn check(node: MergeCheck) -> Option<CheckFacts> {
    match node {
        MergeCheck::CheckRun(run) => run.is_required.then(|| CheckFacts::CheckRun {
            name: run.name,
            status: github_name(&run.status),
            conclusion: run.conclusion.as_ref().map(github_name),
            url: run.details_url.or(Some(run.url)),
        }),
        MergeCheck::StatusContext(status) => status.is_required.then(|| CheckFacts::Status {
            context: status.context,
            state: github_name(&status.state),
            url: status.target_url,
        }),
    }
}

/// The base branch's classic protection, as much of it as the account sees.
fn protection(pull: &PullRequestData) -> Option<Protection> {
    let base = pull.base_ref.as_ref()?;
    let visible = base.ref_update_rule.as_ref();
    let viewer_can_push = visible.is_none_or(|rule| rule.viewer_can_push);
    let count = |count: Option<i64>| count.and_then(|count| u32::try_from(count).ok());
    if let Some(rule) = &base.branch_protection_rule {
        let required_checks = if rule.requires_status_checks {
            (rule.required_status_checks.iter().flatten())
                .map(|check| check.context.clone())
                .collect()
        } else {
            Vec::new()
        };
        return Some(Protection {
            complete: true,
            required_approvals: if rule.requires_approving_reviews {
                count(rule.required_approving_review_count).unwrap_or(0)
            } else {
                0
            },
            code_owner_review: rule.requires_code_owner_reviews,
            conversation_resolution: rule.requires_conversation_resolution,
            dismiss_stale_reviews: rule.dismisses_stale_reviews,
            last_push_approval: rule.require_last_push_approval,
            strict: rule.requires_status_checks && rule.requires_strict_status_checks,
            required_checks,
            linear_history: rule.requires_linear_history,
            signatures: rule.requires_commit_signatures,
            deployments: rule.requires_deployments,
            lock_branch: rule.lock_branch,
            viewer_can_push,
        });
    }
    let rule = visible?;
    Some(Protection {
        complete: false,
        required_approvals: count(rule.required_approving_review_count).unwrap_or(0),
        code_owner_review: rule.requires_code_owner_reviews,
        conversation_resolution: rule.requires_conversation_resolution,
        required_checks: (rule
            .required_status_check_contexts
            .iter()
            .flatten()
            .flatten())
        .cloned()
        .collect(),
        linear_history: rule.requires_linear_history,
        signatures: rule.requires_signatures,
        viewer_can_push,
        ..Protection::default()
    })
}

/// A rule as `GET /repos/{owner}/{repo}/rules/branches/{branch}` lists it.
#[derive(Deserialize)]
struct RestRule {
    #[serde(rename = "type")]
    type_: String,
    #[serde(default)]
    parameters: Option<serde_json::Value>,
}

/// A `pull_request` rule's parameters, as REST names them.
#[derive(Deserialize)]
struct RestPullRequest {
    #[serde(default)]
    required_approving_review_count: u32,
    #[serde(default)]
    require_code_owner_review: bool,
    #[serde(default)]
    required_review_thread_resolution: bool,
    #[serde(default)]
    dismiss_stale_reviews_on_push: bool,
    #[serde(default)]
    require_last_push_approval: bool,
    #[serde(default)]
    allowed_merge_methods: Option<Vec<String>>,
    #[serde(default)]
    required_reviewers: Vec<serde_json::Value>,
    #[serde(default)]
    require_extra_approval_for_unattributed_changes: bool,
}

/// A `required_status_checks` rule's parameters, as REST names them.
#[derive(Deserialize)]
struct RestStatusChecks {
    #[serde(default)]
    strict_required_status_checks_policy: bool,
    required_status_checks: Vec<RestStatusCheck>,
}

#[derive(Deserialize)]
struct RestStatusCheck {
    context: String,
}

/// A `merge_queue` rule's parameters, as REST names them.
#[derive(Deserialize)]
struct RestMergeQueue {
    merge_method: Option<String>,
}

/// The rule types that never keep a pull request from merging.
const HARMLESS_RULES: [&str; 5] = [
    "creation",
    "deletion",
    "non_fast_forward",
    "copilot_code_review",
    "tag_name_pattern",
];

impl BranchRule {
    /// A rule from REST. One whose parameters Lo can't read is one Lo doesn't model.
    fn from_rest(rule: RestRule) -> Self {
        fn parameters<T: serde::de::DeserializeOwned>(rule: &RestRule) -> Option<T> {
            serde_json::from_value(rule.parameters.clone()?).ok()
        }
        let parsed = match rule.type_.as_str() {
            "pull_request" => parameters::<RestPullRequest>(&rule).map(|p| {
                Self::PullRequest(PullRequestRule {
                    required_approvals: p.required_approving_review_count,
                    code_owner_review: p.require_code_owner_review,
                    conversation_resolution: p.required_review_thread_resolution,
                    dismiss_stale_reviews: p.dismiss_stale_reviews_on_push,
                    last_push_approval: p.require_last_push_approval,
                    allowed_methods: p.allowed_merge_methods,
                    required_reviewers: !p.required_reviewers.is_empty(),
                    extra_approval_for_copilot: p.require_extra_approval_for_unattributed_changes,
                })
            }),
            "required_status_checks" => parameters::<RestStatusChecks>(&rule).map(|p| {
                Self::RequiredStatusChecks(StatusChecksRule {
                    strict: p.strict_required_status_checks_policy,
                    checks: (p.required_status_checks.into_iter())
                        .map(|check| check.context)
                        .collect(),
                })
            }),
            "merge_queue" => Some(Self::MergeQueue {
                method: parameters::<RestMergeQueue>(&rule).and_then(|p| p.merge_method),
            }),
            "required_linear_history" => Some(Self::RequiredLinearHistory),
            "required_signatures" => Some(Self::RequiredSignatures),
            kind if HARMLESS_RULES.contains(&kind) => Some(Self::Harmless {
                kind: kind.to_string(),
            }),
            _ => None,
        };
        parsed.unwrap_or(Self::Other { kind: rule.type_ })
    }
}

impl GithubClient {
    /// What decides whether `pull` can merge, read as the account whose login is `login`, with
    /// its base branch's rules. `known`, read before, gives the rules' ETag, so unchanged rules
    /// are free. Rules that can't be read for any reason but the network, the rate limit or the
    /// token are noted as unavailable rather than failing the read.
    pub async fn merge_facts(
        &self,
        pull: &PullRef,
        login: &str,
        known: Option<&MergeFacts>,
    ) -> Result<MergeFacts, GithubError> {
        let mut repository = self
            .graphql_keeping_partial::<MergeSnapshot>(
                Variables {
                    owner: pull.repo.owner.clone(),
                    name: pull.repo.name.clone(),
                    number: pull.number,
                    login: login.to_string(),
                },
                2,
            )
            .await?
            .repository
            .ok_or_else(|| not_found(format!("repository {}", pull.repo)))?;
        let pull_request = (repository.pull_request.take())
            .ok_or_else(|| not_found(format!("pull request {pull}")))?;
        let (mut facts, next) = MergeFacts::from_data(repository, pull_request, login);
        if let Some(next) = next {
            self.read_other_checks(pull, &mut facts, next).await?;
        }
        let branch = facts.pull_request.base.clone();
        facts.rules = self
            .branch_rules(&pull.repo, &branch, known.map(|known| &known.rules))
            .await?;
        Ok(facts)
    }

    /// Reads the checks past the first page, while there are more.
    async fn read_other_checks(
        &self,
        pull: &PullRef,
        facts: &mut MergeFacts,
        next: NextChecks,
    ) -> Result<(), GithubError> {
        let mut pager = Pager::new("checks", next.total, PAGE_SIZE);
        let mut after = pager.next(true, next.cursor)?;
        let mut pages = 1;
        while let Some(cursor) = after {
            if pages == MAX_CHECK_PAGES {
                add(&mut facts.cut_short, "checks");
                break;
            }
            let pull_request = self
                .graphql_keeping_partial::<merge_checks::MergeChecks>(
                    merge_checks::Variables {
                        owner: pull.repo.owner.clone(),
                        name: pull.repo.name.clone(),
                        number: pull.number,
                        after: cursor,
                    },
                    2,
                )
                .await?
                .repository
                .and_then(|repository| repository.pull_request)
                .ok_or_else(|| not_found(format!("pull request {pull}")))?;
            let rollup = (pull_request.head_ref_oid == facts.pull_request.head)
                .then_some(pull_request.status_check_rollup)
                .flatten()
                .ok_or_else(|| {
                    GithubError::Inconsistent(format!(
                        "{pull}'s head moved while its checks were read"
                    ))
                })?;
            let contexts = rollup.contexts;
            let nodes = contexts.nodes.unwrap_or_default();
            if nodes.iter().any(Option::is_none) {
                add(&mut facts.left_out, "checks");
            }
            for node in nodes.into_iter().flatten() {
                (facts.pull_request.checks).extend(check(same_check(&node)?));
            }
            pages += 1;
            after = pager.next(
                contexts.page_info.has_next_page,
                contexts.page_info.end_cursor,
            )?;
        }
        Ok(())
    }

    /// The active rules of `repo`'s `branch`, asked with the ETag of `known` when it's the same
    /// branch's.
    async fn branch_rules(
        &self,
        repo: &RepoRef,
        branch: &str,
        known: Option<&BranchRules>,
    ) -> Result<BranchRules, GithubError> {
        let known = known.filter(|known| known.branch == branch && known.unavailable.is_none());
        let path = format!(
            "/repos/{}/{}/rules/branches/{}?per_page={PAGE_SIZE}",
            repo.owner,
            repo.name,
            path_encoded(branch)
        );
        let etag = known.and_then(|known| known.etag.as_deref());
        let read = async {
            let response = self.send(Method::GET, &path, None, etag).await?;
            if response.status == StatusCode::NOT_MODIFIED
                && let Some(known) = known
            {
                return Ok(known.clone());
            }
            let rules: Vec<RestRule> = response.json()?;
            Ok(BranchRules {
                branch: branch.to_string(),
                rules: rules.into_iter().map(BranchRule::from_rest).collect(),
                etag: response.etag().map(str::to_string),
                unavailable: None,
                cut_short: response.header("link").and_then(next_link).is_some(),
            })
        };
        match read.await {
            Err(error) if !stops_the_read(&error) => {
                log::warn!("reading the rules on {repo} {branch}: {error:#}");
                Ok(BranchRules {
                    branch: branch.to_string(),
                    unavailable: Some(error.to_string()),
                    ..BranchRules::default()
                })
            }
            read => read,
        }
    }
}

/// A check `MergeChecks` read, as `MergeSnapshot` reads one: the two queries select the same
/// fields, which `graphql_client` gives a type each.
fn same_check(node: &merge_checks::MergeCheck) -> Result<MergeCheck, GithubError> {
    serde_json::to_value(node)
        .and_then(serde_json::from_value)
        .map_err(|error| {
            GithubError::Decode(format!(
                "MergeChecks' checks aren't MergeSnapshot's: {error}"
            ))
        })
}

/// [`GithubClient::merge_facts`] as [`crate::poll::Poller::read_cached`] runs it.
pub async fn read_merge_facts(
    client: GithubClient,
    pull: PullRef,
    login: String,
    known: Option<Arc<MergeFacts>>,
) -> Result<MergeFacts, GithubError> {
    client.merge_facts(&pull, &login, known.as_deref()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_queries_read_a_check_the_same_way() {
        let fragment = |query: &str| {
            let start = query.find("fragment MergeCheck").unwrap();
            query[start..].to_string()
        };
        assert_eq!(fragment(QUERY), fragment(merge_checks::QUERY));
    }

    fn rest(json: serde_json::Value) -> BranchRule {
        BranchRule::from_rest(serde_json::from_value(json).unwrap())
    }

    #[test]
    fn rules_are_read_from_rest_and_any_lo_does_not_model_is_other() {
        assert_eq!(
            rest(serde_json::json!({
                "type": "pull_request",
                "parameters": {
                    "required_approving_review_count": 2,
                    "dismiss_stale_reviews_on_push": true,
                    "require_code_owner_review": true,
                    "require_last_push_approval": false,
                    "required_review_thread_resolution": true,
                    "allowed_merge_methods": ["squash", "rebase"],
                    "required_reviewers": [],
                    "require_extra_approval_for_unattributed_changes": true,
                },
                "ruleset_source_type": "Organization",
                "ruleset_source": "acme",
                "ruleset_id": 7,
            })),
            BranchRule::PullRequest(PullRequestRule {
                required_approvals: 2,
                code_owner_review: true,
                conversation_resolution: true,
                dismiss_stale_reviews: true,
                last_push_approval: false,
                allowed_methods: Some(vec!["squash".into(), "rebase".into()]),
                required_reviewers: false,
                extra_approval_for_copilot: true,
            })
        );
        assert_eq!(
            rest(serde_json::json!({
                "type": "required_status_checks",
                "parameters": {
                    "strict_required_status_checks_policy": true,
                    "required_status_checks": [{"context": "ci", "integration_id": 15368}],
                },
            })),
            BranchRule::RequiredStatusChecks(StatusChecksRule {
                strict: true,
                checks: vec!["ci".into()],
            })
        );
        assert_eq!(
            rest(
                serde_json::json!({"type": "merge_queue", "parameters": {"merge_method": "REBASE"}})
            ),
            BranchRule::MergeQueue {
                method: Some("REBASE".into())
            }
        );
        assert_eq!(
            rest(serde_json::json!({"type": "non_fast_forward"})),
            BranchRule::Harmless {
                kind: "non_fast_forward".into()
            }
        );
        for kind in [
            "code_scanning",
            "required_deployments",
            "update",
            "lock_branch",
            "new",
        ] {
            assert_eq!(
                rest(serde_json::json!({"type": kind, "parameters": {}})),
                BranchRule::Other { kind: kind.into() },
                "{kind}"
            );
        }
        // Parameters Lo can't read: a rule it can't model.
        assert_eq!(
            rest(serde_json::json!({"type": "required_status_checks", "parameters": {}})),
            BranchRule::Other {
                kind: "required_status_checks".into()
            }
        );
    }

    fn pull_request_rule(edit: impl FnOnce(&mut PullRequestRule)) -> BranchRule {
        let mut rule = PullRequestRule::default();
        edit(&mut rule);
        BranchRule::PullRequest(rule)
    }

    fn with_rules(rules: Vec<BranchRule>) -> MergeFacts {
        MergeFacts {
            repository: RepositoryFacts {
                merge_commit_allowed: true,
                squash_merge_allowed: true,
                rebase_merge_allowed: true,
                ..RepositoryFacts::default()
            },
            rules: BranchRules {
                branch: "main".into(),
                rules,
                ..BranchRules::default()
            },
            ..MergeFacts::default()
        }
    }

    #[test]
    fn the_strictest_of_the_protection_and_the_rulesets_applies() {
        let mut facts = with_rules(vec![
            pull_request_rule(|rule| {
                rule.required_approvals = 2;
                rule.last_push_approval = true;
            }),
            pull_request_rule(|rule| {
                rule.required_approvals = 1;
                rule.conversation_resolution = true;
            }),
        ]);
        facts.protection = Some(Protection {
            complete: true,
            required_approvals: 1,
            code_owner_review: true,
            ..Protection::default()
        });

        assert_eq!(
            facts.review_rules(),
            ReviewRules {
                required_approvals: 2,
                code_owner_review: true,
                conversation_resolution: true,
                dismiss_stale_reviews: false,
                last_push_approval: true,
            }
        );
    }

    #[test]
    fn each_ruleset_and_linear_history_narrow_the_methods() {
        let squash_or_rebase = pull_request_rule(|rule| {
            rule.allowed_methods = Some(vec!["squash".into(), "rebase".into()]);
        });
        let merge_or_squash = pull_request_rule(|rule| {
            rule.allowed_methods = Some(vec!["merge".into(), "squash".into()]);
        });

        assert_eq!(
            with_rules(vec![squash_or_rebase.clone(), merge_or_squash])
                .allowed_methods()
                .to_vec(),
            [MergeMethod::Squash]
        );
        assert_eq!(
            with_rules(vec![BranchRule::RequiredLinearHistory])
                .allowed_methods()
                .to_vec(),
            [MergeMethod::Squash, MergeMethod::Rebase]
        );
        // Signed commits don't narrow them here: review::merge leaves out rebase itself.
        let signed = with_rules(vec![BranchRule::RequiredSignatures]);
        assert_eq!(signed.allowed_methods().to_vec().len(), 3);
        assert!(signed.signed_commits());
    }

    #[test]
    fn copilots_extra_approval_matters_only_on_copilots_pull_requests() {
        let mut facts = with_rules(vec![pull_request_rule(|rule| {
            rule.extra_approval_for_copilot = true;
        })]);
        facts.pull_request.author = Some("octocat".into());
        assert!(facts.sees_every_rule());
        facts.pull_request.author = Some("Copilot".into());
        assert!(!facts.sees_every_rule());
    }

    #[test]
    fn up_to_date_is_required_only_with_strict_checks_and_a_check_to_require() {
        let strict = |checks: Vec<String>| {
            with_rules(vec![BranchRule::RequiredStatusChecks(StatusChecksRule {
                strict: true,
                checks,
            })])
        };
        assert!(strict(vec!["ci".into()]).requires_up_to_date());
        assert!(!strict(Vec::new()).requires_up_to_date());

        // Partly seen protection says nothing about it.
        let mut seen_in_part = with_rules(Vec::new());
        seen_in_part.protection = Some(Protection {
            required_checks: vec!["ci".into()],
            ..Protection::default()
        });
        assert!(!seen_in_part.requires_up_to_date());
        assert!(!seen_in_part.sees_every_rule());
    }

    #[test]
    fn an_unknown_value_never_reads_as_passing_or_mergeable() {
        let run = |status: &str, conclusion: Option<&str>| {
            CheckFacts::CheckRun {
                name: "ci".into(),
                status: status.into(),
                conclusion: conclusion.map(str::to_string),
                url: None,
            }
            .required()
            .state
        };
        assert_eq!(run("COMPLETED", Some("SUCCESS")), CheckState::Passed);
        assert_eq!(run("COMPLETED", Some("SOMETHING_NEW")), CheckState::Failed);
        assert_eq!(run("SOMETHING_NEW", None), CheckState::Running);
        let status = CheckFacts::Status {
            context: "ci".into(),
            state: "SOMETHING_NEW".into(),
            url: None,
        };
        assert_eq!(status.required().state, CheckState::Running);

        let mut facts = with_rules(Vec::new());
        facts.pull_request.state = "OPEN".into();
        facts.pull_request.mergeable = "SOMETHING_NEW".into();
        facts.pull_request.merge_state = "MERGE_QUEUED".into();
        let snapshot = facts.snapshot(&Local::default());
        assert_eq!(snapshot.mergeable, review::merge::MergeableState::Unknown);
        assert_eq!(
            snapshot.merge_state,
            review::merge::MergeStateStatus::Unknown
        );
    }
}
