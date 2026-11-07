//! `AddComment`: comments on a pull request itself.
//! [`crate::review_writes`] runs it.

use graphql_client::GraphQLQuery;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/schema.graphql",
    query_path = "graphql/add_comment.graphql",
    response_derives = "Debug, Clone, PartialEq",
    variables_derives = "Debug, Clone",
    skip_serializing_none
)]
pub struct AddComment;

// The generated types, named from this module, such as `queries::add_comment::Variables`.
pub use self::add_comment::*;
