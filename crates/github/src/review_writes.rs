//! What Lo writes to a pull request's review (design §8): the comments and reviews
//! `review::pending` plans as a [`Mutation`], resolving and unresolving threads, editing and
//! deleting comments, and commenting on the pull request itself. [`crate::poll::Poller`] runs them
//! in the account's queue and reads back what they change.
//!
//! GitHub refuses some of these in ways its API leaves undocumented. These were checked live, as
//! `rozed-bot` on its own pull requests (the `review_writes_*` recordings in `tests/fixtures`):
//!
//! - `addPullRequestReviewThread` refuses a line outside the diff, or a file the pull request
//!   doesn't change, by answering `thread: null` with no error. Lo takes that as
//!   [`Rejection::NotInDiff`], never as an acceptance. `addPullRequestReview` refuses such a
//!   line with "Line could not be resolved", and REST with "could not be resolved".
//! - While you have a pending review, GitHub refuses another, even one posted at once: "User can
//!   only have one pending review per pull request" ([`Rejection::PendingReviewExists`]).
//! - A pull request's author can only comment: "Review Can not approve your own pull request"
//!   ([`Rejection::OwnPullRequest`]).
//! - A COMMENT review with neither a body nor a comment is refused with an empty message
//!   ([`Rejection::NothingToSubmit`]).
//! - A blank edit is refused: "Body can't be blank". A comment deleted already is "not found".
//! - A review without a summary goes once its last comment is deleted, from the timeline too.
//!
//! Every refusal is a [`GithubError::Rejected`], whose text the UI can show as it is.

use review::pending::{Mutation, NewThread, ReviewEvent};
use review::threads::{self, Lines};
use serde::{Deserialize, Serialize};

use crate::client::GithubClient;
use crate::error::GithubError;
use crate::pulls::PullRef;
use crate::queries::{
    AddComment, AddPullRequestReview, AddPullRequestReviewThread, AddPullRequestReviewThreadReply,
    DeleteIssueComment, DeletePullRequestReviewComment, ResolveReviewThread,
    SubmitPullRequestReview, UnresolveReviewThread, UpdateIssueComment,
    UpdatePullRequestReviewComment, add_comment, add_pull_request_review,
    add_pull_request_review_thread, add_pull_request_review_thread_reply, delete_issue_comment,
    delete_pull_request_review_comment, resolve_review_thread, submit_pull_request_review,
    unresolve_review_thread, update_issue_comment, update_pull_request_review_comment,
};

/// Why GitHub refused a comment, a review, or an edit of one. Its text is for the person who wrote
/// it, as in "Couldn't post the comment: …".
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Rejection {
    /// The lines aren't in the pull request's diff, or the file isn't one it changes. GitHub's
    /// web pages take a comment outside the diff, but its API doesn't, so the UI offers a comment
    /// on the file instead.
    #[error("GitHub takes comments only on lines and files in the pull request's diff")]
    NotInDiff,
    /// You have a pending review already, perhaps started elsewhere, and GitHub allows one per
    /// person per pull request. It also refuses a comment or review posted at once while you have
    /// one.
    #[error("You have a pending review on this pull request already, and GitHub allows only one")]
    PendingReviewExists,
    /// You opened the pull request, and GitHub lets its author only comment.
    #[error("GitHub doesn't let a pull request's author approve it or request changes")]
    OwnPullRequest,
    /// A comment, or a request for changes, with neither a summary nor a comment.
    #[error("A review needs a summary or a comment")]
    NothingToSubmit,
    /// GitHub answered nothing, or refused with an empty message where that means nothing else.
    #[error("GitHub refused it without saying why")]
    Unexplained,
    /// Any other refusal, in GitHub's words, such as a blank comment's.
    #[error("GitHub refused it: {0}")]
    Other(String),
}

impl Rejection {
    /// Sorts GitHub's message. `empty` is what an empty one means for the write that got it.
    fn from_message(message: &str, empty: &Self) -> Self {
        let message = message.trim();
        let lower = message.to_ascii_lowercase();
        if message.is_empty() {
            empty.clone()
        } else if lower.contains("could not be resolved") {
            Self::NotInDiff
        } else if lower.contains("one pending review") {
            Self::PendingReviewExists
        } else if lower.contains("your own pull request") {
            Self::OwnPullRequest
        } else {
            Self::Other(message.to_string())
        }
    }

    /// The most telling of several messages: the first Lo can say more about than GitHub's words,
    /// or else all of them.
    fn from_messages(messages: &[String], empty: &Self) -> Self {
        let sorted: Vec<Self> = (messages.iter())
            .map(|message| Self::from_message(message, empty))
            .collect();
        if let Some(known) = (sorted.iter()).find(|rejection| !matches!(rejection, Self::Other(_)))
        {
            return known.clone();
        }
        let words: Vec<&str> = (messages.iter())
            .map(|message| message.trim())
            .filter(|message| !message.is_empty())
            .collect();
        if words.is_empty() {
            empty.clone()
        } else {
            Self::Other(words.join("; "))
        }
    }
}

/// `error`, with GitHub's refusals as [`GithubError::Rejected`]: GraphQL errors of type
/// `UNPROCESSABLE`, and REST's `422`, whose `errors` hold strings or objects with a `message`.
/// `empty` is what an empty message means. Any other error is returned as it was.
pub(crate) fn rejecting(error: GithubError, empty: Rejection) -> GithubError {
    match error {
        GithubError::GraphQl { errors }
            if (errors.iter()).any(|error| error.r#type.as_deref() == Some("UNPROCESSABLE")) =>
        {
            let messages: Vec<String> = (errors.iter())
                .filter(|error| error.r#type.as_deref() == Some("UNPROCESSABLE"))
                .map(|error| error.message.clone())
                .collect();
            GithubError::Rejected(Rejection::from_messages(&messages, &empty))
        }
        GithubError::Api {
            status: 422,
            message,
            errors,
            ..
        } => {
            let messages: Vec<String> = errors.iter().map(validation_message).collect();
            let rejection = if messages.is_empty() {
                Rejection::from_message(&message, &empty)
            } else {
                Rejection::from_messages(&messages, &empty)
            };
            GithubError::Rejected(rejection)
        }
        other => other,
    }
}

/// One of a REST `422`'s `errors`: a string, or an object with a `message`, or with a `field` and
/// a `code` such as `missing_field`.
fn validation_message(error: &serde_json::Value) -> String {
    match error {
        serde_json::Value::String(message) => message.clone(),
        serde_json::Value::Object(object) => {
            let text = |key: &str| object.get(key).and_then(|value| value.as_str());
            match (text("message"), text("field"), text("code")) {
                (Some(message), _, _) => message.to_string(),
                (None, Some(field), Some(code)) => format!("{field}: {code}"),
                _ => String::new(),
            }
        }
        _ => String::new(),
    }
}

/// What GitHub made of an accepted write, by node ID: what the UI needs to follow it up, such as
/// the review id [`review::pending::YourReview::accepted`] takes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sent {
    /// The review the write started, added to, posted or submitted. A comment posted at once is a
    /// review of its own. REST's file comment doesn't name its review's node ID, so it has none.
    pub review: Option<String>,
    /// The thread it started, or replied to.
    pub thread: Option<String>,
    /// The comment it posted, or added to your pending review.
    pub comment: Option<String>,
}

/// A comment Lo can edit or delete, by node ID.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum CommentId {
    /// A comment in a review thread (`PullRequestReviewComment`, `PRRC_…`), pending or posted.
    Thread(String),
    /// A comment on the pull request itself, in its conversation (`IssueComment`, `IC_…`).
    Conversation(String),
}

/// REST's `POST /pulls/{number}/comments` for a file-level comment.
#[derive(Serialize)]
struct FileComment<'a> {
    commit_id: &'a str,
    path: &'a str,
    body: &'a str,
    subject_type: &'static str,
}

/// What Lo reads of REST's answer to a review comment.
#[derive(Deserialize)]
struct PostedComment {
    node_id: String,
}

fn rejected(rejection: Rejection) -> GithubError {
    GithubError::Rejected(rejection)
}

impl GithubClient {
    /// Sends `mutation`, planned by [`review::pending::YourReview`] for `pull`, whose node ID is
    /// `pull_request_id`, as the mutation's docs name. On success, [`Sent`] names what it made; a
    /// refusal is [`GithubError::Rejected`], including a thread GitHub answered with `null`.
    pub async fn send_review_mutation(
        &self,
        pull: &PullRef,
        pull_request_id: &str,
        mutation: &Mutation,
    ) -> Result<Sent, GithubError> {
        match mutation {
            Mutation::StartReview { commit, thread } => {
                self.add_review(pull_request_id, commit, None, None, thread.as_ref())
                    .await
            }
            Mutation::AddThread { review, thread } => self.add_thread(review, thread).await,
            Mutation::AddReply {
                review,
                thread,
                body,
            } => self.reply(thread, body, Some(review)).await,
            Mutation::PostThread { commit, thread } => {
                let event = Some(ReviewEvent::Comment);
                (self.add_review(pull_request_id, commit, event, None, Some(thread))).await
            }
            Mutation::PostFileComment { commit, path, body } => {
                self.post_file_comment(pull, commit, path, body).await
            }
            Mutation::PostReply { thread, body } => self.reply(thread, body, None).await,
            Mutation::SubmitReview {
                review,
                event,
                body,
            } => self.submit_review(review, *event, body).await,
            Mutation::Review {
                commit,
                event,
                body,
            } => {
                let body = (!body.is_empty()).then_some(body.as_str());
                (self.add_review(pull_request_id, commit, Some(*event), body, None)).await
            }
        }
    }

    /// `addPullRequestReview`: starts a pending review on `commit` without `event`, or posts one
    /// at once with it, holding `thread` if any. A thread sent that comes back without its comment
    /// is a refusal, though GitHub may have started the review.
    async fn add_review(
        &self,
        pull_request_id: &str,
        commit: &str,
        event: Option<ReviewEvent>,
        body: Option<&str>,
        thread: Option<&NewThread>,
    ) -> Result<Sent, GithubError> {
        use add_pull_request_review as q;
        let empty = match event {
            Some(_) => Rejection::NothingToSubmit,
            None => Rejection::Unexplained,
        };
        let variables = q::Variables {
            pull_request_id: pull_request_id.to_string(),
            commit_oid: commit.to_string(),
            event: event.map(|event| match event {
                ReviewEvent::Comment => q::PullRequestReviewEvent::COMMENT,
                ReviewEvent::Approve => q::PullRequestReviewEvent::APPROVE,
                ReviewEvent::RequestChanges => q::PullRequestReviewEvent::REQUEST_CHANGES,
            }),
            body: body.map(str::to_string),
            threads: thread.map(|thread| {
                let (line, side, start_line, start_side) =
                    lines(thread.lines.as_ref(), |side| match side {
                        threads::DiffSide::Left => q::DiffSide::LEFT,
                        threads::DiffSide::Right => q::DiffSide::RIGHT,
                    });
                vec![q::DraftPullRequestReviewThread {
                    body: thread.body.clone(),
                    path: Some(thread.path.clone()),
                    line,
                    side,
                    start_line,
                    start_side,
                }]
            }),
        };
        let data = (self.graphql::<AddPullRequestReview>(variables).await)
            .map_err(|error| rejecting(error, empty))?;
        let review = (data.add_pull_request_review)
            .and_then(|payload| payload.pull_request_review)
            .ok_or_else(|| rejected(Rejection::Unexplained))?;
        let comment = (review.comments.nodes.into_iter().flatten().flatten())
            .next()
            .map(|comment| comment.id);
        if thread.is_some() && comment.is_none() {
            log::warn!("GitHub started review {} without its thread", review.id);
            return Err(rejected(Rejection::NotInDiff));
        }
        Ok(Sent {
            review: Some(review.id),
            thread: None,
            comment,
        })
    }

    /// `addPullRequestReviewThread` to the pending review `review`, on lines or on the file.
    async fn add_thread(&self, review: &str, thread: &NewThread) -> Result<Sent, GithubError> {
        use add_pull_request_review_thread as q;
        let (line, side, start_line, start_side) =
            lines(thread.lines.as_ref(), |side| match side {
                threads::DiffSide::Left => q::DiffSide::LEFT,
                threads::DiffSide::Right => q::DiffSide::RIGHT,
            });
        let variables = q::Variables {
            pull_request_review_id: review.to_string(),
            path: thread.path.clone(),
            body: thread.body.clone(),
            subject_type: match thread.lines {
                Some(_) => q::PullRequestReviewThreadSubjectType::LINE,
                None => q::PullRequestReviewThreadSubjectType::FILE,
            },
            line,
            side,
            start_line,
            start_side,
        };
        let data = (self.graphql::<AddPullRequestReviewThread>(variables).await)
            .map_err(|error| rejecting(error, Rejection::Unexplained))?;
        // GitHub's refusal of a line outside the diff: no thread, and no error.
        let thread = (data.add_pull_request_review_thread)
            .and_then(|payload| payload.thread)
            .ok_or_else(|| rejected(Rejection::NotInDiff))?;
        let comment = (thread.comments.nodes.into_iter().flatten().flatten()).next();
        Ok(Sent {
            review: (comment.as_ref())
                .and_then(|comment| comment.pull_request_review.as_ref())
                .map(|review| review.id.clone()),
            thread: Some(thread.id),
            comment: comment.map(|comment| comment.id),
        })
    }

    /// `addPullRequestReviewThreadReply` to `thread`: in the pending review `review`, or without
    /// one, posted at once.
    async fn reply(
        &self,
        thread: &str,
        body: &str,
        review: Option<&str>,
    ) -> Result<Sent, GithubError> {
        use add_pull_request_review_thread_reply as q;
        let variables = q::Variables {
            pull_request_review_thread_id: thread.to_string(),
            body: body.to_string(),
            pull_request_review_id: review.map(str::to_string),
        };
        let data = (self
            .graphql::<AddPullRequestReviewThreadReply>(variables)
            .await)
            .map_err(|error| rejecting(error, Rejection::Unexplained))?;
        let comment = (data.add_pull_request_review_thread_reply)
            .and_then(|payload| payload.comment)
            .ok_or_else(|| rejected(Rejection::Unexplained))?;
        Ok(Sent {
            review: comment.pull_request_review.map(|review| review.id),
            thread: Some(thread.to_string()),
            comment: Some(comment.id),
        })
    }

    /// REST's file-level comment on `path`, posted at once on `commit`.
    async fn post_file_comment(
        &self,
        pull: &PullRef,
        commit: &str,
        path: &str,
        body: &str,
    ) -> Result<Sent, GithubError> {
        let (owner, name, number) = (&pull.repo.owner, &pull.repo.name, pull.number);
        let request = FileComment {
            commit_id: commit,
            path,
            body,
            subject_type: "file",
        };
        let posted: PostedComment = (self.post(
            &format!("/repos/{owner}/{name}/pulls/{number}/comments"),
            &request,
        ))
        .await
        .map_err(|error| rejecting(error, Rejection::Unexplained))?;
        Ok(Sent {
            review: None,
            thread: None,
            comment: Some(posted.node_id),
        })
    }

    /// `submitPullRequestReview`: submits the pending review `review` with `event` and the summary
    /// `body`, which may be empty.
    async fn submit_review(
        &self,
        review: &str,
        event: ReviewEvent,
        body: &str,
    ) -> Result<Sent, GithubError> {
        use submit_pull_request_review as q;
        let variables = q::Variables {
            pull_request_review_id: review.to_string(),
            event: match event {
                ReviewEvent::Comment => q::PullRequestReviewEvent::COMMENT,
                ReviewEvent::Approve => q::PullRequestReviewEvent::APPROVE,
                ReviewEvent::RequestChanges => q::PullRequestReviewEvent::REQUEST_CHANGES,
            },
            body: Some(body.to_string()),
        };
        let data = (self.graphql::<SubmitPullRequestReview>(variables).await)
            .map_err(|error| rejecting(error, Rejection::NothingToSubmit))?;
        let review = (data.submit_pull_request_review)
            .and_then(|payload| payload.pull_request_review)
            .ok_or_else(|| rejected(Rejection::Unexplained))?;
        Ok(Sent {
            review: Some(review.id),
            ..Sent::default()
        })
    }

    /// Resolves the review thread `thread`, or with `resolved: false` unresolves it.
    pub async fn resolve_thread(&self, thread: &str, resolved: bool) -> Result<(), GithubError> {
        let reject = |error| rejecting(error, Rejection::Unexplained);
        let answered = if resolved {
            let variables = resolve_review_thread::Variables {
                thread_id: thread.to_string(),
            };
            let data = (self.graphql::<ResolveReviewThread>(variables).await).map_err(reject)?;
            (data.resolve_review_thread)
                .and_then(|payload| payload.thread)
                .map(|thread| thread.is_resolved)
        } else {
            let variables = unresolve_review_thread::Variables {
                thread_id: thread.to_string(),
            };
            let data = (self.graphql::<UnresolveReviewThread>(variables).await).map_err(reject)?;
            (data.unresolve_review_thread)
                .and_then(|payload| payload.thread)
                .map(|thread| thread.is_resolved)
        };
        match answered {
            Some(now) if now == resolved => Ok(()),
            _ => Err(rejected(Rejection::Unexplained)),
        }
    }

    /// Replaces `comment`'s text with `body`.
    pub async fn edit_comment(&self, comment: &CommentId, body: &str) -> Result<(), GithubError> {
        let reject = |error| rejecting(error, Rejection::Unexplained);
        let edited = match comment {
            CommentId::Thread(id) => {
                let variables = update_pull_request_review_comment::Variables {
                    id: id.clone(),
                    body: body.to_string(),
                };
                let data = (self
                    .graphql::<UpdatePullRequestReviewComment>(variables)
                    .await)
                    .map_err(reject)?;
                (data.update_pull_request_review_comment)
                    .and_then(|payload| payload.pull_request_review_comment)
                    .is_some()
            }
            CommentId::Conversation(id) => {
                let variables = update_issue_comment::Variables {
                    id: id.clone(),
                    body: body.to_string(),
                };
                let data = (self.graphql::<UpdateIssueComment>(variables).await).map_err(reject)?;
                (data.update_issue_comment)
                    .and_then(|payload| payload.issue_comment)
                    .is_some()
            }
        };
        if edited {
            Ok(())
        } else {
            Err(rejected(Rejection::Unexplained))
        }
    }

    /// Deletes `comment`. A thread whose last comment is deleted is gone.
    pub async fn delete_comment(&self, comment: &CommentId) -> Result<(), GithubError> {
        let reject = |error| rejecting(error, Rejection::Unexplained);
        let deleted = match comment {
            CommentId::Thread(id) => {
                let variables = delete_pull_request_review_comment::Variables { id: id.clone() };
                let data = (self
                    .graphql::<DeletePullRequestReviewComment>(variables)
                    .await)
                    .map_err(reject)?;
                data.delete_pull_request_review_comment.is_some()
            }
            CommentId::Conversation(id) => {
                let variables = delete_issue_comment::Variables { id: id.clone() };
                let data = (self.graphql::<DeleteIssueComment>(variables).await).map_err(reject)?;
                data.delete_issue_comment.is_some()
            }
        };
        if deleted {
            Ok(())
        } else {
            Err(rejected(Rejection::Unexplained))
        }
    }

    /// Comments `body` on the pull request whose node ID is `pull_request_id`, in its
    /// conversation. Returns the comment's node ID.
    pub async fn add_conversation_comment(
        &self,
        pull_request_id: &str,
        body: &str,
    ) -> Result<String, GithubError> {
        let variables = add_comment::Variables {
            subject_id: pull_request_id.to_string(),
            body: body.to_string(),
        };
        let data = (self.graphql::<AddComment>(variables).await)
            .map_err(|error| rejecting(error, Rejection::Unexplained))?;
        (data.add_comment)
            .and_then(|payload| payload.comment_edge)
            .and_then(|edge| edge.node)
            .map(|comment| comment.id)
            .ok_or_else(|| rejected(Rejection::Unexplained))
    }
}

/// `line`, `side`, `startLine` and `startSide` for `lines`, as GitHub's inputs take them, or none
/// for a file-level comment. The start's side is always sent with its line: GitHub's default is
/// `RIGHT`, which would turn a range from the base into one from the head.
fn lines<S>(
    lines: Option<&Lines>,
    side: impl Fn(threads::DiffSide) -> S,
) -> (Option<i64>, Option<S>, Option<i64>, Option<S>) {
    let Some(lines) = lines else {
        return (None, None, None, None);
    };
    (
        Some(i64::from(lines.end.number)),
        Some(side(lines.end.side)),
        lines.start.map(|start| i64::from(start.number)),
        lines.start.map(|start| side(start.side)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::graphql::GraphQlError;

    fn unprocessable(message: &str) -> GithubError {
        GithubError::GraphQl {
            errors: vec![GraphQlError {
                message: message.into(),
                r#type: Some("UNPROCESSABLE".into()),
                path: vec!["addPullRequestReview".into()],
                extensions: None,
            }],
        }
    }

    fn validation(message: &str, errors: serde_json::Value) -> GithubError {
        GithubError::Api {
            status: 422,
            message: message.into(),
            documentation_url: None,
            errors: errors.as_array().unwrap().clone(),
        }
    }

    fn rejection(error: GithubError, empty: Rejection) -> Rejection {
        match rejecting(error, empty) {
            GithubError::Rejected(rejection) => rejection,
            other => panic!("not a rejection: {other:?}"),
        }
    }

    // The messages are GitHub's, from the review API checks and the recordings.

    #[test]
    fn graphqls_refusals_are_explained() {
        let none = || Rejection::Unexplained;
        assert_eq!(
            rejection(unprocessable("Line could not be resolved"), none()),
            Rejection::NotInDiff
        );
        assert_eq!(
            rejection(
                unprocessable("User can only have one pending review per pull request"),
                none()
            ),
            Rejection::PendingReviewExists
        );
        for message in [
            "Review Can not approve your own pull request",
            "Review Can not request changes on your own pull request",
        ] {
            assert_eq!(
                rejection(unprocessable(message), none()),
                Rejection::OwnPullRequest
            );
        }
        // A COMMENT with neither a body nor a thread: an empty message.
        assert_eq!(
            rejection(unprocessable(""), Rejection::NothingToSubmit),
            Rejection::NothingToSubmit
        );
        assert_eq!(rejection(unprocessable(""), none()), Rejection::Unexplained);
        assert_eq!(
            rejection(unprocessable("Body can't be blank"), none()),
            Rejection::Other("Body can't be blank".into())
        );
    }

    #[test]
    fn rests_validation_failures_are_explained() {
        let none = || Rejection::Unexplained;
        let line = serde_json::json!([{
            "resource": "PullRequestReviewComment", "code": "custom",
            "field": "pull_request_review_thread.line", "message": "could not be resolved"
        }]);
        assert_eq!(
            rejection(validation("Validation Failed", line), none()),
            Rejection::NotInDiff
        );
        let pending = serde_json::json!([{
            "resource": "PullRequestReview", "code": "custom", "field": "user_id",
            "message": "user_id can only have one pending review per pull request"
        }]);
        assert_eq!(
            rejection(validation("Validation Failed", pending), none()),
            Rejection::PendingReviewExists
        );
        let own = serde_json::json!(["Review Can not approve your own pull request"]);
        assert_eq!(
            rejection(validation("Unprocessable Entity", own), none()),
            Rejection::OwnPullRequest
        );
        let empty = serde_json::json!([""]);
        assert_eq!(
            rejection(
                validation("Unprocessable Entity", empty),
                Rejection::NothingToSubmit
            ),
            Rejection::NothingToSubmit
        );
        let missing = serde_json::json!([{
            "resource": "IssueComment", "code": "missing_field", "field": "body"
        }]);
        assert_eq!(
            rejection(validation("Validation Failed", missing), none()),
            Rejection::Other("body: missing_field".into())
        );
        assert_eq!(
            rejection(
                validation("Validation Failed", serde_json::json!([])),
                none()
            ),
            Rejection::Other("Validation Failed".into())
        );
    }

    #[test]
    fn other_errors_are_left_as_they_were() {
        let forbidden = GithubError::GraphQl {
            errors: vec![GraphQlError {
                message: "Resource not accessible by integration".into(),
                r#type: Some("FORBIDDEN".into()),
                path: vec![],
                extensions: None,
            }],
        };
        assert!(matches!(
            rejecting(forbidden, Rejection::Unexplained),
            GithubError::GraphQl { .. }
        ));
        let server = GithubError::Api {
            status: 502,
            message: "Bad Gateway".into(),
            documentation_url: None,
            errors: vec![],
        };
        assert!(matches!(
            rejecting(server, Rejection::Unexplained),
            GithubError::Api { status: 502, .. }
        ));
    }

    #[test]
    fn a_rejection_reads_as_a_sentence_the_ui_can_show() {
        assert_eq!(
            GithubError::Rejected(Rejection::OwnPullRequest).to_string(),
            "GitHub doesn't let a pull request's author approve it or request changes"
        );
        assert_eq!(
            Rejection::Other("Body can't be blank".into()).to_string(),
            "GitHub refused it: Body can't be blank"
        );
    }

    #[test]
    fn a_range_from_the_base_sends_its_starts_side() {
        let range = Lines::range(threads::Line::left(10), threads::Line::right(11));
        assert_eq!(
            lines(Some(&range), threads::DiffSide::as_str),
            (Some(11), Some("RIGHT"), Some(10), Some("LEFT"))
        );
        let one = Lines::one(threads::Line::right(12));
        assert_eq!(
            lines(Some(&one), threads::DiffSide::as_str),
            (Some(12), Some("RIGHT"), None, None)
        );
        assert_eq!(
            lines(None, threads::DiffSide::as_str),
            (None, None, None, None)
        );
    }
}
