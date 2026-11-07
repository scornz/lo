//! The typed GraphQL queries on responses recorded from `rozed-dev/playground`, where PR #1
//! (`stack-a` → `main`) and PR #2 (`stack-b` → `stack-a`) form stack #3.

use futures::executor::block_on;
use github::queries::open_pull_requests::RECENTLY_UPDATED_MERGED;
use github::queries::{
    FileViewedState, MergeStateStatus, PullRequestCheckData, PullRequestState, StatusState,
};
use github::replay::ReplayHttpClient;
use github::{GithubClient, GithubError, RepoRef, Token};

fn client(cassette: &str) -> (GithubClient, std::sync::Arc<ReplayHttpClient>) {
    let http = ReplayHttpClient::cassette(cassette);
    (
        GithubClient::new(http.clone(), Token::new("gho_FAKE")),
        http,
    )
}

#[test]
fn viewer() {
    let (client, http) = client("graphql_viewer");
    let viewer = block_on(client.viewer()).unwrap();
    assert_eq!(viewer.login, "rozed-bot");
    assert_eq!(viewer.database_id, Some(353226052));
    assert!(
        viewer
            .avatar_url
            .starts_with("https://avatars.githubusercontent.com/")
    );
    let seen = &http.seen()[0];
    assert_eq!(seen.url, "https://api.github.com/graphql");
    assert_eq!(seen.header("authorization"), Some("token gho_FAKE"));
}

#[test]
fn pull_request() {
    let (client, _) = client("graphql_pull_request_1");
    let pr = block_on(client.pull_request("rozed-dev", "playground", 1)).unwrap();
    assert_eq!(pr.number, 1);
    assert_eq!(pr.state, PullRequestState::OPEN);
    assert!(!pr.is_draft);
    assert_eq!(pr.author.as_ref().unwrap().login, "rozed-bot");
    assert_eq!(
        (pr.base_ref_name.as_str(), pr.head_ref_name.as_str()),
        ("main", "stack-a")
    );
    assert!(pr.base_ref_oid.starts_with("6ce218eb"));
    assert_eq!(pr.head_ref_oid.len(), 40);
    assert!(!pr.is_cross_repository);
    let head = pr.head_repository.unwrap();
    assert_eq!(head.name_with_owner, "rozed-dev/playground");
    assert!(!head.is_fork);
    let files = pr.files.unwrap();
    assert_eq!(files.total_count, 1);
    assert_eq!(files.nodes.unwrap().len(), 1);
}

#[test]
fn pull_request_reads_every_page_of_files() {
    let (client, http) = client("graphql_pull_request_files_paged");
    let pr = block_on(client.pull_request_paged("rozed-dev", "playground", 2, 1)).unwrap();
    let files = pr.files.unwrap();
    assert!(!files.page_info.has_next_page);
    let files: Vec<_> = files
        .nodes
        .unwrap()
        .into_iter()
        .flatten()
        .map(|file| (file.path, file.viewer_viewed_state))
        .collect();
    assert_eq!(
        files,
        [
            ("stack-b.md".to_string(), FileViewedState::VIEWED),
            ("docs/notes.md".to_string(), FileViewedState::UNVIEWED),
        ]
    );
    assert_eq!(http.seen().len(), 2);
}

#[test]
fn pull_request_check() {
    let (client, _) = client("graphql_pull_request_check_1");
    let check = block_on(client.pull_request_check("rozed-dev", "playground", 1)).unwrap();
    assert_eq!(check.head_ref_oid.len(), 40);
    assert_eq!(check.commits.total_count, 1);
    assert_eq!(check.changed_files, 1);
    // Joining a stack moved the timeline but not the PR's own updatedAt.
    assert!(check.timeline_items.updated_at > check.updated_at);
    // What the UI shows without the details: the playground has no checks, required reviews,
    // review threads or merge queue, and PR #1 can merge.
    assert_eq!(check.status_check_rollup, None);
    assert_eq!(check.review_decision, None);
    assert_eq!(check.merge_state_status, MergeStateStatus::CLEAN);
    assert_eq!(check.unresolved_threads(), 0);
    assert_eq!(check.merge_queue_entry, None);
    // The query asks for its own cost, which lands in the account's rate limits. Everything
    // above still costs a single point.
    let limits = client.rate_limits();
    assert_eq!(limits.last_graphql_cost, Some(1));
    assert!(limits.graphql().is_some());
}

#[test]
fn a_check_counts_its_unresolved_threads() {
    let check: PullRequestCheckData = serde_json::from_value(serde_json::json!({
        "updatedAt": "2026-11-06T08:15:07Z",
        "state": "OPEN",
        "isDraft": false,
        "baseRefName": "main",
        "baseRefOid": "6ce218eb1930c49db64d5744c76a6739d4525e82",
        "headRefOid": "b046244d9569964088e952243e5e557312322488",
        "reviewDecision": "CHANGES_REQUESTED",
        "mergeStateStatus": "BLOCKED",
        "statusCheckRollup": {"state": "PENDING"},
        "mergeQueueEntry": {"state": "QUEUED", "position": 2},
        "timelineItems": {"updatedAt": "2026-11-06T08:15:21Z", "totalCount": 2},
        "revisionEvents": {"filteredCount": 0},
        "commits": {"totalCount": 1},
        "reviews": {"totalCount": 1},
        "submittedReviews": {"totalCount": 1},
        "reviewThreads": {
            "totalCount": 3,
            "nodes": [
                {"isResolved": false, "comments": {"totalCount": 1}},
                {"isResolved": true, "comments": {"totalCount": 2}},
                {"isResolved": false, "comments": {"totalCount": 1}}
            ]
        },
        "viewerPendingReview": {"nodes": [{"viewerDidAuthor": true, "comments": {"totalCount": 1}}]},
        "comments": {"totalCount": 0},
        "changedFiles": 1,
        "lastEditedAt": null,
        "timelineEvents": {"filteredCount": 4, "updatedAt": "2026-11-06T08:15:21Z"}
    }))
    .unwrap();
    assert_eq!(check.unresolved_threads(), 2);
    assert_eq!(
        check.status_check_rollup.map(|rollup| rollup.state),
        Some(StatusState::PENDING)
    );
}

#[test]
fn open_pull_requests_with_their_stack() {
    let (client, http) = client("graphql_open_pull_requests_paged");
    let playground = RepoRef::new("rozed-dev", "playground");
    let list =
        block_on(client.open_pull_requests_paged(&playground, 1, RECENTLY_UPDATED_MERGED)).unwrap();
    assert_eq!(list.total_count, 2);
    assert!(!list.is_truncated());
    assert_eq!(http.seen().len(), 2);
    let layers: Vec<_> = list
        .pull_requests
        .iter()
        .map(|pr| {
            let entry = pr.stack_entry.as_ref().expect("both PRs are in stack #3");
            let stack = entry.stack.as_ref().unwrap();
            (
                pr.number,
                pr.base_ref_name.as_str(),
                pr.head_ref_name.as_str(),
                stack.number,
                entry.position,
            )
        })
        .collect();
    // Newest first, so the cap drops the oldest PRs rather than the newest.
    assert_eq!(
        layers,
        [
            (2, "stack-a", "stack-b", 3, 2),
            (1, "main", "stack-a", 3, 1)
        ]
    );
    // Layer 2's base is layer 1's head.
    assert_eq!(
        list.pull_requests[0].base_ref_oid,
        list.pull_requests[1].head_ref_oid
    );
    // The query asks for that order.
    let variables: serde_json::Value = serde_json::from_slice(&http.seen()[0].body).unwrap();
    let query = variables["query"].as_str().unwrap();
    assert!(
        query.contains("orderBy: { field: CREATED_AT, direction: DESC }"),
        "{query}"
    );
}

#[test]
fn a_missing_repository_is_not_found() {
    let (client, _) = client("graphql_not_found");
    let error = block_on(client.pull_request("rozed-dev", "does-not-exist", 1)).unwrap_err();
    let GithubError::NotFound { message, .. } = &error else {
        panic!("expected not found, got {error:?}");
    };
    assert!(message.contains("rozed-dev/does-not-exist"), "{message}");
}

#[test]
fn partial_data_keeps_the_errors_and_their_type() {
    let (client, _) = client("graphql_not_found");
    let (response, _) = block_on(client.graphql_partial::<github::queries::PullRequest>(
        github::queries::pull_request::Variables {
            owner: "rozed-dev".into(),
            name: "does-not-exist".into(),
            number: 1,
            files_first: 100,
            files_after: None,
        },
    ))
    .unwrap();
    assert_eq!(response.data.unwrap().repository, None);
    assert_eq!(response.errors[0].r#type.as_deref(), Some("NOT_FOUND"));
}

/// PR #2's one file and its viewed mark, and the pull request's node ID.
fn pr_2_marks(client: &GithubClient) -> (String, Vec<(String, FileViewedState)>) {
    let pr = block_on(client.pull_request("rozed-dev", "playground", 2)).unwrap();
    let files = pr
        .files
        .unwrap()
        .nodes
        .unwrap()
        .into_iter()
        .flatten()
        .map(|file| (file.path, file.viewer_viewed_state))
        .collect();
    (pr.id, files)
}

#[test]
fn a_file_is_marked_viewed_and_the_mark_cleared() {
    let (client, http) = client("graphql_viewed_marks");
    let (id, before) = pr_2_marks(&client);
    assert_eq!(
        before,
        [("stack-b.md".to_string(), FileViewedState::UNVIEWED)]
    );

    block_on(client.mark_file_as_viewed(&id, "stack-b.md")).unwrap();
    assert_eq!(
        pr_2_marks(&client).1,
        [("stack-b.md".to_string(), FileViewedState::VIEWED)]
    );

    block_on(client.unmark_file_as_viewed(&id, "stack-b.md")).unwrap();
    assert_eq!(
        pr_2_marks(&client).1,
        [("stack-b.md".to_string(), FileViewedState::UNVIEWED)]
    );

    // Each mutation names the pull request by its node ID, and the file by its path.
    let mutations: Vec<(String, serde_json::Value)> = http
        .seen()
        .iter()
        .filter_map(|request| {
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            let operation = body["operationName"].as_str()?.to_string();
            operation
                .ends_with("FileAsViewed")
                .then(|| (operation, body["variables"].clone()))
        })
        .collect();
    let variables = serde_json::json!({ "pullRequestId": id, "path": "stack-b.md" });
    assert_eq!(
        mutations,
        [
            ("MarkFileAsViewed".to_string(), variables.clone()),
            ("UnmarkFileAsViewed".to_string(), variables),
        ]
    );
}

#[test]
fn marking_a_file_of_a_pull_request_that_doesnt_exist_fails() {
    let (client, _) = client("graphql_mark_file_as_viewed_not_found");
    let error =
        block_on(client.mark_file_as_viewed(MISSING_PULL_REQUEST, "README.md")).unwrap_err();
    let GithubError::NotFound { message, .. } = &error else {
        panic!("expected not found, got {error:?}");
    };
    assert!(message.contains(MISSING_PULL_REQUEST), "{message}");
}

#[test]
fn marking_a_path_the_pull_request_doesnt_have_is_refused() {
    let (client, _) = client("graphql_mark_file_as_viewed_unlisted_path");
    let (id, before) = pr_2_marks(&client);

    let error =
        block_on(client.mark_file_as_viewed(&id, "not-in-this-pull-request.md")).unwrap_err();

    let GithubError::GraphQl { errors } = &error else {
        panic!("expected GitHub's refusal, got {error:?}");
    };
    assert_eq!(errors[0].r#type.as_deref(), Some("UNPROCESSABLE"));
    assert_eq!(errors[0].message, "Filepath must be part of pull request");
    // PR #2's one file is still unviewed.
    assert_eq!(
        before,
        [("stack-b.md".to_string(), FileViewedState::UNVIEWED)]
    );
    assert_eq!(pr_2_marks(&client).1, before);
}

/// A pull request node ID that names nothing.
const MISSING_PULL_REQUEST: &str = "PR_kwDOVk6v9M8AAAAAAAAAAA";

#[test]
fn a_viewed_mark_answer_without_its_pull_request_isnt_taken_as_done() {
    use synthetic::*;
    let variables = serde_json::json!({ "pullRequestId": "PR_x", "path": "a.rs" });
    let http = replay(vec![
        (
            "MarkFileAsViewed",
            variables.clone(),
            serde_json::json!({ "markFileAsViewed": { "pullRequest": null } }),
        ),
        (
            "UnmarkFileAsViewed",
            variables,
            serde_json::json!({ "unmarkFileAsViewed": null }),
        ),
    ]);
    let client = client_for(&http);
    for error in [
        block_on(client.mark_file_as_viewed("PR_x", "a.rs")).unwrap_err(),
        block_on(client.unmark_file_as_viewed("PR_x", "a.rs")).unwrap_err(),
    ] {
        assert!(matches!(error, GithubError::Decode(_)), "{error:?}");
    }
}

/// Synthetic GraphQL answers, for what GitHub can't be made to do on demand.
mod synthetic {
    use std::sync::Arc;

    use github::cassette::{Interaction, RecordedRequest, RecordedResponse};
    use github::replay::ReplayHttpClient;
    use serde_json::{Value, json};

    use super::{RECENTLY_UPDATED_MERGED, RepoRef};

    pub fn replay(answers: Vec<(&str, Value, Value)>) -> Arc<ReplayHttpClient> {
        Arc::new(
            ReplayHttpClient::from_interactions(
                "synthetic",
                answers
                    .into_iter()
                    .map(|(operation, variables, data)| Interaction {
                        request: RecordedRequest {
                            method: "POST".into(),
                            url: "https://api.github.com/graphql".into(),
                            operation: Some(operation.into()),
                            variables: Some(variables),
                            ..Default::default()
                        },
                        response: RecordedResponse {
                            status: 200,
                            headers: Default::default(),
                            body: Some(json!({ "data": data })),
                            body_text: None,
                        },
                    }),
            )
            .unwrap(),
        )
    }

    /// The variables of PR #2's page of one file after `after`.
    pub fn files_after(after: Option<&str>) -> Value {
        json!({"owner": "o", "name": "r", "number": 2, "filesFirst": 1, "filesAfter": after})
    }

    /// PR #2 at `head`, with a page of files.
    pub fn pr_page(head: &str, files: &[&str], has_next: bool, end_cursor: Option<&str>) -> Value {
        json!({"repository": {"pullRequest": {
            "id": "PR_x", "number": 2, "title": "t", "url": "https://github.com/o/r/pull/2",
            "state": "OPEN", "isDraft": false, "updatedAt": "2026-11-06T08:15:08Z",
            "author": null, "baseRefName": "main", "baseRefOid": "b".repeat(40),
            "headRefName": "topic", "headRefOid": head, "isCrossRepository": false,
            "headRepository": null,
            "files": {
                "totalCount": 2,
                "pageInfo": {"hasNextPage": has_next, "endCursor": end_cursor},
                "nodes": files.iter().map(|path| json!({"path": path, "viewerViewedState": "UNVIEWED"})).collect::<Vec<_>>(),
            },
        }}})
    }

    pub fn repo() -> RepoRef {
        RepoRef::new("o", "r")
    }

    /// The variables of a page of one open PR after `after`. The first page also asks for the
    /// merged pull requests updated most recently.
    pub fn open_after(after: Option<&str>) -> Value {
        let merged = if after.is_none() {
            RECENTLY_UPDATED_MERGED
        } else {
            0
        };
        json!({"owner": "o", "name": "r", "first": 1, "after": after, "merged": merged})
    }

    /// A page of open PRs, numbered `numbers`, each targeting `main`.
    pub fn open_page(numbers: &[i64], has_next: bool, end_cursor: Option<&str>) -> Value {
        let nodes: Vec<Value> = (numbers.iter())
            .map(|number| {
                json!({
                    "id": format!("PR_{number}"), "number": number, "title": "t",
                    "isDraft": false, "createdAt": "2026-11-06T08:15:08Z",
                    "updatedAt": "2026-11-06T08:15:08Z", "author": null, "baseRefName": "main",
                    "baseRefOid": "b".repeat(40), "baseRef": null,
                    "headRefName": format!("pr-{number}"), "headRefOid": "c".repeat(40),
                    "headRef": null, "isCrossRepository": false, "headRepositoryOwner": null,
                    "commits": {"nodes": []}, "stackEntry": null,
                })
            })
            .collect();
        json!({
            "rateLimit": null,
            "repository": {
                "defaultBranchRef": {"name": "main"},
                "pullRequests": {
                    "totalCount": 3,
                    "pageInfo": {"hasNextPage": has_next, "endCursor": end_cursor},
                    "nodes": nodes,
                },
                "mergedPullRequests": {"nodes": []},
            },
        })
    }
}

fn client_for(http: &std::sync::Arc<ReplayHttpClient>) -> GithubClient {
    GithubClient::new(http.clone(), Token::new("gho_FAKE"))
}

#[test]
fn files_without_a_cursor_to_go_on_from_stop() {
    use synthetic::*;
    let http = replay(vec![(
        "PullRequest",
        files_after(None),
        pr_page("a", &["one"], true, None),
    )]);
    let error = block_on(client_for(&http).pull_request_paged("o", "r", 2, 1)).unwrap_err();
    assert!(matches!(error, GithubError::Inconsistent(_)), "{error:?}");
    assert_eq!(http.seen().len(), 1);
}

#[test]
fn a_page_of_files_that_repeats_stops_the_paging() {
    use synthetic::*;
    let http = replay(vec![
        (
            "PullRequest",
            files_after(None),
            pr_page("a", &["one"], true, Some("c1")),
        ),
        (
            "PullRequest",
            files_after(Some("c1")),
            pr_page("a", &["two"], true, Some("c1")),
        ),
    ]);
    let error = block_on(client_for(&http).pull_request_paged("o", "r", 2, 1)).unwrap_err();
    assert!(matches!(error, GithubError::Inconsistent(_)), "{error:?}");
    assert_eq!(http.seen().len(), 2);
}

#[test]
fn files_are_read_again_when_a_push_lands_between_pages() {
    use synthetic::*;
    let (old, new) = ("a".repeat(40), "d".repeat(40));
    let http = replay(vec![
        (
            "PullRequest",
            files_after(None),
            pr_page(&old, &["one"], true, Some("c1")),
        ),
        (
            "PullRequest",
            files_after(None),
            pr_page(&new, &["one-new"], true, Some("c1")),
        ),
        (
            "PullRequest",
            files_after(Some("c1")),
            pr_page(&new, &["two-new"], false, Some("c2")),
        ),
    ]);
    let pr = block_on(client_for(&http).pull_request_paged("o", "r", 2, 1)).unwrap();
    assert_eq!(pr.head_ref_oid, new);
    let files: Vec<_> = pr
        .files
        .unwrap()
        .nodes
        .unwrap()
        .into_iter()
        .flatten()
        .map(|file| file.path)
        .collect();
    // Never the old head's first page with the new head's second.
    assert_eq!(files, ["one-new", "two-new"]);
    assert_eq!(http.seen().len(), 4);
}

#[test]
fn open_pull_requests_without_a_cursor_to_go_on_from_stop() {
    use synthetic::*;
    let http = replay(vec![(
        "OpenPullRequests",
        open_after(None),
        open_page(&[3], true, None),
    )]);
    let error = block_on(client_for(&http).open_pull_requests_paged(
        &synthetic::repo(),
        1,
        RECENTLY_UPDATED_MERGED,
    ))
    .unwrap_err();
    assert!(matches!(error, GithubError::Inconsistent(_)), "{error:?}");
}

#[test]
fn open_pull_requests_that_repeat_stop_the_paging() {
    use synthetic::*;
    let http = replay(vec![
        (
            "OpenPullRequests",
            open_after(None),
            open_page(&[3], true, Some("c1")),
        ),
        (
            "OpenPullRequests",
            open_after(Some("c1")),
            open_page(&[2], true, Some("c1")),
        ),
    ]);
    let error = block_on(client_for(&http).open_pull_requests_paged(
        &synthetic::repo(),
        1,
        RECENTLY_UPDATED_MERGED,
    ))
    .unwrap_err();
    assert!(matches!(error, GithubError::Inconsistent(_)), "{error:?}");
    assert_eq!(http.seen().len(), 2);
}

#[test]
fn null_pull_requests_dont_make_the_list_look_capped() {
    use synthetic::*;
    // GitHub leaves out a PR it can't show (a null node); the list isn't capped for it.
    let mut page = open_page(&[3, 2], false, Some("c1"));
    page["repository"]["pullRequests"]["nodes"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::Value::Null);
    let http = replay(vec![("OpenPullRequests", open_after(None), page)]);
    let list = block_on(client_for(&http).open_pull_requests_paged(
        &synthetic::repo(),
        1,
        RECENTLY_UPDATED_MERGED,
    ))
    .unwrap();
    assert_eq!(list.pull_requests.len(), 2);
    assert!(!list.is_truncated());
}

#[test]
fn an_error_on_a_field_that_can_be_null_keeps_the_rest() {
    use synthetic::*;
    // A fork in an org that restricts OAuth apps: GitHub nulls the head repository, explains it,
    // and returns the rest of the pull request.
    let mut page = pr_page("a", &["one"], false, None);
    page["repository"]["pullRequest"]["files"]["totalCount"] = 1.into();
    let http = std::sync::Arc::new(
        ReplayHttpClient::from_interactions(
            "partial",
            [github::cassette::Interaction {
                request: github::cassette::RecordedRequest {
                    method: "POST".into(),
                    url: "https://api.github.com/graphql".into(),
                    operation: Some("PullRequest".into()),
                    variables: Some(files_after(None)),
                    ..Default::default()
                },
                response: github::cassette::RecordedResponse {
                    status: 200,
                    headers: Default::default(),
                    body: Some(serde_json::json!({
                        "data": page,
                        "errors": [{
                            "type": "FORBIDDEN",
                            "path": ["repository", "pullRequest", "headRepository"],
                            "message": "Although you appear to have the correct authorization credentials, the `acme` organization has enabled OAuth App access restrictions"
                        }],
                    })),
                    body_text: None,
                },
            }],
        )
        .unwrap(),
    );
    let pr = block_on(client_for(&http).pull_request_paged("o", "r", 2, 1)).unwrap();
    assert_eq!(pr.number, 2);
    assert_eq!(pr.head_repository, None);
}

#[test]
fn an_error_on_what_was_asked_for_is_the_error() {
    use synthetic::*;
    // The pull request itself is forbidden: that's the answer, not a missing field.
    let http = std::sync::Arc::new(
        ReplayHttpClient::from_interactions(
            "forbidden",
            [github::cassette::Interaction {
                request: github::cassette::RecordedRequest {
                    method: "POST".into(),
                    url: "https://api.github.com/graphql".into(),
                    operation: Some("PullRequest".into()),
                    variables: Some(files_after(None)),
                    ..Default::default()
                },
                response: github::cassette::RecordedResponse {
                    status: 200,
                    headers: Default::default(),
                    body: Some(serde_json::json!({
                        "data": {"repository": {"pullRequest": null}},
                        "errors": [{
                            "type": "FORBIDDEN",
                            "path": ["repository", "pullRequest"],
                            "message": "Although you appear to have the correct authorization credentials, the `acme` organization has enabled OAuth App access restrictions"
                        }],
                    })),
                    body_text: None,
                },
            }],
        )
        .unwrap(),
    );
    let error = block_on(client_for(&http).pull_request_paged("o", "r", 2, 1)).unwrap_err();
    assert!(
        matches!(error, GithubError::OAuthAppRestricted { .. }),
        "{error:?}"
    );
}
