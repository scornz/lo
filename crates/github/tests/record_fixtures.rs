//! Records the cassettes in `tests/fixtures` from `rozed-dev/playground`, a private repo where PR
//! #1 (`stack-a` → `main`) and PR #2 (`stack-b` → `stack-a`) form stack #3. The push histories
//! and stacks come from `rozed-dev/e2e`'s fixtures (`script/e2e/README.md`), and from
//! `sharkdp/fd` and `github/gh-stack`, read only. `review_writes_e2e_23` writes comments and
//! reviews to E11, `rozed-dev/e2e#23`, a fixture of rozed-bot's own, and records them.
//!
//! Each test is ignored; run them by hand, one cassette or all:
//!
//! ```sh
//! cargo test -p github --test record_fixtures -- --ignored [cassette_name]
//! ```
//!
//! They use Lo's own OAuth token for the test bot, read from `LO_FIXTURE_TOKEN_FILE`
//! (default `~/.config/rozed-dev/oauth-token`), the same kind of token users have. Don't run a
//! device flow per recording: GitHub keeps 10 tokens per user, app and scope set, and revokes the
//! oldest beyond that. The recorder refuses to write the token or anything shaped like a GitHub
//! token; see `github::record`.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use futures::executor::block_on;
use github::cassette::{CassetteInfo, fixtures_dir};
use github::queries::open_pull_requests::RECENTLY_UPDATED_MERGED;
use github::queries::{FileViewedState, OpenPullRequestList, PAGE_SIZE, TrunkRuleList};
use github::record::RecordingHttpClient;
use github::{Conditional, GithubClient, ListedPull, PullRef, PullSummary, RepoRef, Token};
use serde::de::IgnoredAny;

const REPO: &str = "rozed-dev/playground";

fn token() -> Token {
    let path = std::env::var_os("LO_FIXTURE_TOKEN_FILE")
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
    record_with_schema(name, description, REPO, None, run);
}

/// [`record`] from `repo`, noting the GraphQL schema the queries were checked against, if any. The
/// recorder writes `cassette.json` and checks it like every other file.
fn record_with_schema<F: Future<Output = ()>>(
    name: &str,
    description: &str,
    repo: &str,
    schema: Option<String>,
    run: impl FnOnce(GithubClient) -> F,
) {
    record_as(token(), name, description, repo, schema, run);
}

fn http() -> Arc<reqwest_client::ReqwestClient> {
    Arc::new(
        reqwest_client::ReqwestClient::user_agent(&github::user_agent())
            .expect("starting the HTTP client"),
    )
}

/// [`record_with_schema`] with `token`, returning what `run` returns.
fn record_as<T, F: Future<Output = T>>(
    token: Token,
    name: &str,
    description: &str,
    repo: &str,
    schema: Option<String>,
    run: impl FnOnce(GithubClient) -> F,
) -> T {
    let recorder = RecordingHttpClient::new(
        http(),
        &fixtures_dir().join(name),
        vec![token.expose().as_bytes().to_vec()],
    )
    .expect("starting the recorder");
    let result = block_on(run(GithubClient::new(Arc::new(recorder.clone()), token)));
    recorder
        .finish(CassetteInfo {
            description: description.into(),
            synthetic: false,
            recorded_at: Some(
                chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now())
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            ),
            repo: Some(repo.into()),
            schema,
        })
        .expect("finishing the cassette");
    result
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
        "GET /user as rozed-bot, with the scopes of Lo's OAuth token",
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
    record_with_schema(name, description, REPO, schema_commit(), run);
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
                .open_pull_requests_paged(
                    &RepoRef::new("rozed-dev", "playground"),
                    1,
                    RECENTLY_UPDATED_MERGED,
                )
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
         second poll asks about each PR since its last update, and about the list with its ETag. \
         Then TrunkRules, which the first poll reads after OpenPullRequests",
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
            client
                .open_pull_requests(&repo, &TrunkRuleList::default())
                .await
                .unwrap();

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
            // The first poll reads the playground's rulesets after its open PRs. Recorded last,
            // so the answers above keep their places in the cassette.
            client.trunk_rules(&repo, None, &[]).await.unwrap();
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

/// The test bot, whose last review the push histories read.
const BOT: &str = "rozed-bot";

/// Records `owner/repo#number`'s push history twice, as a poll reads it: first from nothing, then
/// with the first answer, asking about each page of pushes with its ETag.
fn record_push_history(name: &str, description: &str, owner: &str, repo: &str, number: i64) {
    let full_name = format!("{owner}/{repo}");
    record_with_schema(
        name,
        description,
        &full_name,
        schema_commit(),
        |client| async move {
            let pull = PullRef::new(owner, repo, number);
            let first = client.push_history(&pull, BOT, None).await.unwrap();
            let again = client.push_history(&pull, BOT, Some(&first)).await.unwrap();
            assert_eq!(
                again, first,
                "the push history changed while it was recorded"
            );
        },
    );
}

#[test]
#[ignore = "records from GitHub; run by hand"]
fn push_history_e2e_14() {
    record_push_history(
        "push_history_e2e_14",
        "rozed-dev/e2e#14's push history, twice: E1's full run, closed. r1-r4 are normal \
         pushes with rozed-bot's COMMENT review on r2, r5 is GitHub's rebase onto a moved main \
         (updatePullRequestBranch), and r6 a normal push after it",
        "rozed-dev",
        "e2e",
        14,
    );
}

#[test]
#[ignore = "records from GitHub; run by hand"]
fn push_history_e2e_16() {
    record_push_history(
        "push_history_e2e_16",
        "rozed-dev/e2e#16's push history, twice: E1 open at r3, with rozed-bot's COMMENT review \
         on r2",
        "rozed-dev",
        "e2e",
        16,
    );
}

#[test]
#[ignore = "records from GitHub; run by hand"]
fn push_history_fd_2212() {
    record_push_history(
        "push_history_fd_2212",
        "sharkdp/fd#2212's push history, twice, as rozed-bot: a public pull request from the fork \
         voidsipp/fd, two normal pushes, a maintainer's review on the first, and check suites \
         without pushes",
        "sharkdp",
        "fd",
        2212,
    );
}

#[test]
#[ignore = "records from GitHub; run by hand"]
fn push_history_fd_2202() {
    record_push_history(
        "push_history_fd_2202",
        "sharkdp/fd#2202's push history, twice, as rozed-bot: a merged pull request whose fork was \
         deleted, so its head branch's pushes can't be read",
        "sharkdp",
        "fd",
        2202,
    );
}

/// The node IDs of the layers of the stack `number` is in, in the order the tree walks them.
fn stack_ids(list: &OpenPullRequestList, number: u64) -> Vec<String> {
    let stacks = list.stacks(&TrunkRuleList::default(), &[]);
    let stack = stacks.stack_of(number).expect("a layer of a stack");
    (stack.layers.iter())
        .map(|&layer| {
            let pr = list.pull_request(layer as i64).expect("an open layer");
            pr.id.clone()
        })
        .collect()
}

/// Records what a stack's tree reads of `repo`: its open pull requests, with the merged ones
/// looked up, its trunk rules, and the layers `layers` picks.
fn record_stacks(
    name: &str,
    description: &str,
    repo: RepoRef,
    layers: impl FnOnce(&OpenPullRequestList) -> Vec<String>,
) {
    let full_name = repo.to_string();
    record_with_schema(
        name,
        description,
        &full_name,
        schema_commit(),
        |client| async move {
            let rules = client.trunk_rules(&repo, None, &[]).await.unwrap();
            assert_eq!(rules.unavailable, None);
            let list = client.open_pull_requests(&repo, &rules).await.unwrap();
            let ids = layers(&list);
            let now = std::time::SystemTime::now();
            client.stack_layers(&repo, &ids, None, now).await.unwrap();
        },
    );
}

#[test]
#[ignore = "records from GitHub; run by hand"]
fn stacks_playground() {
    record_stacks(
        "stacks_playground",
        "OpenPullRequests and TrunkRules for the playground, where PR #1 (stack-a -> main) and PR \
         #2 (stack-b -> stack-a) form stack #3, then StackLayers for both",
        RepoRef::new("rozed-dev", "playground"),
        |list| stack_ids(list, 1),
    );
}

#[test]
#[ignore = "records from GitHub; run by hand"]
fn stacks_e2e() {
    record_stacks(
        "stacks_e2e",
        "OpenPullRequests and TrunkRules for rozed-dev/e2e: E2's tree (#2 a -> main, #3 b -> a, \
         #4 c -> b, #5 e -> b, #6 d -> main holding c), its native stack #9 (#7, #8), and E3 \
         (#11 y -> e3/x, on #10 x, squash-merged, among the merged pull requests updated most \
         recently); then StackLayers for E2's tree, the native stack, #11, and #19, whose base \
         the fixtures script changed from main to e8/base",
        RepoRef::new("rozed-dev", "e2e"),
        |list| {
            let mut ids = stack_ids(list, 2);
            ids.extend(stack_ids(list, 7));
            ids.extend(stack_ids(list, 11));
            ids.extend(stack_ids(list, 19));
            ids
        },
    );
}

#[test]
#[ignore = "records from GitHub; run by hand"]
fn stacks_e2e_looked_up() {
    record_with_schema(
        "stacks_e2e_looked_up",
        "OpenPullRequests for rozed-dev/e2e without the merged pull requests updated most \
         recently, so E3's #10 comes from looking up the merged pull requests of e3/x, the branch \
         #11 targets",
        "rozed-dev/e2e",
        schema_commit(),
        |client| async move {
            let repo = RepoRef::new("rozed-dev", "e2e");
            let mut list = client
                .open_pull_requests_paged(&repo, PAGE_SIZE, 0)
                .await
                .unwrap();
            let (rules, now) = (TrunkRuleList::default(), std::time::SystemTime::now());
            client
                .look_up_merged(&repo, &mut list, None, &rules, now)
                .await
                .unwrap();
            assert!(list.merged.iter().any(|pr| pr.number == 10));
        },
    );
}

#[test]
#[ignore = "records from GitHub; run by hand"]
fn stacks_gh_stack() {
    record_stacks(
        "stacks_gh_stack",
        "OpenPullRequests and TrunkRules for github/gh-stack as rozed-bot, read only: the \
         rulesets a reader sees, its organization's included, one of them for Copilot reviews on \
         ~ALL, and merged GitHub stacks; then StackLayers for every open pull request, with their \
         reviews and checks",
        RepoRef::new("github", "gh-stack"),
        |list| list.pull_requests.iter().map(|pr| pr.id.clone()).collect(),
    );
}

#[test]
#[ignore = "records from GitHub; run by hand"]
fn stacks_cli() {
    record_stacks(
        "stacks_cli",
        "OpenPullRequests and TrunkRules for cli/cli as rozed-bot, read only, which can't \
         administer it: trunk's branch protection shows only through each pull request's base, \
         and its one ruleset, Copilot reviews on ~ALL, gates nothing; then StackLayers for the \
         stack of draft pull requests on williammartin-multiaccount branches",
        RepoRef::new("cli", "cli"),
        |list| {
            let draft = (list.pull_requests.iter())
                .find(|pr| {
                    pr.head_ref_name
                        .starts_with("williammartin-multiaccount-draft-")
                })
                .expect("the draft stack is open");
            stack_ids(list, draft.number as u64)
        },
    );
}

/// Records `owner/repo`'s requests that `run` makes, noting the schema.
fn record_repo<F: Future<Output = ()>>(
    name: &str,
    description: &str,
    repo: &str,
    run: impl FnOnce(GithubClient) -> F,
) {
    record_with_schema(name, description, repo, schema_commit(), run);
}

#[test]
#[ignore = "records from GitHub; run by hand"]
fn review_threads_fd_2212() {
    record_repo(
        "review_threads_fd_2212",
        "sharkdp/fd#2212's review threads as rozed-bot, read only: a maintainer's \
         changes-requested review on the first push, six threads and seven comments, all outdated \
         by the second push",
        "sharkdp/fd",
        |client| async move {
            let pull = PullRef::new("sharkdp", "fd", 2212);
            let threads = client.review_threads(&pull).await.unwrap();
            assert!(!threads.nodes.is_empty());
        },
    );
}

#[test]
#[ignore = "records from GitHub; run by hand"]
fn review_threads_playground_4_paged() {
    record_graphql(
        "review_threads_playground_4_paged",
        "The playground's PR #4 (review checks, closed), whose 15 threads came from \
         docs/research/review-api-checks.md, read two threads a page, each with two comments, \
         and the rest of a longer thread's comments two a page",
        |client| async move {
            let pull = PullRef::new("rozed-dev", "playground", 4);
            let threads = client.review_threads_paged(&pull, 2).await.unwrap();
            assert!(threads.nodes.len() > 2);
        },
    );
}

#[test]
#[ignore = "records from GitHub; run by hand"]
fn review_threads_e2e_20() {
    record_repo(
        "review_threads_e2e_20",
        "rozed-dev/e2e#20 (E9, script/e2e/README.md): rozed-bot's COMMENT review with two \
         threads, on a moved line and on a line only the old side has; then rozed-bot's pending \
         review, which it has none of",
        "rozed-dev/e2e",
        |client| async move {
            let pull = PullRef::new("rozed-dev", "e2e", 20);
            let threads = client.review_threads(&pull).await.unwrap();
            assert_eq!(threads.nodes.len(), 2);
            let pending = client.pending_review(&pull, BOT).await.unwrap();
            assert_eq!(pending.review, None);
        },
    );
}

/// The time the timeline recordings are read at.
fn recorded_now() -> std::time::SystemTime {
    std::time::SystemTime::now()
}

#[test]
#[ignore = "records from GitHub; run by hand"]
fn timeline_fd_2212_paged() {
    record_repo(
        "timeline_fd_2212_paged",
        "sharkdp/fd#2212's timeline as rozed-bot, read only, three items a page: read in full, \
         newest page first, then again with what the first read gave, which asks for the newest \
         page alone",
        "sharkdp/fd",
        |client| async move {
            let pull = PullRef::new("sharkdp", "fd", 2212);
            let now = recorded_now();
            let first = client.timeline_paged(&pull, None, now, 3).await.unwrap();
            let again = (client.timeline_paged(&pull, Some(&first), now, 3))
                .await
                .unwrap();
            assert_eq!(again, first, "the timeline changed while it was recorded");
        },
    );
}

#[test]
#[ignore = "records from GitHub; run by hand"]
fn timeline_cli_13057() {
    record_repo(
        "timeline_cli_13057",
        "cli/cli#13057's timeline as rozed-bot, read only: a merged pull request with commits, \
         force-pushes, reviews, review requests, cross-references, auto-merge and its merge, two \
         pages newest first, the second repeating items pushed at the same second as the first's \
         oldest. Read again with what that read gave, it asks for the newest page alone, which \
         the replay answers again, so the cassette holds it once",
        "cli/cli",
        |client| async move {
            let pull = PullRef::new("cli", "cli", 13057);
            client.timeline(&pull, None, recorded_now()).await.unwrap();
        },
    );
}

#[test]
#[ignore = "records from GitHub; run by hand"]
fn timeline_playground_4() {
    record_graphql(
        "timeline_playground_4",
        "The playground's PR #4's timeline (review checks, closed): its commits, reviews and close",
        |client| async move {
            let pull = PullRef::new("rozed-dev", "playground", 4);
            client.timeline(&pull, None, recorded_now()).await.unwrap();
        },
    );
}

#[test]
#[ignore = "records from GitHub; run by hand"]
fn timeline_e2e() {
    record_repo(
        "timeline_e2e",
        "rozed-dev/e2e's timelines of #14 (E1's full run: force-pushed by GitHub's rebase, \
         reviewed, closed) and #19 (E8, whose base the fixtures script changed)",
        "rozed-dev/e2e",
        |client| async move {
            for number in [14, 19] {
                let pull = PullRef::new("rozed-dev", "e2e", number);
                client.timeline(&pull, None, recorded_now()).await.unwrap();
            }
        },
    );
}

#[test]
#[ignore = "records from GitHub; run by hand"]
fn rest_pull_files_e2e() {
    record_repo(
        "rest_pull_files_e2e",
        "The files of rozed-dev/e2e's #18 (E6), #20 (E9) and #22 (E12), each with GitHub's patch: \
         script/e2e/trees.sh writes both sides of each, so repo_cache compares its own patches \
         with these",
        "rozed-dev/e2e",
        |client| async move {
            for number in [18, 20, 22] {
                let files = client
                    .pull_files(&PullRef::new("rozed-dev", "e2e", number))
                    .await
                    .unwrap();
                assert!(!files.is_empty());
            }
        },
    );
}

#[test]
#[ignore = "records from GitHub; run by hand"]
fn rest_pull_files_berry_7077() {
    record_repo(
        "rest_pull_files_berry_7077",
        "yarnpkg/berry#7077's files as rozed-bot, read only, and its .gitattributes at the pull \
         request's head: .pnp.cjs and packages/yarnpkg-pnp/sources/hook.js are marked binary, yet \
         GitHub gives .pnp.cjs a text patch. hook.js, one line of 140 kB, gets none",
        "yarnpkg/berry",
        |client| async move {
            let pull = PullRef::new("yarnpkg", "berry", 7077);
            let summary: serde_json::Value = client.get(&pull.rest_path()).await.unwrap();
            let head = summary["head"]["sha"].as_str().unwrap();
            client.pull_files(&pull).await.unwrap();
            client
                .get::<serde_json::Value>(&format!(
                    "/repos/yarnpkg/berry/contents/.gitattributes?ref={head}"
                ))
                .await
                .unwrap();
        },
    );
}

// The replay tests read every field of what the scenarios saw; this prints some.
#[allow(dead_code)]
#[path = "scenarios/review_writes.rs"]
mod scenarios;

/// Writes to E11, `rozed-dev/e2e#23` (`script/e2e/README.md`), as rozed-bot, recording each of
/// `scenarios`' writes into a cassette of its own, then deletes the comments it can. Submitted
/// reviews stay: GitHub doesn't delete them. Prints what GitHub answered.
#[test]
#[ignore = "writes to rozed-dev/e2e#23 as rozed-bot and records it; run by hand"]
fn review_writes_e2e_23() {
    use github::review_writes::CommentId;

    let token = token();
    let plain = GithubClient::new(http(), token.clone());
    // Writes go only to rozed-bot's own fixture: check whose token it is first.
    let user = block_on(plain.user()).unwrap();
    assert_eq!(user.user.login, scenarios::BOT);
    let pause = |duration| std::thread::sleep(duration);
    let record = |name: &str, description: &str| {
        (
            format!("review_writes_e2e_23_{name}"),
            description.to_string(),
        )
    };

    let (name, description) = record(
        "pending",
        "rozed-bot's pending review on rozed-dev/e2e#23 (E11): started with a line thread, then a \
         range from the base to the head and a file-level thread, a line outside the diff and a \
         file the PR doesn't change refused, a reply, a comment posted at once refused while it's \
         pending, then submitted as a comment",
    );
    let pending = record_as(
        token.clone(),
        &name,
        &description,
        scenarios::REPO,
        schema_commit(),
        |client| async move { scenarios::pending(&client, &pause).await },
    );
    eprintln!("pending: started {:?}", pending.started);
    eprintln!("pending: after start {:?}", pending.after_start);
    eprintln!(
        "pending: range {:?}, file {:?}",
        pending.range, pending.file
    );
    eprintln!("pending: outside {:?}", pending.outside);
    eprintln!("pending: unchanged file {:?}", pending.unchanged);
    eprintln!("pending: threads {:#?}", pending.threads.threads());
    eprintln!("pending: reply {:?}", pending.reply);
    eprintln!("pending: before submit {:?}", pending.before_submit);
    eprintln!(
        "pending: check's pending review {:?}",
        pending.check.viewer_pending_review
    );
    eprintln!(
        "pending: posted while pending {:?}",
        pending.post_while_pending
    );
    eprintln!(
        "pending: file comment while pending {:?}",
        pending.file_comment_while_pending
    );
    eprintln!(
        "pending: submitted {:?}, after {:?}",
        pending.submitted, pending.after
    );

    let (name, description) = record(
        "posted",
        "Comments posted at once on rozed-dev/e2e#23 (E11) by rozed-bot, its author: a reply, a \
         line thread, a REST file-level comment and a review with only a summary; approving, \
         requesting changes and an empty comment refused; then a reply that starts an empty \
         pending review, submitted without a summary",
    );
    let posted = record_as(
        token.clone(),
        &name,
        &description,
        scenarios::REPO,
        schema_commit(),
        |client| async move { scenarios::posted(&client, &pause).await },
    );
    eprintln!("posted: reply {:?}", posted.reply);
    eprintln!("posted: line {:?}, file {:?}", posted.line, posted.file);
    eprintln!("posted: summary {:?}", posted.summary);
    eprintln!("posted: approve {:?}", posted.approve);
    eprintln!("posted: request changes {:?}", posted.request_changes);
    eprintln!("posted: empty {:?}", posted.empty);
    eprintln!(
        "posted: started empty {:?}, joined {:?}, submitted {:?}, after {:?}",
        posted.started_empty, posted.joined, posted.submitted_empty, posted.after
    );

    let (name, description) = record(
        "threads",
        "On rozed-dev/e2e#23 (E11), as rozed-bot: a thread resolved and unresolved, a reply \
         edited, edited blank, and deleted twice, with the check read after each",
    );
    let threads = record_as(
        token.clone(),
        &name,
        &description,
        scenarios::REPO,
        schema_commit(),
        |client| async move { scenarios::threads(&client, &pause).await },
    );
    eprintln!(
        "threads: resolved {:?}, unresolved {:?}",
        threads.resolved, threads.unresolved
    );
    eprintln!(
        "threads: edited {:?}, blank {:?}",
        threads.edited, threads.blank
    );
    eprintln!(
        "threads: deleted {:?}, again {:?}",
        threads.deleted, threads.deleted_again
    );
    for (step, check) in threads.checks.iter().enumerate() {
        eprintln!(
            "threads: check {step}: threads {:?}, timeline {:?}",
            serde_json::to_value(&check.review_threads).unwrap(),
            serde_json::to_value(&check.timeline_events).unwrap()
        );
    }

    let (name, description) = record(
        "conversation",
        "On rozed-dev/e2e#23 (E11), as rozed-bot: two comments on the pull request itself, the \
         first edited, then both deleted, with the check read before and after each step",
    );
    let conversation = record_as(
        token.clone(),
        &name,
        &description,
        scenarios::REPO,
        schema_commit(),
        |client| async move { scenarios::conversation(&client, &pause).await },
    );
    eprintln!(
        "conversation: edited {:?}, deleted {:?}",
        conversation.edited, conversation.deleted
    );
    for (step, check) in conversation.checks.iter().enumerate() {
        eprintln!(
            "conversation: check {step}: comments {:?}, timeline {:?}, timeline items {:?}",
            serde_json::to_value(&check.comments).unwrap(),
            serde_json::to_value(&check.timeline_events).unwrap(),
            serde_json::to_value(&check.timeline_items).unwrap()
        );
    }

    // Clean up, unrecorded: every comment of rozed-bot's these wrote. A thread goes with its last.
    block_on(async {
        let threads = plain.review_threads(&scenarios::pull()).await.unwrap();
        let comments = (threads.nodes.iter())
            .flat_map(|thread| thread.comments.nodes.iter().flatten().flatten())
            .filter(|comment| {
                comment.author.as_ref().map(|author| author.login.as_str()) == Some(scenarios::BOT)
                    && comment.body.starts_with(scenarios::MARK)
            });
        for comment in comments {
            let deleted = plain
                .delete_comment(&CommentId::Thread(comment.id.clone()))
                .await;
            eprintln!("clean-up: deleted {}: {deleted:?}", comment.id);
        }
    });
}
