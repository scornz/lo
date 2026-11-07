//! `MergedPullRequests`: branches' merged pull requests, which link the open layers of a stack
//! whose lower layers merged, ten branches a request.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::time::SystemTime;

use graphql_client::GraphQLQuery;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::open_pull_requests::OpenPullRequestList;
use super::{DateTime, GitObjectID, not_found};
use crate::client::GithubClient;
use crate::error::GithubError;
use crate::pulls::RepoRef;
use crate::push_history::stops_the_read;

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
pub const MERGED_PER_BRANCH: i64 = 5;
/// How many branches one `MergedPullRequests` request looks up: its aliases.
pub const BRANCHES_A_REQUEST: usize = 10;
/// The most branches whose merged pull requests are looked up for one read, two requests' worth.
/// Branches whose lookup is carried over from the last read don't count.
pub const MAX_BRANCH_LOOKUPS: usize = 20;

/// A branch whose merged pull requests were looked up, and what was found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BranchLookup {
    pub branch: String,
    /// The branch's tip when it was looked up, from a pull request it's the base of, or `None`
    /// when none said, as once it's deleted. A lookup is carried over to the next read while the
    /// tip is the same.
    pub tip: Option<String>,
    /// The numbers of the merged pull requests found.
    pub found: Vec<u64>,
}

/// What an earlier read looked up, to carry over.
#[derive(Clone, Copy, Default)]
pub(crate) struct Known<'a> {
    pub lookups: &'a [BranchLookup],
    /// The merged pull requests they found, among others.
    pub merged: &'a [MergedPullRequest],
}

impl Known<'_> {
    /// What was found looking up `branch` at `tip`.
    fn lookup(&self, branch: &str, tip: &Option<String>) -> Option<&BranchLookup> {
        (self.lookups.iter()).find(|lookup| lookup.branch == branch && lookup.tip == *tip)
    }
}

/// Adds `pr` to `merged` unless it's there already.
fn add_merged(merged: &mut Vec<MergedPullRequest>, pr: MergedPullRequest) {
    if !merged.iter().any(|known| known.number == pr.number) {
        merged.push(pr);
    }
}

/// A merged pull request, with what finding stacks reads of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MergedPullRequest {
    pub number: u64,
    #[schemars(with = "String")]
    pub created_at: DateTime,
    /// The branch it merged into, and that branch's tip now, if it's still there.
    pub base_ref: String,
    pub base_tip: Option<String>,
    /// The pattern of the branch protection rule that covers the branch it merged into.
    pub base_protection: Option<String>,
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
                    base_protection: (pr.base_ref.as_ref())
                        .and_then(|base| Some(base.ref_update_rule.as_ref()?.pattern.clone())),
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
    /// The merged pull requests from the branches `branches`, [`MERGED_PER_BRANCH`] a branch,
    /// newest first, [`BRANCHES_A_REQUEST`] branches a request. Pull requests from forks'
    /// branches of those names are among them.
    pub async fn merged_pull_requests(
        &self,
        repo: &RepoRef,
        branches: &[String],
    ) -> Result<Vec<MergedPullRequest>, GithubError> {
        let mut merged: Vec<MergedPullRequest> = Vec::new();
        for batch in branches.chunks(BRANCHES_A_REQUEST) {
            // A batch of fewer branches asks about the first again.
            let branch = |index: usize| batch.get(index).unwrap_or(&batch[0]).clone();
            let variables = Variables {
                owner: repo.owner.clone(),
                name: repo.name.clone(),
                b0: branch(0),
                b1: branch(1),
                b2: branch(2),
                b3: branch(3),
                b4: branch(4),
                b5: branch(5),
                b6: branch(6),
                b7: branch(7),
                b8: branch(8),
                b9: branch(9),
                first: MERGED_PER_BRANCH,
            };
            let found = self
                .graphql_keeping_partial::<MergedPullRequests>(variables, 2)
                .await?
                .repository
                .ok_or_else(|| not_found(format!("repository {repo}")))?;
            let nodes = [
                found.b0.nodes,
                found.b1.nodes,
                found.b2.nodes,
                found.b3.nodes,
                found.b4.nodes,
                found.b5.nodes,
                found.b6.nodes,
                found.b7.nodes,
                found.b8.nodes,
                found.b9.nodes,
            ];
            for pr in nodes.iter().flatten().flatten().flatten() {
                add_merged(&mut merged, MergedPullRequest::from(pr));
            }
        }
        Ok(merged)
    }

    /// Looks up the merged pull requests from `branches`, each with its tip, adding them to
    /// `merged` and a record of each lookup to `lookups`. A branch `known` looked up at the same
    /// tip is carried over without a request. Of the rest, at most `room` are looked up, in the
    /// order given. Returns how many were, or an error that stops the read; any other error
    /// leaves them unread, and gives `Ok(None)`.
    pub(crate) async fn look_up_branches(
        &self,
        repo: &RepoRef,
        branches: Vec<(String, Option<String>)>,
        known: Known<'_>,
        room: usize,
        merged: &mut Vec<MergedPullRequest>,
        lookups: &mut Vec<BranchLookup>,
    ) -> Result<Option<usize>, GithubError> {
        let mut unknown = Vec::new();
        for (branch, tip) in branches {
            match known.lookup(&branch, &tip) {
                Some(lookup) => {
                    for pr in (known.merged.iter()).filter(|pr| lookup.found.contains(&pr.number)) {
                        add_merged(merged, pr.clone());
                    }
                    lookups.push(lookup.clone());
                }
                None => unknown.push((branch, tip)),
            }
        }
        unknown.truncate(room);
        if unknown.is_empty() {
            return Ok(Some(0));
        }
        let names: Vec<String> = unknown.iter().map(|(branch, _)| branch.clone()).collect();
        let found = match self.merged_pull_requests(repo, &names).await {
            Ok(found) => found,
            Err(error) if stops_the_read(&error) => return Err(error),
            Err(error) => {
                log::warn!("looking up merged pull requests in {repo}: {error:#}");
                return Ok(None);
            }
        };
        for (branch, tip) in unknown {
            let from_branch = found.iter().filter(|pr| pr.head_ref == branch);
            lookups.push(BranchLookup {
                found: from_branch.map(|pr| pr.number).collect(),
                branch,
                tip,
            });
        }
        for pr in found {
            add_merged(merged, pr);
        }
        Ok(Some(names.len()))
    }

    /// Adds to `list` the merged pull requests its stacks link through that it doesn't have yet:
    /// those from each branch `stacks::branches_to_look_up` names, until every branch it names
    /// was looked up, or [`MAX_BRANCH_LOOKUPS`] were. What `known`, the list read before, found
    /// for a branch at the same tip is carried over. The branches that most open pull requests
    /// target are looked up first. When the lookups stop short, past the most or after an error
    /// that doesn't stop the read, the list says so ([`OpenPullRequestList::merged_truncated`]).
    pub async fn look_up_merged(
        &self,
        repo: &RepoRef,
        list: &mut OpenPullRequestList,
        known: Option<&OpenPullRequestList>,
    ) -> Result<(), GithubError> {
        let known = known.map_or_else(Known::default, |known| Known {
            lookups: &known.lookups,
            merged: &known.merged,
        });
        let mut looked_up = BTreeSet::new();
        let mut queried = 0;
        loop {
            let prs = list.stack_pull_requests(&[]);
            let trunks = list.trunks(None).among(&prs);
            let trunks: Vec<&str> = trunks.iter().map(String::as_str).collect();
            let mut branches: Vec<String> = (stacks::branches_to_look_up(&prs, &trunks)
                .into_iter())
            .filter(|branch| !looked_up.contains(branch))
            .collect();
            if branches.is_empty() {
                return Ok(());
            }
            // The branches most open pull requests target first: the likeliest to link layers.
            let mut targeted: BTreeMap<&str, usize> = BTreeMap::new();
            for pr in &list.pull_requests {
                *targeted.entry(pr.base_ref_name.as_str()).or_default() += 1;
            }
            branches.sort_by_key(|branch| Reverse(targeted.get(branch.as_str()).copied()));
            let branches: Vec<(String, Option<String>)> = (branches.into_iter())
                .map(|branch| {
                    let tip = list.branch_tip(&branch);
                    (branch, tip)
                })
                .collect();
            looked_up.extend(branches.iter().map(|(branch, _)| branch.clone()));
            let wanted = (branches.iter())
                .filter(|(branch, tip)| known.lookup(branch, tip).is_none())
                .count();
            let room = MAX_BRANCH_LOOKUPS - queried;
            let looked = self
                .look_up_branches(
                    repo,
                    branches,
                    known,
                    room,
                    &mut list.merged,
                    &mut list.lookups,
                )
                .await;
            match looked? {
                Some(count) if wanted <= room => queried += count,
                stopped => {
                    if stopped.is_some() {
                        log::info!(
                            "looked up the merged pull requests of {MAX_BRANCH_LOOKUPS} branches \
                             in {repo}, and stopped with more left"
                        );
                    }
                    list.merged_truncated = true;
                    return Ok(());
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
