//! A client for one account: GitHub's REST and GraphQL APIs with that account's token.

use std::sync::Arc;
use std::time::Duration;

use futures::AsyncReadExt as _;
use graphql_client::GraphQLQuery;
use http_client::http::{HeaderMap, StatusCode};
use http_client::{AsyncBody, HttpClient, HttpRequestExt as _, Method, RedirectPolicy, Request};
use parking_lot::Mutex;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::{GithubError, SsoHeader};
use crate::graphql::{self, GraphQlResponse};
use crate::rate_limit::RateLimits;
use crate::token::Token;
use crate::user::AuthenticatedUser;

pub const API_URL: &str = "https://api.github.com";
/// The REST API version rozed is written against. Responses echo it in
/// `x-github-api-version-selected`.
pub const API_VERSION: &str = "2022-11-28";
/// GitHub gives up on a request after 10 s; this leaves room for a slow network.
const TIMEOUT: Duration = Duration::from_secs(30);
/// The REST API answers a renamed repository with a redirect to the same host.
const MAX_REDIRECTS: u32 = 3;

/// The `User-Agent` rozed sends. GitHub requires one.
pub fn user_agent() -> String {
    format!("rozed/{}", env!("CARGO_PKG_VERSION"))
}

/// One account's client. Clones share the account's rate limits.
#[derive(Clone)]
pub struct GithubClient {
    http: Arc<dyn HttpClient>,
    token: Token,
    api_url: Arc<str>,
    state: Arc<Mutex<ClientState>>,
}

/// What the account's responses have said about it.
#[derive(Clone, Debug, Default)]
pub struct ClientState {
    pub rate_limits: RateLimits,
    /// The last `X-GitHub-SSO` header seen.
    pub sso: Option<SsoHeader>,
}

/// A successful REST response, or a `304 Not Modified`.
#[derive(Debug)]
pub struct RestResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl RestResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|value| value.to_str().ok())
    }

    pub fn etag(&self) -> Option<&str> {
        self.header("etag")
    }

    pub fn json<T: DeserializeOwned>(&self) -> Result<T, GithubError> {
        serde_json::from_slice(&self.body).map_err(|error| GithubError::Decode(error.to_string()))
    }
}

/// The answer to a conditional request.
#[derive(Debug, PartialEq)]
pub enum Conditional<T> {
    /// `304`: nothing changed since the ETag was issued, and the request was free.
    NotModified,
    Modified {
        value: T,
        etag: Option<String>,
    },
}

impl GithubClient {
    pub fn new(http: Arc<dyn HttpClient>, token: Token) -> Self {
        Self {
            http,
            token,
            api_url: API_URL.into(),
            state: Arc::default(),
        }
    }

    pub fn token(&self) -> &Token {
        &self.token
    }

    pub fn state(&self) -> ClientState {
        self.state.lock().clone()
    }

    pub fn rate_limits(&self) -> RateLimits {
        self.state.lock().rate_limits.clone()
    }

    fn url(&self, path: &str) -> String {
        if path.starts_with("https://") {
            path.to_string()
        } else {
            format!("{}/{}", self.api_url, path.trim_start_matches('/'))
        }
    }

    fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
        etag: Option<&str>,
    ) -> Result<Request<AsyncBody>, GithubError> {
        Request::builder()
            .method(method)
            .uri(self.url(path))
            .header("Authorization", format!("token {}", self.token.expose()))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
            .header("User-Agent", user_agent())
            .when(body.is_some(), |builder| {
                builder.header("Content-Type", "application/json")
            })
            .when_some(etag, |builder, etag| builder.header("If-None-Match", etag))
            .follow_redirects(RedirectPolicy::FollowLimit(MAX_REDIRECTS))
            .timeout(TIMEOUT)
            .body(body.map(AsyncBody::from).unwrap_or_default())
            .map_err(|error| GithubError::Decode(format!("building the request: {error}")))
    }

    /// Sends a request and reads the whole response, recording its rate limit and SSO header.
    async fn execute(&self, request: Request<AsyncBody>) -> Result<RestResponse, GithubError> {
        let response = self
            .http
            .send(request)
            .await
            .map_err(GithubError::Network)?;
        let (parts, mut body) = response.into_parts();
        let mut bytes = Vec::new();
        body.read_to_end(&mut bytes)
            .await
            .map_err(|error| GithubError::Network(error.into()))?;
        {
            let mut state = self.state.lock();
            state.rate_limits.record_headers(&parts.headers);
            if let Some(sso) = parts
                .headers
                .get("x-github-sso")
                .and_then(|value| value.to_str().ok())
                .and_then(SsoHeader::parse)
            {
                state.sso = Some(sso);
            }
        }
        Ok(RestResponse {
            status: parts.status,
            headers: parts.headers,
            body: bytes,
        })
    }

    /// Sends a REST request. Any status other than 2xx or `304` becomes an error.
    pub async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
        etag: Option<&str>,
    ) -> Result<RestResponse, GithubError> {
        let request = self.request(method, path, body, etag)?;
        let response = self.execute(request).await?;
        if response.status.is_success() || response.status == StatusCode::NOT_MODIFIED {
            Ok(response)
        } else {
            Err(GithubError::from_response(
                response.status.as_u16(),
                &response.headers,
                &response.body,
            ))
        }
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, GithubError> {
        self.send(Method::GET, path, None, None).await?.json()
    }

    /// A GET that's free when nothing changed: sends `etag` as `If-None-Match`, and returns
    /// [`Conditional::NotModified`] on a `304`.
    pub async fn get_conditional<T: DeserializeOwned>(
        &self,
        path: &str,
        etag: Option<&str>,
    ) -> Result<Conditional<T>, GithubError> {
        let response = self.send(Method::GET, path, None, etag).await?;
        if response.status == StatusCode::NOT_MODIFIED {
            return Ok(Conditional::NotModified);
        }
        Ok(Conditional::Modified {
            value: response.json()?,
            etag: response.etag().map(str::to_string),
        })
    }

    async fn send_json<T: DeserializeOwned, B: Serialize>(
        &self,
        method: Method,
        path: &str,
        body: &B,
    ) -> Result<T, GithubError> {
        let body = serde_json::to_vec(body)
            .map_err(|error| GithubError::Decode(format!("encoding the request: {error}")))?;
        self.send(method, path, Some(body), None).await?.json()
    }

    pub async fn post<T: DeserializeOwned, B: Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, GithubError> {
        self.send_json(Method::POST, path, body).await
    }

    pub async fn put<T: DeserializeOwned, B: Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, GithubError> {
        self.send_json(Method::PUT, path, body).await
    }

    pub async fn patch<T: DeserializeOwned, B: Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, GithubError> {
        self.send_json(Method::PATCH, path, body).await
    }

    pub async fn delete(&self, path: &str) -> Result<(), GithubError> {
        self.send(Method::DELETE, path, None, None).await?;
        Ok(())
    }

    /// Runs a GraphQL query, returning partial data along with any errors.
    pub async fn graphql_partial<Q: GraphQLQuery>(
        &self,
        variables: Q::Variables,
    ) -> Result<(GraphQlResponse<Q::ResponseData>, HeaderMap), GithubError> {
        let body = serde_json::to_vec(&Q::build_query(variables))
            .map_err(|error| GithubError::Decode(format!("encoding the query: {error}")))?;
        let response = self
            .send(Method::POST, "/graphql", Some(body), None)
            .await?;
        let (decoded, rate_limit) =
            graphql::decode::<Q::ResponseData>(&response.body).map_err(GithubError::Decode)?;
        if let Some(rate_limit) = rate_limit {
            self.state
                .lock()
                .rate_limits
                .record_graphql_field(&rate_limit);
        }
        Ok((decoded, response.headers))
    }

    /// Runs a GraphQL query. Any error in the response fails it, typed where GitHub's error is
    /// one rozed handles (not found, SSO, an org's OAuth restriction, the rate limit).
    pub async fn graphql<Q: GraphQLQuery>(
        &self,
        variables: Q::Variables,
    ) -> Result<Q::ResponseData, GithubError> {
        let (response, headers) = self.graphql_partial::<Q>(variables).await?;
        if !response.errors.is_empty() {
            return Err(GithubError::from_graphql(response.errors, &headers));
        }
        response
            .data
            .ok_or_else(|| GithubError::Decode("the response had no data".into()))
    }

    /// The signed-in user and the token's scopes (`GET /user`).
    pub async fn user(&self) -> Result<AuthenticatedUser, GithubError> {
        let response = self.send(Method::GET, "/user", None, None).await?;
        Ok(AuthenticatedUser {
            user: response.json()?,
            scopes: crate::user::parse_scopes(response.header("x-oauth-scopes")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::cassette::{Interaction, RecordedRequest, RecordedResponse};
    use crate::replay::ReplayHttpClient;

    /// A response to replay: method, URL, status, headers and body.
    type Canned<'a> = (&'a str, &'a str, u16, &'a [(&'a str, &'a str)], &'a str);

    fn replay(interactions: Vec<Canned>) -> Arc<ReplayHttpClient> {
        Arc::new(
            ReplayHttpClient::from_interactions(
                "test",
                interactions
                    .into_iter()
                    .map(|(method, url, status, headers, body)| Interaction {
                        request: RecordedRequest {
                            method: method.into(),
                            url: url.into(),
                            ..Default::default()
                        },
                        response: RecordedResponse {
                            status,
                            headers: headers
                                .iter()
                                .map(|(k, v)| (k.to_string(), v.to_string()))
                                .collect(),
                            body: serde_json::from_str(body).ok(),
                            body_text: None,
                        },
                    }),
            )
            .unwrap(),
        )
    }

    fn client(http: &Arc<ReplayHttpClient>) -> GithubClient {
        GithubClient::new(http.clone(), Token::new("gho_FAKE"))
    }

    #[test]
    fn sends_githubs_headers() {
        let http = replay(vec![("GET", "https://api.github.com/user", 200, &[], "{}")]);
        let _: serde_json::Value = futures::executor::block_on(client(&http).get("/user")).unwrap();
        let seen = &http.seen()[0];
        assert_eq!(seen.header("authorization"), Some("token gho_FAKE"));
        assert_eq!(seen.header("accept"), Some("application/vnd.github+json"));
        assert_eq!(seen.header("x-github-api-version"), Some("2022-11-28"));
        assert_eq!(
            seen.header("user-agent"),
            Some(concat!("rozed/", env!("CARGO_PKG_VERSION")))
        );
        assert_eq!(seen.header("if-none-match"), None);
    }

    #[test]
    fn errors_keep_githubs_message() {
        let http = replay(vec![(
            "GET",
            "https://api.github.com/user",
            401,
            &[],
            r#"{"message":"Bad credentials","documentation_url":"https://docs.github.com/rest","status":"401"}"#,
        )]);
        let error = futures::executor::block_on(client(&http).get::<serde_json::Value>("/user"))
            .unwrap_err();
        assert!(error.is_unauthorized(), "{error:?}");
        assert!(error.to_string().contains("Bad credentials"));
    }

    #[test]
    fn tracks_the_rate_limit_and_sso_header_per_account() {
        let http = replay(vec![(
            "GET",
            "https://api.github.com/user/repos",
            200,
            &[
                ("x-ratelimit-limit", "5000"),
                ("x-ratelimit-remaining", "4321"),
                ("x-ratelimit-used", "679"),
                ("x-ratelimit-reset", "1762419600"),
                ("x-ratelimit-resource", "core"),
                ("x-github-sso", "partial-results; organizations=1,2"),
            ],
            "[]",
        )]);
        let account = client(&http);
        let other = GithubClient::new(http.clone(), Token::new("gho_OTHER"));
        let _: Vec<serde_json::Value> =
            futures::executor::block_on(account.get("/user/repos")).unwrap();
        assert_eq!(account.rate_limits().core().unwrap().remaining, 4321);
        assert_eq!(account.state().sso.unwrap().organizations, vec![1, 2]);
        assert_eq!(other.rate_limits().core(), None);
        assert_eq!(account.clone().rate_limits().core().unwrap().used, 679);
    }

    #[test]
    fn a_full_url_is_used_as_is() {
        let http = replay(vec![(
            "GET",
            "https://uploads.github.com/x",
            200,
            &[],
            "{}",
        )]);
        let _: serde_json::Value =
            futures::executor::block_on(client(&http).get("https://uploads.github.com/x")).unwrap();
    }
}
