//! `UnmarkFileAsViewed`: clears the signed-in user's viewed mark on one of a pull request's files.

use graphql_client::GraphQLQuery;

use super::marked;
use crate::client::GithubClient;
use crate::error::GithubError;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/schema.graphql",
    query_path = "graphql/unmark_file_as_viewed.graphql",
    response_derives = "Debug, Clone, PartialEq",
    variables_derives = "Debug, Clone"
)]
pub struct UnmarkFileAsViewed;

// The generated types, named from this module, such as
// `queries::unmark_file_as_viewed::Variables`.
pub use self::unmark_file_as_viewed::*;

impl GithubClient {
    /// Clears the signed-in user's viewed mark on one of a pull request's files
    /// (`unmarkFileAsViewed`), as [`Self::mark_file_as_viewed`] sets it.
    pub async fn unmark_file_as_viewed(
        &self,
        pull_request_id: &str,
        path: &str,
    ) -> Result<(), GithubError> {
        let data = self
            .graphql::<UnmarkFileAsViewed>(Variables {
                pull_request_id: pull_request_id.to_string(),
                path: path.to_string(),
            })
            .await?;
        marked(
            data.unmark_file_as_viewed
                .and_then(|payload| payload.pull_request),
        )
    }
}
