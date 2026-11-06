//! The REST client on responses recorded from `rozed-dev/playground`.

use futures::executor::block_on;
use github::files::FileStatus;
use github::patch::HunkLine;
use github::replay::ReplayHttpClient;
use github::{Conditional, GithubClient, GithubError, PullRef, Token};

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

/// Recorded alone, with `GET /rate_limit` (itself free) before and after: the plain GET cost 1,
/// and three 304s cost nothing.
#[test]
fn a_304_costs_no_rate_limit() {
    let http = ReplayHttpClient::cassette("rest_pull_304_rate_limit");
    let client = GithubClient::new(http, Token::new("gho_FAKE"));
    let core_used = || {
        let limits: serde_json::Value = block_on(client.get("/rate_limit")).unwrap();
        limits["resources"]["core"]["used"].as_u64().unwrap()
    };
    let before = core_used();
    let Conditional::Modified { etag, .. } =
        block_on(client.get_conditional::<serde_json::Value>(PULL, None)).unwrap()
    else {
        panic!("expected the PR");
    };
    let after_200 = core_used();
    for _ in 0..3 {
        let answer =
            block_on(client.get_conditional::<serde_json::Value>(PULL, etag.as_deref())).unwrap();
        assert_eq!(answer, Conditional::NotModified);
    }
    let after_304s = core_used();
    assert_eq!(after_200, before + 1);
    assert_eq!(after_304s, after_200);
}

#[test]
fn a_pull_requests_files_come_with_their_patches() {
    let http = ReplayHttpClient::cassette("rest_pull_files");
    let client = GithubClient::new(http.clone(), Token::new("gho_FAKE"));
    let files = block_on(client.pull_files(&PullRef::new("rozed-dev", "playground", 2))).unwrap();

    let [file] = &files[..] else {
        panic!("expected PR #2's one file, got {files:?}");
    };
    assert_eq!(file.filename, "stack-b.md");
    assert_eq!(file.status, FileStatus::Added);
    // The head GitHub listed it at, PR #2's.
    assert_eq!(
        file.head(),
        Some("d712aa7652cc723fee5fd9b3eba867b0480d40da")
    );
    assert_eq!((file.additions, file.deletions), (1, 0));
    let patch = file.parsed_patch().unwrap().unwrap();
    let [hunk] = &patch.hunks[..] else {
        panic!("expected one hunk, got {patch:?}");
    };
    assert_eq!((hunk.old.clone(), hunk.new.clone()), (0..0, 0..1));
    assert_eq!(
        hunk.lines,
        [HunkLine::Added(
            "Layer B of the rozed stack fixture.".into()
        )]
    );
    assert!(!patch.new_missing_newline);

    let seen = http.seen();
    assert_eq!(
        seen[0].url,
        "https://api.github.com/repos/rozed-dev/playground/pulls/2/files?per_page=100"
    );
    assert_eq!(seen[0].header("authorization"), Some("token gho_FAKE"));
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
