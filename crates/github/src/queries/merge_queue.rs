//! `MergeQueue`: a base branch's merge queue, its settings and its entries in order (design §9,
//! "The merge queue"). The queue view reads it while it's open.

use graphql_client::GraphQLQuery;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{DateTime, GitObjectID, URI, not_found};
use crate::cache::{CacheEntry, schema_shape};
use crate::client::GithubClient;
use crate::error::GithubError;
use crate::pulls::RepoRef;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/schema.graphql",
    query_path = "graphql/merge_queue.graphql",
    response_derives = "Debug, Clone, PartialEq, Serialize",
    variables_derives = "Debug, Clone"
)]
pub struct MergeQueue;

// The generated types, named from this module, such as `queries::merge_queue::Variables`.
pub use self::merge_queue::*;

/// A queue as `MergeQueue` reads it.
pub type MergeQueueData = MergeQueueRepositoryMergeQueue;
/// An entry as `MergeQueue` reads it.
pub type MergeQueueEntry = MergeQueueRepositoryMergeQueueEntriesNodes;

/// A base branch's merge queue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MergeQueueList {
    /// The branch.
    pub branch: String,
    /// Its queue, or `None` when it has none. Its `configuration` is `None` for anyone who
    /// can't write to the repository: the branch's `merge_queue` rule says the same
    /// ([`super::BranchRule::MergeQueue`]).
    #[schemars(with = "Option<serde_json::Value>")]
    pub queue: Option<MergeQueueData>,
}

impl MergeQueueList {
    /// The entries, in the queue's order: the first merges next. A queue longer than the 100
    /// read has the rest left out ([`Self::cut_short`]).
    pub fn entries(&self) -> Vec<&MergeQueueEntry> {
        let mut entries: Vec<&MergeQueueEntry> = (self.queue.iter())
            .flat_map(|queue| queue.entries.iter())
            .flat_map(|entries| entries.nodes.iter().flatten().flatten())
            .collect();
        entries.sort_by_key(|entry| entry.position);
        entries
    }

    /// Whether the queue holds more entries than were read, or GitHub left some out.
    pub fn cut_short(&self) -> bool {
        let Some(entries) = self.queue.as_ref().and_then(|queue| queue.entries.as_ref()) else {
            return false;
        };
        let read = entries.nodes.iter().flatten().flatten().count();
        usize::try_from(entries.total_count).unwrap_or(0) > read
    }
}

impl CacheEntry for MergeQueueList {
    const NAME: &'static str = OPERATION_NAME;

    fn shape() -> String {
        format!("{}\n{}", schema_shape::<Self>(), QUERY)
    }
}

impl GithubClient {
    /// `repo`'s merge queue for `branch`, with its first 100 entries.
    pub async fn merge_queue(
        &self,
        repo: &RepoRef,
        branch: &str,
    ) -> Result<MergeQueueList, GithubError> {
        let repository = self
            .graphql_keeping_partial::<MergeQueue>(
                Variables {
                    owner: repo.owner.clone(),
                    name: repo.name.clone(),
                    branch: branch.to_string(),
                },
                2,
            )
            .await?
            .repository
            .ok_or_else(|| not_found(format!("repository {repo}")))?;
        Ok(MergeQueueList {
            branch: branch.to_string(),
            queue: repository.merge_queue,
        })
    }
}
