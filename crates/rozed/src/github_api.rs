//! The one GitHub API call the spike makes: reading a public pull request, without signing in.
//! Milestone 3's `github` crate replaces it.

use std::sync::Arc;

use anyhow::{Context as _, bail};
use futures::AsyncReadExt as _;
use http_client::http::{HeaderMap, StatusCode};
use http_client::{AsyncBody, HttpClient, HttpRequestExt as _, RedirectPolicy, Request};
use serde::Deserialize;

use crate::pr_link::PrLink;

/// The parts of GitHub's pull request object rozed reads.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PullRequest {
    pub title: String,
    pub head: Head,
    pub base: Base,
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
    pub repo: BaseRepo,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BaseRepo {
    /// Where to clone the repository from. Pull requests from forks are fetched from here too,
    /// through `refs/pull/<n>/head`.
    pub clone_url: String,
}

/// `GET /repos/{owner}/{repo}/pulls/{number}`, unauthenticated, so it works for public
/// repositories only and counts against GitHub's 60 requests an hour per IP address.
pub async fn fetch_pull_request(
    http: Arc<dyn HttpClient>,
    link: &PrLink,
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
    check_status(response.status(), response.headers(), &body, link)?;
    parse_pull_request(&body).with_context(|| format!("reading GitHub's answer for {link}"))
}

/// Turns an unsuccessful answer into an error that says what to do about it.
fn check_status(
    status: StatusCode,
    headers: &HeaderMap,
    body: &str,
    link: &PrLink,
) -> anyhow::Result<()> {
    if status.is_success() {
        return Ok(());
    }
    let rate_limited = headers
        .get("x-ratelimit-remaining")
        .is_some_and(|remaining| remaining == "0");
    if rate_limited && matches!(status.as_u16(), 403 | 429) {
        bail!(
            "GitHub's limit of 60 requests an hour without signing in is used up; try again later"
        );
    }
    if status == StatusCode::NOT_FOUND {
        bail!(
            "GitHub has no public pull request {link}. rozed can't sign in yet, so it opens \
             pull requests in public repositories only"
        );
    }
    let message = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|body| body.get("message")?.as_str().map(str::to_string))
        .unwrap_or_default();
    bail!("GitHub answered {status} for {link}: {message}");
}

fn parse_pull_request(body: &str) -> anyhow::Result<PullRequest> {
    let pull_request: PullRequest = serde_json::from_str(body)?;
    let clone_url = &pull_request.base.repo.clone_url;
    anyhow::ensure!(
        clone_url.starts_with("https://github.com/"),
        "the repository's clone URL {clone_url} isn't on github.com"
    );
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
        futures::executor::block_on(fetch_pull_request(http, &fd_2212()))
    }

    #[test]
    fn reads_the_base_branch_head_and_clone_url() {
        let http = Canned::new(200, FD_2212);
        let pull_request = fetch(http.clone()).unwrap();
        assert_eq!(
            pull_request,
            PullRequest {
                title: "Honor explicit use-jemalloc requests".into(),
                head: Head {
                    sha: "22c70acc6992c0c51cb68527b87adf114d01acbc".into()
                },
                base: Base {
                    branch: "master".into(),
                    repo: BaseRepo {
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
    fn a_missing_or_private_pull_request_says_only_public_ones_open() {
        let error = fetch(Canned::new(404, r#"{"message":"Not Found"}"#)).unwrap_err();
        assert!(
            error.to_string().contains("public repositories only"),
            "{error}"
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
