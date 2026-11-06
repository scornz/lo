//! Your pending review, and what rozed sends GitHub to comment and to submit a review (design §8,
//! "Reviews"; §11, "Acting on what you saw").
//!
//! Comments collect into a pending review, which GitHub stores, so it survives quitting rozed and
//! shows as pending on the web. GitHub keeps at most one per person per pull request.
//! [`YourReview`] holds yours and plans each step as a [`Mutation`]: `c` adds a comment to it,
//! starting it on the first; `C` posts one comment at once; the submit dialog submits it, or
//! reviews without one.
//!
//! GitHub's API leaves much of this undocumented. These were checked against GitHub:
//!
//! - A review is recorded on the commit it's started on (`addPullRequestReview`'s `commitOID`),
//!   and so are the threads it's started with. A thread added to it later
//!   (`addPullRequestReviewThread`, which takes no commit) goes on the pull request's current head
//!   instead, whatever the review's commit. So a line comment on an older revision is possible
//!   only by starting a review there. With a review pending, a new line comment is taken only
//!   while the right column shows the latest head; elsewhere it's refused, and the UI offers to
//!   submit the pending review or to switch to the latest revision.
//! - Replies have no lines, so they join the pending review from any revision.
//! - `addPullRequestReview`'s threads can't be file-level (its draft thread input has no subject
//!   type). So `c` on a file with no pending review is two calls: start an empty review, then
//!   `addPullRequestReviewThread` with `subjectType: FILE`. A reply as the first comment is two
//!   calls too: start, then reply. `C` on a file posts REST's file comment.
//! - A comment posted at once is a review of its own, which GitHub refuses while you have a
//!   pending review ("User can only have one pending review per pull request"), so `C` adds to
//!   the pending review instead, and says so.
//! - GitHub refuses a line outside the diff. `addPullRequestReview` answers "Line could not be
//!   resolved" and REST "could not be resolved", but `addPullRequestReviewThread` returns
//!   `thread: null` with no error. A null thread is a refusal, not an acceptance.
//!
//! Each call is planned and accepted on its own, so a failure loses nothing: the pending count
//! changes only when GitHub accepts a comment, and a comment's draft is kept until a mutation
//! that [sends it](Mutation::sends_comment) is accepted. The UI sends one call at a time and waits
//! for its answer: two `c`s before the first is accepted would plan two reviews, and GitHub
//! refuses the second.
//!
//! ```
//! use review::pending::{Mutation, NewComment, NewThread, ReviewEvent, YourReview};
//! use review::threads::{Line, Lines};
//!
//! let head = "c7f30e9b1a5d7c3e9f1b5d7a3c9e1f5b7d3a9c62";
//! let comment = NewComment::Thread(NewThread {
//!     path: "src/parse.rs".into(),
//!     lines: Some(Lines::one(Line::right(14))),
//!     body: "Off by one?".into(),
//! });
//!
//! // `c`: the first comment starts your pending review on the revision on screen.
//! let mut review = YourReview::default();
//! let start = review.add(head, head, &comment).unwrap();
//! assert!(matches!(&start, Mutation::StartReview { commit, .. } if commit == head));
//! review.accepted(&start, Some("PRR_kwDOAbc"));
//! assert_eq!(review.pending_comments(), 1);
//!
//! // `⌘⏎`: the dialog offers Comment, Approve and Request changes, and approving submits it.
//! assert_eq!(review.submit_dialog("alice", false, head).choices.len(), 3);
//! let submit = review.submit(ReviewEvent::Approve, "", false, head).unwrap();
//! assert!(matches!(submit, Mutation::SubmitReview { event: ReviewEvent::Approve, .. }));
//! ```

use crate::github::github_enum;
use crate::threads::Lines;

github_enum! {
    /// What submitting a review does: GraphQL's `PullRequestReviewEvent`, without `DISMISS`, which
    /// dismisses someone else's review. Ordered as the submit dialog lists them.
    pub enum ReviewEvent {
        /// Feedback, without approving or requesting changes.
        Comment = "COMMENT",
        Approve = "APPROVE",
        RequestChanges = "REQUEST_CHANGES",
    }
}

/// Your pending review on GitHub: a review of yours whose `state` is `PENDING`, such as
/// `reviews(states: PENDING, author: $login, first: 1)` returns. Field docs name the
/// `PullRequestReview` field each one comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingReview {
    /// `id`.
    pub id: String,
    /// `commit.oid`: the revision it's recorded on, which the review shows once submitted, and the
    /// one the threads it was started with are on. Threads added to it later are on the heads
    /// that were current when they were added, so they may be on later revisions.
    pub commit: String,
    /// `comments.totalCount`: its threads and replies. [`YourReview::accepted`] counts the ones
    /// rozed adds. Deleting a pending comment isn't a mutation here, so the GitHub layer reads the
    /// pending review again after one.
    pub comments: usize,
}

/// A new thread, as `addPullRequestReviewThread` and `addPullRequestReview`'s `threads` take it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewThread {
    /// `path`.
    pub path: String,
    /// `line`, `side`, `startLine` and `startSide`, from
    /// [`CommentableLines`](crate::threads::CommentableLines). `None` for a file-level comment,
    /// `subjectType: FILE`, which only `addPullRequestReviewThread` takes.
    pub lines: Option<Lines>,
    /// `body`, in Markdown.
    pub body: String,
}

/// A comment you write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NewComment {
    /// A new thread, on lines of the right column's revision or on a file.
    Thread(NewThread),
    /// A reply to the thread with id `thread`.
    Reply { thread: String, body: String },
}

impl NewComment {
    /// The comment's text.
    pub fn body(&self) -> &str {
        match self {
            Self::Thread(thread) => &thread.body,
            Self::Reply { body, .. } => body,
        }
    }
}

/// What to send GitHub. Each variant names its GraphQL mutation, or REST endpoint; all of them
/// also take the pull request's `pullRequestId` where GitHub asks for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mutation {
    /// `addPullRequestReview` with `commitOID: commit` and no `event`, which starts your pending
    /// review, recorded on `commit`. A line comment goes in its `threads`, on `commit`'s lines:
    /// the only way to put a pending thread on a revision older than the latest head.
    ///
    /// Without one, it starts an empty review, for a file-level comment or a reply, which can't
    /// go in `threads`. It starts on the latest head, where a file-level thread goes anyway, so
    /// approving the latest revision isn't refused later. Once it's accepted,
    /// [`YourReview::add`] plans the comment's own mutation.
    StartReview {
        commit: String,
        /// The line comment, never a file-level one.
        thread: Option<NewThread>,
    },
    /// `addPullRequestReviewThread` with `pullRequestReviewId: review`, and `subjectType: FILE`
    /// for a file-level comment. GitHub puts the thread on the pull request's current head. It
    /// refuses a line outside the diff by returning `thread: null` without an error, which isn't
    /// an acceptance.
    AddThread { review: String, thread: NewThread },
    /// `addPullRequestReviewThreadReply` with `pullRequestReviewId: review` and
    /// `pullRequestReviewThreadId: thread`.
    AddReply {
        review: String,
        thread: String,
        body: String,
    },
    /// `addPullRequestReview` with `commitOID: commit`, `event: COMMENT` and the line comment in
    /// `threads`: one comment, posted at once on `commit`.
    PostThread { commit: String, thread: NewThread },
    /// REST's `POST /repos/{owner}/{repo}/pulls/{number}/comments` with `commit_id: commit`,
    /// `path`, `body` and `subject_type: "file"`: a file-level comment, posted at once.
    /// `addPullRequestReview`'s threads can't be file-level.
    PostFileComment {
        commit: String,
        path: String,
        body: String,
    },
    /// `addPullRequestReviewThreadReply` with `pullRequestReviewThreadId: thread` and no
    /// `pullRequestReviewId`: a reply, posted at once.
    PostReply { thread: String, body: String },
    /// `submitPullRequestReview` with `pullRequestReviewId: review`, `event` and `body`, which may
    /// be empty.
    SubmitReview {
        review: String,
        event: ReviewEvent,
        body: String,
    },
    /// `addPullRequestReview` with `commitOID: commit`, `event` and `body`, which may be empty: a
    /// review with no comments, submitted at once.
    Review {
        commit: String,
        event: ReviewEvent,
        body: String,
    },
}

impl Mutation {
    /// Whether accepting it sends the comment it was planned for, so the comment's draft can go:
    /// every comment mutation does but a [`StartReview`](Self::StartReview) without a thread,
    /// which only starts the review the comment then joins. Submitting a review sends no comment.
    pub fn sends_comment(&self) -> bool {
        match self {
            Self::StartReview { thread, .. } => thread.is_some(),
            Self::AddThread { .. }
            | Self::AddReply { .. }
            | Self::PostThread { .. }
            | Self::PostFileComment { .. }
            | Self::PostReply { .. } => true,
            Self::SubmitReview { .. } | Self::Review { .. } => false,
        }
    }
}

/// What `C` sends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Posting {
    /// The comment, posted at once.
    Now(Mutation),
    /// The comment, added to your pending review instead: GitHub posts nothing else from you while
    /// you have one, so the UI says so.
    Pending(Mutation),
}

/// Why rozed won't send a comment or a review.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The comment is blank, which GitHub refuses.
    Blank,
    /// The right column shows a revision older than the latest head, `head`, but a new line
    /// comment would join your pending review, and GitHub puts the threads it adds to one on the
    /// latest head. Submit the pending review first, or switch to the latest revision.
    NotLatest { head: String },
    /// Your pending review is recorded on another revision, `review_on`, so approving it or
    /// requesting changes with it would record that on code that isn't on screen. Submit it as a
    /// comment, or switch to its revision.
    OtherRevision { review_on: String },
    /// You opened the pull request, and GitHub lets its author only comment.
    OwnPullRequest,
    /// A comment or a request for changes needs a summary or a pending comment: GitHub requires a
    /// body for them when the review holds nothing else.
    NothingToSubmit,
}

/// What the submit dialog shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubmitDialog {
    /// The choices, in order: Comment, Approve and Request changes, or only Comment on your own
    /// pull request.
    pub choices: &'static [ReviewEvent],
    /// How many comments your pending review holds.
    pub pending_comments: usize,
    /// The account you're acting as.
    pub account: String,
    /// The revision the review is recorded on: your pending review's, or else the right column's.
    pub on: String,
    /// Whether that's the revision on screen. When it isn't, your pending review was started on
    /// another, and the dialog says which: it submits only as a comment there.
    pub on_screen: bool,
}

/// The submit dialog's choices: only Comment on your own pull request, since GitHub refuses its
/// author's approval or request for changes.
pub fn choices(own: bool) -> &'static [ReviewEvent] {
    if own {
        &[ReviewEvent::Comment]
    } else {
        &[
            ReviewEvent::Comment,
            ReviewEvent::Approve,
            ReviewEvent::RequestChanges,
        ]
    }
}

/// Your review of a pull request: your pending review on GitHub, if you have one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct YourReview {
    pending: Option<PendingReview>,
}

impl YourReview {
    /// Your review, from your pending review on GitHub.
    pub fn new(pending: Option<PendingReview>) -> Self {
        Self { pending }
    }

    /// Your pending review, if you have one.
    pub fn pending(&self) -> Option<&PendingReview> {
        self.pending.as_ref()
    }

    /// How many comments your pending review holds.
    pub fn pending_comments(&self) -> usize {
        self.pending.as_ref().map_or(0, |pending| pending.comments)
    }

    /// What `c` sends next: `comment`, written while the right column shows revision `on`, added
    /// to your pending review, or starting one. `head` is the pull request's latest head
    /// (`headRefOid`).
    ///
    /// With no pending review, a line comment starts one on `on`, with the comment in it. A
    /// file-level comment or a reply starts an empty one on `head` first: once that's accepted,
    /// call it again for the comment's own mutation.
    pub fn add(&self, on: &str, head: &str, comment: &NewComment) -> Result<Mutation, Refusal> {
        if comment.body().trim().is_empty() {
            return Err(Refusal::Blank);
        }
        let Some(pending) = &self.pending else {
            return Ok(match comment {
                NewComment::Thread(thread) if thread.lines.is_some() => Mutation::StartReview {
                    commit: on.to_string(),
                    thread: Some(thread.clone()),
                },
                _ => Mutation::StartReview {
                    commit: head.to_string(),
                    thread: None,
                },
            });
        };
        let review = pending.id.clone();
        match comment {
            NewComment::Thread(thread) if thread.lines.is_some() && on != head => {
                Err(Refusal::NotLatest {
                    head: head.to_string(),
                })
            }
            NewComment::Thread(thread) => Ok(Mutation::AddThread {
                review,
                thread: thread.clone(),
            }),
            NewComment::Reply { thread, body } => Ok(Mutation::AddReply {
                review,
                thread: thread.clone(),
                body: body.clone(),
            }),
        }
    }

    /// What `C` sends: `comment`, written while the right column shows revision `on`, posted at
    /// once, or added to your pending review if you have one, as [`add`](Self::add) adds it.
    pub fn post(&self, on: &str, head: &str, comment: &NewComment) -> Result<Posting, Refusal> {
        if self.pending.is_some() {
            return self.add(on, head, comment).map(Posting::Pending);
        }
        if comment.body().trim().is_empty() {
            return Err(Refusal::Blank);
        }
        Ok(Posting::Now(match comment {
            NewComment::Thread(thread) if thread.lines.is_some() => Mutation::PostThread {
                commit: on.to_string(),
                thread: thread.clone(),
            },
            NewComment::Thread(thread) => Mutation::PostFileComment {
                commit: on.to_string(),
                path: thread.path.clone(),
                body: thread.body.clone(),
            },
            NewComment::Reply { thread, body } => Mutation::PostReply {
                thread: thread.clone(),
                body: body.clone(),
            },
        }))
    }

    /// Records that GitHub accepted `mutation`. `review_id` is the id GitHub gave a review the
    /// mutation started (`addPullRequestReview`'s `pullRequestReview.id`); without it, starting a
    /// review records nothing.
    ///
    /// Call it only for an acceptance. GitHub refuses most mutations with an error, but
    /// `addPullRequestReviewThread` refuses a line outside the diff by returning `thread: null`
    /// with no error: that's a refusal, so don't call it, and keep the comment's draft.
    pub fn accepted(&mut self, mutation: &Mutation, review_id: Option<&str>) {
        match mutation {
            Mutation::StartReview { commit, thread } => {
                if let Some(id) = review_id {
                    self.pending = Some(PendingReview {
                        id: id.to_string(),
                        commit: commit.clone(),
                        comments: usize::from(thread.is_some()),
                    });
                }
            }
            Mutation::AddThread { .. } | Mutation::AddReply { .. } => {
                if let Some(pending) = &mut self.pending {
                    pending.comments += 1;
                }
            }
            Mutation::PostThread { .. }
            | Mutation::PostFileComment { .. }
            | Mutation::PostReply { .. } => {}
            Mutation::SubmitReview { .. } | Mutation::Review { .. } => self.pending = None,
        }
    }

    /// What the submit dialog shows, for `account`, on your own pull request or not, with the
    /// right column showing revision `on`.
    pub fn submit_dialog(&self, account: &str, own: bool, on: &str) -> SubmitDialog {
        let review_on = self.pending.as_ref().map_or(on, |pending| &pending.commit);
        SubmitDialog {
            choices: choices(own),
            pending_comments: self.pending_comments(),
            account: account.to_string(),
            on: review_on.to_string(),
            on_screen: review_on == on,
        }
    }

    /// What submitting sends: your pending review with `event` and the summary `body`, or a review
    /// of revision `on`, the right column's, when you have no pending review. A blank summary is
    /// sent empty. Approving or requesting changes with a pending review recorded on another
    /// revision than `on` is refused (design §11: a review records the commit you looked at).
    pub fn submit(
        &self,
        event: ReviewEvent,
        body: &str,
        own: bool,
        on: &str,
    ) -> Result<Mutation, Refusal> {
        if !choices(own).contains(&event) {
            return Err(Refusal::OwnPullRequest);
        }
        if let Some(pending) = &self.pending
            && event != ReviewEvent::Comment
            && pending.commit != on
        {
            return Err(Refusal::OtherRevision {
                review_on: pending.commit.clone(),
            });
        }
        let blank = body.trim().is_empty();
        if event != ReviewEvent::Approve && blank && self.pending_comments() == 0 {
            return Err(Refusal::NothingToSubmit);
        }
        let body = if blank { "" } else { body }.to_string();
        Ok(match &self.pending {
            Some(pending) => Mutation::SubmitReview {
                review: pending.id.clone(),
                event,
                body,
            },
            None => Mutation::Review {
                commit: on.to_string(),
                event,
                body,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::threads::Line;

    /// Revisions r1 to r3 have heads `a1` to `a3`; `a3` is the latest.
    const HEAD: &str = "a3";

    fn thread(body: &str) -> NewComment {
        NewComment::Thread(NewThread {
            path: "src/parse.rs".to_string(),
            lines: Some(Lines::one(Line::right(12))),
            body: body.to_string(),
        })
    }

    fn file(body: &str) -> NewComment {
        NewComment::Thread(NewThread {
            path: "README.md".to_string(),
            lines: None,
            body: body.to_string(),
        })
    }

    fn reply(body: &str) -> NewComment {
        NewComment::Reply {
            thread: "PRRT_9".to_string(),
            body: body.to_string(),
        }
    }

    fn new_thread(comment: NewComment) -> NewThread {
        match comment {
            NewComment::Thread(thread) => thread,
            NewComment::Reply { .. } => unreachable!(),
        }
    }

    fn pending(commit: &str, comments: usize) -> YourReview {
        YourReview::new(Some(PendingReview {
            id: "PRR_1".to_string(),
            commit: commit.to_string(),
            comments,
        }))
    }

    #[test]
    fn the_first_comment_starts_a_pending_review_on_the_revision_on_screen() {
        assert_eq!(
            YourReview::default().add("a3", HEAD, &thread("Off by one?")),
            Ok(Mutation::StartReview {
                commit: "a3".to_string(),
                thread: Some(new_thread(thread("Off by one?"))),
            })
        );
    }

    #[test]
    fn later_comments_join_the_pending_review_and_count() {
        let mut review = YourReview::default();
        let start = review.add("a3", HEAD, &thread("Off by one?")).unwrap();
        review.accepted(&start, Some("PRR_1"));

        assert_eq!(
            review.pending(),
            Some(&PendingReview {
                id: "PRR_1".to_string(),
                commit: "a3".to_string(),
                comments: 1,
            })
        );

        let add = review.add("a3", HEAD, &thread("And here.")).unwrap();
        assert_eq!(
            add,
            Mutation::AddThread {
                review: "PRR_1".to_string(),
                thread: new_thread(thread("And here.")),
            }
        );
        review.accepted(&add, None);

        let answer = review.add("a3", HEAD, &reply("Agreed.")).unwrap();
        assert_eq!(
            answer,
            Mutation::AddReply {
                review: "PRR_1".to_string(),
                thread: "PRRT_9".to_string(),
                body: "Agreed.".to_string(),
            }
        );
        review.accepted(&answer, None);

        assert_eq!(review.pending_comments(), 3);
    }

    #[test]
    fn a_reply_as_the_first_comment_starts_an_empty_review_then_joins_it() {
        let mut review = YourReview::default();
        // From any revision, the review starts on the latest head: a reply has no revision.
        let start = review.add("a1", HEAD, &reply("Agreed.")).unwrap();
        assert_eq!(
            start,
            Mutation::StartReview {
                commit: "a3".to_string(),
                thread: None,
            }
        );
        assert!(!start.sends_comment());
        review.accepted(&start, Some("PRR_1"));
        assert_eq!(review.pending_comments(), 0);

        let answer = review.add("a1", HEAD, &reply("Agreed.")).unwrap();
        assert_eq!(
            answer,
            Mutation::AddReply {
                review: "PRR_1".to_string(),
                thread: "PRRT_9".to_string(),
                body: "Agreed.".to_string(),
            }
        );
        assert!(answer.sends_comment());
        review.accepted(&answer, None);
        assert_eq!(review.pending_comments(), 1);
    }

    #[test]
    fn a_file_level_first_comment_starts_an_empty_review_then_adds_its_thread() {
        // `addPullRequestReview`'s threads can't be file-level.
        let mut review = YourReview::default();
        let start = review
            .add("a2", HEAD, &file("Needs a changelog entry."))
            .unwrap();
        assert_eq!(
            start,
            Mutation::StartReview {
                commit: "a3".to_string(),
                thread: None,
            }
        );
        review.accepted(&start, Some("PRR_1"));

        assert_eq!(
            review.add("a2", HEAD, &file("Needs a changelog entry.")),
            Ok(Mutation::AddThread {
                review: "PRR_1".to_string(),
                thread: new_thread(file("Needs a changelog entry.")),
            })
        );
    }

    #[test]
    fn a_file_level_comment_joins_a_pending_review_from_any_revision() {
        assert_eq!(
            pending("a3", 1).add("a1", HEAD, &file("Needs a changelog entry.")),
            Ok(Mutation::AddThread {
                review: "PRR_1".to_string(),
                thread: new_thread(file("Needs a changelog entry.")),
            })
        );
    }

    #[test]
    fn an_empty_review_whose_first_comment_failed_has_nothing_to_submit() {
        let mut review = YourReview::default();
        let start = review.add("a3", HEAD, &reply("Agreed.")).unwrap();
        review.accepted(&start, Some("PRR_1"));
        // GitHub refused the reply, so it's never accepted.
        let _reply = review.add("a3", HEAD, &reply("Agreed.")).unwrap();

        assert_eq!(review.pending_comments(), 0);
        assert_eq!(
            review.submit(ReviewEvent::Comment, "", false, "a3"),
            Err(Refusal::NothingToSubmit)
        );
    }

    #[test]
    fn a_new_thread_joins_a_pending_review_on_any_revision_from_the_latest_head() {
        // GitHub puts it on the latest head, whatever revision the review was started on.
        assert_eq!(
            pending("a2", 1).add("a3", HEAD, &thread("Off by one?")),
            Ok(Mutation::AddThread {
                review: "PRR_1".to_string(),
                thread: new_thread(thread("Off by one?")),
            })
        );
    }

    #[test]
    fn a_new_line_thread_on_an_older_revision_does_not_join_the_pending_review() {
        // It would land on the latest head's line of that number, so the UI offers to submit the
        // pending review or to switch to the latest revision.
        let not_latest = Err(Refusal::NotLatest {
            head: "a3".to_string(),
        });

        assert_eq!(
            pending("a3", 1).add("a2", HEAD, &thread("Off by one?")),
            not_latest
        );
        // Even on the revision the review was started on.
        assert_eq!(
            pending("a2", 1).add("a2", HEAD, &thread("Off by one?")),
            not_latest
        );
    }

    #[test]
    fn a_reply_joins_the_pending_review_from_any_revision() {
        for on in ["a1", "a2", "a3"] {
            assert_eq!(
                pending("a2", 1).add(on, HEAD, &reply("Agreed.")),
                Ok(Mutation::AddReply {
                    review: "PRR_1".to_string(),
                    thread: "PRRT_9".to_string(),
                    body: "Agreed.".to_string(),
                })
            );
        }
    }

    #[test]
    fn without_a_pending_review_a_line_comment_on_an_older_revision_starts_one_there() {
        // `addPullRequestReview`'s threads are on its `commitOID`.
        assert_eq!(
            YourReview::default().add("a2", HEAD, &thread("Off by one?")),
            Ok(Mutation::StartReview {
                commit: "a2".to_string(),
                thread: Some(new_thread(thread("Off by one?"))),
            })
        );
    }

    #[test]
    fn a_blank_comment_is_refused() {
        assert_eq!(
            YourReview::default().add("a3", HEAD, &thread(" \n\t")),
            Err(Refusal::Blank)
        );
        assert_eq!(
            pending("a3", 1).add("a3", HEAD, &reply("")),
            Err(Refusal::Blank)
        );
        assert_eq!(
            YourReview::default().post("a3", HEAD, &thread("")),
            Err(Refusal::Blank)
        );
    }

    #[test]
    fn c_posts_one_comment_at_once_without_a_pending_review() {
        let review = YourReview::default();

        assert_eq!(
            review.post("a3", HEAD, &thread("Off by one?")),
            Ok(Posting::Now(Mutation::PostThread {
                commit: "a3".to_string(),
                thread: new_thread(thread("Off by one?")),
            }))
        );
        assert_eq!(
            review.post("a3", HEAD, &reply("Agreed.")),
            Ok(Posting::Now(Mutation::PostReply {
                thread: "PRRT_9".to_string(),
                body: "Agreed.".to_string(),
            }))
        );
        // On an older revision, a line comment is posted on that revision.
        assert_eq!(
            review.post("a2", HEAD, &thread("Off by one?")),
            Ok(Posting::Now(Mutation::PostThread {
                commit: "a2".to_string(),
                thread: new_thread(thread("Off by one?")),
            }))
        );
    }

    #[test]
    fn c_on_a_file_posts_a_file_comment_through_rest() {
        assert_eq!(
            YourReview::default().post("a2", HEAD, &file("Needs a changelog entry.")),
            Ok(Posting::Now(Mutation::PostFileComment {
                commit: "a2".to_string(),
                path: "README.md".to_string(),
                body: "Needs a changelog entry.".to_string(),
            }))
        );
        assert_eq!(
            pending("a3", 1).post("a2", HEAD, &file("Needs a changelog entry.")),
            Ok(Posting::Pending(Mutation::AddThread {
                review: "PRR_1".to_string(),
                thread: new_thread(file("Needs a changelog entry.")),
            }))
        );
    }

    #[test]
    fn every_mutation_but_an_empty_start_or_a_submission_sends_the_comment() {
        let line = new_thread(thread("Off by one?"));
        let sends = |mutation: Mutation| mutation.sends_comment();
        let review = || "PRR_1".to_string();
        let commit = || "a3".to_string();
        let body = || "Agreed.".to_string();

        assert!(sends(Mutation::StartReview {
            commit: commit(),
            thread: Some(line.clone()),
        }));
        assert!(!sends(Mutation::StartReview {
            commit: commit(),
            thread: None,
        }));
        assert!(sends(Mutation::AddThread {
            review: review(),
            thread: line.clone(),
        }));
        assert!(sends(Mutation::AddReply {
            review: review(),
            thread: "PRRT_9".to_string(),
            body: body(),
        }));
        assert!(sends(Mutation::PostThread {
            commit: commit(),
            thread: line,
        }));
        assert!(sends(Mutation::PostFileComment {
            commit: commit(),
            path: "README.md".to_string(),
            body: body(),
        }));
        assert!(sends(Mutation::PostReply {
            thread: "PRRT_9".to_string(),
            body: body(),
        }));
        assert!(!sends(Mutation::SubmitReview {
            review: review(),
            event: ReviewEvent::Comment,
            body: body(),
        }));
        assert!(!sends(Mutation::Review {
            commit: commit(),
            event: ReviewEvent::Comment,
            body: body(),
        }));
    }

    #[test]
    fn a_comment_posted_at_once_leaves_no_pending_review() {
        let mut review = YourReview::default();
        for comment in [thread("Off by one?"), file("Changelog?"), reply("Agreed.")] {
            let Ok(Posting::Now(post)) = review.post("a3", HEAD, &comment) else {
                unreachable!()
            };
            // GitHub gives a review id for a comment posted at once too: it's a review of its own.
            review.accepted(&post, Some("PRR_posted"));
        }

        assert_eq!(review.pending(), None);
        assert_eq!(review.pending_comments(), 0);
        // So `c` still starts one.
        assert!(matches!(
            review.add("a3", HEAD, &thread("Off by one?")),
            Ok(Mutation::StartReview { .. })
        ));
    }

    #[test]
    fn c_adds_to_the_pending_review_when_there_is_one() {
        assert_eq!(
            pending("a3", 2).post("a3", HEAD, &thread("Off by one?")),
            Ok(Posting::Pending(Mutation::AddThread {
                review: "PRR_1".to_string(),
                thread: new_thread(thread("Off by one?")),
            }))
        );
        assert_eq!(
            pending("a3", 2).post("a2", HEAD, &thread("Off by one?")),
            Err(Refusal::NotLatest {
                head: "a3".to_string(),
            })
        );
        assert_eq!(
            pending("a3", 2).post("a1", HEAD, &reply("Agreed.")),
            Ok(Posting::Pending(Mutation::AddReply {
                review: "PRR_1".to_string(),
                thread: "PRRT_9".to_string(),
                body: "Agreed.".to_string(),
            }))
        );
    }

    #[test]
    fn submitting_sends_the_pending_review_and_ends_it() {
        let mut review = pending("a3", 2);
        let submit = review
            .submit(ReviewEvent::Approve, "", false, "a3")
            .unwrap();

        assert_eq!(
            submit,
            Mutation::SubmitReview {
                review: "PRR_1".to_string(),
                event: ReviewEvent::Approve,
                body: String::new(),
            }
        );
        review.accepted(&submit, None);
        assert_eq!(review.pending(), None);
        assert_eq!(review.pending_comments(), 0);
    }

    #[test]
    fn approving_or_requesting_changes_needs_the_pending_reviews_revision_on_screen() {
        // GitHub would record the approval on r3, code that isn't on screen.
        let review = pending("a3", 1);
        let other_revision = Err(Refusal::OtherRevision {
            review_on: "a3".to_string(),
        });

        assert_eq!(
            review.submit(ReviewEvent::Approve, "", false, "a2"),
            other_revision
        );
        assert_eq!(
            review.submit(ReviewEvent::RequestChanges, "Wait.", false, "a2"),
            other_revision
        );
        // A comment approves nothing, so it's sent from any revision.
        assert_eq!(
            review.submit(ReviewEvent::Comment, "", false, "a2"),
            Ok(Mutation::SubmitReview {
                review: "PRR_1".to_string(),
                event: ReviewEvent::Comment,
                body: String::new(),
            })
        );
        // Your own pull request's refusal comes first.
        assert_eq!(
            review.submit(ReviewEvent::Approve, "", true, "a2"),
            Err(Refusal::OwnPullRequest)
        );
    }

    #[test]
    fn without_a_pending_review_submitting_reviews_the_revision_on_screen() {
        assert_eq!(
            YourReview::default().submit(ReviewEvent::RequestChanges, "Needs tests.", false, "a3"),
            Ok(Mutation::Review {
                commit: "a3".to_string(),
                event: ReviewEvent::RequestChanges,
                body: "Needs tests.".to_string(),
            })
        );
    }

    #[test]
    fn your_own_pull_request_offers_only_comment() {
        assert_eq!(choices(true), [ReviewEvent::Comment]);
        assert_eq!(
            choices(false),
            [
                ReviewEvent::Comment,
                ReviewEvent::Approve,
                ReviewEvent::RequestChanges
            ]
        );

        let review = pending("a3", 1);
        assert_eq!(
            review.submit(ReviewEvent::Approve, "", true, "a3"),
            Err(Refusal::OwnPullRequest)
        );
        assert_eq!(
            review.submit(ReviewEvent::RequestChanges, "Wait.", true, "a3"),
            Err(Refusal::OwnPullRequest)
        );
        assert!(review.submit(ReviewEvent::Comment, "", true, "a3").is_ok());
    }

    #[test]
    fn a_comment_or_request_for_changes_needs_a_summary_or_a_pending_comment() {
        let none = YourReview::default();
        let empty = pending("a3", 0);
        let some = pending("a3", 1);

        assert_eq!(
            some.submit(ReviewEvent::Comment, " \n", false, "a3"),
            Ok(Mutation::SubmitReview {
                review: "PRR_1".to_string(),
                event: ReviewEvent::Comment,
                body: String::new(),
            })
        );
        for event in [ReviewEvent::Comment, ReviewEvent::RequestChanges] {
            assert_eq!(
                none.submit(event, " ", false, "a3"),
                Err(Refusal::NothingToSubmit)
            );
            assert_eq!(
                empty.submit(event, "", false, "a3"),
                Err(Refusal::NothingToSubmit)
            );
            assert!(none.submit(event, "Looks off.", false, "a3").is_ok());
            assert!(some.submit(event, "", false, "a3").is_ok());
        }
        assert!(none.submit(ReviewEvent::Approve, "", false, "a3").is_ok());
    }

    #[test]
    fn the_submit_dialog_shows_the_count_the_account_and_the_revision() {
        assert_eq!(
            pending("a3", 2).submit_dialog("alice", false, "a3"),
            SubmitDialog {
                choices: choices(false),
                pending_comments: 2,
                account: "alice".to_string(),
                on: "a3".to_string(),
                on_screen: true,
            }
        );
        assert_eq!(
            YourReview::default().submit_dialog("alice", true, "a3"),
            SubmitDialog {
                choices: choices(true),
                pending_comments: 0,
                account: "alice".to_string(),
                on: "a3".to_string(),
                on_screen: true,
            }
        );
    }

    #[test]
    fn the_submit_dialog_says_when_the_pending_review_is_on_another_revision() {
        let dialog = pending("a2", 1).submit_dialog("alice", false, "a3");

        assert_eq!(dialog.on, "a2");
        assert!(!dialog.on_screen);
    }

    #[test]
    fn review_events_have_graphql_names() {
        assert_eq!("REQUEST_CHANGES".parse(), Ok(ReviewEvent::RequestChanges));
        assert_eq!(ReviewEvent::Approve.as_str(), "APPROVE");
        assert!("DISMISS".parse::<ReviewEvent>().is_err());
    }
}
