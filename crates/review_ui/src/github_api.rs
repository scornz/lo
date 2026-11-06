//! GitHub's REST view of a pull request (`GET /repos/{owner}/{repo}/pulls/{number}`): what the tab
//! needs to copy it locally, and when it opened. Signed in, it's read with the account's client;
//! without an account, anonymously, which works for public repositories only.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, bail};
use chrono::{DateTime, Utc};
use futures::FutureExt as _;
use github::{GithubClient, GithubError};
use gpui::{App, BackgroundExecutor, Global};
use http_client::http::{HeaderMap, StatusCode};
use http_client::{AsyncBody, HttpClient, HttpRequestExt as _, Request};
use serde::Deserialize;

use crate::pr_link::PrLink;

/// The parts of GitHub's pull request object rozed reads.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PullRequest {
    pub title: String,
    /// The author. GitHub leaves it null for a deleted account.
    pub user: Option<User>,
    /// When the pull request was opened, where its revisions start.
    pub created_at: DateTime<Utc>,
    pub head: Head,
    pub base: Base,
    /// How many files it changes. GitHub lists no more than 3,000 of them.
    #[serde(default)]
    pub changed_files: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct User {
    pub login: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Head {
    pub sha: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Base {
    /// The branch the pull request merges into.
    #[serde(rename = "ref")]
    pub branch: String,
    /// The base commit GitHub compared against, which stands in for the branch once it's gone.
    pub sha: String,
    pub repo: BaseRepo,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BaseRepo {
    /// The repository's canonical `owner/name`, which names its local copy. A link to a renamed
    /// or transferred repository still names the old one.
    pub full_name: String,
    /// Where to clone the repository from. Pull requests from forks are fetched from here too,
    /// through `refs/pull/<n>/head`.
    pub clone_url: String,
}

impl BaseRepo {
    /// The owner and name in [`Self::full_name`].
    pub fn owner_and_name(&self) -> anyhow::Result<(&str, &str)> {
        self.full_name
            .split_once('/')
            .filter(|(owner, name)| !owner.is_empty() && !name.is_empty() && !name.contains('/'))
            .with_context(|| format!("GitHub named the repository {:?}", self.full_name))
    }
}

/// Reads the pull request with `login`'s client.
pub async fn fetch_as(
    client: &GithubClient,
    login: &str,
    link: &PrLink,
) -> anyhow::Result<PullRequest> {
    let path = format!("/repos/{}/{}/pulls/{}", link.owner, link.repo, link.number);
    match client.get::<PullRequest>(&path).await {
        Ok(pull_request) => {
            checked(pull_request).with_context(|| format!("reading GitHub's answer for {link}"))
        }
        Err(GithubError::NotFound { .. }) => {
            bail!("GitHub has no pull request {link}, or {login} can't see it")
        }
        Err(error) => Err(anyhow::Error::new(error).context(format!("asking GitHub for {link}"))),
    }
}

/// Whether rozed can sign in to GitHub, which it can't when it couldn't read its accounts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CanSignIn {
    Yes,
    No,
}

/// The app's one client without an account. GitHub allows 60 requests an hour per IP address
/// without a token, and this client keeps what GitHub last said about that limit for every tab.
struct Anonymous {
    http: Arc<dyn HttpClient>,
    client: GithubClient,
}

impl Global for Anonymous {}

/// The app's client without an account, made anew only if the app's HTTP client changed.
pub(crate) fn anonymous_client(cx: &mut App) -> GithubClient {
    let http = cx.http_client();
    if let Some(anonymous) = cx.try_global::<Anonymous>()
        && std::ptr::addr_eq(Arc::as_ptr(&anonymous.http), Arc::as_ptr(&http))
    {
        return anonymous.client.clone();
    }
    let client = GithubClient::anonymous(http.clone());
    cx.set_global(Anonymous {
        http,
        client: client.clone(),
    });
    client
}

/// `GET /repos/{owner}/{repo}/pulls/{number}` with `client`, the app's client without an account
/// ([`anonymous_client`]). It works for public repositories only and counts against GitHub's 60
/// requests an hour per IP address.
pub async fn fetch_pull_request(
    client: &GithubClient,
    link: &PrLink,
    can_sign_in: CanSignIn,
) -> anyhow::Result<PullRequest> {
    let path = format!("/repos/{}/{}/pulls/{}", link.owner, link.repo, link.number);
    // A renamed repository answers with a redirect to its new name, which the client follows.
    let response = client
        .get_answer(&path)
        .await
        .with_context(|| format!("asking GitHub for {link}"))?;
    let body = String::from_utf8_lossy(&response.body);
    check_status(response.status, &response.headers, &body, link, can_sign_in)?;
    parse_pull_request(&body).with_context(|| format!("reading GitHub's answer for {link}"))
}

/// How long the check that GitHub answers waits for an answer.
pub const ANSWER_TIMEOUT: Duration = Duration::from_secs(15);

/// Whether it's worth loading from GitHub again: `GET /rate_limit` without signing in, which
/// doesn't count against any rate limit, got an answer with any status within
/// [`ANSWER_TIMEOUT`], or failed in a way waiting won't fix, such as a proxy that refuses it,
/// which loading then reports. `executor`'s clock bounds the wait too, whatever the client does
/// with the request's own timeout.
pub async fn github_answers(http: Arc<dyn HttpClient>, executor: BackgroundExecutor) -> bool {
    let request = Request::get("https://api.github.com/rate_limit")
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .timeout(ANSWER_TIMEOUT)
        .body(AsyncBody::empty());
    let Ok(request) = request else {
        return false;
    };
    let send = http.send(request).fuse();
    let timer = executor.timer(ANSWER_TIMEOUT).fuse();
    futures::pin_mut!(send, timer);
    futures::select_biased! {
        answer = send => match answer {
            Ok(_) => true,
            Err(error) => !GithubError::Network(error).is_retryable(),
        },
        _ = timer => false,
    }
}

/// Turns an unsuccessful answer into an error that says what to do about it.
fn check_status(
    status: StatusCode,
    headers: &HeaderMap,
    body: &str,
    link: &PrLink,
    can_sign_in: CanSignIn,
) -> anyhow::Result<()> {
    if status.is_success() {
        return Ok(());
    }
    let rate_limited = headers
        .get("x-ratelimit-remaining")
        .is_some_and(|remaining| remaining == "0");
    if rate_limited && matches!(status.as_u16(), 403 | 429) {
        match can_sign_in {
            CanSignIn::Yes => bail!(
                "GitHub's limit of 60 requests an hour without signing in is used up; sign in \
                 from Accounts… in the rozed menu, or try again later"
            ),
            CanSignIn::No => bail!(
                "GitHub's limit of 60 requests an hour without signing in is used up; try again \
                 later"
            ),
        }
    }
    if status == StatusCode::NOT_FOUND {
        match can_sign_in {
            CanSignIn::Yes => bail!(
                "GitHub has no public pull request {link}, and no account rozed is signed in to \
                 can see it. Sign in from Accounts… in the rozed menu"
            ),
            CanSignIn::No => bail!(
                "GitHub has no public pull request {link}, and rozed couldn't read its accounts \
                 to sign in"
            ),
        }
    }
    let message = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|body| body.get("message")?.as_str().map(str::to_string))
        .unwrap_or_default();
    bail!("GitHub answered {status} for {link}: {message}");
}

fn parse_pull_request(body: &str) -> anyhow::Result<PullRequest> {
    checked(serde_json::from_str(body)?)
}

/// `pull_request`, if its repository is on github.com and named `owner/name`.
fn checked(pull_request: PullRequest) -> anyhow::Result<PullRequest> {
    let clone_url = &pull_request.base.repo.clone_url;
    anyhow::ensure!(
        clone_url.starts_with("https://github.com/"),
        "the repository's clone URL {clone_url} isn't on github.com"
    );
    pull_request.base.repo.owner_and_name()?;
    Ok(pull_request)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Mutex;

    use futures::future::BoxFuture;
    use http_client::http::HeaderValue;
    use http_client::{AsyncBody, RedirectPolicy, Request, Response, Url};

    /// GitHub's answer for sharkdp/fd#2212, trimmed to the fields rozed reads and a few around
    /// them.
    const FD_2212: &str = r#"{
      "number": 2212,
      "title": "Honor explicit use-jemalloc requests",
      "state": "open",
      "user": { "login": "voidsipp", "id": 1 },
      "created_at": "2026-03-02T10:20:30Z",
      "head": {
        "sha": "22c70acc6992c0c51cb68527b87adf114d01acbc",
        "ref": "fix/1822-explicit-jemalloc",
        "repo": { "full_name": "voidsipp/fd", "clone_url": "https://github.com/voidsipp/fd.git" }
      },
      "base": {
        "sha": "cc4e40fa4e32d61733cd72926e233a3fb6c8d56d",
        "ref": "master",
        "repo": { "full_name": "sharkdp/fd", "clone_url": "https://github.com/sharkdp/fd.git" }
      }
    }"#;

    fn fd_2212() -> PrLink {
        "https://github.com/sharkdp/fd/pull/2212".parse().unwrap()
    }

    /// Answers every request with one canned response and remembers the request.
    struct Canned {
        status: u16,
        headers: Vec<(&'static str, &'static str)>,
        body: &'static str,
        request: Mutex<Option<Request<AsyncBody>>>,
    }

    impl Canned {
        fn new(status: u16, body: &'static str) -> Arc<Self> {
            Self::with_headers(status, &[], body)
        }

        fn with_headers(
            status: u16,
            headers: &[(&'static str, &'static str)],
            body: &'static str,
        ) -> Arc<Self> {
            Arc::new(Self {
                status,
                headers: headers.to_vec(),
                body,
                request: Mutex::new(None),
            })
        }
    }

    impl HttpClient for Canned {
        fn user_agent(&self) -> Option<&HeaderValue> {
            None
        }

        fn proxy(&self) -> Option<&Url> {
            None
        }

        fn send(
            &self,
            request: Request<AsyncBody>,
        ) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>> {
            *self.request.lock().unwrap() = Some(request);
            let mut response = Response::builder().status(self.status);
            for (name, value) in &self.headers {
                response = response.header(*name, *value);
            }
            let response = response.body(AsyncBody::from(self.body)).unwrap();
            Box::pin(async move { Ok(response) })
        }
    }

    fn fetch(http: Arc<Canned>) -> anyhow::Result<PullRequest> {
        let client = GithubClient::anonymous(http);
        futures::executor::block_on(fetch_pull_request(&client, &fd_2212(), CanSignIn::Yes))
    }

    fn fetch_signed_in(http: Arc<Canned>) -> anyhow::Result<PullRequest> {
        let client = GithubClient::new(http, github::Token::new("gho_secret"));
        futures::executor::block_on(fetch_as(&client, "rozed-bot", &fd_2212()))
    }

    #[test]
    fn reads_the_base_branch_head_and_clone_url() {
        let http = Canned::new(200, FD_2212);
        let pull_request = fetch(http.clone()).unwrap();
        assert_eq!(
            pull_request,
            PullRequest {
                title: "Honor explicit use-jemalloc requests".into(),
                user: Some(User {
                    login: "voidsipp".into()
                }),
                created_at: "2026-03-02T10:20:30Z".parse().unwrap(),
                head: Head {
                    sha: "22c70acc6992c0c51cb68527b87adf114d01acbc".into()
                },
                base: Base {
                    branch: "master".into(),
                    sha: "cc4e40fa4e32d61733cd72926e233a3fb6c8d56d".into(),
                    repo: BaseRepo {
                        full_name: "sharkdp/fd".into(),
                        clone_url: "https://github.com/sharkdp/fd.git".into()
                    },
                },
                changed_files: None,
            }
        );

        let request = http.request.lock().unwrap().take().unwrap();
        assert_eq!(
            request.uri(),
            "https://api.github.com/repos/sharkdp/fd/pulls/2212"
        );
        assert_eq!(request.method(), "GET");
        assert_eq!(request.headers()["Accept"], "application/vnd.github+json");
        assert!(request.headers().get("Authorization").is_none());
        // A renamed repository's redirect is followed.
        assert!(matches!(
            request.extensions().get::<RedirectPolicy>(),
            Some(RedirectPolicy::FollowLimit(_))
        ));
    }

    #[test]
    fn signed_in_it_reads_with_the_accounts_client() {
        let http = Canned::new(200, FD_2212);
        let pull_request = fetch_signed_in(http.clone()).unwrap();
        assert_eq!(pull_request, fetch(Canned::new(200, FD_2212)).unwrap());
        let request = http.request.lock().unwrap().take().unwrap();
        assert_eq!(
            request.uri(),
            "https://api.github.com/repos/sharkdp/fd/pulls/2212"
        );
        assert_eq!(request.headers()["Authorization"], "token gho_secret");

        let error = fetch_signed_in(Canned::new(404, r#"{"message":"Not Found"}"#)).unwrap_err();
        assert_eq!(
            error.to_string(),
            "GitHub has no pull request sharkdp/fd#2212, or rozed-bot can't see it"
        );
        let error = fetch_signed_in(Canned::new(500, r#"{"message":"Server Error"}"#)).unwrap_err();
        assert_eq!(
            format!("{error:#}"),
            "asking GitHub for sharkdp/fd#2212: GitHub answered 500: Server Error"
        );
    }

    #[test]
    fn a_deleted_author_is_no_author() {
        let body = FD_2212.replace(r#"{ "login": "voidsipp", "id": 1 }"#, "null");
        assert_eq!(parse_pull_request(&body).unwrap().user, None);
    }

    #[test]
    fn refuses_a_repository_name_that_isnt_owner_slash_name() {
        for name in ["fd", "/fd", "sharkdp/", "a/b/c"] {
            let body = FD_2212.replace("\"sharkdp/fd\"", &format!("{name:?}"));
            let error = parse_pull_request(&body).unwrap_err();
            assert!(
                error.to_string().contains("named the repository"),
                "{error}"
            );
        }
    }

    #[test]
    fn a_missing_or_private_pull_request_says_to_sign_in() {
        let error = fetch(Canned::new(404, r#"{"message":"Not Found"}"#)).unwrap_err();
        assert!(
            error.to_string().contains("Sign in from Accounts…"),
            "{error}"
        );
    }

    #[test]
    fn without_an_accounts_database_it_doesnt_say_to_sign_in() {
        let fetch = |http| {
            let client = GithubClient::anonymous(http);
            futures::executor::block_on(fetch_pull_request(&client, &fd_2212(), CanSignIn::No))
        };
        let error = fetch(Canned::new(404, r#"{"message":"Not Found"}"#)).unwrap_err();
        assert_eq!(
            error.to_string(),
            "GitHub has no public pull request sharkdp/fd#2212, and rozed couldn't read its \
             accounts to sign in"
        );
        let rate_limited = Canned::with_headers(
            403,
            &[("x-ratelimit-remaining", "0")],
            r#"{"message":"API rate limit exceeded"}"#,
        );
        let error = fetch(rate_limited).unwrap_err();
        assert_eq!(
            error.to_string(),
            "GitHub's limit of 60 requests an hour without signing in is used up; try again later"
        );
    }

    #[test]
    fn a_used_up_rate_limit_says_so() {
        let http = Canned::with_headers(
            403,
            &[("x-ratelimit-remaining", "0")],
            r#"{"message":"API rate limit exceeded"}"#,
        );
        let error = fetch(http).unwrap_err();
        assert!(error.to_string().contains("try again later"), "{error}");
    }

    #[test]
    fn other_errors_carry_githubs_message() {
        let http = Canned::new(500, r#"{"message":"Server Error"}"#);
        let error = fetch(http).unwrap_err();
        assert_eq!(
            error.to_string(),
            "GitHub answered 500 Internal Server Error for sharkdp/fd#2212: Server Error"
        );
    }

    #[test]
    fn refuses_a_clone_url_off_github() {
        let body = FD_2212.replace(
            "https://github.com/sharkdp/fd.git",
            "https://example.com/fd.git",
        );
        let error = parse_pull_request(&body).unwrap_err();
        assert!(error.to_string().contains("isn't on github.com"), "{error}");
    }

    #[test]
    fn an_unexpected_body_is_an_error() {
        let error = fetch(Canned::new(200, "<html>")).unwrap_err();
        assert!(
            format!("{error:#}").contains("reading GitHub's answer"),
            "{error:#}"
        );
    }
}
