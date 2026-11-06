//! Signing in with GitHub's device flow: rozed shows a code, the user enters it on GitHub, and
//! rozed polls until GitHub hands over a token.
//!
//! GitHub answers every poll with HTTP 200 and an `error` field while it waits
//! (`authorization_pending`), when polled too fast (`slow_down`, with a new, longer interval), and
//! when the flow ends without a token (`expired_token`, `access_denied` and misconfigurations).
//! See [GitHub's docs](https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/authorizing-oauth-apps#device-flow).

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use futures::AsyncReadExt as _;
use gpui::BackgroundExecutor;
use http_client::{AsyncBody, HttpClient, HttpRequestExt as _, Method, RedirectPolicy, Request};
use serde::Deserialize;

use crate::client::{GithubClient, user_agent};
use crate::error::{CLIENT_ID, GithubError};
use crate::token::Token;
use crate::user::{AuthenticatedUser, SCOPES};

pub const DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
pub const ACCESS_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
/// Where the user enters the code. rozed opens this rather than the `verification_uri` GitHub's
/// answer names, so an answer from anything posing as GitHub can't open another page or app.
pub const VERIFICATION_URL: &str = "https://github.com/login/device";
const GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";
/// GitHub's minimum gap between polls.
const MIN_INTERVAL: Duration = Duration::from_secs(5);
/// The longest gap between polls rozed accepts. GitHub's are seconds; a bigger number is a
/// broken answer, and left as is would overflow the clock.
const MAX_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// The longest a code is waited for. GitHub's codes last 15 minutes.
const MAX_EXPIRES_IN: Duration = Duration::from_secs(60 * 60);
/// How much `slow_down` adds when it doesn't say the new interval.
const SLOW_DOWN_STEP: Duration = Duration::from_secs(5);
/// Polls wait a little longer than the interval, so clock drift never earns a `slow_down`.
const INTERVAL_MARGIN: f64 = 1.2;
const TIMEOUT: Duration = Duration::from_secs(30);

/// The codes GitHub issued for one sign-in.
#[derive(Clone)]
pub struct DeviceCodes {
    device_code: String,
    /// The code the user types on GitHub, at [`VERIFICATION_URL`], e.g. `50B0-5666`.
    pub user_code: String,
    /// How long the codes are good for, at most an hour.
    pub expires_in: Duration,
    /// The minimum gap between polls, between 5 seconds and 5 minutes.
    pub interval: Duration,
}

impl fmt::Debug for DeviceCodes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceCodes")
            .field("user_code", &self.user_code)
            .field("expires_in", &self.expires_in)
            .field("interval", &self.interval)
            .finish_non_exhaustive()
    }
}

/// Why a sign-in ended without an account.
#[derive(Debug, thiserror::Error)]
pub enum DeviceFlowError {
    #[error("The code expired before it was entered on GitHub.")]
    Expired,
    #[error("The sign-in was cancelled on GitHub.")]
    Denied,
    #[error("GitHub refused the sign-in: {0}")]
    Refused(String),
    #[error("The token is missing the {} scope{}.", .0.join(", "), if .0.len() == 1 { "" } else { "s" })]
    MissingScopes(Vec<&'static str>),
    #[error(transparent)]
    Github(#[from] GithubError),
}

/// What one poll said.
#[derive(Debug, PartialEq)]
enum Poll {
    Pending,
    /// Poll less often: every `interval`, or 5 s more than now if GitHub didn't say.
    SlowDown(Option<Duration>),
    Token(Token),
    Expired,
    Denied,
    Refused(String),
}

#[derive(Deserialize)]
struct CodesResponse {
    device_code: String,
    user_code: String,
    expires_in: u64,
    interval: u64,
}

/// A poll interval GitHub gave, kept within [`MIN_INTERVAL`] and [`MAX_INTERVAL`].
fn clamp_interval(interval: Duration) -> Duration {
    interval.clamp(MIN_INTERVAL, MAX_INTERVAL)
}

#[derive(Deserialize)]
struct PollResponse {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
    #[serde(default)]
    interval: Option<u64>,
}

/// Posts a form to github.com and reads the whole response.
async fn post_form(
    http: &Arc<dyn HttpClient>,
    url: &str,
    form: &[(&str, &str)],
) -> Result<(u16, Vec<u8>), GithubError> {
    let body = serde_urlencoded::to_string(form)
        .map_err(|error| GithubError::Decode(format!("encoding the form: {error}")))?;
    let request = Request::builder()
        .method(Method::POST)
        .uri(url)
        // Without it GitHub answers form-encoded.
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("User-Agent", user_agent())
        .follow_redirects(RedirectPolicy::NoFollow)
        .timeout(TIMEOUT)
        .body(AsyncBody::from(body))
        .map_err(|error| GithubError::Decode(format!("building the request: {error}")))?;
    let response = http.send(request).await.map_err(GithubError::Network)?;
    let status = response.status().as_u16();
    let mut bytes = Vec::new();
    response
        .into_body()
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| GithubError::Network(error.into()))?;
    Ok((status, bytes))
}

/// Asks GitHub for a device code and the user code to show.
pub async fn request_codes(http: &Arc<dyn HttpClient>) -> Result<DeviceCodes, DeviceFlowError> {
    let scope = SCOPES.join(" ");
    let (status, body) = post_form(
        http,
        DEVICE_CODE_URL,
        &[("client_id", CLIENT_ID), ("scope", &scope)],
    )
    .await?;
    if status != 200 {
        return Err(DeviceFlowError::Refused(format!(
            "HTTP {status}: {}",
            String::from_utf8_lossy(&body).trim()
        )));
    }
    if let Ok(PollResponse {
        error: Some(error),
        error_description,
        ..
    }) = serde_json::from_slice::<PollResponse>(&body)
    {
        return Err(DeviceFlowError::Refused(error_description.unwrap_or(error)));
    }
    let codes: CodesResponse = serde_json::from_slice(&body)
        .map_err(|error| GithubError::Decode(format!("the device code response: {error}")))?;
    Ok(DeviceCodes {
        device_code: codes.device_code,
        user_code: codes.user_code,
        expires_in: Duration::from_secs(codes.expires_in).min(MAX_EXPIRES_IN),
        interval: clamp_interval(Duration::from_secs(codes.interval)),
    })
}

fn parse_poll(status: u16, body: &[u8]) -> Poll {
    if status != 200 {
        return Poll::Refused(format!(
            "HTTP {status}: {}",
            String::from_utf8_lossy(body).trim()
        ));
    }
    let Ok(response) = serde_json::from_slice::<PollResponse>(body) else {
        return Poll::Refused("GitHub's answer wasn't JSON".into());
    };
    if let Some(token) = response.access_token.filter(|token| !token.is_empty()) {
        return Poll::Token(Token::new(token));
    }
    match response.error.as_deref() {
        Some("authorization_pending") => Poll::Pending,
        Some("slow_down") => Poll::SlowDown(response.interval.map(Duration::from_secs)),
        Some("expired_token") | Some("token_expired") => Poll::Expired,
        Some("access_denied") => Poll::Denied,
        Some(error) => Poll::Refused(
            response
                .error_description
                .unwrap_or_else(|| error.to_string()),
        ),
        None => Poll::Refused("GitHub's answer had neither a token nor an error".into()),
    }
}

/// Polls until the user enters the code on GitHub, returning the token. Each poll waits the
/// interval × 1.2; a `slow_down` adopts the interval GitHub returns. It stops when the codes
/// expire, without waiting for GitHub to say so. Dropping the future cancels it.
pub async fn wait_for_token(
    http: &Arc<dyn HttpClient>,
    codes: &DeviceCodes,
    executor: &BackgroundExecutor,
) -> Result<Token, DeviceFlowError> {
    let deadline = executor.now() + codes.expires_in.min(MAX_EXPIRES_IN);
    let mut interval = clamp_interval(codes.interval);
    loop {
        let wait = interval.mul_f64(INTERVAL_MARGIN);
        if executor.now() + wait >= deadline {
            return Err(DeviceFlowError::Expired);
        }
        executor.timer(wait).await;
        let (status, body) = post_form(
            http,
            ACCESS_TOKEN_URL,
            &[
                ("client_id", CLIENT_ID),
                ("device_code", &codes.device_code),
                ("grant_type", GRANT_TYPE),
            ],
        )
        .await?;
        match parse_poll(status, &body) {
            Poll::Pending => {}
            Poll::SlowDown(new_interval) => {
                interval = clamp_interval(new_interval.unwrap_or(interval + SLOW_DOWN_STEP));
                log::info!("GitHub asked to poll every {interval:?}");
            }
            Poll::Token(token) => return Ok(token),
            Poll::Expired => return Err(DeviceFlowError::Expired),
            Poll::Denied => return Err(DeviceFlowError::Denied),
            Poll::Refused(message) => return Err(DeviceFlowError::Refused(message)),
        }
    }
}

/// Reads who a new token belongs to (`GET /user`) and checks it has every scope rozed needs.
pub async fn check_token(
    http: &Arc<dyn HttpClient>,
    token: Token,
) -> Result<AuthenticatedUser, DeviceFlowError> {
    let user = GithubClient::new(http.clone(), token).user().await?;
    let missing = user.missing_scopes();
    if !missing.is_empty() {
        return Err(DeviceFlowError::MissingScopes(missing));
    }
    Ok(user)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_poll_answer() {
        let poll = |body: &str| parse_poll(200, body.as_bytes());
        assert_eq!(
            poll(
                r#"{"error":"authorization_pending","error_description":"The authorization request is still pending."}"#
            ),
            Poll::Pending
        );
        assert_eq!(
            poll(r#"{"error":"slow_down","error_description":"Too many requests.","interval":10}"#),
            Poll::SlowDown(Some(Duration::from_secs(10)))
        );
        assert_eq!(poll(r#"{"error":"slow_down"}"#), Poll::SlowDown(None));
        assert_eq!(poll(r#"{"error":"expired_token"}"#), Poll::Expired);
        assert_eq!(poll(r#"{"error":"access_denied"}"#), Poll::Denied);
        assert_eq!(
            poll(
                r#"{"error":"incorrect_device_code","error_description":"The device_code provided is not valid."}"#
            ),
            Poll::Refused("The device_code provided is not valid.".into())
        );
        assert_eq!(
            poll(r#"{"error":"device_flow_disabled"}"#),
            Poll::Refused("device_flow_disabled".into())
        );
        assert_eq!(
            poll(
                r#"{"access_token":"gho_FAKE","token_type":"bearer","scope":"read:org,repo,workflow"}"#
            ),
            Poll::Token(Token::new("gho_FAKE"))
        );
        assert!(matches!(
            poll("error=slow_down&interval=20"),
            Poll::Refused(_)
        ));
        assert!(matches!(parse_poll(500, b"oops"), Poll::Refused(_)));
    }

    #[test]
    fn codes_never_show_the_device_code() {
        let codes = DeviceCodes {
            device_code: "secret-device-code".into(),
            user_code: "50B0-5666".into(),
            expires_in: Duration::from_secs(899),
            interval: Duration::from_secs(5),
        };
        let debug = format!("{codes:?}");
        assert!(!debug.contains("secret"), "{debug}");
        assert!(debug.contains("50B0-5666"), "{debug}");
    }

    use crate::cassette::{Interaction, RecordedRequest, RecordedResponse};
    use crate::replay::ReplayHttpClient;

    /// A replay answering the device code request, then every poll, with these bodies.
    fn github_answering(
        codes: serde_json::Value,
        polls: &[serde_json::Value],
    ) -> Arc<dyn HttpClient> {
        let interaction = |url: &str, body: &serde_json::Value| Interaction {
            request: RecordedRequest {
                method: "POST".into(),
                url: url.into(),
                body: Some(match url {
                    DEVICE_CODE_URL => {
                        format!("client_id={CLIENT_ID}&scope=repo+read%3Aorg+workflow")
                    }
                    _ => format!(
                        "client_id={CLIENT_ID}&device_code=d&grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code"
                    ),
                }),
                ..Default::default()
            },
            response: RecordedResponse {
                status: 200,
                headers: Default::default(),
                body: Some(body.clone()),
                body_text: None,
            },
        };
        let interactions = std::iter::once(interaction(DEVICE_CODE_URL, &codes))
            .chain(polls.iter().map(|poll| interaction(ACCESS_TOKEN_URL, poll)));
        Arc::new(ReplayHttpClient::from_interactions("synthetic", interactions).unwrap())
    }

    #[gpui::test]
    async fn absurd_numbers_from_github_are_clamped(cx: &mut gpui::TestAppContext) {
        let http = github_answering(
            serde_json::json!({
                "device_code": "d",
                "user_code": "FAKE-1234",
                "verification_uri": "file:///Applications/Calculator.app",
                "expires_in": u64::MAX,
                "interval": u64::MAX,
            }),
            &[serde_json::json!({"error": "slow_down", "interval": u64::MAX})],
        );
        let codes = request_codes(&http).await.unwrap();
        assert_eq!(codes.expires_in, MAX_EXPIRES_IN);
        assert_eq!(codes.interval, MAX_INTERVAL);
        // Waiting doesn't overflow the clock, and gives up when the clamped code expires.
        let result = wait_for_token(&http, &codes, &cx.executor()).await;
        assert!(
            matches!(result, Err(DeviceFlowError::Expired)),
            "{result:?}"
        );
    }

    #[test]
    fn rozed_opens_only_githubs_page() {
        assert_eq!(VERIFICATION_URL, "https://github.com/login/device");
    }

    #[test]
    fn missing_scopes_read_well() {
        assert_eq!(
            DeviceFlowError::MissingScopes(vec!["workflow"]).to_string(),
            "The token is missing the workflow scope."
        );
        assert_eq!(
            DeviceFlowError::MissingScopes(vec!["repo", "workflow"]).to_string(),
            "The token is missing the repo, workflow scopes."
        );
    }
}
