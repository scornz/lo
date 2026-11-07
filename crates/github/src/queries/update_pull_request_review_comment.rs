//! `UpdatePullRequestReviewComment`: changes a review thread comment's text.
//! [`crate::review_writes`] runs it.

use graphql_client::GraphQLQuery;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/schema.graphql",
    query_path = "graphql/update_pull_request_review_comment.graphql",
    response_derives = "Debug, Clone, PartialEq",
    variables_derives = "Debug, Clone",
    skip_serializing_none
)]
pub struct UpdatePullRequestReviewComment;

// The generated types, named from this module, such as
// `queries::update_pull_request_review_comment::Variables`.
pub use self::update_pull_request_review_comment::*;
