//! `DeletePullRequestReview`: deletes your pending review. [`crate::review_writes`] runs it.

use graphql_client::GraphQLQuery;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/schema.graphql",
    query_path = "graphql/delete_pull_request_review.graphql",
    response_derives = "Debug, Clone, PartialEq",
    variables_derives = "Debug, Clone",
    skip_serializing_none
)]
pub struct DeletePullRequestReview;

// The generated types, named from this module, such as
// `queries::delete_pull_request_review::Variables`.
pub use self::delete_pull_request_review::*;
