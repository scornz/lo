//! `AddPullRequestReviewThread`: adds a thread to your pending review, on lines or on a file.
//! [`crate::review_writes`] runs it.

use graphql_client::GraphQLQuery;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/schema.graphql",
    query_path = "graphql/add_pull_request_review_thread.graphql",
    response_derives = "Debug, Clone, PartialEq",
    variables_derives = "Debug, Clone",
    skip_serializing_none
)]
pub struct AddPullRequestReviewThread;

// The generated types, named from this module, such as
// `queries::add_pull_request_review_thread::Variables`.
pub use self::add_pull_request_review_thread::*;
