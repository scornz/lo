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
use github::record::RecordingHttpClient;
use github::{Conditional, GithubClient, Token};

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
            schema: None,
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
