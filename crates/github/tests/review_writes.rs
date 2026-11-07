//! Comments, replies, reviews and their edits, replayed from what `rozed-bot` wrote on its own
//! pull request, E11 (`rozed-dev/e2e#23`, `script/e2e/README.md`), through the scenarios that
//! recorded them (`scenarios/review_writes.rs`). The replaying client answers a request only when
//! it's the one recorded, variables and all, so each test also checks what Lo sends.

#[path = "scenarios/review_writes.rs"]
mod scenarios;

use std::sync::Arc;
use std::time::Duration;

use futures::executor::block_on;
use github::queries::PullRequestCheckData;
use github::replay::ReplayHttpClient;
use github::review_writes::Rejection;
use github::{GithubClient, GithubError, Token};
use review::threads::{CommentState, Line, Lines, Subject};

use scenarios::{BOT, NOTES, REVIEWED};

fn client(cassette: &str) -> (GithubClient, Arc<ReplayHttpClient>) {
    let http = ReplayHttpClient::cassette(cassette);
    (
        GithubClient::new(http.clone(), Token::new("gho_FAKE")),
        http,
    )
}

fn no_pause(_: Duration) {}

/// What each request was: a GraphQL operation's name, or a REST method and path.
fn requests(http: &ReplayHttpClient) -> Vec<String> {
    (http.seen().iter())
        .map(|seen| {
            match serde_json::from_slice::<serde_json::Value>(&seen.body) {
                Ok(body) if body["operationName"].is_string() => {
                    return body["operationName"].as_str().unwrap().to_string();
                }
                _ => {}
            }
            let path = seen.url.trim_start_matches("https://api.github.com");
            format!("{} {path}", seen.method)
        })
        .collect()
}

/// The variables of each GraphQL request named `operation`, in order.
fn variables(http: &ReplayHttpClient, operation: &str) -> Vec<serde_json::Value> {
    (http.seen().iter())
        .filter_map(|seen| serde_json::from_slice::<serde_json::Value>(&seen.body).ok())
        .filter(|body| body["operationName"] == operation)
        .map(|body| body["variables"].clone())
        .collect()
}

fn rejection<T: std::fmt::Debug>(result: &Result<T, GithubError>) -> &Rejection {
    match result {
        Err(GithubError::Rejected(rejection)) => rejection,
        other => panic!("not refused: {other:?}"),
    }
}

/// The check's field at `pointer`, as GraphQL names it.
fn field(check: &PullRequestCheckData, pointer: &str) -> serde_json::Value {
    let check = serde_json::to_value(check).unwrap();
    check.pointer(pointer).unwrap().clone()
}

#[test]
fn a_pending_review_starts_with_a_line_thread_and_gathers_threads_and_replies() {
    let (client, http) = client("review_writes_e2e_23_pending");
    let seen = block_on(scenarios::pending(&client, &no_pause));

    assert_eq!(
        requests(&http),
        [
            "GET /repos/rozed-dev/e2e/pulls/23",
            "PendingReview",
            "AddPullRequestReview",
            "PendingReview",
            "AddPullRequestReviewThread",
            "AddPullRequestReviewThread",
            "AddPullRequestReviewThread",
            "AddPullRequestReviewThread",
            "ReviewThreads",
            "AddPullRequestReviewThreadReply",
            "PendingReview",
            "PullRequestCheck",
            "AddPullRequestReview",
            "POST /repos/rozed-dev/e2e/pulls/23/comments",
            "SubmitPullRequestReview",
            "PendingReview",
        ]
    );
    assert!(seen.id.starts_with("PR_"), "{}", seen.id);
    assert_eq!(seen.before.review, None);
    let review = seen.started.review.clone().unwrap();
    assert!(review.starts_with("PRR_"), "{review}");
    assert!(seen.started.comment.is_some());

    // `reviews(states: PENDING, author: $login)` finds your pending review, on the commit it was
    // started on.
    let after_start = seen.after_start.pending_review().unwrap();
    assert_eq!(after_start.id, review);
    assert_eq!(after_start.commit.as_deref(), Some(seen.head.as_str()));
    assert_eq!(after_start.comments, 1);

    // Each thread added to it names it, and the thread GitHub made.
    for sent in [&seen.range, &seen.file, &seen.reply] {
        assert_eq!(sent.review.as_deref(), Some(review.as_str()), "{sent:?}");
        assert!(sent.thread.is_some() && sent.comment.is_some(), "{sent:?}");
    }
    // A range from the base starts on the base's side; GitHub's default would be the head's.
    // A file-level thread has no lines.
    let added = variables(&http, "AddPullRequestReviewThread");
    assert_eq!(added[0]["startSide"], "LEFT");
    assert_eq!(added[0]["startLine"], 10);
    assert_eq!(added[1]["subjectType"], "FILE");
    assert!(added[1].get("line").is_none(), "{}", added[1]);
    // GitHub answers `thread: null`, with no error, for a line outside the diff and for a file
    // the pull request doesn't change.
    assert_eq!(rejection(&seen.outside), &Rejection::NotInDiff);
    assert_eq!(rejection(&seen.unchanged), &Rejection::NotInDiff);

    // Your pending threads are among the review threads, for you: pending, and not resolvable.
    let threads = seen.threads.threads();
    assert_eq!(threads.len(), 3);
    for thread in &threads {
        assert_eq!(thread.comments.len(), 1);
        assert_eq!(thread.comments[0].state, CommentState::Pending);
        assert_eq!(thread.comments[0].author.as_deref(), Some(BOT));
        assert!(thread.viewer_can_reply && !thread.viewer_can_resolve);
        assert_eq!(thread.current_on.as_deref(), Some(seen.head.as_str()));
    }
    let on_lines =
        |lines| (threads.iter()).any(|t| t.path == REVIEWED && t.current_lines() == Some(lines));
    assert!(on_lines(Lines::one(Line::right(10))));
    assert!(on_lines(Lines::range(Line::left(10), Line::right(11))));
    let file = threads.iter().find(|thread| thread.path == NOTES).unwrap();
    assert_eq!(file.subject, Subject::File);
    assert_eq!(file.current_lines(), None);

    // Four comments, which the check counts too, without asking for yours by login.
    assert_eq!(seen.before_submit.pending_review().unwrap().comments, 4);
    assert_eq!(
        field(&seen.check, "/viewerPendingReview/nodes/0"),
        serde_json::json!({"viewerDidAuthor": true, "comments": {"totalCount": 4}})
    );

    // While it's pending, GitHub refuses a comment posted at once, through GraphQL or REST.
    assert_eq!(
        rejection(&seen.post_while_pending),
        &Rejection::PendingReviewExists
    );
    assert_eq!(
        rejection(&seen.file_comment_while_pending),
        &Rejection::PendingReviewExists
    );

    assert_eq!(seen.submitted.review.as_deref(), Some(review.as_str()));
    assert_eq!(seen.after.review, None);
    assert_eq!(seen.review.pending(), None);
}

#[test]
fn comments_and_reviews_post_at_once_and_its_author_can_only_comment() {
    let (client, http) = client("review_writes_e2e_23_posted");
    let seen = block_on(scenarios::posted(&client, &no_pause));

    assert_eq!(
        requests(&http),
        [
            "GET /repos/rozed-dev/e2e/pulls/23",
            "ReviewThreads",
            "AddPullRequestReviewThreadReply",
            "AddPullRequestReview",
            "POST /repos/rozed-dev/e2e/pulls/23/comments",
            "AddPullRequestReview",
            "AddPullRequestReview",
            "AddPullRequestReview",
            "AddPullRequestReview",
            "AddPullRequestReview",
            "AddPullRequestReviewThreadReply",
            "SubmitPullRequestReview",
            "PendingReview",
        ]
    );
    // A reply posted at once is a review of its own, as is a line comment.
    let posted_reviews = [&seen.reply.review, &seen.line.review, &seen.summary.review];
    for review in posted_reviews {
        assert!(review.as_deref().is_some_and(|id| id.starts_with("PRR_")));
    }
    assert!(seen.reply.comment.is_some() && seen.line.comment.is_some());
    // REST's file comment names its comment, not its review's node ID.
    assert_eq!(seen.file.review, None);
    assert!(seen.file.comment.as_deref().unwrap().starts_with("PRRC_"));
    assert_eq!(
        variables(&http, "AddPullRequestReviewThreadReply")[0]["pullRequestReviewId"],
        serde_json::Value::Null
    );

    assert_eq!(rejection(&seen.approve), &Rejection::OwnPullRequest);
    assert_eq!(rejection(&seen.request_changes), &Rejection::OwnPullRequest);
    // GitHub's message for a comment with nothing in it is empty.
    assert_eq!(rejection(&seen.empty), &Rejection::NothingToSubmit);
    // An approval without a summary sends none.
    let reviews = variables(&http, "AddPullRequestReview");
    assert_eq!(reviews[2]["event"], "APPROVE");
    assert!(reviews[2].get("body").is_none(), "{}", reviews[2]);

    // A reply as the first comment: an empty review, then the reply joins it.
    let started = seen.started_empty.review.clone().unwrap();
    assert_eq!(seen.started_empty.comment, None);
    assert_eq!(seen.joined.review.as_deref(), Some(started.as_str()));
    assert_eq!(
        seen.submitted_empty.review.as_deref(),
        Some(started.as_str())
    );
    assert_eq!(seen.after.review, None);
}

#[test]
fn threads_resolve_and_comments_edit_and_delete_and_the_check_sees_some() {
    let (client, http) = client("review_writes_e2e_23_threads");
    let seen = block_on(scenarios::threads(&client, &no_pause));

    assert_eq!(
        requests(&http),
        [
            "ReviewThreads",
            "PullRequestCheck",
            "ResolveReviewThread",
            "PullRequestCheck",
            "UnresolveReviewThread",
            "PullRequestCheck",
            "UpdatePullRequestReviewComment",
            "PullRequestCheck",
            "UpdatePullRequestReviewComment",
            "DeletePullRequestReviewComment",
            "PullRequestCheck",
            "DeletePullRequestReviewComment",
            "ReviewThreads",
        ]
    );
    seen.resolved.as_ref().unwrap();
    seen.unresolved.as_ref().unwrap();
    seen.edited.as_ref().unwrap();
    seen.deleted.as_ref().unwrap();
    assert_eq!(
        rejection(&seen.blank),
        &Rejection::Other("Body can't be blank".into())
    );
    assert!(
        matches!(&seen.deleted_again, Err(GithubError::NotFound { .. })),
        "{:?}",
        seen.deleted_again
    );

    let [initial, resolved, unresolved, edited, deleted] = &seen.checks;
    let index = (seen.before.nodes.iter())
        .position(|thread| thread.id == seen.thread)
        .unwrap();
    let state = |check| field(check, &format!("/reviewThreads/nodes/{index}"));
    assert_eq!(state(resolved)["isResolved"], true);
    assert_eq!(state(unresolved), state(initial));
    // An edit moves nothing the threads watch, only the pull request's time: Lo reads them again
    // itself.
    assert_eq!(
        field(edited, "/reviewThreads"),
        field(unresolved, "/reviewThreads")
    );
    assert_eq!(
        field(edited, "/timelineEvents"),
        field(unresolved, "/timelineEvents")
    );
    assert_ne!(field(edited, "/updatedAt"), field(unresolved, "/updatedAt"));
    // Resolving doesn't move even that.
    assert_eq!(field(resolved, "/updatedAt"), field(initial, "/updatedAt"));
    // A deleted reply moves its thread's count.
    let count = |check| state(check)["comments"]["totalCount"].as_i64().unwrap();
    assert_eq!(count(deleted), count(edited) - 1);
    let comments = |threads: &github::queries::ReviewThreadList| -> Vec<String> {
        (threads.threads().into_iter())
            .flat_map(|thread| thread.comments)
            .map(|comment| comment.id)
            .collect()
    };
    assert!(comments(&seen.before).contains(&seen.comment));
    assert!(!comments(&seen.after).contains(&seen.comment));
}

#[test]
fn an_edit_of_a_conversation_comment_moves_only_the_pull_requests_time() {
    let (client, http) = client("review_writes_e2e_23_conversation");
    let seen = block_on(scenarios::conversation(&client, &no_pause));

    assert_eq!(
        requests(&http),
        [
            "GET /repos/rozed-dev/e2e/pulls/23",
            "PullRequestCheck",
            "AddComment",
            "AddComment",
            "PullRequestCheck",
            "UpdateIssueComment",
            "PullRequestCheck",
            "DeleteIssueComment",
            "DeleteIssueComment",
            "PullRequestCheck",
        ]
    );
    assert!(seen.first.starts_with("IC_") && seen.second.starts_with("IC_"));
    seen.edited.as_ref().unwrap();
    for deleted in &seen.deleted {
        deleted.as_ref().unwrap();
    }
    let [initial, commented, edited, deleted] = &seen.checks;
    let count = |check| {
        field(check, "/timelineEvents/filteredCount")
            .as_i64()
            .unwrap()
    };
    assert_eq!(count(commented), count(initial) + 2);
    assert_ne!(
        field(commented, "/timelineEvents/updatedAt"),
        field(initial, "/timelineEvents/updatedAt")
    );
    // The timeline's time is its newest item's creation: an edit doesn't move it, so Lo reads
    // the timeline again itself. The pull request's own time moves.
    assert_eq!(
        field(edited, "/timelineEvents"),
        field(commented, "/timelineEvents")
    );
    assert_ne!(field(edited, "/updatedAt"), field(commented, "/updatedAt"));
    // Deleting both takes the count and the time back.
    assert_eq!(
        field(deleted, "/timelineEvents"),
        field(initial, "/timelineEvents")
    );
    assert_eq!(field(deleted, "/comments"), field(initial, "/comments"));
}
