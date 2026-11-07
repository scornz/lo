//! `MergedPullRequests`: a branch's merged pull requests, which link the open layers of a stack
//! whose lower layers merged.

use std::collections::BTreeSet;
use std::time::SystemTime;

use graphql_client::GraphQLQuery;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::open_pull_requests::OpenPullRequestList;
use super::{DateTime, GitObjectID, not_found};
use crate::client::GithubClient;
use crate::error::GithubError;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/schema.graphql",
    query_path = "graphql/merged_pull_requests.graphql",
    response_derives = "Debug, Clone, PartialEq, Serialize",
    variables_derives = "Debug, Clone"
)]
pub struct MergedPullRequests;

// The generated types, named from this module, such as `queries::merged_pull_requests::Variables`.
pub use self::merged_pull_requests::*;

/// How many of a branch's merged pull requests are read, newest first. A branch name is rarely
/// used for more than one.
pub const MERGED_PER_BRANCH: i64 = 10;
/// The most branches whose merged pull requests are looked up for one list of open pull
/// requests, a request each.
pub const MAX_BRANCH_LOOKUPS: usize = 10;

/// A merged pull request, with what finding stacks reads of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MergedPullRequest {
    pub number: u64,
    #[schemars(with = "String")]
    pub created_at: DateTime,
    /// The branch it merged into, and that branch's tip now, if it's still there.
    pub base_ref: String,
    pub base_tip: Option<String>,
    pub head_ref: String,
    /// Whether its head branch is in the base repository, not a fork.
    pub head_in_base_repo: bool,
    pub head_oid: String,
    /// Its latest commits, oldest first, the head last.
    pub commits: Vec<String>,
    /// Whether its head is in its base branch: it's `mergeCommit`, as after a fast-forward, or
    /// one of its parents, as after a merge commit. Not after a squash or a rebase. GitHub names
    /// a merge commit for every merged pull request; without one, the head is taken to be in.
    pub head_in_base: bool,
    /// Its place in a GitHub stack, if it was in one.
    pub native_stack: Option<StackEntry>,
}

/// A pull request's place in a GitHub stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StackEntry {
    /// The stack's number.
    pub stack: u64,
    /// 1 at the bottom.
    pub position: u32,
}

impl StackEntry {
    /// The entry for `position` in the stack numbered `stack`, if both are in range.
    pub fn new(stack: i64, position: i64) -> Option<Self> {
        Some(Self {
            stack: u64::try_from(stack).ok()?,
            position: u32::try_from(position).ok()?,
        })
    }
}

impl From<StackEntry> for stacks::NativeStackEntry {
    fn from(entry: StackEntry) -> Self {
        Self {
            stack: entry.stack,
            position: entry.position,
        }
    }
}

impl From<&MergedPullRequest> for stacks::PullRequest {
    fn from(pr: &MergedPullRequest) -> Self {
        Self {
            number: pr.number,
            state: stacks::State::Merged {
                head_in_base: pr.head_in_base,
            },
            base_ref: pr.base_ref.clone(),
            base_tip: pr.base_tip.clone(),
            head_ref: pr.head_ref.clone(),
            head_in_base_repo: pr.head_in_base_repo,
            head_oid: pr.head_oid.clone(),
            commits: pr.commits.clone(),
            created_at: SystemTime::from(pr.created_at),
            native_stack: pr.native_stack.map(Into::into),
        }
    }
}

/// Maps the `MergedPullRequestFields` fragment, which this query and `OpenPullRequests` share
/// but generate a type each for.
macro_rules! merged_pull_request_from {
    ($fields:ty) => {
        impl From<&$fields> for MergedPullRequest {
            fn from(pr: &$fields) -> Self {
                let commits = (pr.commits.nodes.iter().flatten().flatten())
                    .map(|node| node.commit.oid.clone())
                    .collect();
                let head_in_base = pr.merge_commit.as_ref().is_none_or(|merge| {
                    merge.oid == pr.head_ref_oid
                        || (merge.parents.nodes.iter().flatten().flatten())
                            .any(|parent| parent.oid == pr.head_ref_oid)
                });
                let native_stack = pr.stack_entry.as_ref().and_then(|entry| {
                    StackEntry::new(entry.stack.as_ref()?.number, entry.position)
                });
                Self {
                    number: u64::try_from(pr.number).unwrap_or_default(),
                    created_at: pr.created_at,
                    base_ref: pr.base_ref_name.clone(),
                    base_tip: (pr.base_ref.as_ref())
                        .and_then(|base| Some(base.target.as_ref()?.oid.clone())),
                    head_ref: pr.head_ref_name.clone(),
                    head_in_base_repo: !pr.is_cross_repository,
                    head_oid: pr.head_ref_oid.clone(),
                    commits,
                    head_in_base,
                    native_stack,
                }
            }
        }
    };
}

merged_pull_request_from!(MergedPullRequestFields);
merged_pull_request_from!(super::open_pull_requests::MergedPullRequestFields);

impl GithubClient {
    /// A repository's merged pull requests from the branch `head_ref`, newest first, at most
    /// `first`. Pull requests from forks' branches of that name are among them.
    pub async fn merged_pull_requests(
        &self,
        owner: &str,
        name: &str,
        head_ref: &str,
        first: i64,
    ) -> Result<Vec<MergedPullRequest>, GithubError> {
        let pull_requests = self
            .graphql_keeping_partial::<MergedPullRequests>(
                Variables {
                    owner: owner.to_string(),
                    name: name.to_string(),
                    head_ref_name: head_ref.to_string(),
                    first,
                },
                2,
            )
            .await?
            .repository
            .ok_or_else(|| not_found(format!("repository {owner}/{name}")))?
            .pull_requests;
        Ok((pull_requests.nodes.iter().flatten().flatten())
            .map(MergedPullRequest::from)
            .collect())
    }

    /// Adds to `list` the merged pull requests its stacks link through that it doesn't have yet:
    /// those from each branch `stacks::branches_to_look_up` names, until every branch it names
    /// was looked up, or [`MAX_BRANCH_LOOKUPS`] were.
    pub(super) async fn look_up_merged(
        &self,
        owner: &str,
        name: &str,
        list: &mut OpenPullRequestList,
    ) -> Result<(), GithubError> {
        let mut looked_up = BTreeSet::new();
        loop {
            let prs = list.stack_pull_requests();
            let trunks = list.trunks.among(&prs);
            let trunks: Vec<&str> = trunks.iter().map(String::as_str).collect();
            let branches: Vec<String> = (stacks::branches_to_look_up(&prs, &trunks).into_iter())
                .filter(|branch| !looked_up.contains(branch))
                .collect();
            if branches.is_empty() {
                return Ok(());
            }
            for branch in branches {
                if looked_up.len() >= MAX_BRANCH_LOOKUPS {
                    log::info!(
                        "looked up the merged pull requests of {MAX_BRANCH_LOOKUPS} branches in \
                         {owner}/{name}, and stopped with more left"
                    );
                    list.merged_truncated = true;
                    return Ok(());
                }
                let found =
                    (self.merged_pull_requests(owner, name, &branch, MERGED_PER_BRANCH)).await?;
                looked_up.insert(branch);
                for pr in found {
                    if !list.merged.iter().any(|known| known.number == pr.number) {
                        list.merged.push(pr);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fragment from its definition to the end of the query, where both queries have it.
    fn fragment(query: &str) -> &str {
        let start = query.find("fragment MergedPullRequestFields").unwrap();
        &query[start..]
    }

    #[test]
    fn both_queries_read_merged_pull_requests_alike() {
        assert_eq!(
            fragment(QUERY),
            fragment(super::super::open_pull_requests::QUERY)
        );
    }
}
