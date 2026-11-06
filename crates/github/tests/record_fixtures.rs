//! Records the cassettes in `tests/fixtures` from `rozed-dev/playground`, a private repo where PR
//! #1 (`stack-a` → `main`) and PR #2 (`stack-b` → `stack-a`) form stack #3.
//!
//! Each test is ignored; run them by hand, one cassette or all:
//!
//! ```sh
//! cargo test -p github --test record_fixtures -- --ignored [cassette_name]
//! ```
//!
//! They use rozed's own OAuth token for the test bot, read from `ROZED_FIXTURE_TOKEN_FILE`
//! (default `~/.config/rozed-dev/oauth-token`), the same kind of token users have. Don't run a
//! device flow per recording: GitHub keeps 10 tokens per user, app and scope set, and revokes the
//! oldest beyond that. The recorder refuses to write the token or anything shaped like a GitHub
//! token; see `github::record`.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use futures::executor::block_on;
use github::cassette::{CassetteInfo, fixtures_dir};
use github::queries::FileViewedState;
use github::record::RecordingHttpClient;
use github::{Conditional, GithubClient, ListedPull, PullRef, PullSummary, RepoRef, Token};
use serde::de::IgnoredAny;

const REPO: &str = "rozed-dev/playground";

fn token() -> Token {
    let path = std::env::var_os("ROZED_FIXTURE_TOKEN_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").expect("HOME is set"))
                .join(".config/rozed-dev/oauth-token")
        });
    let token = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("reading the token from {}: {error}", path.display()));
    Token::new(token)
}

/// Records the requests `run` makes into the cassette `name`.
fn record<F: Future<Output = ()>>(
    name: &str,
    description: &str,
    run: impl FnOnce(GithubClient) -> F,
) {
    record_with_schema(name, description, None, run);
}

/// [`record`], noting the GraphQL schema the queries were checked against, if any. The recorder
/// writes `cassette.json` and checks it like every other file.
fn record_with_schema<F: Future<Output = ()>>(
    name: &str,
    description: &str,
    schema: Option<String>,
    run: impl FnOnce(GithubClient) -> F,
) {
    let token = token();
    let http = Arc::new(
        reqwest_client::ReqwestClient::user_agent(&github::user_agent())
            .expect("starting the HTTP client"),
    );
    let recorder = RecordingHttpClient::new(
        http,
        &fixtures_dir().join(name),
        vec![token.expose().as_bytes().to_vec()],
    )
    .expect("starting the recorder");
    block_on(run(GithubClient::new(Arc::new(recorder.clone()), token)));
    recorder
        .finish(CassetteInfo {
            description: description.into(),
            synthetic: false,
            recorded_at: Some(
                chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now())
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            ),
            repo: Some(REPO.into()),
            schema,
        })
        .expect("finishing the cassette");
}

#[test]
#[ignore = "records from GitHub; run by hand"]
fn rest_pull_conditional() {
    record(
        "rest_pull_conditional",
        "GET /pulls/1, then the same request with its ETag, answered 304",
        |client| async move {
            let path = format!("/repos/{REPO}/pulls/1");
            let Conditional::Modified { etag, .. } = client
                .get_conditional::<serde_json::Value>(&path, None)
                .await
                .unwrap()
            else {
                panic!("the first request has no ETag to send");
            };
            let second = client
                .get_conditional::<serde_json::Value>(&path, etag.as_deref())
                .await
                .unwrap();
            assert_eq!(second, Conditional::NotModified);
        },
    );
}

#[test]
#[ignore = "records from GitHub; run by hand"]
fn rest_pull_files() {
    record(
        "rest_pull_files",
        "GET /pulls/2/files: the files PR #2 changes, each with its patch",
        |client| async move {
            let files = client
                .pull_files(&PullRef::new("rozed-dev", "playground", 2))
                .await
                .unwrap();
            assert!(!files.is_empty());
        },
    );
}

#[test]
#[ignore = "records from GitHub; run by hand"]
fn user() {
    record(
        "user",
        "GET /user as rozed-bot, with the scopes of rozed's OAuth token",
        |client| async move {
            client.user().await.unwrap();
        },
    );
}

#[test]
#[ignore = "records from GitHub; run by hand"]
fn rest_not_found() {
    record(
        "rest_not_found",
        "GET a repository that doesn't exist",
        |client| async move {
            client
                .get::<serde_json::Value>("/repos/rozed-dev/does-not-exist")
                .await
                .unwrap_err();
        },
    );
}

/// The `github/docs` commit the vendored schema came from, from its header.
fn schema_commit() -> Option<String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("graphql/schema.graphql");
    let schema = std::fs::read_to_string(path).ok()?;
    let first_line = schema.lines().next()?;
    let start = first_line.find("github/docs@")?;
    let commit: String = first_line[start..]
        .chars()
        .take_while(|c| !c.is_whitespace())
        .collect();
    Some(commit)
}

/// Like [`record`], noting the schema the queries were checked against.
fn record_graphql<F: Future<Output = ()>>(
    name: &str,
    description: &str,
    run: impl FnOnce(GithubClient) -> F,
) {
    record_with_schema(name, description, schema_commit(), run);
}

#[test]
#[ignore = "records from GitHub; run by hand"]
fn graphql_viewer() {
    record_graphql(
        "graphql_viewer",
        "The Viewer query as rozed-bot",
        |client| async move {
            client.viewer().await.unwrap();
        },
    );
}

#[test]
#[ignore = "records from GitHub; run by hand"]
fn graphql_pull_request_1() {
    record_graphql(
        "graphql_pull_request_1",
        "The PullRequest query for PR #1 (stack-a -> main)",
        |client| async move {
            client
                .pull_request("rozed-dev", "playground", 1)
                .await
                .unwrap();
        },
    );
}

#[test]
#[ignore = "records from GitHub; run by hand"]
fn graphql_pull_request_2_paged() {
    record_graphql(
        "graphql_pull_request_2_paged",
        "The PullRequest query for PR #2 (stack-b -> stack-a), one file a page",
        |client| async move {
            client
                .pull_request_paged("rozed-dev", "playground", 2, 1)
                .await
                .unwrap();
        },
    );
}

#[test]
#[ignore = "records from GitHub; run by hand"]
fn graphql_pull_request_check_1() {
    record_graphql(
        "graphql_pull_request_check_1",
        "The PullRequestCheck query for PR #1",
        |client| async move {
            client
                .pull_request_check("rozed-dev", "playground", 1)
                .await
                .unwrap();
        },
    );
}

#[test]
#[ignore = "records from GitHub; run by hand"]
fn graphql_open_pull_requests_paged() {
    record_graphql(
        "graphql_open_pull_requests_paged",
        "The OpenPullRequests query for the playground, one PR a page",
        |client| async move {
            client
                .open_pull_requests_paged("rozed-dev", "playground", 1)
                .await
                .unwrap();
        },
    );
}

#[test]
#[ignore = "records from GitHub; run by hand"]
fn graphql_not_found() {
    record_graphql(
        "graphql_not_found",
        "The PullRequest query for a repository that doesn't exist",
        |client| async move {
            client
                .pull_request("rozed-dev", "does-not-exist", 1)
                .await
                .unwrap_err();
        },
    );
}

/// PR #2's one file, the only file the recordings mark viewed. They leave it unviewed.
const PR_2_FILE: &str = "stack-b.md";

#[test]
#[ignore = "records from GitHub; run by hand"]
fn graphql_viewed_marks() {
    record_graphql(
        "graphql_viewed_marks",
        "PR #2's one file marked viewed and its mark cleared, reading the PullRequest query \
         before, between and after: UNVIEWED, VIEWED, UNVIEWED",
        |client| async move {
            let state = || async {
                let pr = client
                    .pull_request("rozed-dev", "playground", 2)
                    .await
                    .unwrap();
                let file = pr
                    .files
                    .unwrap()
                    .nodes
                    .unwrap()
                    .into_iter()
                    .flatten()
                    .next();
                (pr.id, file.unwrap().viewer_viewed_state)
            };
            let (id, before) = state().await;
            assert_eq!(
                before,
                FileViewedState::UNVIEWED,
                "start from an unviewed file"
            );
            client.mark_file_as_viewed(&id, PR_2_FILE).await.unwrap();
            assert_eq!(state().await.1, FileViewedState::VIEWED);
            client.unmark_file_as_viewed(&id, PR_2_FILE).await.unwrap();
            assert_eq!(state().await.1, FileViewedState::UNVIEWED);
        },
    );
}

#[test]
#[ignore = "records from GitHub; run by hand"]
fn graphql_mark_file_as_viewed_not_found() {
    record_graphql(
        "graphql_mark_file_as_viewed_not_found",
        "markFileAsViewed on a pull request node ID that names nothing",
        |client| async move {
            client
                .mark_file_as_viewed("PR_kwDOVk6v9M8AAAAAAAAAAA", "README.md")
                .await
                .unwrap_err();
        },
    );
}

/// A path PR #2 doesn't have.
const NOT_IN_PR_2: &str = "not-in-this-pull-request.md";

#[test]
#[ignore = "records from GitHub; run by hand"]
fn graphql_mark_file_as_viewed_unlisted_path() {
    record_graphql(
        "graphql_mark_file_as_viewed_unlisted_path",
        "markFileAsViewed on PR #2 with a path the pull request doesn't have, reading the \
         PullRequest query before and after; PR #2's one file stays unviewed",
        |client| async move {
            let state = || async {
                let pr = client
                    .pull_request("rozed-dev", "playground", 2)
                    .await
                    .unwrap();
                let files: Vec<(String, FileViewedState)> = pr
                    .files
                    .unwrap()
                    .nodes
                    .unwrap()
                    .into_iter()
                    .flatten()
                    .map(|file| (file.path, file.viewer_viewed_state))
                    .collect();
                (pr.id, files)
            };
            let unviewed = vec![(PR_2_FILE.to_string(), FileViewedState::UNVIEWED)];
            let (id, before) = state().await;
            assert_eq!(before, unviewed, "start from an unviewed file");
            let answer = client.mark_file_as_viewed(&id, NOT_IN_PR_2).await;
            eprintln!("markFileAsViewed on {NOT_IN_PR_2}: {answer:?}");
            assert_eq!(state().await.1, unviewed, "PR #2's one file stays unviewed");
        },
    );
}

/// What a conditional GET returned: its ETag, after checking it was a `200`.
fn modified<T: std::fmt::Debug>(answer: Conditional<T>) -> Option<String> {
    match answer {
        Conditional::Modified { etag, .. } => etag,
        Conditional::NotModified => panic!("expected a 200, got a 304"),
    }
}

/// What a conditional GET returned, after checking it was a `200`.
fn modified_value<T: std::fmt::Debug>(answer: Conditional<T>) -> (T, Option<String>) {
    match answer {
        Conditional::Modified { value, etag } => (value, etag),
        Conditional::NotModified => panic!("expected a 200, got a 304"),
    }
}

#[test]
#[ignore = "records from GitHub; run by hand"]
fn poll_playground() {
    record_graphql(
        "poll_playground",
        "Two polls in the poller's order, the second unchanged: PR #1 as the open PR (GET, \
         PullRequestCheck, then PullRequest the first time), PR #2 as a watched one (GET), and \
         the playground's open PRs (GET of the list, then OpenPullRequests the first time). The \
         second poll asks about each PR since its last update, and about the list with its ETag",
        |client| async move {
            let open = PullRef::new("rozed-dev", "playground", 1);
            let watched = PullRef::new("rozed-dev", "playground", 2);
            let repo = RepoRef::new("rozed-dev", "playground");
            let (owner, name) = (repo.owner.as_str(), repo.name.as_str());

            let (open_summary, _) = modified_value(
                client
                    .get_modified_since::<PullSummary>(&open.rest_path(), None)
                    .await
                    .unwrap(),
            );
            let check = client.pull_request_check(owner, name, 1).await.unwrap();
            client.pull_request(owner, name, 1).await.unwrap();
            let (watched_summary, _) = modified_value(
                client
                    .get_modified_since::<PullSummary>(&watched.rest_path(), None)
                    .await
                    .unwrap(),
            );
            let (_, list_etag) = modified_value(
                client
                    .get_conditional::<Vec<ListedPull>>(&repo.open_pulls_path(1), None)
                    .await
                    .unwrap(),
            );
            client.open_pull_requests(owner, name).await.unwrap();

            let since = open_summary.last_modified();
            let unchanged = client
                .get_modified_since::<PullSummary>(&open.rest_path(), since.as_deref())
                .await
                .unwrap();
            assert_eq!(unchanged, Conditional::NotModified);
            assert_eq!(
                client.pull_request_check(owner, name, 1).await.unwrap(),
                check
            );
            let since = watched_summary.last_modified();
            let unchanged = client
                .get_modified_since::<PullSummary>(&watched.rest_path(), since.as_deref())
                .await
                .unwrap();
            assert_eq!(unchanged, Conditional::NotModified);
            let unchanged = client
                .get_conditional::<Vec<ListedPull>>(&repo.open_pulls_path(1), list_etag.as_deref())
                .await
                .unwrap();
            assert_eq!(unchanged, Conditional::NotModified);
        },
    );
}

#[test]
#[ignore = "records from GitHub; run by hand"]
fn rest_pull_304_rate_limit() {
    record(
        "rest_pull_304_rate_limit",
        "Whether a 304 costs rate limit: GET /rate_limit (itself free) around a plain GET of PR \
         #1 and three conditional GETs answered 304",
        |client| async move {
            let pull = PullRef::new("rozed-dev", "playground", 1);
            let rate_limit = || client.get::<serde_json::Value>("/rate_limit");
            rate_limit().await.unwrap();
            let etag = modified(
                client
                    .get_conditional::<IgnoredAny>(&pull.rest_path(), None)
                    .await
                    .unwrap(),
            );
            rate_limit().await.unwrap();
            for _ in 0..3 {
                let answer = client
                    .get_conditional::<IgnoredAny>(&pull.rest_path(), etag.as_deref())
                    .await
                    .unwrap();
                assert_eq!(answer, Conditional::NotModified);
            }
            rate_limit().await.unwrap();
        },
    );
}
