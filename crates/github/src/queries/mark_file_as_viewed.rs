//! `MarkFileAsViewed`: marks one of a pull request's files viewed for the signed-in user.

use graphql_client::GraphQLQuery;

use super::marked;
use crate::client::GithubClient;
use crate::error::GithubError;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/schema.graphql",
    query_path = "graphql/mark_file_as_viewed.graphql",
    response_derives = "Debug, Clone, PartialEq",
    variables_derives = "Debug, Clone"
)]
pub struct MarkFileAsViewed;

// The generated types, named from this module, such as `queries::mark_file_as_viewed::Variables`.
pub use self::mark_file_as_viewed::*;

impl GithubClient {
    /// Marks one of a pull request's files viewed for the signed-in user (`markFileAsViewed`).
    /// `pull_request_id` is the pull request's node ID, its details' `id`. GitHub's mark is per
    /// user, and it resets to "changed since viewed" when a push changes the file.
    pub async fn mark_file_as_viewed(
        &self,
        pull_request_id: &str,
        path: &str,
    ) -> Result<(), GithubError> {
        let data = self
            .graphql::<MarkFileAsViewed>(Variables {
                pull_request_id: pull_request_id.to_string(),
                path: path.to_string(),
            })
            .await?;
        marked(
            data.mark_file_as_viewed
                .and_then(|payload| payload.pull_request),
        )
    }
}
