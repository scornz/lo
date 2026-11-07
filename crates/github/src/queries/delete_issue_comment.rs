//! `DeleteIssueComment`: deletes a comment on a pull request itself.
//! [`crate::review_writes`] runs it.

use graphql_client::GraphQLQuery;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/schema.graphql",
    query_path = "graphql/delete_issue_comment.graphql",
    response_derives = "Debug, Clone, PartialEq",
    variables_derives = "Debug, Clone",
    skip_serializing_none
)]
pub struct DeleteIssueComment;

// The generated types, named from this module, such as `queries::delete_issue_comment::Variables`.
pub use self::delete_issue_comment::*;
