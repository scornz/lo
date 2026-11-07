//! `AddPullRequestReview`: starts your pending review, or posts a review at once.
//! [`crate::review_writes`] runs it.

use graphql_client::GraphQLQuery;

use super::GitObjectID;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/schema.graphql",
    query_path = "graphql/add_pull_request_review.graphql",
    response_derives = "Debug, Clone, PartialEq",
    variables_derives = "Debug, Clone",
    skip_serializing_none
)]
pub struct AddPullRequestReview;

// The generated types, named from this module, such as
// `queries::add_pull_request_review::Variables`.
pub use self::add_pull_request_review::*;
