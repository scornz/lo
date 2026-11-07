//! The writes recorded on E11, `rozed-dev/e2e#23` (`script/e2e/README.md`), as `rozed-bot`, its
//! author, which can only comment on it. `record_fixtures` runs each scenario against GitHub,
//! recording it, and `review_writes` replays the recording through the same scenario, so both
//! send the same requests in the same order. Comments are planned by `review::pending`, as the UI
//! plans them.

use std::time::Duration;

use github::queries::{PullRequestCheckData, ReviewThreadList, ViewerPendingReview};
use github::review_writes::{CommentId, Sent};
use github::{GithubClient, GithubError, PullRef};
use review::pending::{
    Mutation, NewComment, NewThread, PendingReview, Posting, ReviewEvent, YourReview,
};
use review::threads::{Line, Lines};

pub const BOT: &str = "rozed-bot";
pub const REPO: &str = "rozed-dev/e2e";
pub const NUMBER: i64 = 23;
pub const REVIEWED: &str = "e11/review.txt";
pub const NOTES: &str = "e11/notes.md";

/// Every comment's text starts with this, so the recorder's clean-up finds them.
pub const MARK: &str = "Fixture E11:";

/// Waits for GitHub to show a write: a pause when recording, nothing when replaying.
pub type Pause<'a> = &'a dyn Fn(Duration);

const SETTLE: Duration = Duration::from_secs(2);

pub fn pull() -> PullRef {
    PullRef::new("rozed-dev", "e2e", NUMBER)
}

fn text(what: &str) -> String {
    format!("{MARK} {what}")
}

fn thread(path: &str, lines: Option<Lines>, what: &str) -> NewComment {
    NewComment::Thread(NewThread {
        path: path.into(),
        lines,
        body: text(what),
    })
}

fn reply_to(thread: &str, what: &str) -> NewComment {
    NewComment::Reply {
        thread: thread.into(),
        body: text(what),
    }
}

/// The pull request's node ID and head, from REST.
pub async fn pull_request(client: &GithubClient) -> (String, String) {
    let pull: serde_json::Value = client
        .get(&format!("/repos/{REPO}/pulls/{NUMBER}"))
        .await
        .unwrap();
    (
        pull["node_id"].as_str().unwrap().to_string(),
        pull["head"]["sha"].as_str().unwrap().to_string(),
    )
}

async fn check(client: &GithubClient) -> PullRequestCheckData {
    client
        .pull_request_check("rozed-dev", "e2e", NUMBER)
        .await
        .unwrap()
}

/// Sends what `review` planned and records its acceptance there, as the UI does.
async fn send(
    client: &GithubClient,
    id: &str,
    review: &mut YourReview,
    mutation: &Mutation,
) -> Result<Sent, GithubError> {
    let sent = client.send_review_mutation(&pull(), id, mutation).await;
    if let Ok(sent) = &sent {
        review.accepted(mutation, sent.review.as_deref());
    }
    sent
}

/// The thread whose first comment says `what`.
pub fn thread_saying<'a>(
    threads: &'a ReviewThreadList,
    what: &str,
) -> &'a github::queries::ReviewThread {
    let body = text(what);
    (threads.nodes.iter())
        .find(|thread| {
            (thread.comments.nodes.iter().flatten().flatten())
                .next()
                .is_some_and(|comment| comment.body == body)
        })
        .unwrap_or_else(|| panic!("no thread says {body:?}"))
}

/// The comment that says `what`, in any thread.
pub fn comment_saying(threads: &ReviewThreadList, what: &str) -> String {
    let body = text(what);
    (threads.nodes.iter())
        .flat_map(|thread| thread.comments.nodes.iter().flatten().flatten())
        .find(|comment| comment.body == body)
        .map(|comment| comment.id.clone())
        .unwrap_or_else(|| panic!("no comment says {body:?}"))
}

pub const LINE: &str = "a line thread, the first comment of a pending review.";
pub const RANGE: &str = "a range from the base's line 10 to the head's line 11.";
pub const FILE: &str = "a file-level thread.";
pub const OUTSIDE: &str = "line 25, outside the diff's hunks.";
pub const UNCHANGED: &str = "a file-level thread on a file the pull request doesn't change.";
pub const PENDING_REPLY: &str = "a reply in the pending review.";
pub const SUMMARY: &str = "the pending review's summary, submitted as a comment.";
pub const JOINED_REPLY: &str =
    "a reply sent to be posted at once, which joined the pending review.";

/// What the pending review scenario saw.
pub struct Pending {
    pub id: String,
    pub head: String,
    pub before: ViewerPendingReview,
    pub started: Sent,
    pub after_start: ViewerPendingReview,
    pub range: Sent,
    pub file: Sent,
    pub outside: Result<Sent, GithubError>,
    pub unchanged: Result<Sent, GithubError>,
    pub threads: ReviewThreadList,
    pub reply: Sent,
    pub before_submit: ViewerPendingReview,
    pub check: PullRequestCheckData,
    pub post_while_pending: Result<Sent, GithubError>,
    pub file_comment_while_pending: Result<Sent, GithubError>,
    pub reply_while_pending: Sent,
    pub submitted: Sent,
    pub after: ViewerPendingReview,
    pub review: YourReview,
}

/// Starts a pending review with a line thread, adds a range, a file-level thread and a reply,
/// tries a line outside the diff and a file the pull request doesn't change, and a thread, a file
/// comment and a reply posted at once while the review is pending, then submits it as a comment.
pub async fn pending(client: &GithubClient, pause: Pause<'_>) -> Pending {
    let pull = pull();
    let (id, head) = pull_request(client).await;
    let before = client.pending_review(&pull, BOT).await.unwrap();
    assert_eq!(before.review, None, "rozed-bot has a pending review on #23");
    let mut review = YourReview::new(before.pending_review());

    let line = thread(REVIEWED, Some(Lines::one(Line::right(10))), LINE);
    let start = review.add(&head, &head, &line).unwrap();
    assert!(matches!(
        start,
        Mutation::StartReview {
            thread: Some(_),
            ..
        }
    ));
    let started = send(client, &id, &mut review, &start).await.unwrap();
    pause(SETTLE);
    let after_start = client.pending_review(&pull, BOT).await.unwrap();

    let range = Lines::range(Line::left(10), Line::right(11));
    let add = review.add(&head, &head, &thread(REVIEWED, Some(range), RANGE));
    let range = send(client, &id, &mut review, &add.unwrap()).await.unwrap();
    let add = review.add(&head, &head, &thread(NOTES, None, FILE));
    let file = send(client, &id, &mut review, &add.unwrap()).await.unwrap();
    let outside = thread(REVIEWED, Some(Lines::one(Line::right(25))), OUTSIDE);
    let add = review.add(&head, &head, &outside).unwrap();
    let outside = send(client, &id, &mut review, &add).await;
    let add = review.add(&head, &head, &thread("README.md", None, UNCHANGED));
    let unchanged = send(client, &id, &mut review, &add.unwrap()).await;
    pause(SETTLE);

    let threads = client.review_threads(&pull).await.unwrap();
    let line_thread = thread_saying(&threads, LINE).id.clone();
    let add = review.add(&head, &head, &reply_to(&line_thread, PENDING_REPLY));
    let reply = send(client, &id, &mut review, &add.unwrap()).await.unwrap();
    pause(SETTLE);
    let before_submit = client.pending_review(&pull, BOT).await.unwrap();
    let check = check(client).await;

    // Posted at once, as `C` would without a pending review: GitHub refuses both while one is.
    let mut nothing_pending = YourReview::default();
    let line_50 = thread(REVIEWED, Some(Lines::one(Line::right(50))), "never posted.");
    let Ok(Posting::Now(post)) = nothing_pending.post(&head, &head, &line_50) else {
        panic!("not posted at once");
    };
    let post_while_pending = send(client, &id, &mut nothing_pending, &post).await;
    let notes = thread(NOTES, None, "never posted either.");
    let Ok(Posting::Now(post)) = nothing_pending.post(&head, &head, &notes) else {
        panic!("not posted at once");
    };
    let file_comment_while_pending = send(client, &id, &mut nothing_pending, &post).await;
    // A reply isn't refused: it quietly joins the pending review.
    let joining = reply_to(&line_thread, JOINED_REPLY);
    let Ok(Posting::Now(post)) = nothing_pending.post(&head, &head, &joining) else {
        panic!("not posted at once");
    };
    let reply_while_pending = (send(client, &id, &mut nothing_pending, &post).await).unwrap();

    let submit = review.submit(ReviewEvent::Comment, &text(SUMMARY), true, &head);
    let submitted = send(client, &id, &mut review, &submit.unwrap())
        .await
        .unwrap();
    pause(SETTLE);
    let after = client.pending_review(&pull, BOT).await.unwrap();
    Pending {
        id,
        head,
        before,
        started,
        after_start,
        range,
        file,
        outside,
        unchanged,
        threads,
        reply,
        before_submit,
        check,
        post_while_pending,
        file_comment_while_pending,
        reply_while_pending,
        submitted,
        after,
        review,
    }
}

pub const POSTED_REPLY: &str = "a reply posted at once.";
pub const POSTED_LINE: &str = "a line thread posted at once, on line 50.";
pub const POSTED_FILE: &str = "a file-level comment posted at once, through REST.";
pub const REVIEW_SUMMARY: &str = "a review with only a summary.";
pub const JOINING_REPLY: &str = "a reply that started an empty pending review.";

/// What the posted-at-once scenario saw.
pub struct Posted {
    pub reply: Sent,
    pub line: Sent,
    pub file: Sent,
    pub summary: Sent,
    pub approve: Result<Sent, GithubError>,
    pub request_changes: Result<Sent, GithubError>,
    pub empty: Result<Sent, GithubError>,
    pub started_empty: Sent,
    pub joined: Sent,
    pub submitted_empty: Sent,
    pub after: ViewerPendingReview,
}

/// Posts a reply, a line thread, a file-level comment and a review with only a summary at once;
/// tries approving, requesting changes and an empty comment, which GitHub refuses its author; then
/// a reply as the first comment of a pending review, submitted without a summary.
pub async fn posted(client: &GithubClient, pause: Pause<'_>) -> Posted {
    let pull = pull();
    let (id, head) = pull_request(client).await;
    let threads = client.review_threads(&pull).await.unwrap();
    let line_thread = thread_saying(&threads, LINE).id.clone();
    let range_thread = thread_saying(&threads, RANGE).id.clone();
    let mut review = YourReview::default();
    let now = |posting| match posting {
        Ok(Posting::Now(mutation)) => mutation,
        other => panic!("not posted at once: {other:?}"),
    };

    let post = now(review.post(&head, &head, &reply_to(&line_thread, POSTED_REPLY)));
    let reply = send(client, &id, &mut review, &post).await.unwrap();
    let line_50 = thread(REVIEWED, Some(Lines::one(Line::right(50))), POSTED_LINE);
    let post = now(review.post(&head, &head, &line_50));
    let line = send(client, &id, &mut review, &post).await.unwrap();
    let post = now(review.post(&head, &head, &thread(NOTES, None, POSTED_FILE)));
    let file = send(client, &id, &mut review, &post).await.unwrap();
    let submit = review.submit(ReviewEvent::Comment, &text(REVIEW_SUMMARY), true, &head);
    let summary = send(client, &id, &mut review, &submit.unwrap())
        .await
        .unwrap();

    // Lo offers its author only Comment; these ask GitHub anyway.
    let approve = review
        .submit(ReviewEvent::Approve, "", false, &head)
        .unwrap();
    let approve = send(client, &id, &mut review, &approve).await;
    let request = review.submit(
        ReviewEvent::RequestChanges,
        &text("never sent."),
        false,
        &head,
    );
    let request_changes = send(client, &id, &mut review, &request.unwrap()).await;
    let empty = Mutation::Review {
        commit: head.clone(),
        event: ReviewEvent::Comment,
        body: String::new(),
    };
    let empty = send(client, &id, &mut review, &empty).await;

    let first = reply_to(&range_thread, JOINING_REPLY);
    let start = review.add(&head, &head, &first).unwrap();
    assert!(matches!(start, Mutation::StartReview { thread: None, .. }));
    let started_empty = send(client, &id, &mut review, &start).await.unwrap();
    let add = review.add(&head, &head, &first).unwrap();
    assert!(matches!(add, Mutation::AddReply { .. }));
    let joined = send(client, &id, &mut review, &add).await.unwrap();
    let submit = review
        .submit(ReviewEvent::Comment, "", true, &head)
        .unwrap();
    let submitted_empty = send(client, &id, &mut review, &submit).await.unwrap();
    pause(SETTLE);
    let after = client.pending_review(&pull, BOT).await.unwrap();
    Posted {
        reply,
        line,
        file,
        summary,
        approve,
        request_changes,
        empty,
        started_empty,
        joined,
        submitted_empty,
        after,
    }
}

pub const EDITED: &str = "a reply posted at once, edited.";

/// What the threads scenario saw.
pub struct Threads {
    pub before: ReviewThreadList,
    pub thread: String,
    pub comment: String,
    pub checks: [PullRequestCheckData; 5],
    pub resolved: Result<(), GithubError>,
    pub unresolved: Result<(), GithubError>,
    pub edited: Result<(), GithubError>,
    pub blank: Result<(), GithubError>,
    pub deleted: Result<(), GithubError>,
    pub deleted_again: Result<(), GithubError>,
    pub after: ReviewThreadList,
}

/// Resolves and unresolves the line thread, then edits the reply posted at once, tries a blank
/// edit, and deletes the reply, twice, reading the check after each.
pub async fn threads(client: &GithubClient, pause: Pause<'_>) -> Threads {
    let pull = pull();
    let before = client.review_threads(&pull).await.unwrap();
    let thread = thread_saying(&before, LINE).id.clone();
    let comment = comment_saying(&before, POSTED_REPLY);
    let initial = check(client).await;

    let resolved = client.resolve_thread(&thread, true).await;
    pause(SETTLE);
    let after_resolving = check(client).await;
    let unresolved = client.resolve_thread(&thread, false).await;
    pause(SETTLE);
    let after_unresolving = check(client).await;

    let id = CommentId::Thread(comment.clone());
    let edited = client.edit_comment(&id, &text(EDITED)).await;
    pause(SETTLE);
    let after_editing = check(client).await;
    let blank = client.edit_comment(&id, "").await;

    let deleted = client.delete_comment(&id).await;
    pause(SETTLE);
    let after_deleting = check(client).await;
    let deleted_again = client.delete_comment(&id).await;
    let after = client.review_threads(&pull).await.unwrap();
    Threads {
        before,
        thread,
        comment,
        checks: [
            initial,
            after_resolving,
            after_unresolving,
            after_editing,
            after_deleting,
        ],
        resolved,
        unresolved,
        edited,
        blank,
        deleted,
        deleted_again,
        after,
    }
}

pub const FIRST: &str = "a comment on the pull request itself.";
pub const SECOND: &str = "a second comment, so the first isn't the newest item.";
pub const FIRST_EDITED: &str = "a comment on the pull request itself, edited.";

/// What the conversation scenario saw.
pub struct Conversation {
    pub first: String,
    pub second: String,
    pub checks: [PullRequestCheckData; 4],
    pub edited: Result<(), GithubError>,
    pub deleted: [Result<(), GithubError>; 2],
}

/// Comments twice on the pull request itself, edits the first, then deletes both, reading the
/// check before and after each step.
pub async fn conversation(client: &GithubClient, pause: Pause<'_>) -> Conversation {
    let (id, _) = pull_request(client).await;
    let initial = check(client).await;
    let first = client
        .add_conversation_comment(&id, &text(FIRST))
        .await
        .unwrap();
    pause(SETTLE);
    let second = client
        .add_conversation_comment(&id, &text(SECOND))
        .await
        .unwrap();
    pause(SETTLE);
    let after_commenting = check(client).await;
    let edited = client
        .edit_comment(&CommentId::Conversation(first.clone()), &text(FIRST_EDITED))
        .await;
    pause(SETTLE);
    let after_editing = check(client).await;
    let deleted = [
        client
            .delete_comment(&CommentId::Conversation(first.clone()))
            .await,
        client
            .delete_comment(&CommentId::Conversation(second.clone()))
            .await,
    ];
    pause(SETTLE);
    let after_deleting = check(client).await;
    Conversation {
        first,
        second,
        checks: [initial, after_commenting, after_editing, after_deleting],
        edited,
        deleted,
    }
}

pub const STALE_LINE: &str = "a thread for a pending review deleted since.";
pub const STALE_SUBMITTED: &str = "the thread of a review submitted since.";
pub const STALE_ADDED: &str = "a thread for a review submitted since.";
pub const STALE_REPLY: &str = "a reply for a review submitted since.";
pub const STALE_ORPHAN: &str = "a reply to a thread deleted since.";

/// What the stale scenario saw.
pub struct Stale {
    pub deleted: Result<(), GithubError>,
    pub thread_on_deleted: Result<Sent, GithubError>,
    pub submit_deleted: Result<Sent, GithubError>,
    pub submitted: Sent,
    pub thread_on_submitted: Result<Sent, GithubError>,
    pub reply_on_submitted: Result<Sent, GithubError>,
    pub deleted_comment: Result<(), GithubError>,
    pub reply_to_deleted: Result<Sent, GithubError>,
    pub after: ViewerPendingReview,
}

/// Sends what Lo would plan with IDs that are no longer current, as when a review or thread
/// changed elsewhere since Lo read it: a thread added to, and a submission of, a pending review
/// deleted since; a thread and a reply added to a review submitted since; and a reply to a thread
/// whose only comment was deleted since.
pub async fn stale(client: &GithubClient, pause: Pause<'_>) -> Stale {
    let pull = pull();
    let (id, head) = pull_request(client).await;
    let before = client.pending_review(&pull, BOT).await.unwrap();
    assert_eq!(before.review, None, "rozed-bot has a pending review on #23");

    // An empty pending review, deleted as on GitHub's web pages, while Lo still holds it.
    let mut review = YourReview::default();
    let start = Mutation::StartReview {
        commit: head.clone(),
        thread: None,
    };
    let started = send(client, &id, &mut review, &start).await.unwrap();
    let deleted = client
        .delete_pending_review(started.review.as_deref().unwrap())
        .await;
    pause(SETTLE);
    let line_10 = thread(REVIEWED, Some(Lines::one(Line::right(10))), STALE_LINE);
    let add = review.add(&head, &head, &line_10).unwrap();
    let thread_on_deleted = send(client, &id, &mut review, &add).await;
    let submit = review.submit(ReviewEvent::Comment, &text("never submitted."), true, &head);
    let submit_deleted = send(client, &id, &mut review, &submit.unwrap()).await;

    // A pending review with a thread, submitted, while Lo still holds it as pending.
    let mut fresh = YourReview::default();
    let line_30 = thread(REVIEWED, Some(Lines::one(Line::right(30))), STALE_SUBMITTED);
    let start = fresh.add(&head, &head, &line_30).unwrap();
    let started = send(client, &id, &mut fresh, &start).await.unwrap();
    let submit = fresh.submit(ReviewEvent::Comment, "", true, &head).unwrap();
    let submitted = send(client, &id, &mut fresh, &submit).await.unwrap();
    pause(SETTLE);
    let mut held = YourReview::new(Some(PendingReview {
        id: started.review.clone().unwrap(),
        commit: Some(head.clone()),
        comments: 1,
    }));
    let line_50 = thread(REVIEWED, Some(Lines::one(Line::right(50))), STALE_ADDED);
    let add = held.add(&head, &head, &line_50).unwrap();
    let thread_on_submitted = send(client, &id, &mut held, &add).await;
    let threads = client.review_threads(&pull).await.unwrap();
    let submitted_thread = thread_saying(&threads, STALE_SUBMITTED).id.clone();
    let add = held.add(&head, &head, &reply_to(&submitted_thread, STALE_REPLY));
    let reply_on_submitted = send(client, &id, &mut held, &add.unwrap()).await;
    pause(SETTLE);

    // The thread's only comment deleted, as elsewhere, then a reply to it posted at once.
    let only = CommentId::Thread(started.comment.clone().unwrap());
    let deleted_comment = client.delete_comment(&only).await;
    pause(SETTLE);
    let mut nothing_pending = YourReview::default();
    let reply = reply_to(&submitted_thread, STALE_ORPHAN);
    let Ok(Posting::Now(post)) = nothing_pending.post(&head, &head, &reply) else {
        panic!("not posted at once");
    };
    let reply_to_deleted = send(client, &id, &mut nothing_pending, &post).await;
    pause(SETTLE);
    let after = client.pending_review(&pull, BOT).await.unwrap();
    Stale {
        deleted,
        thread_on_deleted,
        submit_deleted,
        submitted,
        thread_on_submitted,
        reply_on_submitted,
        deleted_comment,
        reply_to_deleted,
        after,
    }
}
