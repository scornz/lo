//! `AddPullRequestReviewThreadReply`: replies to a review thread, in your pending review or at
//! once. [`crate::review_writes`] runs it.

use graphql_client::GraphQLQuery;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/schema.graphql",
    query_path = "graphql/add_pull_request_review_thread_reply.graphql",
    response_derives = "Debug, Clone, PartialEq",
    variables_derives = "Debug, Clone",
    skip_serializing_none
)]
pub struct AddPullRequestReviewThreadReply;

// The generated types, named from this module, such as
// `queries::add_pull_request_review_thread_reply::Variables`.
pub use self::add_pull_request_review_thread_reply::*;
