//! `StackLayers`: what a stack's tree shows of its layers that the open pull request list doesn't
//! keep current. The open pull request's poller keeps it in the cache and current
//! ([`crate::poll::Poller::stack_layers`]).

use std::sync::Arc;

use graphql_client::GraphQLQuery;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::merged_pull_requests::{BranchLookup, Known, MAX_BRANCH_LOOKUPS, MergedPullRequest};
use super::pull_request::{FileViewedState, PullRequestState};
use super::pull_request_check::{MergeQueueEntryState, PullRequestReviewDecision, StatusState};
use super::{DateTime, GitObjectID, PAGE_SIZE};
use crate::cache::{CacheEntry, schema_shape};
use crate::client::GithubClient;
use crate::error::GithubError;
use crate::poll::CachedRead;
use crate::pulls::RepoRef;

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
/// many files the viewer has viewed. With them, the merged pull requests below the layers GitHub
/// moved to another branch after their base branch was deleted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StackLayerList {
    /// The layers' node IDs, as asked for.
    pub ids: Vec<String>,
    /// The layers GitHub found, in the order asked for. A layer that's gone, or that the
    /// account can't see, is left out.
    #[schemars(with = "Vec<serde_json::Value>")]
    pub layers: Vec<StackLayer>,
    /// The merged pull requests from the branches the layers were moved from
    /// ([`StackLayer::moved_from`]), for [`super::OpenPullRequestList::stacks`]: once a squashed
    /// layer's branch is deleted, only they can mark the layers it held as needing a rebase.
    pub merged: Vec<MergedPullRequest>,
    /// The branches whose merged pull requests were looked up, which the next read carries over.
    pub lookups: Vec<BranchLookup>,
    /// Why some branches weren't looked up, past [`MAX_BRANCH_LOOKUPS`] or after a lookup
    /// failed. The read is tried again after a wait.
    pub lookups_failed: Option<String>,
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
    /// The branch GitHub last moved it from, once that branch was deleted: the head branch of the
    /// layer it sat on, if that one merged.
    pub fn moved_from(&self) -> Option<&str> {
        let events = self.moved_from.nodes.iter().flatten().flatten();
        events.last().and_then(|event| match event {
            StackLayersNodesOnPullRequestMovedFromNodes::AutomaticBaseChangeSucceededEvent(
                event,
            ) => Some(event.old_base.as_str()),
            _ => None,
        })
    }

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
    /// and queue entry, and the events that move it to another branch. The other layers' are
    /// read again on the 5-minute refresh, when the layers asked for change, and when Lo writes
    /// to one of them, as when you mark a file viewed ([`crate::poll::Poller::write`]).
    const WATCHED: &'static [&'static str] = &[
        "/state",
        "/headRefOid",
        "/reviewDecision",
        "/statusCheckRollup",
        "/mergeQueueEntry",
        "/revisionEvents/filteredCount",
    ];

    fn is_complete(&self) -> bool {
        self.lookups_failed.is_none()
    }
}

impl GithubClient {
    /// The layers whose node IDs are `ids`, [`PAGE_SIZE`] a request, in `repo`, with the merged
    /// pull requests from the branches they were moved from. What `known`, read before, looked
    /// up is carried over.
    pub async fn stack_layers(
        &self,
        repo: &RepoRef,
        ids: &[String],
        known: Option<&StackLayerList>,
    ) -> Result<StackLayerList, GithubError> {
        let mut list = StackLayerList {
            ids: ids.to_vec(),
            layers: Vec::with_capacity(ids.len()),
            merged: Vec::new(),
            lookups: Vec::new(),
            lookups_failed: None,
        };
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
            list.layers
                .extend(nodes.into_iter().flatten().filter_map(|node| match node {
                    StackLayersNodes::PullRequest(layer) => Some(layer),
                    _ => None,
                }));
        }
        let mut moved_from: Vec<String> = (list.layers.iter())
            .filter_map(|layer| layer.moved_from().map(String::from))
            .collect();
        moved_from.sort_unstable();
        moved_from.dedup();
        let wanted = moved_from.len();
        // A deleted branch's tip is unknown, and its merged pull requests don't change.
        let branches = moved_from
            .into_iter()
            .map(|branch| (branch, None))
            .collect();
        let known = known.map_or_else(Known::default, |known| Known {
            lookups: &known.lookups,
            merged: &known.merged,
        });
        let looked = self
            .look_up_branches(
                repo,
                branches,
                known,
                MAX_BRANCH_LOOKUPS,
                &mut list.merged,
                &mut list.lookups,
            )
            .await?;
        list.lookups_failed = if looked.is_none() {
            Some("a lookup of merged pull requests failed".into())
        } else if list.lookups.len() < wanted {
            Some(format!(
                "more than {MAX_BRANCH_LOOKUPS} branches to look up"
            ))
        } else {
            None
        };
        Ok(list)
    }
}

/// [`GithubClient::stack_layers`] as [`crate::poll::Poller::read_cached`] runs it.
pub async fn read_stack_layers(
    client: GithubClient,
    repo: RepoRef,
    ids: Vec<String>,
    known: Option<Arc<StackLayerList>>,
) -> Result<StackLayerList, GithubError> {
    client.stack_layers(&repo, &ids, known.as_deref()).await
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
            "movedFrom": {"nodes": [
                {"__typename": "AutomaticBaseChangeSucceededEvent", "oldBase": "first"},
                {"__typename": "AutomaticBaseChangeSucceededEvent", "oldBase": "last"},
            ]},
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
        assert_eq!(read.moved_from(), Some("last"));
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
