//! `OpenPullRequests`: a repository's open pull requests, with what finding stacks needs besides:
//! the repository's default branch, and the merged pull requests its stacks link through
//! ([`super::merged_pull_requests`]).

use graphql_client::GraphQLQuery;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::merged_pull_requests::{BranchLookup, MergedPullRequest, StackEntry};
use super::trunk_rules::TrunkRuleList;
use super::{DateTime, GitObjectID, MAX_OPEN_PULL_REQUESTS, PAGE_SIZE, Pager, not_found};
use crate::client::GithubClient;
use crate::error::GithubError;
use crate::pulls::RepoRef;
use crate::trunks::Trunks;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/schema.graphql",
    query_path = "graphql/open_pull_requests.graphql",
    response_derives = "Debug, Clone, PartialEq, Serialize",
    variables_derives = "Debug, Clone"
)]
pub struct OpenPullRequests;

// The generated types, named from this module, such as `queries::open_pull_requests::Variables`.
pub use self::open_pull_requests::*;

pub type OpenPullRequest = OpenPullRequestsRepositoryPullRequestsNodes;

/// How many of the merged pull requests updated most recently are read with the open ones.
pub const RECENTLY_UPDATED_MERGED: i64 = 20;

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
    /// The repository's default branch, or `None` for an empty repository.
    pub default_branch: Option<String>,
    /// The merged pull requests finding stacks needs, each once: the
    /// [`RECENTLY_UPDATED_MERGED`] updated most recently, and those from the branches open pull
    /// requests target, followed down (`stacks::branches_to_look_up`).
    pub merged: Vec<MergedPullRequest>,
    /// The branches whose merged pull requests were looked up, which the next read carries over
    /// while their tips stay put.
    pub lookups: Vec<BranchLookup>,
    /// Whether some branches' merged pull requests weren't looked up, past
    /// [`super::merged_pull_requests::MAX_BRANCH_LOOKUPS`] or after a lookup failed. Layers
    /// above them sit on those branches as roots.
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

    /// The open and merged pull requests, as [`stacks::Stacks::find`] takes them, with the
    /// merged ones of `more` that the list doesn't have.
    pub fn stack_pull_requests(&self, more: &[MergedPullRequest]) -> Vec<stacks::PullRequest> {
        let more = (more.iter()).filter(|pr| !self.merged.iter().any(|m| m.number == pr.number));
        (self.pull_requests.iter().map(stacks::PullRequest::from))
            .chain(
                self.merged
                    .iter()
                    .chain(more)
                    .map(stacks::PullRequest::from),
            )
            .collect()
    }

    /// What decides the repository's trunks: its default branch, the protection rules that cover
    /// its pull requests' branches, and the rulesets in `rules`, once they've been read.
    pub fn trunks(&self, rules: Option<&TrunkRuleList>) -> Trunks {
        let open = self.pull_requests.iter().flat_map(|pr| {
            let base = (pr.base_ref.as_ref())
                .and_then(|base| Some(&base.ref_update_rule.as_ref()?.pattern));
            let head = (pr.head_ref.as_ref())
                .and_then(|head| Some(&head.ref_update_rule.as_ref()?.pattern));
            base.into_iter().chain(head)
        });
        let merged = self
            .merged
            .iter()
            .filter_map(|pr| pr.base_protection.as_ref());
        let mut protected: Vec<String> = open.chain(merged).cloned().collect();
        protected.sort_unstable();
        protected.dedup();
        Trunks {
            default_branch: self.default_branch.clone(),
            protected,
            rulesets: rules
                .map(|rules| rules.rulesets.clone())
                .unwrap_or_default(),
        }
    }

    /// The repository's stacks, with the rulesets in `rules` and the merged pull requests of
    /// `more`, such as those below the layers GitHub moved
    /// ([`super::stack_layers::StackLayerList::merged`]).
    pub fn stacks(
        &self,
        rules: Option<&TrunkRuleList>,
        more: &[MergedPullRequest],
    ) -> stacks::Stacks {
        let prs = self.stack_pull_requests(more);
        let trunks = self.trunks(rules).among(&prs);
        let trunks: Vec<&str> = trunks.iter().map(String::as_str).collect();
        stacks::Stacks::find(&prs, &trunks)
    }

    /// `branch`'s tip, as a pull request that targets it says.
    pub fn branch_tip(&self, branch: &str) -> Option<String> {
        let open = (self.pull_requests.iter())
            .filter(|pr| pr.base_ref_name == branch)
            .find_map(|pr| Some(pr.base_ref.as_ref()?.target.as_ref()?.oid.clone()));
        open.or_else(|| {
            (self.merged.iter())
                .filter(|pr| pr.base_ref == branch)
                .find_map(|pr| pr.base_tip.clone())
        })
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

impl GithubClient {
    /// A repository's open pull requests, newest first, up to [`MAX_OPEN_PULL_REQUESTS`], with
    /// the merged pull requests finding stacks needs.
    pub async fn open_pull_requests(
        &self,
        repo: &RepoRef,
    ) -> Result<OpenPullRequestList, GithubError> {
        let mut list =
            (self.open_pull_requests_paged(repo, PAGE_SIZE, RECENTLY_UPDATED_MERGED)).await?;
        self.look_up_merged(repo, &mut list, None).await?;
        Ok(list)
    }

    /// The pages of [`Self::open_pull_requests`], `page_size` open pull requests a page, with
    /// the `recently_updated` merged pull requests updated most recently, but none of the merged
    /// pull requests looked up by branch ([`Self::look_up_merged`]).
    pub async fn open_pull_requests_paged(
        &self,
        repo: &RepoRef,
        page_size: i64,
        recently_updated: i64,
    ) -> Result<OpenPullRequestList, GithubError> {
        let mut list = OpenPullRequestList {
            pull_requests: Vec::new(),
            total_count: 0,
            truncated: false,
            default_branch: None,
            merged: Vec::new(),
            lookups: Vec::new(),
            merged_truncated: false,
        };
        let mut pager = None;
        let mut after = None;
        loop {
            let first_page = pager.is_none();
            let repository = self
                .graphql_keeping_partial::<OpenPullRequests>(
                    Variables {
                        owner: repo.owner.clone(),
                        name: repo.name.clone(),
                        first: page_size,
                        after: after.take(),
                        merged: if first_page { recently_updated } else { 0 },
                    },
                    2,
                )
                .await?
                .repository
                .ok_or_else(|| not_found(format!("repository {repo}")))?;
            if first_page {
                list.default_branch = (repository.default_branch_ref).map(|branch| branch.name);
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
                return Ok(list);
            }
            match pager.next(page.page_info.has_next_page, page.page_info.end_cursor)? {
                Some(cursor) => after = Some(cursor),
                None => return Ok(list),
            }
        }
    }
}
