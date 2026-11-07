//! `StackLayers`: what a stack's tree shows of its layers that the open pull request list doesn't
//! keep current. The open pull request's poller keeps it in the cache and current
//! ([`crate::poll::Poller::stack_layers`]).

use graphql_client::GraphQLQuery;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::pull_request::{FileViewedState, PullRequestState};
use super::pull_request_check::{MergeQueueEntryState, PullRequestReviewDecision, StatusState};
use super::{DateTime, GitObjectID, PAGE_SIZE};
use crate::cache::{CacheEntry, schema_shape};
use crate::client::GithubClient;
use crate::error::GithubError;
use crate::poll::CachedRead;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/schema.graphql",
    query_path = "graphql/stack_layers.graphql",
    response_derives = "Debug, Clone, PartialEq, Serialize",
    variables_derives = "Debug, Clone",
    extern_enums(
        "FileViewedState",
        "MergeQueueEntryState",
        "PullRequestReviewDecision",
        "PullRequestState",
        "StatusState"
    )
)]
pub struct StackLayers;

// The generated types, named from this module, such as `queries::stack_layers::Variables`.
pub use self::stack_layers::*;

/// A layer as `StackLayers` reads it.
pub type StackLayer = StackLayersNodesOnPullRequest;

/// What a stack's tree shows of each of its layers (design §4, "The stack tree") that the open
/// pull request list doesn't keep current: review decision, checks, merge queue entry, and how
/// many files the viewer has viewed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StackLayerList {
    /// The layers' node IDs, as asked for.
    pub ids: Vec<String>,
    /// The layers GitHub found, in the order asked for. A layer that's gone, or that the
    /// account can't see, is left out.
    #[schemars(with = "Vec<serde_json::Value>")]
    pub layers: Vec<StackLayer>,
}

/// How many of a layer's files the viewer has viewed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewedFiles {
    pub viewed: usize,
    /// How many files the layer changes.
    pub files: usize,
    /// Whether every file's mark was read. Only the first 100 are, so for a layer with more,
    /// `viewed` counts among those.
    pub complete: bool,
}

impl StackLayerList {
    /// The layer whose node ID is `id`.
    pub fn layer(&self, id: &str) -> Option<&StackLayer> {
        self.layers.iter().find(|layer| layer.id == id)
    }
}

impl StackLayer {
    /// How many of its files the viewer has viewed.
    pub fn viewed_files(&self) -> ViewedFiles {
        let marks: Vec<&FileViewedState> = (self.files.iter())
            .flat_map(|files| files.nodes.iter().flatten().flatten())
            .map(|file| &file.viewer_viewed_state)
            .collect();
        let files = (self.files.as_ref()).map_or(0, |files| files.total_count);
        let files = usize::try_from(files).unwrap_or_default();
        ViewedFiles {
            viewed: (marks.iter())
                .filter(|&&mark| *mark == FileViewedState::VIEWED)
                .count(),
            files,
            complete: marks.len() >= files,
        }
    }
}

impl CacheEntry for StackLayerList {
    const NAME: &'static str = OPERATION_NAME;

    fn shape() -> String {
        format!(
            "{}\n{}\n{PAGE_SIZE} layers a request",
            schema_shape::<Self>(),
            QUERY
        )
    }
}

impl CachedRead for StackLayerList {
    /// What the open pull request's own layer shows: its state, head, review decision, checks
    /// and queue entry. The other layers' are read again on the 5-minute refresh, and when the
    /// layers asked for change.
    const WATCHED: &'static [&'static str] = &[
        "/state",
        "/headRefOid",
        "/reviewDecision",
        "/statusCheckRollup",
        "/mergeQueueEntry",
    ];
}

impl GithubClient {
    /// The layers whose node IDs are `ids`, [`PAGE_SIZE`] a request.
    pub async fn stack_layers(&self, ids: &[String]) -> Result<StackLayerList, GithubError> {
        let mut layers = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(PAGE_SIZE as usize) {
            // GitHub answers an ID it can't find with null, and an error about that node.
            let nodes = self
                .graphql_keeping_partial::<StackLayers>(
                    Variables {
                        ids: chunk.to_vec(),
                    },
                    1,
                )
                .await?
                .nodes;
            layers.extend(nodes.into_iter().flatten().filter_map(|node| match node {
                StackLayersNodes::PullRequest(layer) => Some(layer),
                _ => None,
            }));
        }
        Ok(StackLayerList {
            ids: ids.to_vec(),
            layers,
        })
    }
}

/// [`GithubClient::stack_layers`] as [`crate::poll::Poller::read_cached`] runs it.
pub async fn read_stack_layers(
    client: GithubClient,
    ids: Vec<String>,
) -> Result<StackLayerList, GithubError> {
    client.stack_layers(&ids).await
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::cassette::{fixtures_dir, read_cassette};

    fn layer(files: usize, listed: &[FileViewedState]) -> StackLayer {
        let nodes: Vec<_> = (listed.iter())
            .map(|mark| serde_json::json!({ "viewerViewedState": mark }))
            .collect();
        serde_json::from_value(serde_json::json!({
            "id": "PR_x", "number": 1, "state": "OPEN", "headRefOid": "a".repeat(40),
            "reviewDecision": "APPROVED", "statusCheckRollup": {"state": "FAILURE"},
            "isInMergeQueue": true, "mergeQueueEntry": {"position": 2, "state": "AWAITING_CHECKS"},
            "files": {"totalCount": files, "nodes": nodes},
        }))
        .unwrap()
    }

    #[test]
    fn viewed_files_count_the_marks_read() {
        use FileViewedState::{DISMISSED, UNVIEWED, VIEWED};
        let read = layer(3, &[VIEWED, DISMISSED, VIEWED]);
        assert_eq!(
            read.viewed_files(),
            ViewedFiles {
                viewed: 2,
                files: 3,
                complete: true
            }
        );
        // Past the first 100, marks aren't read.
        let large = layer(150, &vec![UNVIEWED; 100]);
        assert_eq!(
            large.viewed_files(),
            ViewedFiles {
                viewed: 0,
                files: 150,
                complete: false
            }
        );
        assert_eq!(
            read.review_decision,
            Some(PullRequestReviewDecision::APPROVED)
        );
        let entry = read.merge_queue_entry.unwrap();
        assert_eq!(
            (entry.position, entry.state),
            (2, MergeQueueEntryState::AWAITING_CHECKS)
        );
    }

    #[test]
    fn it_watches_fields_the_open_prs_check_has() {
        let (_, interactions) = read_cassette(&fixtures_dir().join("poll_playground")).unwrap();
        let (_, check) = (interactions.iter())
            .find(|(_, i)| i.request.operation.as_deref() == Some("PullRequestCheck"))
            .unwrap();
        let check = &check.response.body.as_ref().unwrap()["data"]["repository"]["pullRequest"];
        for pointer in StackLayerList::WATCHED {
            assert!(check.pointer(pointer).is_some(), "{pointer}");
        }
    }
}
