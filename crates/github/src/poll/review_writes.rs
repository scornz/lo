//! The writes that comment and review (design §8), run in an account's queue like any
//! [`Poller::write`], each reading back what it changed.
//!
//! None is ever sent again by Lo itself. A write that failed may still have been done, when its
//! answer was lost or GitHub's server failed ([`GithubError::may_have_posted`]); the read-back
//! below still runs, so the UI can check what's there before it offers to send again.
//!
//! The open pull request's check (`PullRequestCheck`) sees most of what they change, so the reads
//! that watch it read again on their own: a new thread or comment moves the count of threads, a
//! reply or a deleted comment its thread's count of comments, resolving a thread its state, a
//! change to your pending review its count of comments, a comment on the pull request itself, or
//! deleting one, the timeline's count, and an edit the pull request's own time, which the timeline
//! watches for its newest page. The check doesn't see (checked live, in the
//! `review_writes_e2e_23_*` recordings):
//!
//! - an edit of a comment in a review thread, which the threads don't watch the pull request's
//!   time for: every comment on the pull request would read them all again;
//! - an edit of an item past the timeline's newest page, which a read of the newest page misses;
//! - anything on a thread past the first [`CHECKED_THREADS`], whose state and comments it doesn't
//!   read;
//! - a deleted comment's review, in the timeline, whose count of comments it shows;
//! - your pending review, when a reply sent to be posted at once joined it, or when GitHub's
//!   refusal says it changed elsewhere: a second one refused, something it names gone, or a
//!   thread refused that may have started one.
//!
//! For those the write reads the threads, your pending review or the timeline again itself, the
//! timeline in full.

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
    /// is `pull_request_id`. [`Sent`] names what GitHub made, including a reply that joined a
    /// pending review instead of posting ([`Sent::joined_pending`]); a refusal, including a thread
    /// GitHub answered with `null`, is [`GithubError::Rejected`].
    ///
    /// It's never sent again by itself. When it fails in a way that may have posted it anyway
    /// ([`GithubError::may_have_posted`]), the UI compares the draft with your newest comments,
    /// as the read-back brings them, before offering to send it again.
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
        // A thread refused may have started a review, or been refused because the review is gone.
        let named_review = matches!(
            mutation,
            Mutation::AddThread { .. }
                | Mutation::StartReview {
                    thread: Some(_),
                    ..
                }
        );
        let target = pull.clone();
        self.write_reading_again(
            pull,
            move |client| async move {
                (client.send_review_mutation(&target, &pull_request_id, &mutation)).await
            },
            move |result| {
                let mut again = threads_if(unchecked);
                let changed_elsewhere = match result {
                    Ok(sent) => sent.joined_pending.is_some(),
                    Err(GithubError::Rejected(Rejection::NotInDiff)) => named_review,
                    Err(GithubError::Rejected(
                        Rejection::PendingReviewExists | Rejection::Gone,
                    )) => true,
                    Err(_) => false,
                };
                if changed_elsewhere {
                    again.extend([PENDING, THREADS]);
                }
                again
            },
            cx,
        )
    }

    /// Resolves the review thread `thread` on `pull`, or with `resolved: false` unresolves it.
    /// Never sent again by itself ([`GithubError::may_have_posted`]).
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
            move |result| threads_if(unchecked || gone(result)),
            cx,
        )
    }

    /// Replaces the text of `comment`, on `pull`, with `body`. The threads don't watch for edits,
    /// so an edit in a thread reads them again; one on the pull request itself reads the
    /// timeline in full, since the comment may be past its newest page. Never sent again by
    /// itself ([`GithubError::may_have_posted`]).
    pub fn edit_comment(
        &mut self,
        pull: PullRef,
        comment: CommentId,
        body: String,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), GithubError>> {
        let again = match comment {
            CommentId::Thread(_) => vec![THREADS],
            CommentId::Conversation(_) => vec![TIMELINE],
        };
        let in_thread = matches!(comment, CommentId::Thread(_));
        self.write_reading_again(
            pull,
            move |client| async move { client.edit_comment(&comment, &body).await },
            move |result| {
                let mut again = again;
                // A pending comment that's gone may have gone with its review.
                if in_thread && gone(result) {
                    again.push(PENDING);
                }
                again
            },
            cx,
        )
    }

    /// Deletes `comment`, on `pull`. A thread whose last comment goes is gone, and so is a posted
    /// review without a summary, so a posted comment reads the timeline again in full: its
    /// review may be past the newest page, and the check doesn't count it out. A pending comment
    /// doesn't. Never sent again by itself ([`GithubError::may_have_posted`]).
    pub fn delete_comment(
        &mut self,
        pull: PullRef,
        comment: CommentId,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), GithubError>> {
        let mut again = Vec::new();
        let in_thread = matches!(comment, CommentId::Thread(_));
        match &comment {
            CommentId::Thread(id) => {
                let held = self.held_comment(&pull, id);
                if held.is_none_or(|held| !held.checked) {
                    again.push(THREADS);
                }
                if held.is_none_or(|held| !held.pending) {
                    again.push(TIMELINE);
                }
            }
            CommentId::Conversation(_) => again.push(TIMELINE),
        }
        self.write_reading_again(
            pull,
            move |client| async move { client.delete_comment(&comment).await },
            move |result| {
                if in_thread && gone(result) {
                    again.extend([PENDING, THREADS]);
                }
                again
            },
            cx,
        )
    }

    /// Comments `body` on `pull`, whose node ID is `pull_request_id`, in its conversation: the box
    /// at the bottom of its timeline. Returns the comment's node ID. Never sent again by itself
    /// ([`GithubError::may_have_posted`]).
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

    /// What Lo last read of `comment`, in a review thread of `pull`: whether the check counts its
    /// thread's comments, and whether it's pending. `None` when Lo hasn't read it.
    fn held_comment(&self, pull: &PullRef, comment: &str) -> Option<HeldComment> {
        use crate::queries::review_threads::PullRequestReviewCommentState as State;
        let threads = self.cached_read::<ReviewThreadList>(pull)?;
        (threads.value.nodes.iter().enumerate()).find_map(|(index, node)| {
            let held =
                (node.comments.nodes.iter().flatten().flatten()).find(|held| held.id == comment)?;
            Some(HeldComment {
                checked: index < CHECKED_THREADS,
                pending: matches!(held.state, State::PENDING),
            })
        })
    }
}

/// What Lo last read of a comment in a review thread.
#[derive(Clone, Copy)]
struct HeldComment {
    /// Whether the check counts its thread's comments.
    checked: bool,
    /// Whether it's in your pending review.
    pending: bool,
}

const THREADS: Reread = reread(ReviewThreadList::NAME);
const PENDING: Reread = reread(ViewerPendingReview::NAME);
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

/// Whether GitHub said what the write named is gone.
fn gone<T>(result: &Result<T, GithubError>) -> bool {
    matches!(result, Err(GithubError::Rejected(Rejection::Gone)))
}
