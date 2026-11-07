//! `PendingReview`: the signed-in user's pending review, mapped into
//! `review::pending::PendingReview`. The open pull request's poller keeps it in the cache and
//! current ([`crate::poll::Poller::pending_review`]).

use std::sync::Arc;

use graphql_client::GraphQLQuery;
use review::pending::PendingReview as Pending;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{DateTime, GitObjectID, not_found};
use crate::cache::{CacheEntry, schema_shape};
use crate::client::GithubClient;
use crate::error::GithubError;
use crate::poll::CachedRead;
use crate::pulls::PullRef;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/schema.graphql",
    query_path = "graphql/pending_review.graphql",
    response_derives = "Debug, Clone, PartialEq, Serialize",
    variables_derives = "Debug, Clone"
)]
pub struct PendingReview;

// The generated types, named from this module, such as `queries::pending_review::Variables`.
pub use self::pending_review::*;

/// Your pending review on a pull request, as `PendingReview` reads it (design §8, "Reviews").
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ViewerPendingReview {
    /// Your pending review, or `None` when you have none.
    pub review: Option<PendingReviewNode>,
}

/// A pending review's fields, named after `PullRequestReview`'s.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PendingReviewNode {
    /// `id`.
    pub id: String,
    /// `commit.oid`: the revision it's recorded on. `None` once GitHub no longer has the commit.
    pub commit: Option<String>,
    /// `comments.totalCount`.
    pub comments: usize,
}

impl ViewerPendingReview {
    /// Your pending review as `review::pending` takes it. One on a commit GitHub no longer has
    /// is recorded on no revision Lo shows, so its commit is empty, which names none: it submits
    /// only as a comment.
    pub fn pending_review(&self) -> Option<Pending> {
        let review = self.review.as_ref()?;
        Some(Pending {
            id: review.id.clone(),
            commit: review.commit.clone().unwrap_or_default(),
            comments: review.comments,
        })
    }
}

impl CacheEntry for ViewerPendingReview {
    const NAME: &'static str = OPERATION_NAME;

    fn shape() -> String {
        format!("{}\n{}", schema_shape::<Self>(), QUERY)
    }
}

impl CachedRead for ViewerPendingReview {
    /// Only you change your pending review, and the check holds its comment count, or nothing
    /// when you have none, so starting, adding to, submitting or deleting it from anywhere moves
    /// it.
    const WATCHED: &'static [&'static str] = &["/viewerPendingReview"];
}

impl GithubClient {
    /// The pending review the user with login `login` has on `pull`, if any. `login` is the
    /// signed-in user's: nobody else's pending review can be read.
    pub async fn pending_review(
        &self,
        pull: &PullRef,
        login: &str,
    ) -> Result<ViewerPendingReview, GithubError> {
        let reviews = self
            .graphql_keeping_partial::<PendingReview>(
                Variables {
                    owner: pull.repo.owner.clone(),
                    name: pull.repo.name.clone(),
                    number: pull.number,
                    login: login.to_string(),
                },
                2,
            )
            .await?
            .repository
            .and_then(|repository| repository.pull_request)
            .ok_or_else(|| not_found(format!("pull request {pull}")))?
            .reviews;
        let node = (reviews.and_then(|reviews| reviews.nodes))
            .into_iter()
            .flatten()
            .flatten()
            .next();
        Ok(ViewerPendingReview {
            review: node.map(|node| PendingReviewNode {
                id: node.id,
                commit: node.commit.map(|commit| commit.oid),
                comments: usize::try_from(node.comments.total_count).unwrap_or_default(),
            }),
        })
    }
}

/// [`GithubClient::pending_review`] as [`crate::poll::Poller::read_cached`] runs it.
pub async fn read_pending_review(
    client: GithubClient,
    pull: PullRef,
    login: String,
    _known: Option<Arc<ViewerPendingReview>>,
) -> Result<ViewerPendingReview, GithubError> {
    client.pending_review(&pull, &login).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pending_review_maps_into_reviews() {
        let read = ViewerPendingReview {
            review: Some(PendingReviewNode {
                id: "PRR_kwDOAbc".into(),
                commit: Some("c7f30e9b1a5d7c3e9f1b5d7a3c9e1f5b7d3a9c62".into()),
                comments: 2,
            }),
        };
        assert_eq!(
            read.pending_review(),
            Some(Pending {
                id: "PRR_kwDOAbc".into(),
                commit: "c7f30e9b1a5d7c3e9f1b5d7a3c9e1f5b7d3a9c62".into(),
                comments: 2,
            })
        );
        assert_eq!(ViewerPendingReview::default().pending_review(), None);
    }

    #[test]
    fn a_pending_review_on_a_lost_commit_is_on_no_revision() {
        let read = ViewerPendingReview {
            review: Some(PendingReviewNode {
                id: "PRR_kwDOAbc".into(),
                commit: None,
                comments: 1,
            }),
        };
        let pending = read.pending_review().unwrap();
        let review = review::pending::YourReview::new(Some(pending));
        let head = "c7f30e9b1a5d7c3e9f1b5d7a3c9e1f5b7d3a9c62";
        assert!(!review.submit_dialog("alice", false, head).on_screen);
    }
}
