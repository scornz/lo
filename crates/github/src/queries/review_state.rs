//! `ReviewState`: whether a review is still pending. [`crate::review_writes`] asks, to explain a
//! thread or a reply GitHub answered with null.

use graphql_client::GraphQLQuery;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/schema.graphql",
    query_path = "graphql/review_state.graphql",
    response_derives = "Debug, Clone, PartialEq",
    variables_derives = "Debug, Clone"
)]
pub struct ReviewState;

// The generated types, named from this module, such as `queries::review_state::Variables`.
pub use self::review_state::*;
