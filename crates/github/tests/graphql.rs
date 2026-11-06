//! The typed GraphQL queries on responses recorded from `rozed-dev/playground`, where PR #1
//! (`stack-a` → `main`) and PR #2 (`stack-b` → `stack-a`) form stack #3.

use futures::executor::block_on;
use github::queries::{FileViewedState, PullRequestState};
use github::replay::ReplayHttpClient;
use github::{GithubClient, GithubError, Token};

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
    // The query asks for its own cost, which lands in the account's rate limits.
    let limits = client.rate_limits();
    assert_eq!(limits.last_graphql_cost, Some(1));
    assert!(limits.graphql().is_some());
}

#[test]
fn open_pull_requests_with_their_stack() {
    let (client, http) = client("graphql_open_pull_requests_paged");
    let list = block_on(client.open_pull_requests_paged("rozed-dev", "playground", 1)).unwrap();
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
