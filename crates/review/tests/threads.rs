//! Threads, commentable lines and the pending review for whole pull requests: threads filled the
//! way the GitHub layer fills them from GraphQL, revisions keyed by their full head SHAs, and
//! GitHub's own `patch` for a changed file.

use std::time::{Duration, SystemTime};

use review::drafts::{DraftKey, Drafts, Place, PullRequestId};
use review::pending::{
    Mutation, NewComment, NewThread, PendingReview, Posting, Refusal, ReviewEvent, YourReview,
};
use review::revisions::{PushRecord, Revisions, build_revisions};
use review::threads::{
    CommentState, CommentableLines, Hunk, Line, Lines, PanelFilter, Placement, Screen, Thread,
    ThreadComment, panel, place, suggestion_block,
};

const R1: &str = "5b1e0c7a9d3f4e2b8c6a1d0f9e8b7c6a5d4e3f21";
const R2: &str = "9e4d2a6c8b0f1e3d5c7a9b2e4f6d8c0a1b3e5f74";
/// The latest head, `headRefOid`.
const R3: &str = "c7f30e9b1a5d7c3e9f1b5d7a3c9e1f5b7d3a9c62";

fn at(minute: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000 + minute * 60)
}

/// r1 to r3, three normal pushes.
fn revisions() -> Revisions {
    let push = |before: Option<&str>, after: &str, minute| PushRecord::Activity {
        before: before.map(str::to_string),
        after: after.to_string(),
        at: at(minute),
        forced: false,
    };
    build_revisions(
        &[
            push(None, R1, 10),
            push(Some(R1), R2, 20),
            push(Some(R2), R3, 30),
        ],
        &[R1, R2, R3].map(String::from),
        R3,
        at(0),
    )
}

/// A `PullRequestReviewThread` node, with GraphQL's names and values.
struct Node {
    id: &'static str,
    path: &'static str,
    subject_type: &'static str,
    diff_side: &'static str,
    start_diff_side: Option<&'static str>,
    line: Option<u32>,
    start_line: Option<u32>,
    original_line: Option<u32>,
    original_start_line: Option<u32>,
    is_outdated: bool,
    is_resolved: bool,
    /// The first comment's `originalCommit.oid`.
    original_commit: Option<&'static str>,
    /// Each comment's `author.login` and `state`.
    comments: &'static [(&'static str, &'static str)],
}

/// The thread the GitHub layer fills from `node`.
fn thread(node: Node) -> Thread {
    Thread {
        id: node.id.to_string(),
        path: node.path.to_string(),
        subject: node.subject_type.parse().unwrap(),
        side: node.diff_side.parse().unwrap(),
        start_side: node.start_diff_side.map(|side| side.parse().unwrap()),
        line: node.line,
        start_line: node.start_line,
        original_line: node.original_line,
        original_start_line: node.original_start_line,
        outdated: node.is_outdated,
        resolved: node.is_resolved,
        written_on: node.original_commit.map(str::to_string),
        comments: (node.comments.iter().enumerate())
            .map(|(index, (author, state))| ThreadComment {
                id: format!("{}_{index}", node.id),
                author: Some(author.to_string()),
                body: "…".to_string(),
                state: state.parse().unwrap(),
                created_at: at(40 + index as u64),
            })
            .collect(),
    }
}

/// Bob's pull request's threads, after three pushes.
fn threads() -> [Thread; 5] {
    let line = Node {
        id: "",
        path: "src/parse.rs",
        subject_type: "LINE",
        diff_side: "RIGHT",
        start_diff_side: None,
        line: None,
        start_line: None,
        original_line: None,
        original_start_line: None,
        is_outdated: false,
        is_resolved: false,
        original_commit: None,
        comments: &[("bob", "SUBMITTED")],
    };
    [
        // Written on r1 at line 10. r2 added four lines above it, and it hasn't changed since.
        thread(Node {
            id: "PRRT_current",
            line: Some(14),
            original_line: Some(10),
            original_commit: Some(R1),
            ..line
        }),
        // Written on r1 at line 20, which r2 changed.
        thread(Node {
            id: "PRRT_outdated",
            line: None,
            original_line: Some(20),
            is_outdated: true,
            original_commit: Some(R1),
            comments: &[("bob", "SUBMITTED"), ("alice", "SUBMITTED")],
            ..line
        }),
        // Alice's pending comment on lines 30 to 33 of r3.
        thread(Node {
            id: "PRRT_range",
            start_diff_side: Some("RIGHT"),
            line: Some(33),
            start_line: Some(30),
            original_line: Some(33),
            original_start_line: Some(30),
            original_commit: Some(R3),
            comments: &[("alice", "PENDING")],
            ..line
        }),
        // On line 5 of the base, a removed line, written on r2 and since resolved.
        thread(Node {
            id: "PRRT_base",
            diff_side: "LEFT",
            line: Some(5),
            original_line: Some(5),
            is_resolved: true,
            original_commit: Some(R2),
            comments: &[("carol", "SUBMITTED")],
            ..line
        }),
        thread(Node {
            id: "PRRT_file",
            path: "README.md",
            subject_type: "FILE",
            original_commit: Some(R2),
            ..line
        }),
    ]
}

fn placements(screen: &Screen) -> Vec<(&'static str, Placement)> {
    let ids = [
        "PRRT_current",
        "PRRT_outdated",
        "PRRT_range",
        "PRRT_base",
        "PRRT_file",
    ];
    (ids.into_iter())
        .zip(threads().iter().map(|thread| place(thread, screen, R3)))
        .collect()
}

fn lines(lines: Lines) -> Placement {
    Placement::Lines(lines)
}

#[test]
fn threads_on_the_latest_revisions_whole_diff() {
    assert_eq!(
        placements(&Screen::full(R3)),
        [
            ("PRRT_current", lines(Lines::one(Line::right(14)))),
            ("PRRT_outdated", Placement::Outdated),
            (
                "PRRT_range",
                lines(Lines::range(Line::right(30), Line::right(33)))
            ),
            ("PRRT_base", lines(Lines::one(Line::left(5)))),
            ("PRRT_file", Placement::File),
        ]
    );
}

#[test]
fn threads_between_r1_and_r3() {
    // The left column shows r1: the outdated thread is where it was written, and the base isn't on
    // screen.
    assert_eq!(
        placements(&Screen::between(R1, R3)),
        [
            ("PRRT_current", lines(Lines::one(Line::right(14)))),
            ("PRRT_outdated", lines(Lines::one(Line::left(20)))),
            (
                "PRRT_range",
                lines(Lines::range(Line::right(30), Line::right(33)))
            ),
            ("PRRT_base", Placement::NotOnScreen),
            ("PRRT_file", Placement::File),
        ]
    );
}

#[test]
fn threads_on_r1s_whole_diff_are_where_they_were_written() {
    assert_eq!(
        placements(&Screen::full(R1)),
        [
            ("PRRT_current", lines(Lines::one(Line::right(10)))),
            ("PRRT_outdated", lines(Lines::one(Line::right(20)))),
            ("PRRT_range", Placement::NotOnScreen),
            ("PRRT_base", Placement::NotOnScreen),
            ("PRRT_file", Placement::File),
        ]
    );
}

#[test]
fn threads_on_r2s_whole_diff() {
    assert_eq!(
        placements(&Screen::full(R2)),
        [
            ("PRRT_current", Placement::NotOnScreen),
            ("PRRT_outdated", Placement::Outdated),
            ("PRRT_range", Placement::NotOnScreen),
            ("PRRT_base", lines(Lines::one(Line::left(5)))),
            ("PRRT_file", Placement::File),
        ]
    );
}

#[test]
fn threads_with_r1_replayed_onto_r3s_base() {
    assert_eq!(
        placements(&Screen::replayed(R1, R3)),
        [
            ("PRRT_current", lines(Lines::one(Line::right(14)))),
            ("PRRT_outdated", Placement::Outdated),
            (
                "PRRT_range",
                lines(Lines::range(Line::right(30), Line::right(33)))
            ),
            ("PRRT_base", Placement::NotOnScreen),
            ("PRRT_file", Placement::File),
        ]
    );
}

#[test]
fn the_threads_panel_and_how_threads_start() {
    let threads = threads();
    let listed = |filter| -> Vec<&str> {
        panel(&threads, filter, "alice")
            .map(|thread| thread.id.as_str())
            .collect()
    };

    assert_eq!(
        listed(PanelFilter::Unresolved),
        ["PRRT_current", "PRRT_outdated", "PRRT_range", "PRRT_file"]
    );
    assert_eq!(listed(PanelFilter::Yours), ["PRRT_outdated", "PRRT_range"]);
    assert_eq!(listed(PanelFilter::All).len(), 5);

    let open: Vec<bool> = threads.iter().map(Thread::starts_open).collect();
    assert_eq!(open, [true, true, true, false, true]);
    let pending: Vec<bool> = threads.iter().map(Thread::is_pending).collect();
    assert_eq!(pending, [false, false, true, false, false]);
    assert_eq!(threads[2].comments[0].state, CommentState::Pending);
}

/// GitHub's `patch` for `src/parse.rs` at r3: `build` now fails, and `validate` runs after it.
const PATCH: &str = "\
@@ -8,8 +8,9 @@ use crate::token::Token;
\x20
 pub fn parse(input: &str) -> Result<Ast, Error> {
     let tokens = tokenize(input)?;
-    let ast = build(tokens);
-    Ok(ast)
+    let ast = build(tokens)?;
+    validate(&ast)?;
+    Ok(ast)
 }
\x20
 fn tokenize(input: &str) -> Result<Vec<Token>, Error> {";

fn pull_request() -> PullRequestId {
    PullRequestId {
        owner: "acme".to_string(),
        repo: "shop".to_string(),
        number: 42,
    }
}

fn new_thread(lines: Option<Lines>, body: &str) -> NewThread {
    NewThread {
        path: "src/parse.rs".to_string(),
        lines,
        body: body.to_string(),
    }
}

#[test]
fn alice_reviews_r3_and_approves() {
    let screen = Screen::full(R3);
    let commentable = CommentableLines::new(&screen, Hunk::parse(PATCH).unwrap());
    let mut review = YourReview::new(None);
    let mut drafts = Drafts::new();

    // `v` from line 12 up to line 11, then `c`: the text is kept until it's posted.
    let selection = commentable.range(Line::right(12), Line::right(11)).unwrap();
    assert_eq!(selection, Lines::range(Line::right(11), Line::right(12)));
    let key = DraftKey {
        account: "alice".to_string(),
        pull_request: pull_request(),
        place: Place::Thread {
            commit: screen.right.clone(),
            path: "src/parse.rs".to_string(),
            lines: Some(selection),
        },
    };
    assert!(drafts.set(key.clone(), "Does validate need the tokens?"));
    let first = NewComment::Thread(new_thread(Some(selection), drafts.get(&key).unwrap()));
    let start = review.add(&screen.right, &first).unwrap();
    assert_eq!(
        start,
        Mutation::StartReview {
            commit: R3.to_string(),
            first: first.clone(),
        }
    );
    review.accepted(&start, Some("PRR_kwDOAbc"));
    assert_eq!(
        drafts.take(&key),
        Some("Does validate need the tokens?".to_string())
    );

    // `s` on line 13 suggests a change; the removed line 12 takes a comment but no suggestion.
    let added = commentable.line(Line::right(13)).unwrap();
    assert!(added.takes_suggestion());
    assert!(!commentable.line(Line::left(12)).unwrap().takes_suggestion());
    let suggestion = suggestion_block(&["    Ok(validate(ast)?)"]);
    assert_eq!(suggestion, "```suggestion\n    Ok(validate(ast)?)\n```");
    let second = NewComment::Thread(new_thread(Some(added), &suggestion));
    let add = review.add(R3, &second).unwrap();
    assert!(matches!(&add, Mutation::AddThread { review, .. } if review == "PRR_kwDOAbc"));
    review.accepted(&add, None);

    // Line 20 is outside the diff, so the comment goes on the file.
    assert_eq!(commentable.line(Line::right(20)), None);
    let file = NewComment::Thread(new_thread(None, "Worth a test for empty input."));
    let add = review.add(R3, &file).unwrap();
    review.accepted(&add, None);

    // `C` on a reply joins the pending review: GitHub posts nothing else from her until it's
    // submitted.
    let reply = NewComment::Reply {
        thread: "PRRT_current".to_string(),
        body: "Fixed in r3, thanks.".to_string(),
    };
    let Ok(Posting::Pending(post)) = review.post(R3, &reply) else {
        panic!("a reply posted at once with a pending review should join it");
    };
    review.accepted(&post, None);

    // `⌘⏎`.
    let dialog = review.submit_dialog("alice", false, R3);
    assert_eq!(
        dialog.choices,
        [
            ReviewEvent::Comment,
            ReviewEvent::Approve,
            ReviewEvent::RequestChanges
        ]
    );
    assert_eq!(dialog.pending_comments, 4);
    assert_eq!((dialog.on.as_str(), dialog.on_screen), (R3, true));
    let submit = review.submit(ReviewEvent::Approve, "", false, R3).unwrap();
    assert_eq!(
        submit,
        Mutation::SubmitReview {
            review: "PRR_kwDOAbc".to_string(),
            event: ReviewEvent::Approve,
            body: String::new(),
        }
    );
    review.accepted(&submit, None);
    assert_eq!(review.pending(), None);
}

#[test]
fn bob_can_only_comment_on_his_own_pull_request() {
    let review = YourReview::new(None);
    let dialog = review.submit_dialog("bob", true, R3);

    assert_eq!(dialog.choices, [ReviewEvent::Comment]);
    assert_eq!(
        review.submit(ReviewEvent::Approve, "Ship it", true, R3),
        Err(Refusal::OwnPullRequest)
    );
    assert_eq!(
        review.submit(ReviewEvent::Comment, "Ready for another look.", true, R3),
        Ok(Mutation::Review {
            commit: R3.to_string(),
            event: ReviewEvent::Comment,
            body: "Ready for another look.".to_string(),
        })
    );
}

#[test]
fn a_pending_review_started_before_a_push_keeps_its_revision() {
    // Alice started a review on r2; Bob then pushed r3.
    let revisions = revisions();
    let review = YourReview::new(Some(PendingReview {
        id: "PRR_kwDOAbc".to_string(),
        commit: R2.to_string(),
        comments: 2,
    }));
    let screen = Screen::between(R2, R3);
    let commentable = CommentableLines::new(&screen, Hunk::parse(PATCH).unwrap());

    // Between revisions, only the right column takes comments, and they'd go on r3.
    assert_eq!(commentable.line(Line::left(11)), None);
    let lines = commentable.line(Line::right(11)).unwrap();
    let refused = review.add(R3, &NewComment::Thread(new_thread(Some(lines), "Nit")));
    let Err(Refusal::OtherRevision { review_on }) = refused else {
        panic!("a thread on r3 shouldn't join a review on r2");
    };
    assert_eq!(revisions.label(&review_on).as_deref(), Some("r2"));

    // Replies have no lines, so they join it.
    let reply = NewComment::Reply {
        thread: "PRRT_current".to_string(),
        body: "Still applies.".to_string(),
    };
    assert!(matches!(
        review.add(R3, &reply),
        Ok(Mutation::AddReply { .. })
    ));

    // The dialog says the review is recorded on r2.
    let dialog = review.submit_dialog("alice", false, R3);
    assert_eq!(revisions.label(&dialog.on).as_deref(), Some("r2"));
    assert!(!dialog.on_screen);
}

#[test]
fn without_a_pending_review_c_posts_on_the_revision_on_screen() {
    let screen = Screen::between(R1, R2);
    let thread = new_thread(Some(Lines::one(Line::right(11))), "Nit: rename.");

    assert_eq!(
        YourReview::new(None).post(&screen.right, &NewComment::Thread(thread.clone())),
        Ok(Posting::Now(Mutation::PostThread {
            commit: R2.to_string(),
            thread,
        }))
    );
}
