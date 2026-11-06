//! A client for one account: GitHub's REST and GraphQL APIs with that account's token.

use std::sync::Arc;
use std::time::Duration;

use futures::AsyncReadExt as _;
use futures::future::BoxFuture;
use graphql_client::GraphQLQuery;
use http_client::http::{HeaderMap, HeaderValue, StatusCode};
use http_client::{
    AsyncBody, HttpClient, HttpRequestExt as _, Method, RedirectPolicy, Request, Url,
};
use parking_lot::{Mutex, RwLock};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::{GithubError, SsoHeader};
use crate::graphql::{self, GraphQlResponse};
use crate::rate_limit::RateLimits;
use crate::token::Token;
use crate::user::AuthenticatedUser;

pub const API_URL: &str = "https://api.github.com";
/// The only hosts a client sends its token to: the API, and the API's host for uploads, which
/// responses name in full URLs. Anything else found in a response, such as an image or a check's
/// details page, is fetched without the token by whatever shows it.
const API_HOSTS: &[&str] = &["api.github.com", "uploads.github.com"];
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

/// What a client does when GitHub refuses its token (a `401`). It's given the refused token,
/// and returns a token to retry the request with once, or `None` to fail it. [`Accounts`] gives
/// its clients one that signs the account out, after re-reading a `gh` account's token once.
///
/// [`Accounts`]: crate::accounts::Accounts
pub type OnUnauthorized = Arc<dyn Fn(Token) -> BoxFuture<'static, Option<Token>> + Send + Sync>;

/// One account's client. Clones share the account's token and rate limits.
#[derive(Clone)]
pub struct GithubClient {
    http: Arc<dyn HttpClient>,
    token: Arc<RwLock<Token>>,
    api_url: Arc<str>,
    state: Arc<Mutex<ClientState>>,
    on_unauthorized: Option<OnUnauthorized>,
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

/// What a conditional request asks about: changes since an ETag, or since a time.
#[derive(Clone, Copy, Debug)]
enum Condition<'a> {
    None,
    /// `If-None-Match`. Any change to the response's body changes its ETag.
    ETag(&'a str),
    /// `If-Modified-Since`, an HTTP date such as a response's `Last-Modified`.
    ModifiedSince(&'a str),
}

impl<'a> Condition<'a> {
    fn etag(etag: Option<&'a str>) -> Self {
        etag.map_or(Self::None, Self::ETag)
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
            token: Arc::new(RwLock::new(token)),
            api_url: API_URL.into(),
            state: Arc::default(),
            on_unauthorized: None,
        }
    }

    /// A client that signs in as nobody, for public repositories: it sends no token, and its
    /// requests count against GitHub's 60 an hour for each IP address.
    pub fn anonymous(http: Arc<dyn HttpClient>) -> Self {
        Self::new(http, Token::new(""))
    }

    /// Whether the client sends no token: [`Self::anonymous`].
    pub fn is_anonymous(&self) -> bool {
        self.token.read().is_empty()
    }

    /// Calls `hook` whenever GitHub refuses the token, retrying the request once with the token
    /// it returns.
    pub fn with_unauthorized_hook(mut self, hook: OnUnauthorized) -> Self {
        self.on_unauthorized = Some(hook);
        self
    }

    pub fn token(&self) -> Token {
        self.token.read().clone()
    }

    /// Replaces the token for this client and every clone of it.
    pub fn set_token(&self, token: Token) {
        *self.token.write() = token;
    }

    pub fn state(&self) -> ClientState {
        self.state.lock().clone()
    }

    pub fn rate_limits(&self) -> RateLimits {
        self.state.lock().rate_limits.clone()
    }

    /// The URL for `path`, which is either relative to the API or a full URL on one of
    /// [`API_HOSTS`], over HTTPS on the default port. Anything else is refused.
    fn url(&self, path: &str) -> Result<String, GithubError> {
        let url = match Url::parse(path) {
            Ok(url) => url,
            Err(_) => Url::parse(&format!(
                "{}/{}",
                self.api_url,
                path.trim_start_matches('/')
            ))
            .map_err(|_| GithubError::RefusedUrl {
                host: "a malformed URL".into(),
            })?,
        };
        let allowed = url.scheme() == "https"
            && url.host_str().is_some_and(|host| API_HOSTS.contains(&host))
            && url.port().is_none()
            && url.username().is_empty()
            && url.password().is_none();
        if !allowed {
            return Err(GithubError::RefusedUrl {
                host: url
                    .host_str()
                    .map_or_else(|| url.scheme().to_string(), str::to_string),
            });
        }
        Ok(url.to_string())
    }

    fn request(
        &self,
        token: &Token,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
        condition: Condition<'_>,
    ) -> Result<Request<AsyncBody>, GithubError> {
        // An anonymous client's token is empty, and it sends none.
        let authorization = (!token.is_empty())
            .then(|| {
                let mut authorization = HeaderValue::from_str(&format!("token {}", token.expose()))
                    .map_err(|_| {
                        GithubError::Decode("the token isn't a valid header value".into())
                    })?;
                authorization.set_sensitive(true);
                Ok::<_, GithubError>(authorization)
            })
            .transpose()?;
        Request::builder()
            .method(method)
            .uri(self.url(path)?)
            .when_some(authorization, |builder, authorization| {
                builder.header("Authorization", authorization)
            })
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", API_VERSION)
            .header("User-Agent", user_agent())
            .when(body.is_some(), |builder| {
                builder.header("Content-Type", "application/json")
            })
            .when_some(
                match condition {
                    Condition::None => None,
                    Condition::ETag(etag) => Some(("If-None-Match", etag)),
                    Condition::ModifiedSince(time) => Some(("If-Modified-Since", time)),
                },
                |builder, (name, value)| builder.header(name, value),
            )
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

    /// Sends a REST request. Any status other than 2xx or `304` becomes an error. When GitHub
    /// refuses the token, the client's [`OnUnauthorized`] hook may hand it a new one, and the
    /// request is retried once with it.
    pub async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
        etag: Option<&str>,
    ) -> Result<RestResponse, GithubError> {
        self.send_if(method, path, body, Condition::etag(etag))
            .await
    }

    async fn send_if(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
        condition: Condition<'_>,
    ) -> Result<RestResponse, GithubError> {
        let token = self.token();
        let result = self
            .send_with(&token, method.clone(), path, body.clone(), condition)
            .await;
        let Some(hook) = &self.on_unauthorized else {
            return result;
        };
        if !matches!(&result, Err(error) if error.is_unauthorized()) {
            return result;
        }
        let Some(new_token) = hook(token).await else {
            return result;
        };
        self.set_token(new_token.clone());
        self.send_with(&new_token, method, path, body, condition)
            .await
    }

    async fn send_with(
        &self,
        token: &Token,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
        condition: Condition<'_>,
    ) -> Result<RestResponse, GithubError> {
        let request = self.request(token, method, path, body, condition)?;
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
        self.get_if(path, Condition::etag(etag)).await
    }

    /// A GET that's free when nothing changed since `since`, an HTTP date such as an earlier
    /// answer's `Last-Modified`: sends it as `If-Modified-Since`, and returns
    /// [`Conditional::NotModified`] on a `304`. Unlike an ETag, which changes with anything in the
    /// body, this asks only whether the resource itself was updated.
    pub async fn get_modified_since<T: DeserializeOwned>(
        &self,
        path: &str,
        since: Option<&str>,
    ) -> Result<Conditional<T>, GithubError> {
        self.get_if(
            path,
            since.map_or(Condition::None, Condition::ModifiedSince),
        )
        .await
    }

    async fn get_if<T: DeserializeOwned>(
        &self,
        path: &str,
        condition: Condition<'_>,
    ) -> Result<Conditional<T>, GithubError> {
        let response = self.send_if(Method::GET, path, None, condition).await?;
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

    /// Runs a GraphQL query whose answer is still useful when GitHub left out a field it
    /// couldn't show, such as a fork's repository in an org that restricts OAuth apps. Errors
    /// about fields deeper than `depth` from the root are logged and the rest of the data is
    /// returned; any other error fails the query, as in [`Self::graphql`].
    pub(crate) async fn graphql_keeping_partial<Q: GraphQLQuery>(
        &self,
        variables: Q::Variables,
        depth: usize,
    ) -> Result<Q::ResponseData, GithubError> {
        let (response, headers) = self.graphql_partial::<Q>(variables).await?;
        match response.data {
            Some(data) if response.errors.iter().all(|error| error.is_below(depth)) => {
                for error in &response.errors {
                    log::warn!(
                        "GitHub left out {}: {}",
                        serde_json::Value::from(error.path.clone()),
                        error.message
                    );
                }
                Ok(data)
            }
            data if response.errors.is_empty() => {
                data.ok_or_else(|| GithubError::Decode("the response had no data".into()))
            }
            _ => Err(GithubError::from_graphql(response.errors, &headers)),
        }
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
    fn a_get_modified_since_a_time_sends_it() {
        let date = "Fri, 06 Nov 2026 08:15:07 GMT";
        let mut since = RecordedRequest {
            method: "GET".into(),
            url: "https://api.github.com/repos/o/r/pulls/1".into(),
            ..Default::default()
        };
        since.if_modified_since = Some(date.into());
        let http = Arc::new(
            ReplayHttpClient::from_interactions(
                "test",
                [Interaction {
                    request: since,
                    response: RecordedResponse {
                        status: 304,
                        headers: Default::default(),
                        body: None,
                        body_text: None,
                    },
                }],
            )
            .unwrap(),
        );
        let answer = futures::executor::block_on(
            client(&http).get_modified_since::<serde_json::Value>("/repos/o/r/pulls/1", Some(date)),
        )
        .unwrap();
        assert_eq!(answer, Conditional::NotModified);
        let seen = &http.seen()[0];
        assert_eq!(seen.header("if-modified-since"), Some(date));
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

    /// A hook that hands out `new`, noting each token it was given.
    fn hook(new: Option<&'static str>) -> (OnUnauthorized, Arc<Mutex<Vec<String>>>) {
        let refused = Arc::new(Mutex::new(Vec::new()));
        let hook: OnUnauthorized = Arc::new({
            let refused = refused.clone();
            move |token: Token| {
                refused.lock().push(token.expose().to_string());
                Box::pin(async move { new.map(Token::new) })
            }
        });
        (hook, refused)
    }

    const BAD_CREDENTIALS: &str = r#"{"message":"Bad credentials","status":"401"}"#;

    #[test]
    fn a_401_asks_the_hook_and_retries_once_with_its_token() {
        let http = replay(vec![
            (
                "GET",
                "https://api.github.com/user",
                401,
                &[],
                BAD_CREDENTIALS,
            ),
            ("GET", "https://api.github.com/user", 200, &[], "{}"),
        ]);
        let (hook, refused) = hook(Some("gho_NEW"));
        let client = client(&http).with_unauthorized_hook(hook);
        let other = client.clone();
        let _: serde_json::Value = futures::executor::block_on(client.get("/user")).unwrap();
        assert_eq!(*refused.lock(), ["gho_FAKE"]);
        let seen = http.seen();
        assert_eq!(seen[0].header("authorization"), Some("token gho_FAKE"));
        assert_eq!(seen[1].header("authorization"), Some("token gho_NEW"));
        // Every clone uses the new token from now on.
        assert_eq!(other.token().expose(), "gho_NEW");
    }

    #[test]
    fn without_a_new_token_the_401_is_returned() {
        let http = replay(vec![(
            "GET",
            "https://api.github.com/user",
            401,
            &[],
            BAD_CREDENTIALS,
        )]);
        let (hook, refused) = hook(None);
        let client = client(&http).with_unauthorized_hook(hook);
        let error =
            futures::executor::block_on(client.get::<serde_json::Value>("/user")).unwrap_err();
        assert!(error.is_unauthorized(), "{error:?}");
        assert_eq!(refused.lock().len(), 1);
        assert_eq!(http.seen().len(), 1);
    }

    #[test]
    fn a_401_on_the_retry_is_returned_without_asking_again() {
        let http = replay(vec![(
            "GET",
            "https://api.github.com/user",
            401,
            &[],
            BAD_CREDENTIALS,
        )]);
        let (hook, refused) = hook(Some("gho_NEW"));
        let client = client(&http).with_unauthorized_hook(hook);
        let error =
            futures::executor::block_on(client.get::<serde_json::Value>("/user")).unwrap_err();
        assert!(error.is_unauthorized(), "{error:?}");
        assert_eq!(*refused.lock(), ["gho_FAKE"]);
        assert_eq!(http.seen().len(), 2);
    }

    #[test]
    fn the_token_goes_only_to_githubs_api_hosts() {
        let http = replay(vec![]);
        let client = client(&http);
        for url in [
            "https://evil.example/x",
            "https://api.github.com.evil.example/x",
            "https://github.com/login",
            "http://api.github.com/user",
            "https://api.github.com:8443/user",
            "https://someone@api.github.com/user",
            "file:///etc/passwd",
        ] {
            let error =
                futures::executor::block_on(client.get::<serde_json::Value>(url)).unwrap_err();
            assert!(
                matches!(error, GithubError::RefusedUrl { .. }),
                "{url}: {error:?}"
            );
            assert!(!error.to_string().contains("/x"), "{error}");
        }
        assert!(http.seen().is_empty());
    }

    #[test]
    fn the_authorization_header_is_marked_sensitive() {
        let http = replay(vec![("GET", "https://api.github.com/user", 200, &[], "{}")]);
        let _: serde_json::Value = futures::executor::block_on(client(&http).get("/user")).unwrap();
        let seen = &http.seen()[0];
        assert!(seen.headers["authorization"].is_sensitive());
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
