//! Your pending review, and what rozed sends GitHub to comment and to submit a review (design §8,
//! "Reviews"; §11, "Acting on what you saw").
//!
//! Comments collect into a pending review, which GitHub stores, so it survives quitting rozed and
//! shows as pending on the web. GitHub keeps at most one per person per pull request, and puts
//! every comment in it on the lines of the revision it was started on. [`YourReview`] holds yours
//! and plans each step as a [`Mutation`]: `c` adds a comment to it, starting it on the first;
//! `C` posts one comment at once; the submit dialog submits it, or reviews without one.
//!
//! GitHub's API leaves some of this undocumented, so these are rozed's reading of it:
//!
//! - A thread added to a pending review goes on the review's revision (`addPullRequestReviewThread`
//!   takes no commit), so a thread written on another revision doesn't join it.
//! - A comment posted at once is a review of its own, which GitHub refuses while you have a
//!   pending review ("can only have one pending review per pull request"), so `C` adds to the
//!   pending review instead, and says so.
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
//! let start = review.add(head, &comment).unwrap();
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
    /// `commit.oid`: the revision its threads' lines are on, and the one it's recorded on when
    /// submitted.
    pub commit: String,
    /// `comments.totalCount`: its threads and replies.
    pub comments: usize,
}

/// A new thread, as `addPullRequestReviewThread` and `addPullRequestReview`'s `threads` take it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewThread {
    /// `path`.
    pub path: String,
    /// `line`, `side`, `startLine` and `startSide`, from
    /// [`CommentableLines`](crate::threads::CommentableLines). `None` for a file-level comment,
    /// `subjectType: FILE`.
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

/// What to send GitHub. Each variant names its GraphQL mutation; all of them also take the pull
/// request's `pullRequestId` where GitHub asks for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mutation {
    /// `addPullRequestReview` with `commitOID: commit` and no `event`, which starts your pending
    /// review. A thread goes in its `threads`. A reply needs the review first: after it,
    /// `addPullRequestReviewThreadReply` adds the reply with the new review's id as
    /// `pullRequestReviewId`.
    StartReview { commit: String, first: NewComment },
    /// `addPullRequestReviewThread` with `pullRequestReviewId: review`.
    AddThread { review: String, thread: NewThread },
    /// `addPullRequestReviewThreadReply` with `pullRequestReviewId: review` and
    /// `pullRequestReviewThreadId: thread`.
    AddReply {
        review: String,
        thread: String,
        body: String,
    },
    /// `addPullRequestReview` with `commitOID: commit`, `event: COMMENT` and the thread in
    /// `threads`: one comment, posted at once.
    PostThread { commit: String, thread: NewThread },
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
    /// Your pending review is on another revision, `review_on`, and its threads go on that
    /// revision's lines. Submit it first, or comment on its revision.
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
    /// another, and the dialog says which.
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

    /// What `c` sends: `comment`, written while the right column shows revision `on`, added to
    /// your pending review, or starting one on `on`.
    pub fn add(&self, on: &str, comment: &NewComment) -> Result<Mutation, Refusal> {
        if comment.body().trim().is_empty() {
            return Err(Refusal::Blank);
        }
        let Some(pending) = &self.pending else {
            return Ok(Mutation::StartReview {
                commit: on.to_string(),
                first: comment.clone(),
            });
        };
        let review = pending.id.clone();
        match comment {
            NewComment::Thread(_) if pending.commit != on => Err(Refusal::OtherRevision {
                review_on: pending.commit.clone(),
            }),
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
    /// once, or added to your pending review if you have one.
    pub fn post(&self, on: &str, comment: &NewComment) -> Result<Posting, Refusal> {
        if self.pending.is_some() {
            return self.add(on, comment).map(Posting::Pending);
        }
        if comment.body().trim().is_empty() {
            return Err(Refusal::Blank);
        }
        Ok(Posting::Now(match comment {
            NewComment::Thread(thread) => Mutation::PostThread {
                commit: on.to_string(),
                thread: thread.clone(),
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
    pub fn accepted(&mut self, mutation: &Mutation, review_id: Option<&str>) {
        match mutation {
            Mutation::StartReview { commit, .. } => {
                if let Some(id) = review_id {
                    self.pending = Some(PendingReview {
                        id: id.to_string(),
                        commit: commit.clone(),
                        comments: 1,
                    });
                }
            }
            Mutation::AddThread { .. } | Mutation::AddReply { .. } => {
                if let Some(pending) = &mut self.pending {
                    pending.comments += 1;
                }
            }
            Mutation::PostThread { .. } | Mutation::PostReply { .. } => {}
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
    /// of revision `on` when you have no pending review. A blank summary is sent empty.
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

    fn thread(body: &str) -> NewComment {
        NewComment::Thread(NewThread {
            path: "src/parse.rs".to_string(),
            lines: Some(Lines::one(Line::right(12))),
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
            YourReview::default().add("a3", &thread("Off by one?")),
            Ok(Mutation::StartReview {
                commit: "a3".to_string(),
                first: thread("Off by one?"),
            })
        );
    }

    #[test]
    fn later_comments_join_the_pending_review_and_count() {
        let mut review = YourReview::default();
        let start = review.add("a3", &thread("Off by one?")).unwrap();
        review.accepted(&start, Some("PRR_1"));

        assert_eq!(
            review.pending(),
            Some(&PendingReview {
                id: "PRR_1".to_string(),
                commit: "a3".to_string(),
                comments: 1,
            })
        );

        let add = review.add("a3", &thread("And here.")).unwrap();
        assert_eq!(
            add,
            Mutation::AddThread {
                review: "PRR_1".to_string(),
                thread: new_thread(thread("And here.")),
            }
        );
        review.accepted(&add, None);

        let answer = review.add("a3", &reply("Agreed.")).unwrap();
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
    fn a_reply_as_the_first_comment_starts_the_review_too() {
        assert_eq!(
            YourReview::default().add("a3", &reply("Agreed.")),
            Ok(Mutation::StartReview {
                commit: "a3".to_string(),
                first: reply("Agreed."),
            })
        );
    }

    #[test]
    fn a_thread_on_another_revision_does_not_join_the_pending_review() {
        assert_eq!(
            pending("a2", 1).add("a3", &thread("Off by one?")),
            Err(Refusal::OtherRevision {
                review_on: "a2".to_string(),
            })
        );
    }

    #[test]
    fn a_reply_joins_the_pending_review_from_any_revision() {
        assert_eq!(
            pending("a2", 1).add("a3", &reply("Agreed.")),
            Ok(Mutation::AddReply {
                review: "PRR_1".to_string(),
                thread: "PRRT_9".to_string(),
                body: "Agreed.".to_string(),
            })
        );
    }

    #[test]
    fn a_blank_comment_is_refused() {
        assert_eq!(
            YourReview::default().add("a3", &thread(" \n\t")),
            Err(Refusal::Blank)
        );
        assert_eq!(pending("a3", 1).add("a3", &reply("")), Err(Refusal::Blank));
        assert_eq!(
            YourReview::default().post("a3", &thread("")),
            Err(Refusal::Blank)
        );
    }

    #[test]
    fn c_posts_one_comment_at_once_without_a_pending_review() {
        let review = YourReview::default();

        assert_eq!(
            review.post("a3", &thread("Off by one?")),
            Ok(Posting::Now(Mutation::PostThread {
                commit: "a3".to_string(),
                thread: new_thread(thread("Off by one?")),
            }))
        );
        assert_eq!(
            review.post("a3", &reply("Agreed.")),
            Ok(Posting::Now(Mutation::PostReply {
                thread: "PRRT_9".to_string(),
                body: "Agreed.".to_string(),
            }))
        );
    }

    #[test]
    fn a_comment_posted_at_once_leaves_no_pending_review() {
        let mut review = YourReview::default();
        let Ok(Posting::Now(post)) = review.post("a3", &thread("Off by one?")) else {
            unreachable!()
        };
        review.accepted(&post, None);

        assert_eq!(review.pending(), None);
        assert_eq!(review.pending_comments(), 0);
    }

    #[test]
    fn c_adds_to_the_pending_review_when_there_is_one() {
        assert_eq!(
            pending("a3", 2).post("a3", &thread("Off by one?")),
            Ok(Posting::Pending(Mutation::AddThread {
                review: "PRR_1".to_string(),
                thread: new_thread(thread("Off by one?")),
            }))
        );
        assert_eq!(
            pending("a2", 2).post("a3", &thread("Off by one?")),
            Err(Refusal::OtherRevision {
                review_on: "a2".to_string(),
            })
        );
    }

    #[test]
    fn submitting_sends_the_pending_review_and_ends_it() {
        let mut review = pending("a2", 2);
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
