//! `SubmitPullRequestReview`: submits your pending review.
//! [`crate::review_writes`] runs it.

use graphql_client::GraphQLQuery;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/schema.graphql",
    query_path = "graphql/submit_pull_request_review.graphql",
    response_derives = "Debug, Clone, PartialEq",
    variables_derives = "Debug, Clone",
    skip_serializing_none
)]
pub struct SubmitPullRequestReview;

// The generated types, named from this module, such as
// `queries::submit_pull_request_review::Variables`.
pub use self::submit_pull_request_review::*;
