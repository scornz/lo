//! `OpenPullRequests`: a repository's open pull requests, with what finding stacks needs: the
//! repository's trunks, and the merged pull requests its stacks link through
//! ([`super::merged_pull_requests`]).

use graphql_client::GraphQLQuery;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::merged_pull_requests::{MergedPullRequest, StackEntry};
use super::pull_request_check::{MergeQueueEntryState, PullRequestReviewDecision, StatusState};
use super::{DateTime, GitObjectID, MAX_OPEN_PULL_REQUESTS, PAGE_SIZE, Pager, not_found};
use crate::client::GithubClient;
use crate::error::GithubError;
use crate::trunks::{RefNames, Trunks};

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/schema.graphql",
    query_path = "graphql/open_pull_requests.graphql",
    response_derives = "Debug, Clone, PartialEq, Serialize",
    variables_derives = "Debug, Clone",
    extern_enums("PullRequestReviewDecision", "StatusState", "MergeQueueEntryState")
)]
pub struct OpenPullRequests;

// The generated types, named from this module, such as `queries::open_pull_requests::Variables`.
pub use self::open_pull_requests::*;

pub type OpenPullRequest = OpenPullRequestsRepositoryPullRequestsNodes;

/// How many of the pull requests merged most recently are read with the open ones.
pub const RECENTLY_MERGED: i64 = 20;

/// A repository's open pull requests, with what finding stacks needs besides
/// ([`Self::stacks`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OpenPullRequestList {
    /// Their shape is the `OpenPullRequests` query's, which the cache's version covers.
    #[schemars(with = "Vec<serde_json::Value>")]
    pub pull_requests: Vec<OpenPullRequest>,
    /// How many are open, which is more than `pull_requests` holds when it was capped, or when
    /// GitHub left some out as null.
    pub total_count: i64,
    /// Whether reading stopped at [`MAX_OPEN_PULL_REQUESTS`] with more left.
    pub truncated: bool,
    /// What decides the repository's trunks.
    pub trunks: Trunks,
    /// The merged pull requests finding stacks needs, each once: the [`RECENTLY_MERGED`] merged
    /// most recently, and those from the branches open pull requests target, followed down
    /// (`stacks::branches_to_look_up`).
    pub merged: Vec<MergedPullRequest>,
    /// Whether some branches' merged pull requests weren't looked up, past
    /// [`super::merged_pull_requests::MAX_BRANCH_LOOKUPS`]. Layers above them sit on those
    /// branches as roots.
    pub merged_truncated: bool,
}

impl OpenPullRequestList {
    /// Whether there were more open pull requests than [`MAX_OPEN_PULL_REQUESTS`].
    pub fn is_truncated(&self) -> bool {
        self.truncated
    }

    /// The open pull request numbered `number`, if it's listed.
    pub fn pull_request(&self, number: i64) -> Option<&OpenPullRequest> {
        self.pull_requests.iter().find(|pr| pr.number == number)
    }

    /// The open and merged pull requests, as [`stacks::Stacks::find`] takes them.
    pub fn stack_pull_requests(&self) -> Vec<stacks::PullRequest> {
        (self.pull_requests.iter().map(stacks::PullRequest::from))
            .chain(self.merged.iter().map(stacks::PullRequest::from))
            .collect()
    }

    /// The repository's stacks.
    pub fn stacks(&self) -> stacks::Stacks {
        let prs = self.stack_pull_requests();
        let trunks = self.trunks.among(&prs);
        let trunks: Vec<&str> = trunks.iter().map(String::as_str).collect();
        stacks::Stacks::find(&prs, &trunks)
    }
}

impl From<&OpenPullRequest> for stacks::PullRequest {
    fn from(pr: &OpenPullRequest) -> Self {
        let commits = (pr.commits.nodes.iter().flatten().flatten())
            .map(|node| node.commit.oid.clone())
            .collect();
        let native_stack = pr.stack_entry.as_ref().and_then(|entry| {
            StackEntry::new(entry.stack.as_ref()?.number, entry.position).map(Into::into)
        });
        Self {
            number: u64::try_from(pr.number).unwrap_or_default(),
            state: stacks::State::Open,
            base_ref: pr.base_ref_name.clone(),
            base_tip: (pr.base_ref.as_ref())
                .and_then(|base| Some(base.target.as_ref()?.oid.clone())),
            head_ref: pr.head_ref_name.clone(),
            head_in_base_repo: !pr.is_cross_repository,
            head_oid: pr.head_ref_oid.clone(),
            commits,
            created_at: pr.created_at.into(),
            native_stack,
        }
    }
}

/// What decides the repository's trunks, from a page of the query.
fn trunks(repository: &OpenPullRequestsRepository) -> Trunks {
    let rules = repository.branch_protection_rules.nodes.iter().flatten();
    let protected = rules.flatten().map(|rule| rule.pattern.clone()).collect();
    let rulesets = (repository.rulesets.iter())
        .flat_map(|rulesets| rulesets.nodes.iter().flatten().flatten())
        .filter(|ruleset| ruleset.enforcement != RuleEnforcement::DISABLED)
        .filter_map(|ruleset| ruleset.conditions.ref_name.as_ref())
        .map(|ref_name| RefNames {
            include: ref_name.include.clone(),
            exclude: ref_name.exclude.clone(),
        })
        .collect();
    Trunks {
        default_branch: (repository.default_branch_ref.as_ref()).map(|branch| branch.name.clone()),
        protected,
        rulesets,
    }
}

impl GithubClient {
    /// A repository's open pull requests, newest first, up to [`MAX_OPEN_PULL_REQUESTS`], with
    /// its trunks and the merged pull requests finding stacks needs.
    pub async fn open_pull_requests(
        &self,
        owner: &str,
        name: &str,
    ) -> Result<OpenPullRequestList, GithubError> {
        (self.open_pull_requests_paged(owner, name, PAGE_SIZE, RECENTLY_MERGED)).await
    }

    /// [`Self::open_pull_requests`], reading `page_size` open pull requests a page, and the
    /// `recently_merged` pull requests merged most recently.
    pub async fn open_pull_requests_paged(
        &self,
        owner: &str,
        name: &str,
        page_size: i64,
        recently_merged: i64,
    ) -> Result<OpenPullRequestList, GithubError> {
        let mut list = OpenPullRequestList {
            pull_requests: Vec::new(),
            total_count: 0,
            truncated: false,
            trunks: Trunks::default(),
            merged: Vec::new(),
            merged_truncated: false,
        };
        let mut pager = None;
        let mut after = None;
        loop {
            let first_page = pager.is_none();
            let repository = self
                .graphql_keeping_partial::<OpenPullRequests>(
                    Variables {
                        owner: owner.to_string(),
                        name: name.to_string(),
                        first: page_size,
                        after: after.take(),
                        merged: if first_page { recently_merged } else { 0 },
                    },
                    2,
                )
                .await?
                .repository
                .ok_or_else(|| not_found(format!("repository {owner}/{name}")))?;
            if first_page {
                list.trunks = trunks(&repository);
                let merged = repository.merged_pull_requests.nodes.iter().flatten();
                list.merged = merged.flatten().map(MergedPullRequest::from).collect();
            }
            let page = repository.pull_requests;
            list.total_count = page.total_count;
            list.pull_requests
                .extend(page.nodes.unwrap_or_default().into_iter().flatten());
            let pager = pager.get_or_insert_with(|| {
                let capped = page.total_count.min(MAX_OPEN_PULL_REQUESTS as i64);
                Pager::new("open pull requests", capped, page_size)
            });
            if list.pull_requests.len() >= MAX_OPEN_PULL_REQUESTS {
                list.truncated = page.page_info.has_next_page
                    || list.pull_requests.len() > MAX_OPEN_PULL_REQUESTS;
                list.pull_requests.truncate(MAX_OPEN_PULL_REQUESTS);
                break;
            }
            match pager.next(page.page_info.has_next_page, page.page_info.end_cursor)? {
                Some(cursor) => after = Some(cursor),
                None => break,
            }
        }
        self.look_up_merged(owner, name, &mut list).await?;
        Ok(list)
    }
}
