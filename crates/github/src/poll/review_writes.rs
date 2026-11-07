//! The writes that comment and review (design §8), run in an account's queue like any
//! [`Poller::write`], each reading back what it changed.
//!
//! The open pull request's check (`PullRequestCheck`) sees most of what they change, so the reads
//! that watch it read again on their own: a new thread or comment moves the count of threads, a
//! reply or a deleted comment its thread's count of comments, resolving a thread its state, a
//! change to your pending review its count of comments, and a comment on the pull request itself,
//! or deleting one, the timeline's count. The check doesn't see (checked live, in the
//! `review_writes_e2e_23_*` recordings):
//!
//! - an edit, of a comment in a review thread or on the pull request itself: the timeline's time
//!   is its newest item's creation, and only the pull request's own `updatedAt` moves;
//! - anything on a thread past the first [`CHECKED_THREADS`], whose state and comments it doesn't
//!   read;
//! - a deleted comment's review, in the timeline, whose count of comments it shows;
//! - a pending review started elsewhere, which GitHub's refusal of a second one reveals.
//!
//! For those the write reads the threads, your pending review or the timeline again itself. A
//! timeline past its newest page reads only that page when it's read again, so after an edit or a
//! deletion it's read in full: the item may be older.

use gpui::{Context, Task};
use review::pending::Mutation;

use super::Poller;
use super::poller::Reread;
use crate::cache::CacheEntry as _;
use crate::error::GithubError;
use crate::pulls::PullRef;
use crate::queries::pull_request_check::CHECKED_THREADS;
use crate::queries::{PullRequestTimeline, ReviewThreadList, ViewerPendingReview};
use crate::review_writes::{CommentId, Rejection, Sent};

impl Poller {
    /// Sends `mutation`, which [`review::pending::YourReview`] planned for `pull`, whose node ID
    /// is `pull_request_id`. [`Sent`] names what GitHub made; a refusal, including a thread
    /// GitHub answered with `null`, is [`GithubError::Rejected`].
    pub fn send_review(
        &mut self,
        pull: PullRef,
        pull_request_id: String,
        mutation: Mutation,
        cx: &mut Context<Self>,
    ) -> Task<Result<Sent, GithubError>> {
        // A reply posted at once moves only its thread's count of comments.
        let unchecked = match &mutation {
            Mutation::PostReply { thread, .. } => !self.checks_thread(&pull, thread),
            _ => false,
        };
        let target = pull.clone();
        self.write_reading_again(
            pull,
            move |client| async move {
                (client.send_review_mutation(&target, &pull_request_id, &mutation)).await
            },
            move |result| {
                let mut again = threads_if(unchecked);
                // Lo didn't know of your pending review: it was started elsewhere since.
                if let Err(GithubError::Rejected(Rejection::PendingReviewExists)) = result {
                    again.extend([reread(ViewerPendingReview::NAME), THREADS]);
                }
                again
            },
            cx,
        )
    }

    /// Resolves the review thread `thread` on `pull`, or with `resolved: false` unresolves it.
    pub fn resolve_thread(
        &mut self,
        pull: PullRef,
        thread: String,
        resolved: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), GithubError>> {
        let unchecked = !self.checks_thread(&pull, &thread);
        self.write_reading_again(
            pull,
            move |client| async move { client.resolve_thread(&thread, resolved).await },
            move |_| threads_if(unchecked),
            cx,
        )
    }

    /// Replaces the text of `comment`, on `pull`, with `body`. The check sees no edit, so it reads
    /// the threads again, or the timeline in full.
    pub fn edit_comment(
        &mut self,
        pull: PullRef,
        comment: CommentId,
        body: String,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), GithubError>> {
        let again = match comment {
            CommentId::Thread(_) => THREADS,
            CommentId::Conversation(_) => TIMELINE,
        };
        self.write_reading_again(
            pull,
            move |client| async move { client.edit_comment(&comment, &body).await },
            move |_| vec![again],
            cx,
        )
    }

    /// Deletes `comment`, on `pull`. A thread whose last comment goes is gone.
    pub fn delete_comment(
        &mut self,
        pull: PullRef,
        comment: CommentId,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), GithubError>> {
        // The timeline shows a review's count of comments, and may hold the comment's item on a
        // page past the newest.
        let mut again = vec![TIMELINE];
        if let CommentId::Thread(id) = &comment
            && !self.checks_comment(&pull, id)
        {
            again.push(THREADS);
        }
        self.write_reading_again(
            pull,
            move |client| async move { client.delete_comment(&comment).await },
            move |_| again,
            cx,
        )
    }

    /// Comments `body` on `pull`, whose node ID is `pull_request_id`, in its conversation: the box
    /// at the bottom of its timeline. Returns the comment's node ID.
    pub fn add_conversation_comment(
        &mut self,
        pull: PullRef,
        pull_request_id: String,
        body: String,
        cx: &mut Context<Self>,
    ) -> Task<Result<String, GithubError>> {
        self.write(
            pull,
            move |client| async move {
                (client.add_conversation_comment(&pull_request_id, &body)).await
            },
            cx,
        )
    }

    /// Whether the check reads `thread`'s state and count of comments: whether it's among the
    /// first [`CHECKED_THREADS`] of `pull`'s threads, as last read. One Lo hasn't read isn't.
    fn checks_thread(&self, pull: &PullRef, thread: &str) -> bool {
        (self.cached_read::<ReviewThreadList>(pull)).is_some_and(|threads| {
            (threads.value.nodes.iter().take(CHECKED_THREADS)).any(|node| node.id == thread)
        })
    }

    /// Whether the check reads the count of comments of the thread that holds `comment`.
    fn checks_comment(&self, pull: &PullRef, comment: &str) -> bool {
        (self.cached_read::<ReviewThreadList>(pull)).is_some_and(|threads| {
            (threads.value.nodes.iter().take(CHECKED_THREADS)).any(|node| {
                (node.comments.nodes.iter().flatten().flatten()).any(|held| held.id == comment)
            })
        })
    }
}

const THREADS: Reread = reread(ReviewThreadList::NAME);
const TIMELINE: Reread = Reread {
    name: PullRequestTimeline::NAME,
    afresh: true,
};

const fn reread(name: &'static str) -> Reread {
    Reread {
        name,
        afresh: false,
    }
}

/// The threads' read, when `unchecked`.
fn threads_if(unchecked: bool) -> Vec<Reread> {
    if unchecked { vec![THREADS] } else { Vec::new() }
}
