//! GitHub's REST view of a pull request (`GET /repos/{owner}/{repo}/pulls/{number}`): what the tab
//! needs to copy it locally, and when it opened. Signed in, it's read with the account's client;
//! without an account, anonymously, which works for public repositories only.

use std::sync::Arc;

use anyhow::{Context as _, bail};
use chrono::{DateTime, Utc};
use futures::AsyncReadExt as _;
use github::{GithubClient, GithubError};
use http_client::http::{HeaderMap, StatusCode};
use http_client::{AsyncBody, HttpClient, HttpRequestExt as _, RedirectPolicy, Request};
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

/// `GET /repos/{owner}/{repo}/pulls/{number}` without signing in. It works for public
/// repositories only and counts against GitHub's 60 requests an hour per IP address.
pub async fn fetch_pull_request(
    http: Arc<dyn HttpClient>,
    link: &PrLink,
    can_sign_in: CanSignIn,
) -> anyhow::Result<PullRequest> {
    let url = format!(
        "https://api.github.com/repos/{}/{}/pulls/{}",
        link.owner, link.repo, link.number
    );
    let request = Request::get(&url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        // A renamed repository answers with a redirect to its new name.
        .follow_redirects(RedirectPolicy::FollowAll)
        .body(AsyncBody::empty())?;
    let mut response = http
        .send(request)
        .await
        .with_context(|| format!("asking GitHub for {link}"))?;
    let mut body = String::new();
    response
        .body_mut()
        .read_to_string(&mut body)
        .await
        .with_context(|| format!("reading GitHub's answer for {link}"))?;
    check_status(
        response.status(),
        response.headers(),
        &body,
        link,
        can_sign_in,
    )?;
    parse_pull_request(&body).with_context(|| format!("reading GitHub's answer for {link}"))
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
    use http_client::{Response, Url};

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
        futures::executor::block_on(fetch_pull_request(http, &fd_2212(), CanSignIn::Yes))
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
        assert_eq!(
            request.extensions().get::<RedirectPolicy>(),
            Some(&RedirectPolicy::FollowAll)
        );
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
        let fetch =
            |http| futures::executor::block_on(fetch_pull_request(http, &fd_2212(), CanSignIn::No));
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
