//! `ReviewThreadComments`: a review thread's comments after the first page `ReviewThreads` reads
//! ([`super::review_threads`]).

use graphql_client::GraphQLQuery;

use super::{DateTime, GitObjectID, URI};

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/schema.graphql",
    query_path = "graphql/review_thread_comments.graphql",
    response_derives = "Debug, Clone, PartialEq, Serialize",
    variables_derives = "Debug, Clone"
)]
pub struct ReviewThreadComments;

// The generated types, named from this module, such as
// `queries::review_thread_comments::Variables`.
pub use self::review_thread_comments::*;
