//! `PullRequestCheck`: the cheap check for whether a pull request changed.

use graphql_client::GraphQLQuery;

use super::{DateTime, GitObjectID, not_found};
use crate::client::GithubClient;
use crate::error::GithubError;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/schema.graphql",
    query_path = "graphql/pull_request_check.graphql",
    response_derives = "Debug, Clone, PartialEq, Serialize",
    variables_derives = "Debug, Clone"
)]
pub struct PullRequestCheck;

// The generated types, named from this module, such as `queries::pull_request_check::Variables`.
pub use self::pull_request_check::*;

pub type PullRequestCheckData = PullRequestCheckRepositoryPullRequest;

/// How many of a pull request's review threads the check reads the state and comment count of:
/// its `reviewThreads(first: 100)`. Past them, only the count of threads moves.
pub const CHECKED_THREADS: usize = 100;

impl PullRequestCheckData {
    /// How many of the first 100 review threads aren't resolved.
    pub fn unresolved_threads(&self) -> usize {
        self.review_threads
            .nodes
            .iter()
            .flatten()
            .flatten()
            .filter(|thread| !thread.is_resolved)
            .count()
    }
}

impl GithubClient {
    /// The cheap check for whether a pull request changed.
    pub async fn pull_request_check(
        &self,
        owner: &str,
        name: &str,
        number: i64,
    ) -> Result<PullRequestCheckData, GithubError> {
        self.graphql_keeping_partial::<PullRequestCheck>(
            Variables {
                owner: owner.to_string(),
                name: name.to_string(),
                number,
            },
            2,
        )
        .await?
        .repository
        .and_then(|repository| repository.pull_request)
        .ok_or_else(|| not_found(format!("pull request {owner}/{name}#{number}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_check_reads_as_many_threads_as_it_says() {
        assert!(QUERY.contains(&format!("reviewThreads(first: {CHECKED_THREADS})")));
    }
}
