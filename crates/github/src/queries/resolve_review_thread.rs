//! `ResolveReviewThread`: resolves a review thread.
//! [`crate::review_writes`] runs it.

use graphql_client::GraphQLQuery;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/schema.graphql",
    query_path = "graphql/resolve_review_thread.graphql",
    response_derives = "Debug, Clone, PartialEq",
    variables_derives = "Debug, Clone",
    skip_serializing_none
)]
pub struct ResolveReviewThread;

// The generated types, named from this module, such as `queries::resolve_review_thread::Variables`.
pub use self::resolve_review_thread::*;
