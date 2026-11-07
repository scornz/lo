//! `ReviewThreads`: a pull request's review threads, each with every comment, mapped into
//! `review::threads::Thread`. A thread with more comments than a page holds reads the rest with
//! `ReviewThreadComments`. The open pull request's poller keeps them in the cache and current
//! ([`crate::poll::Poller::review_threads`]).

use std::sync::Arc;
use std::time::SystemTime;

use graphql_client::GraphQLQuery;
use review::threads::{self, CommentState, Subject, Thread, ThreadComment};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::review_thread_comments::{self, ReviewThreadComments};
use super::{DateTime, GitObjectID, PAGE_SIZE, Pager, URI, not_found};
use crate::cache::{CacheEntry, schema_shape};
use crate::client::GithubClient;
use crate::error::GithubError;
use crate::poll::CachedRead;
use crate::pulls::PullRef;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/schema.graphql",
    query_path = "graphql/review_threads.graphql",
    response_derives = "Debug, Clone, PartialEq, Serialize",
    variables_derives = "Debug, Clone"
)]
pub struct ReviewThreads;

// The generated types, named from this module, such as `queries::review_threads::Variables`.
pub use self::review_threads::*;

/// A thread as `ReviewThreads` reads it. In a [`ReviewThreadList`], it holds every comment.
pub type ReviewThread = ReviewThreadsRepositoryPullRequestReviewThreadsNodes;

/// A pull request's review threads (design §8, "Comments", "Threads panel"), each with every
/// comment, in GitHub's order. Your pending review's threads and replies are among them; nobody
/// else's are.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReviewThreadList {
    /// The threads as GitHub gives them. [`Self::threads`] maps them for `review`.
    #[schemars(with = "Vec<serde_json::Value>")]
    pub nodes: Vec<ReviewThread>,
}

impl ReviewThreadList {
    /// The threads, as `review::threads` takes them.
    pub fn threads(&self) -> Vec<Thread> {
        self.nodes.iter().map(ReviewThread::thread).collect()
    }
}

impl ReviewThread {
    /// The thread as `review::threads` takes it. Its first comment's commits say which revisions
    /// its lines are on: `commit` for `line` and `startLine`, `originalCommit` for `originalLine`
    /// and `originalStartLine`.
    ///
    /// A value GitHub added after Lo was built reads as the one that shows the thread most
    /// safely: an unknown subject as the whole file, an unknown side as the head's, and an
    /// unknown comment state as posted.
    pub fn thread(&self) -> Thread {
        let comments: Vec<&ReviewComment> =
            self.comments.nodes.iter().flatten().flatten().collect();
        let first = comments.first();
        let line = |line: Option<i64>| line.and_then(|line| u32::try_from(line).ok());
        Thread {
            id: self.id.clone(),
            path: self.path.clone(),
            subject: match &self.subject_type {
                PullRequestReviewThreadSubjectType::LINE => Subject::Line,
                PullRequestReviewThreadSubjectType::FILE => Subject::File,
                PullRequestReviewThreadSubjectType::Other(value) => {
                    log::warn!("a review thread on a subject Lo doesn't know: {value}");
                    Subject::File
                }
            },
            side: side(&self.diff_side),
            start_side: self.start_diff_side.as_ref().map(side),
            line: line(self.line),
            start_line: line(self.start_line),
            original_line: line(self.original_line),
            original_start_line: line(self.original_start_line),
            outdated: self.is_outdated,
            resolved: self.is_resolved,
            current_on: first
                .and_then(|comment| comment.commit.as_ref())
                .map(|commit| commit.oid.clone()),
            written_on: first
                .and_then(|comment| comment.original_commit.as_ref())
                .map(|commit| commit.oid.clone()),
            comments: comments.into_iter().map(ReviewComment::comment).collect(),
            viewer_can_reply: self.viewer_can_reply,
            viewer_can_resolve: self.viewer_can_resolve,
            viewer_can_unresolve: self.viewer_can_unresolve,
        }
    }
}

fn side(side: &DiffSide) -> threads::DiffSide {
    match side {
        DiffSide::LEFT => threads::DiffSide::Left,
        DiffSide::RIGHT => threads::DiffSide::Right,
        DiffSide::Other(value) => {
            log::warn!("a review thread on a side Lo doesn't know: {value}");
            threads::DiffSide::Right
        }
    }
}

impl ReviewComment {
    /// The comment as `review::threads` takes it.
    pub fn comment(&self) -> ThreadComment {
        ThreadComment {
            id: self.id.clone(),
            author: self.author.as_ref().map(|author| author.login.clone()),
            body: self.body.clone(),
            state: match &self.state {
                PullRequestReviewCommentState::PENDING => CommentState::Pending,
                PullRequestReviewCommentState::SUBMITTED => CommentState::Submitted,
                PullRequestReviewCommentState::Other(value) => {
                    log::warn!("a review comment in a state Lo doesn't know: {value}");
                    CommentState::Submitted
                }
            },
            created_at: SystemTime::from(self.created_at),
            url: self.url.clone(),
            viewer_can_update: self.viewer_can_update,
            viewer_can_delete: self.viewer_can_delete,
        }
    }
}

impl CacheEntry for ReviewThreadList {
    const NAME: &'static str = OPERATION_NAME;

    fn shape() -> String {
        format!(
            "{}\n{}\n{}",
            schema_shape::<Self>(),
            QUERY,
            review_thread_comments::QUERY
        )
    }
}

impl CachedRead for ReviewThreadList {
    /// A push moves threads and makes some outdated. A new or deleted thread moves the count, a
    /// reply or a deleted comment the comment count of its thread, and resolving one its state:
    /// the check holds both for the first 100 threads. Edits show on the 5-minute refresh, and
    /// so do replies to threads past the first 100. A comment on the pull request itself, a
    /// review without comments, or a label changes none of them.
    const WATCHED: &'static [&'static str] = &["/headRefOid", "/reviewThreads"];
}

impl GithubClient {
    /// `pull`'s review threads, each with every comment.
    pub async fn review_threads(&self, pull: &PullRef) -> Result<ReviewThreadList, GithubError> {
        self.review_threads_paged(pull, PAGE_SIZE).await
    }

    /// [`Self::review_threads`], reading `page_size` threads a page, each with its first
    /// `page_size` comments, and a longer thread's other comments `page_size` a page.
    pub async fn review_threads_paged(
        &self,
        pull: &PullRef,
        page_size: i64,
    ) -> Result<ReviewThreadList, GithubError> {
        let (owner, name, number) = (&pull.repo.owner, &pull.repo.name, pull.number);
        let mut nodes = Vec::new();
        let mut after = None;
        let mut pager = None;
        loop {
            let threads = self
                .graphql_keeping_partial::<ReviewThreads>(
                    Variables {
                        owner: owner.clone(),
                        name: name.clone(),
                        number,
                        first: page_size,
                        after,
                    },
                    2,
                )
                .await?
                .repository
                .and_then(|repository| repository.pull_request)
                .ok_or_else(|| not_found(format!("pull request {pull}")))?
                .review_threads;
            let pager = pager.get_or_insert_with(|| {
                Pager::new("review threads", threads.total_count, page_size)
            });
            nodes.extend(threads.nodes.into_iter().flatten().flatten());
            after = pager.next(
                threads.page_info.has_next_page,
                threads.page_info.end_cursor,
            )?;
            if after.is_none() {
                break;
            }
        }
        for thread in &mut nodes {
            self.read_other_comments(thread, page_size).await?;
        }
        Ok(ReviewThreadList { nodes })
    }

    /// Reads the comments of `thread` past the ones its page held.
    async fn read_other_comments(
        &self,
        thread: &mut ReviewThread,
        page_size: i64,
    ) -> Result<(), GithubError> {
        let comments = &mut thread.comments;
        let mut pager = Pager::new("a thread's comments", comments.total_count, page_size);
        let mut after = pager.next(
            comments.page_info.has_next_page,
            comments.page_info.end_cursor.clone(),
        )?;
        while let Some(cursor) = after {
            use review_thread_comments::ReviewThreadCommentsNode as Node;
            let node = self
                .graphql_keeping_partial::<ReviewThreadComments>(
                    review_thread_comments::Variables {
                        id: thread.id.clone(),
                        first: page_size,
                        after: Some(cursor),
                    },
                    1,
                )
                .await?
                .node;
            let Some(Node::PullRequestReviewThread(node)) = node else {
                return Err(not_found(format!("review thread {}", thread.id)));
            };
            let page = node.comments;
            let more = (page.nodes.into_iter().flatten().flatten())
                .map(|comment| same_comment(&comment))
                .collect::<Result<Vec<_>, _>>()?;
            (comments.nodes.get_or_insert_with(Vec::new)).extend(more.into_iter().map(Some));
            comments.page_info.has_next_page = page.page_info.has_next_page;
            comments.page_info.end_cursor = page.page_info.end_cursor;
            after = pager.next(
                comments.page_info.has_next_page,
                comments.page_info.end_cursor.clone(),
            )?;
        }
        Ok(())
    }
}

/// A comment `ReviewThreadComments` read, as `ReviewThreads` reads one: the two queries select
/// the same fields, which `graphql_client` gives a type each.
fn same_comment(
    comment: &review_thread_comments::ReviewComment,
) -> Result<ReviewComment, GithubError> {
    serde_json::to_value(comment)
        .and_then(serde_json::from_value)
        .map_err(|error| {
            GithubError::Decode(format!(
                "ReviewThreadComments' comments aren't ReviewThreads': {error}"
            ))
        })
}

/// [`GithubClient::review_threads`] as [`crate::poll::Poller::read_cached`] runs it.
pub async fn read_review_threads(
    client: GithubClient,
    pull: PullRef,
    _known: Option<Arc<ReviewThreadList>>,
) -> Result<ReviewThreadList, GithubError> {
    client.review_threads(&pull).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_comments_queries_select_the_same_fields() {
        let fragment = |query: &str| {
            let start = query.find("fragment ReviewComment").unwrap();
            query[start..].to_string()
        };
        assert_eq!(fragment(QUERY), fragment(review_thread_comments::QUERY));
    }
}
