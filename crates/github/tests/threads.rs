//! Review threads and the pending review, read from recorded answers and mapped into
//! `review::threads` and `review::pending`:
//!
//! - `sharkdp/fd#2212`, a maintainer's changes-requested review whose six threads a later push
//!   made outdated, read as rozed-bot, which can reply but not resolve.
//! - `rozed-dev/playground#4`, the review API checks' pull request
//!   (`docs/research/review-api-checks.md`): ranges, a thread on the base, outdated threads and a
//!   thread with replies, read two threads and two comments a page.
//! - `rozed-dev/e2e#20`, E9 (`script/e2e/README.md`): a thread on a moved line and one on a line
//!   only the old side has.

use std::sync::Arc;

use futures::executor::block_on;
use github::cassette::{Interaction, fixtures_dir, read_cassette};
use github::poll::CachedRead;
use github::queries::ReviewThreadList;
use github::replay::ReplayHttpClient;
use github::{GithubClient, GithubError, PullRef, Token};
use review::threads::{
    CommentState, DiffSide, Line, Lines, Placement, Screen, Subject, Thread, place,
};

const BOT: &str = "rozed-bot";

fn client(cassette: &str) -> (GithubClient, Arc<ReplayHttpClient>) {
    let http = ReplayHttpClient::cassette(cassette);
    (
        GithubClient::new(http.clone(), Token::new("gho_FAKE")),
        http,
    )
}

/// The GraphQL operations a replaying client was asked, in order.
fn operations(http: &ReplayHttpClient) -> Vec<String> {
    (http.seen().iter())
        .map(|seen| {
            let body: serde_json::Value = serde_json::from_slice(&seen.body).unwrap();
            body["operationName"].as_str().unwrap().to_string()
        })
        .collect()
}

/// The thread on `path` whose last line was `original_line` when it was written.
fn written_at<'a>(threads: &'a [Thread], path: &str, original_line: u32) -> Vec<&'a Thread> {
    (threads.iter())
        .filter(|thread| thread.path == path && thread.original_line == Some(original_line))
        .collect()
}

/// The one commit all of `shas` are, checking that it starts with `short`.
fn one_commit<'a>(shas: impl IntoIterator<Item = Option<&'a String>>, short: &str) -> String {
    let mut shas: Vec<&String> = shas.into_iter().map(Option::unwrap).collect();
    shas.dedup();
    assert_eq!(shas.len(), 1, "{shas:?}");
    assert!(shas[0].starts_with(short), "{} isn't {short}", shas[0]);
    shas[0].clone()
}

#[test]
fn fd_2212s_threads_are_outdated_by_the_second_push() {
    let (client, http) = client("review_threads_fd_2212");
    let pull = PullRef::new("sharkdp", "fd", 2212);
    let list: ReviewThreadList = block_on(client.review_threads(&pull)).unwrap();
    assert_eq!(operations(&http), ["ReviewThreads"]);
    let threads = list.threads();

    assert_eq!(threads.len(), 6);
    let comments: usize = threads.iter().map(|thread| thread.comments.len()).sum();
    assert_eq!(comments, 7);
    // Every thread was written on the first push and is outdated since: GitHub leaves its first
    // comment on the commit it was written on, with no current lines.
    let first_push = one_commit(
        (threads.iter())
            .flat_map(|thread| [thread.current_on.as_ref(), thread.written_on.as_ref()]),
        "15635d6",
    );
    for thread in &threads {
        assert!(thread.outdated, "{}", thread.path);
        assert_eq!(thread.current_lines(), None);
        assert_eq!(thread.subject, Subject::Line);
        assert_eq!(thread.side, DiffSide::Right);
        assert!(!thread.resolved);
        // rozed-bot can reply to a public pull request, but not resolve its threads or edit a
        // maintainer's comments.
        assert!(thread.viewer_can_reply);
        assert!(!thread.viewer_can_resolve && !thread.viewer_can_unresolve);
        for comment in &thread.comments {
            assert_eq!(comment.author.as_deref(), Some("tmccombs"));
            assert_eq!(comment.state, CommentState::Submitted);
            assert_eq!(comment.hidden, None);
            assert!(!comment.viewer_can_update && !comment.viewer_can_delete);
            assert!(
                (comment.url).starts_with("https://github.com/sharkdp/fd/pull/2212#discussion_r"),
                "{}",
                comment.url
            );
        }
    }
    let makefile = written_at(&threads, "Makefile", 17);
    assert_eq!(makefile.len(), 1);
    assert_eq!(makefile[0].comments.len(), 2);
    assert!(makefile[0].comments[0].created_at <= makefile[0].comments[1].created_at);

    // They show where they were written on the first push, and only in the panel on the second.
    let head = "22c70acc6992c0c51cb68527b87adf114d01acbc";
    let workflow = written_at(&threads, ".github/workflows/CICD.yml", 187)[0];
    assert_eq!(
        place(workflow, &Screen::full(first_push.clone())),
        Placement::Lines(Lines::one(Line::right(187)))
    );
    assert_eq!(
        place(workflow, &Screen::between(first_push, head)),
        Placement::Lines(Lines::one(Line::left(187)))
    );
    assert_eq!(place(workflow, &Screen::full(head)), Placement::Outdated);
}

#[test]
fn the_playgrounds_review_checks_read_two_threads_and_two_comments_a_page() {
    let (client, http) = client("review_threads_playground_4_paged");
    let pull = PullRef::new("rozed-dev", "playground", 4);
    let threads = block_on(client.review_threads_paged(&pull, 2))
        .unwrap()
        .threads();
    // 15 threads, two a page, and the third comment of the one thread that has three.
    let mut expected = vec!["ReviewThreads"; 8];
    expected.push("ReviewThreadComments");
    assert_eq!(operations(&http), expected);
    assert_eq!(threads.len(), 15);
    let mut ids: Vec<&str> = threads.iter().map(|thread| thread.id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 15);

    // c2 and c4 of docs/research/review-api-checks.md: the outdated threads were written on c2,
    // and GitHub has moved every other thread to c4.
    let c2 = one_commit(
        (threads.iter())
            .filter(|thread| thread.outdated)
            .map(|thread| thread.written_on.as_ref()),
        "fc0f4b4",
    );
    let c4 = one_commit(
        (threads.iter())
            .filter(|thread| !thread.outdated)
            .map(|thread| thread.current_on.as_ref()),
        "b405bfc",
    );

    // Its first thread, on line 30, has a reply in a review of its own and one in the pending
    // review that was then submitted, all in order.
    let replied = (threads.iter())
        .find(|thread| thread.comments.len() > 2)
        .unwrap();
    assert_eq!(replied.comments.len(), 3);
    assert_eq!(replied.current_lines(), Some(Lines::one(Line::right(30))));
    assert!((replied.comments.windows(2)).all(|pair| pair[0].created_at <= pair[1].created_at));
    let mut comment_ids: Vec<&str> = (replied.comments.iter())
        .map(|comment| comment.id.as_str())
        .collect();
    comment_ids.dedup();
    assert_eq!(comment_ids.len(), 3);
    for comment in &replied.comments {
        assert_eq!(comment.author.as_deref(), Some(BOT));
        // rozed-bot's own comments, in its own repository.
        assert!(comment.viewer_can_update && comment.viewer_can_delete);
    }
    assert!(replied.viewer_can_reply && replied.viewer_can_resolve);
    assert!(!replied.viewer_can_unresolve);

    // P1 of check 1, on line 10 of c2, outdated by c3: it shows on c2 only.
    let outdated: Vec<&Thread> = (threads.iter()).filter(|thread| thread.outdated).collect();
    assert_eq!(outdated.len(), 3);
    for thread in &outdated {
        assert_eq!(thread.current_on.as_ref(), Some(&c2));
        assert_eq!(thread.written_on.as_ref(), Some(&c2));
        assert_eq!(
            place(thread, &Screen::full(c2.clone())),
            Placement::Lines(Lines::one(Line::right(10)))
        );
        assert_eq!(
            place(thread, &Screen::full(c4.clone())),
            Placement::Outdated
        );
    }

    // Check 6's ranges, all on c4 now.
    let ranges: Vec<Lines> = (threads.iter())
        .filter_map(|thread| thread.current_lines())
        .filter(Lines::is_range)
        .collect();
    assert!(ranges.contains(&Lines::range(Line::right(10), Line::right(11))));
    assert!(ranges.contains(&Lines::range(Line::right(10), Line::right(30))));
    assert!(ranges.contains(&Lines::range(Line::left(10), Line::left(11))));
    // From a removed line to the added line that replaced it.
    let across = Lines::range(Line::left(10), Line::right(10));
    assert!(ranges.contains(&across));
    let across = (threads.iter())
        .find(|thread| thread.current_lines() == Some(across))
        .unwrap();
    assert_eq!(
        place(across, &Screen::full(c4.clone())),
        Placement::Lines(Lines::range(Line::left(10), Line::right(10)))
    );
    // Between revisions, only its added line is on screen.
    assert_eq!(
        place(across, &Screen::between(c2, c4)),
        Placement::Lines(Lines::one(Line::right(10)))
    );
}

#[test]
fn e9s_threads_show_on_the_moved_line_and_the_line_only_the_old_side_has() {
    let (client, http) = client("review_threads_e2e_20");
    let pull = PullRef::new("rozed-dev", "e2e", 20);
    let threads = block_on(client.review_threads(&pull)).unwrap().threads();
    let pending = block_on(client.pending_review(&pull, BOT)).unwrap();
    assert_eq!(operations(&http), ["ReviewThreads", "PendingReview"]);
    // rozed-bot submitted its review, so it has nothing pending.
    assert_eq!(pending.pending_review(), None);

    // #20's only revision, `243ff70` in script/e2e/README.md.
    let head = one_commit(
        (threads.iter())
            .flat_map(|thread| [thread.current_on.as_ref(), thread.written_on.as_ref()]),
        "243ff70",
    );
    let screen = Screen::full(head.clone());
    let on = |path: &str| {
        let thread = (threads.iter()).find(|thread| thread.path == path).unwrap();
        (thread, place(thread, &screen))
    };

    // `const middle = …` in `Names`, where `fullName` moved to.
    let (moved, placement) = on("e9/api/names.ts");
    assert_eq!(placement, Placement::Lines(Lines::one(Line::right(10))));
    assert!(moved.comments[0].body.contains("moved line"));
    // In `legacyName`, which the pull request deletes: on the left, the base's side.
    let (deleted, placement) = on("e9/api/user.ts");
    assert_eq!(placement, Placement::Lines(Lines::one(Line::left(26))));
    assert_eq!(deleted.side, DiffSide::Left);
    assert!(deleted.comments[0].body.contains("only the old side"));

    for thread in [moved, deleted] {
        assert!(!thread.outdated && !thread.resolved);
        assert_eq!(thread.comments.len(), 1);
        let comment = &thread.comments[0];
        assert_eq!(comment.author.as_deref(), Some(BOT));
        assert!(comment.viewer_can_update && comment.viewer_can_delete);
        assert!((comment.url).starts_with("https://github.com/rozed-dev/e2e/pull/20#discussion_r"));
        assert!(thread.viewer_can_reply && thread.viewer_can_resolve);
    }
    // A line on the base shows only in the revision's whole diff.
    let parent = "0".repeat(40);
    assert_eq!(
        place(deleted, &Screen::between(parent, head)),
        Placement::NotOnScreen
    );
}

/// `review_threads_e2e_20`'s answer to `operation`, with `edit` made to its body.
fn edited(operation: &str, edit: impl FnOnce(&mut serde_json::Value)) -> Arc<ReplayHttpClient> {
    let (_, interactions) = read_cassette(&fixtures_dir().join("review_threads_e2e_20")).unwrap();
    let mut interaction: Interaction = (interactions.into_iter())
        .map(|(_, interaction)| interaction)
        .find(|interaction| interaction.request.operation.as_deref() == Some(operation))
        .unwrap();
    edit(interaction.response.body.as_mut().unwrap());
    Arc::new(ReplayHttpClient::from_interactions(operation, [interaction]).unwrap())
}

#[test]
fn threads_github_leaves_out_make_the_list_incomplete() {
    // An error nulls the first thread, and the second thread's comment.
    let http = edited("ReviewThreads", |body| {
        let threads = &mut body["data"]["repository"]["pullRequest"]["reviewThreads"]["nodes"];
        threads[0] = serde_json::Value::Null;
        threads[1]["comments"]["nodes"][0] = serde_json::Value::Null;
        body["errors"] = serde_json::json!([
            {"message": "Something went wrong", "path": ["repository", "pullRequest", "reviewThreads", "nodes", 0]},
            {"message": "Something went wrong", "path": ["repository", "pullRequest", "reviewThreads", "nodes", 1, "comments", "nodes", 0]}
        ]);
    });
    let client = GithubClient::new(http, Token::new("gho_FAKE"));
    let list = block_on(client.review_threads(&PullRef::new("rozed-dev", "e2e", 20))).unwrap();
    assert_eq!(list.missing, 2);
    assert!(!list.is_complete());
    let threads = list.threads();
    assert_eq!(threads.len(), 1);
    assert!(threads[0].comments.is_empty());

    let whole = block_on(client_for("review_threads_e2e_20").review_threads(&e9())).unwrap();
    assert!(whole.is_complete());
}

fn client_for(cassette: &str) -> GithubClient {
    client(cassette).0
}

fn e9() -> PullRef {
    PullRef::new("rozed-dev", "e2e", 20)
}

#[test]
fn a_pending_review_github_leaves_out_isnt_taken_for_none() {
    let refused = edited("PendingReview", |body| {
        body["data"]["repository"]["pullRequest"]["reviews"] = serde_json::Value::Null;
        body["errors"] = serde_json::json!([
            {"message": "Something went wrong", "path": ["repository", "pullRequest", "reviews"]}
        ]);
    });
    let client = GithubClient::new(refused, Token::new("gho_FAKE"));
    assert!(block_on(client.pending_review(&e9(), BOT)).is_err());

    let unlisted = edited("PendingReview", |body| {
        body["data"]["repository"]["pullRequest"]["reviews"] = serde_json::Value::Null;
    });
    let client = GithubClient::new(unlisted, Token::new("gho_FAKE"));
    assert!(matches!(
        block_on(client.pending_review(&e9(), BOT)),
        Err(GithubError::Decode(_))
    ));
}
