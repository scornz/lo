//! `Viewer`: the signed-in user.

use graphql_client::GraphQLQuery;

use super::URI;
use crate::client::GithubClient;
use crate::error::GithubError;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/schema.graphql",
    query_path = "graphql/viewer.graphql",
    response_derives = "Debug, Clone, PartialEq",
    variables_derives = "Debug, Clone"
)]
pub struct Viewer;

// The generated types, named from this module, such as `queries::viewer::Variables`.
pub use self::viewer::*;

pub type ViewerData = ViewerViewer;

impl GithubClient {
    /// The signed-in user.
    pub async fn viewer(&self) -> Result<ViewerData, GithubError> {
        Ok(self
            .graphql_keeping_partial::<Viewer>(Variables, 1)
            .await?
            .viewer)
    }
}
