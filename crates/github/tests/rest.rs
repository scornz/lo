//! The REST client on responses recorded from `rozed-dev/playground`.

use futures::executor::block_on;
use github::replay::ReplayHttpClient;
use github::{Conditional, GithubClient, GithubError, Token};

const PULL: &str = "/repos/rozed-dev/playground/pulls/1";

#[test]
fn a_conditional_get_returns_not_modified_when_nothing_changed() {
    let http = ReplayHttpClient::cassette("rest_pull_conditional");
    let client = GithubClient::new(http.clone(), Token::new("gho_FAKE"));

    let first = block_on(client.get_conditional::<serde_json::Value>(PULL, None)).unwrap();
    let Conditional::Modified { value, etag } = first else {
        panic!("expected the PR, got {first:?}");
    };
    assert_eq!(value["number"], 1);
    assert_eq!(value["head"]["ref"], "stack-a");
    let etag = etag.expect("GitHub sends an ETag for a PR");

    let second = block_on(client.get_conditional::<serde_json::Value>(PULL, Some(&etag))).unwrap();
    assert_eq!(second, Conditional::NotModified);

    let seen = http.seen();
    assert_eq!(seen[0].header("if-none-match"), None);
    assert_eq!(seen[1].header("if-none-match"), Some(etag.as_str()));
    assert_eq!(seen[1].header("authorization"), Some("token gho_FAKE"));
    // The 304 still reports the limit.
    assert!(client.rate_limits().core().is_some());
}

#[test]
fn user_reads_the_login_and_scopes() {
    let http = ReplayHttpClient::cassette("user");
    let client = GithubClient::new(http, Token::new("gho_FAKE"));
    let user = block_on(client.user()).unwrap();
    assert_eq!(user.user.login, "rozed-bot");
    assert_eq!(user.user.id, 353226052);
    assert_eq!(user.scopes, ["read:org", "repo", "workflow"]);
    assert!(user.missing_scopes().is_empty());
}

#[test]
fn a_missing_repository_is_not_found() {
    let http = ReplayHttpClient::cassette("rest_not_found");
    let client = GithubClient::new(http, Token::new("gho_FAKE"));
    let error =
        block_on(client.get::<serde_json::Value>("/repos/rozed-dev/does-not-exist")).unwrap_err();
    let GithubError::NotFound {
        message,
        documentation_url,
    } = error
    else {
        panic!("expected not found, got {error:?}");
    };
    assert_eq!(message, "Not Found");
    assert!(
        documentation_url
            .unwrap()
            .starts_with("https://docs.github.com/")
    );
}

/// Every committed fixture is free of tokens. The recorder checks as it writes; this catches a
/// hand-edited or synthetic fixture.
#[test]
fn fixtures_hold_no_tokens() {
    let mut checked = 0;
    for cassette in std::fs::read_dir(github::cassette::fixtures_dir()).unwrap() {
        let cassette = cassette.unwrap().path();
        for file in std::fs::read_dir(&cassette).unwrap() {
            let path = file.unwrap().path();
            let bytes = std::fs::read(&path).unwrap();
            if let Some(what) = github::cassette::find_secret(&bytes, &[]) {
                panic!("{} contains {what}", path.display());
            }
            checked += 1;
        }
    }
    assert!(checked > 0);
}
