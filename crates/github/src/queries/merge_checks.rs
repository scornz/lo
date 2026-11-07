//! `MergeChecks`: a pull request's checks on its head after the first page `MergeSnapshot` reads
//! ([`super::merge_snapshot`]).

use graphql_client::GraphQLQuery;

use super::{DateTime, GitObjectID, URI};

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/schema.graphql",
    query_path = "graphql/merge_checks.graphql",
    response_derives = "Debug, Clone, PartialEq, Serialize",
    variables_derives = "Debug, Clone"
)]
pub struct MergeChecks;

// The generated types, named from this module, such as `queries::merge_checks::Variables`.
pub use self::merge_checks::*;
