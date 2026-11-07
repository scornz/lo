//! What can go wrong talking to GitHub, keeping what GitHub said.

use std::time::Duration;

use http_client::http::HeaderMap;
use serde::Deserialize;

use crate::graphql::GraphQlError;
use crate::review_writes::Rejection;

/// Lo's OAuth app's client ID.
pub const CLIENT_ID: &str = "Ov23liBORqwRdiPzqsPP";

/// The page where a user asks an organization to approve Lo: it lists each org with a
/// Request button.
pub fn approval_url() -> String {
    format!("https://github.com/settings/connections/applications/{CLIENT_ID}")
}

#[derive(Debug, thiserror::Error)]
pub enum GithubError {
    /// The token was revoked or is invalid (`401`). The account is signed out.
    #[error("GitHub didn't accept the token: {message}")]
    Unauthorized { message: String },

    /// An organization requires single sign-on, and the token isn't authorized for it. `url`
    /// authorizes it; GitHub's link is valid for an hour.
    #[error("{message}")]
    SsoRequired {
        message: String,
        url: Option<String>,
    },

    /// An organization restricts OAuth apps and hasn't approved Lo.
    #[error("{message}")]
    OAuthAppRestricted {
        message: String,
        org: Option<String>,
        documentation_url: Option<String>,
    },

    /// The rate limit is used up (`secondary: false`), or GitHub's secondary limits kicked in.
    #[error("GitHub's rate limit was hit: {message}")]
    RateLimited {
        message: String,
        secondary: bool,
        /// When the primary limit resets, in seconds since the Unix epoch.
        reset: Option<u64>,
        /// How long GitHub asked to wait.
        retry_after: Option<Duration>,
        /// The limit that was hit (`core`, `graphql`, `search` and so on), from
        /// `x-ratelimit-resource`.
        resource: Option<String>,
    },

    #[error("{message}")]
    NotFound {
        message: String,
        documentation_url: Option<String>,
    },

    /// Any other error status.
    #[error("GitHub answered {status}: {message}")]
    Api {
        status: u16,
        message: String,
        documentation_url: Option<String>,
        /// GitHub's `errors` array, for validation failures.
        errors: Vec<serde_json::Value>,
    },

    /// A GraphQL response's errors, when none of the cases above fits.
    #[error("{}", graphql_messages(.errors))]
    GraphQl { errors: Vec<GraphQlError> },

    /// GitHub refused a comment, a review or an edit of one, as it explains in words: GraphQL's
    /// `UNPROCESSABLE`, REST's `422`, or a mutation that answered nothing without an error
    /// ([`crate::review_writes`]).
    #[error("{0}")]
    Rejected(Rejection),

    /// GitHub's pages didn't fit together: a cursor that doesn't move on, or a pull request that
    /// kept changing while its pages were read. Trying again later may work.
    #[error("GitHub's pages didn't fit together: {0}")]
    Inconsistent(String),

    /// A request to somewhere other than GitHub's API. Lo sends its token nowhere else, so
    /// it never sent this one.
    #[error("Lo sends GitHub requests only to api.github.com and uploads.github.com, not {host}")]
    RefusedUrl { host: String },

    /// The request didn't get an answer.
    #[error("couldn't reach GitHub: {0:#}")]
    Network(anyhow::Error),

    /// The account's token couldn't be read, such as from a locked Keychain. The account is still
    /// signed in; trying again later may work.
    #[error("couldn't read the account's token: {0}")]
    TokenUnavailable(String),

    /// The answer wasn't what Lo expected.
    #[error("couldn't read GitHub's answer: {0}")]
    Decode(String),
}

fn graphql_messages(errors: &[GraphQlError]) -> String {
    let messages: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
    if messages.is_empty() {
        "GitHub's GraphQL API returned an error".to_string()
    } else {
        messages.join("; ")
    }
}

/// A link that helps the user fix an error, labelled for a button. It's always an `https` page
/// on github.com: GitHub's answers can name a link, such as the single sign-on URL, and Lo
/// opens nothing else they name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpLink {
    label: &'static str,
    url: String,
}

impl HelpLink {
    /// A link to `url`, if it's an `https` page on github.com.
    pub fn new(label: &'static str, url: impl Into<String>) -> Option<Self> {
        let url = url.into();
        let parsed = http_client::Url::parse(&url).ok()?;
        let on_github = parsed.scheme() == "https"
            && parsed.host_str() == Some("github.com")
            && parsed.port().is_none()
            && parsed.username().is_empty()
            && parsed.password().is_none();
        on_github.then_some(Self { label, url })
    }

    pub fn label(&self) -> &'static str {
        self.label
    }

    pub fn url(&self) -> &str {
        &self.url
    }
}

impl GithubError {
    /// GitHub's link for fixing this error: authorizing single sign-on, or asking an organization
    /// to approve Lo.
    pub fn help_link(&self) -> Option<HelpLink> {
        match self {
            Self::SsoRequired { url: Some(url), .. } => {
                HelpLink::new("Authorize on GitHub", url.as_str())
            }
            Self::OAuthAppRestricted { .. } => {
                HelpLink::new("Request approval on GitHub", approval_url())
            }
            _ => None,
        }
    }

    /// Whether the token is no good and the account needs signing in again.
    pub fn is_unauthorized(&self) -> bool {
        matches!(self, Self::Unauthorized { .. })
    }

    /// Whether GitHub couldn't be reached at all: no connection could be made, as when the Mac
    /// is offline, has no route or DNS answer, or goes through a proxy that's down. A request
    /// that connected and then timed out isn't, nor is a TLS failure, a proxy that refused the
    /// request, or any answer GitHub gave.
    pub fn is_unreachable(&self) -> bool {
        match self {
            Self::Network(error) => is_connection_failure(error),
            _ => false,
        }
    }

    /// Whether trying again soon may get an answer: GitHub couldn't be reached
    /// ([`Self::is_unreachable`]), or a request got no answer in time or lost its connection, as
    /// on a connection kept from before the Mac slept. A TLS failure or a proxy's refusal isn't:
    /// waiting doesn't fix those.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Network(error) => is_connection_failure(error) || is_lost_answer(error),
            _ => false,
        }
    }

    /// Whether waiting may fix it, with nothing for the user to do: [`Self::is_retryable`], a
    /// rate limit, or a server error or `429` from GitHub. A refused token, single sign-on, an
    /// organization's restriction or "not found" isn't.
    pub fn is_waitable(&self) -> bool {
        match self {
            Self::RateLimited { .. } => true,
            Self::Api { status, .. } => *status >= 500 || *status == 429,
            _ => self.is_retryable(),
        }
    }

    /// The error for an unsuccessful REST response, or a GraphQL one that failed before running.
    pub fn from_response(status: u16, headers: &HeaderMap, body: &[u8]) -> Self {
        #[derive(Deserialize, Default)]
        struct Body {
            #[serde(default)]
            message: Option<String>,
            #[serde(default)]
            documentation_url: Option<String>,
            #[serde(default)]
            errors: Vec<serde_json::Value>,
        }
        let parsed: Body = serde_json::from_slice(body).unwrap_or_default();
        let message = parsed
            .message
            .unwrap_or_else(|| summarize_body(body).unwrap_or_else(|| format!("HTTP {status}")));
        let documentation_url = parsed.documentation_url;
        let header = |name: &str| {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        };

        if status == 401 {
            return Self::Unauthorized { message };
        }
        if matches!(status, 403 | 404)
            && let Some(sso) = header("x-github-sso").and_then(|value| SsoHeader::parse(&value))
            && sso.required
        {
            return Self::SsoRequired {
                message,
                url: sso.url,
            };
        }
        if status == 403 && is_oauth_restriction(&message) {
            return Self::OAuthAppRestricted {
                org: restricting_org(&message),
                message,
                documentation_url,
            };
        }
        if matches!(status, 403 | 429) {
            let remaining = header("x-ratelimit-remaining").and_then(|v| v.parse::<u64>().ok());
            let retry_after = header("retry-after")
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map(Duration::from_secs);
            if remaining == Some(0)
                || retry_after.is_some()
                || message.to_lowercase().contains("rate limit")
            {
                return Self::RateLimited {
                    secondary: remaining != Some(0),
                    reset: header("x-ratelimit-reset").and_then(|v| v.parse().ok()),
                    retry_after,
                    resource: header("x-ratelimit-resource"),
                    message,
                };
            }
        }
        if status == 404 {
            return Self::NotFound {
                message,
                documentation_url,
            };
        }
        Self::Api {
            status,
            message,
            documentation_url,
            errors: parsed.errors,
        }
    }

    /// The error for a GraphQL response's `errors`, which come with HTTP 200. Every error is
    /// looked at, and the one Lo can say most about wins: single sign-on, then an org's
    /// OAuth restriction, then the rate limit, then not found.
    pub fn from_graphql(errors: Vec<GraphQlError>, headers: &HeaderMap) -> Self {
        let header = |name: &str| {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        };
        let sso_required = header("x-github-sso")
            .and_then(|value| SsoHeader::parse(&value))
            .filter(|sso| sso.required);
        let kind = |error: &GraphQlError| error.r#type.clone().unwrap_or_default();
        let find = |matches: &dyn Fn(&GraphQlError) -> bool| {
            errors.iter().find(|error| matches(error)).cloned()
        };

        let sso_error = find(&|error| error.message.contains("SAML enforcement"));
        if sso_required.is_some() || sso_error.is_some() {
            return Self::SsoRequired {
                message: sso_error
                    .or_else(|| errors.first().cloned())
                    .map(|error| error.message)
                    .unwrap_or_default(),
                url: sso_required.and_then(|sso| sso.url),
            };
        }
        if let Some(error) = find(&|error| is_oauth_restriction(&error.message)) {
            return Self::OAuthAppRestricted {
                org: restricting_org(&error.message),
                message: error.message,
                documentation_url: None,
            };
        }
        if let Some(error) =
            find(&|error| matches!(kind(error).as_str(), "RATE_LIMITED" | "RATE_LIMIT"))
        {
            // As for REST: the limit is used up when GitHub says none of it is left, and
            // otherwise its secondary limits kicked in.
            let remaining = header("x-ratelimit-remaining").and_then(|v| v.parse::<u64>().ok());
            return Self::RateLimited {
                message: error.message,
                secondary: remaining != Some(0),
                reset: header("x-ratelimit-reset").and_then(|v| v.parse().ok()),
                retry_after: header("retry-after")
                    .and_then(|v| v.trim().parse::<u64>().ok())
                    .map(Duration::from_secs),
                resource: header("x-ratelimit-resource"),
            };
        }
        if let Some(error) = find(&|error| kind(error) == "NOT_FOUND") {
            return Self::NotFound {
                message: error.message,
                documentation_url: None,
            };
        }
        Self::GraphQl { errors }
    }
}

/// The most of an error body that's worth showing: its text with whitespace collapsed, cut short,
/// or just "(an HTML page)" for one of GitHub's error pages. `None` when it's empty.
pub(crate) fn summarize_body(body: &[u8]) -> Option<String> {
    const MAX_CHARS: usize = 200;
    let text = String::from_utf8_lossy(body);
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return None;
    }
    if text.starts_with('<') {
        return Some("(an HTML page)".into());
    }
    if text.chars().count() <= MAX_CHARS {
        return Some(text);
    }
    Some(text.chars().take(MAX_CHARS).collect::<String>() + "…")
}

fn is_oauth_restriction(message: &str) -> bool {
    message.contains("OAuth App access restrictions")
}

/// The org in GitHub's message "… the `<org>` organization has enabled OAuth App access
/// restrictions …".
fn restricting_org(message: &str) -> Option<String> {
    let start = message.find("the `")? + "the `".len();
    let end = message[start..].find("` organization")? + start;
    Some(message[start..end].to_string())
}

/// GitHub's `X-GitHub-SSO` header: `required; url=<url>`, or
/// `partial-results; organizations=<id>,<id>` on lists that left some orgs out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SsoHeader {
    pub required: bool,
    pub url: Option<String>,
    /// The IDs of the orgs a list left out.
    pub organizations: Vec<u64>,
}

impl SsoHeader {
    pub fn parse(value: &str) -> Option<Self> {
        let mut parts = value.split(';').map(str::trim);
        let kind = parts.next()?;
        let mut header = Self {
            required: kind == "required",
            url: None,
            organizations: Vec::new(),
        };
        if kind != "required" && kind != "partial-results" {
            return None;
        }
        for part in parts {
            if let Some(url) = part.strip_prefix("url=") {
                header.url = Some(url.to_string());
            } else if let Some(ids) = part.strip_prefix("organizations=") {
                header.organizations = ids
                    .split(',')
                    .filter_map(|id| id.trim().parse().ok())
                    .collect();
            }
        }
        Some(header)
    }
}

/// Whether an HTTP client's `error` says no connection could be made. Lo's client is
/// reqwest, built on hyper-util, which wraps everything that goes wrong before a request is sent
/// in `client error (Connect)`: looking up the host, opening the TCP connection, the TLS
/// handshake and a proxy's answer to `CONNECT`. Only the first two mean the network is missing,
/// and hyper-util names them `dns error` and `tcp connect error`, with the OS's error under them
/// (its connect timeout too). A client that isn't hyper-util's counts when its innermost error is
/// the OS's refusal or lack of a route.
pub fn is_connection_failure(error: &anyhow::Error) -> bool {
    use std::io::ErrorKind;
    let named = error.chain().any(|cause| {
        let text = cause.to_string();
        text == "tcp connect error" || text == "dns error"
    });
    let innermost = error
        .chain()
        .last()
        .and_then(|cause| cause.downcast_ref::<std::io::Error>())
        .is_some_and(|io| {
            matches!(
                io.kind(),
                ErrorKind::ConnectionRefused
                    | ErrorKind::NotConnected
                    | ErrorKind::AddrNotAvailable
                    | ErrorKind::NetworkUnreachable
                    | ErrorKind::HostUnreachable
                    | ErrorKind::NetworkDown
            )
        });
    named || innermost
}

/// Whether a request was sent, or about to be, and got no answer: it timed out (reqwest's
/// `operation timed out`, or the OS's), or its connection went away under it (hyper-util's
/// `client error (SendRequest)`, a reset or a closed connection), as with a pooled connection
/// that died while the Mac slept, or a proxy couldn't open the tunnel to GitHub (reqwest's
/// `unsuccessful tunnel`, which a proxy that can't reach GitHub answers with, or `unexpected eof
/// while tunneling`).
fn is_lost_answer(error: &anyhow::Error) -> bool {
    use std::io::ErrorKind;
    error.chain().any(|cause| {
        if let Some(io) = cause.downcast_ref::<std::io::Error>()
            && matches!(
                io.kind(),
                ErrorKind::TimedOut
                    | ErrorKind::ConnectionReset
                    | ErrorKind::ConnectionAborted
                    | ErrorKind::BrokenPipe
                    | ErrorKind::UnexpectedEof
            )
        {
            return true;
        }
        let text = cause.to_string();
        text == "operation timed out"
            || text == "client error (SendRequest)"
            || text == "unsuccessful tunnel"
            || text == "unexpected eof while tunneling"
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_failure_to_connect_means_github_cant_be_reached() {
        let network = |error: anyhow::Error| GithubError::Network(error);
        let io = |kind: std::io::ErrorKind| anyhow::Error::new(std::io::Error::from(kind));
        // A proxy that's down, and no network at all.
        assert!(network(io(std::io::ErrorKind::ConnectionRefused)).is_unreachable());
        assert!(network(io(std::io::ErrorKind::NetworkDown)).is_unreachable());
        assert!(
            network(
                io(std::io::ErrorKind::HostUnreachable).context("error sending request for url")
            )
            .is_unreachable()
        );
        // hyper's errors around the OS's, as reqwest nests them.
        let dns = anyhow::anyhow!("failed to lookup address information: nodename nor servname")
            .context("dns error")
            .context("client error (Connect)")
            .context("error sending request for url (https://api.github.com/user)");
        assert!(network(dns).is_unreachable());
        let connect_timeout = io(std::io::ErrorKind::TimedOut)
            .context("tcp connect error")
            .context("client error (Connect)")
            .context("error sending request for url (https://api.github.com/user)");
        assert!(network(connect_timeout).is_unreachable());

        // Connected, and then slow, or cut off: worth trying again, but not offline.
        let slow = anyhow::anyhow!("operation timed out")
            .context("error sending request for url (https://api.github.com/user)");
        assert!(!network(slow).is_unreachable());
        let slow = anyhow::anyhow!("operation timed out")
            .context("error sending request for url (https://api.github.com/user)");
        assert!(network(slow).is_retryable());
        assert!(!network(io(std::io::ErrorKind::TimedOut)).is_unreachable());
        assert!(network(io(std::io::ErrorKind::TimedOut)).is_retryable());
        assert!(!network(io(std::io::ErrorKind::ConnectionReset)).is_unreachable());
        assert!(network(io(std::io::ErrorKind::ConnectionReset)).is_retryable());
        // A TLS failure or a proxy's refusal, which hyper-util also calls a connect error.
        let proxy = anyhow::anyhow!("proxy authentication required")
            .context("client error (Connect)")
            .context("error sending request for url (https://api.github.com/user)");
        assert!(!network(proxy).is_unreachable());
        let proxy =
            anyhow::anyhow!("proxy authentication required").context("client error (Connect)");
        assert!(!network(proxy).is_retryable());
        // A proxy that can't open the tunnel, as when it can't reach GitHub itself.
        let tunnel = || anyhow::anyhow!("unsuccessful tunnel").context("client error (Connect)");
        assert!(network(tunnel()).is_retryable());
        assert!(!network(tunnel()).is_unreachable());
        // Waiting fixes a server error or a rate limit, not a refusal.
        let api = |status| GithubError::Api {
            status,
            message: "x".into(),
            documentation_url: None,
            errors: Vec::new(),
        };
        assert!(api(502).is_waitable());
        assert!(api(429).is_waitable());
        assert!(!api(403).is_waitable());
        assert!(
            !GithubError::SsoRequired {
                message: "x".into(),
                url: None
            }
            .is_waitable()
        );
        // Any answer.
        assert!(
            !GithubError::NotFound {
                message: "Not Found".into(),
                documentation_url: None
            }
            .is_unreachable()
        );
    }

    use http_client::http::{HeaderName, HeaderValue};

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        pairs
            .iter()
            .map(|(name, value)| {
                (
                    HeaderName::from_static(name),
                    HeaderValue::from_str(value).unwrap(),
                )
            })
            .collect()
    }

    const RESTRICTED: &str = "Although you appear to have the correct authorization credentials, the `rozed-dev` organization has enabled OAuth App access restrictions, meaning that data access to third-parties is limited. For more information on these restrictions, including how to enable this app, visit https://docs.github.com/articles/restricting-access-to-your-organization-s-data/";

    #[test]
    fn a_401_signs_the_account_out() {
        let error = GithubError::from_response(
            401,
            &HeaderMap::new(),
            br#"{"message":"Bad credentials","documentation_url":"https://docs.github.com/rest","status":"401"}"#,
        );
        assert!(error.is_unauthorized());
        assert_eq!(
            error.to_string(),
            "GitHub didn't accept the token: Bad credentials"
        );
    }

    #[test]
    fn sso_keeps_githubs_link() {
        let error = GithubError::from_response(
            403,
            &headers(&[(
                "x-github-sso",
                "required; url=https://github.com/orgs/acme/sso?authorization_request=ABC",
            )]),
            br#"{"message":"Resource protected by organization SAML enforcement. You must grant your OAuth token access to this organization."}"#,
        );
        let GithubError::SsoRequired { url, .. } = &error else {
            panic!("{error:?}");
        };
        assert_eq!(
            url.as_deref(),
            Some("https://github.com/orgs/acme/sso?authorization_request=ABC")
        );
        assert_eq!(
            error.help_link(),
            HelpLink::new(
                "Authorize on GitHub",
                "https://github.com/orgs/acme/sso?authorization_request=ABC"
            )
        );
    }

    #[test]
    fn help_links_open_only_https_pages_on_github() {
        let sso = |url: &str| GithubError::SsoRequired {
            message: "SAML".into(),
            url: Some(url.into()),
        };
        for url in [
            "file:///Applications/Calculator.app",
            "x-apple-systempreferences:com.apple.preference",
            "http://github.com/orgs/acme/sso",
            "https://github.com.evil.example/orgs/acme/sso",
            "https://evil.example/github.com/orgs/acme/sso",
            "https://gist.github.com/x",
            "https://user@github.com/orgs/acme/sso",
        ] {
            assert_eq!(sso(url).help_link(), None, "{url}");
        }
        let link = sso("https://github.com/orgs/acme/sso?authorization_request=X")
            .help_link()
            .unwrap();
        assert_eq!(
            link.url(),
            "https://github.com/orgs/acme/sso?authorization_request=X"
        );
        assert_eq!(link.label(), "Authorize on GitHub");
    }

    #[test]
    fn an_oauth_restriction_names_the_org_and_links_to_approval() {
        let body = serde_json::json!({
            "message": RESTRICTED,
            "documentation_url": "https://docs.github.com/articles/restricting-access-to-your-organization-s-data/"
        });
        let error = GithubError::from_response(403, &HeaderMap::new(), body.to_string().as_bytes());
        let GithubError::OAuthAppRestricted {
            org,
            documentation_url,
            ..
        } = &error
        else {
            panic!("{error:?}");
        };
        assert_eq!(org.as_deref(), Some("rozed-dev"));
        assert!(documentation_url.is_some());
        assert_eq!(
            error.help_link().unwrap().url(),
            "https://github.com/settings/connections/applications/Ov23liBORqwRdiPzqsPP"
        );
    }

    #[test]
    fn rate_limits_are_primary_or_secondary() {
        let primary = GithubError::from_response(
            403,
            &headers(&[
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", "1762419600"),
            ]),
            br#"{"message":"API rate limit exceeded for user ID 1."}"#,
        );
        let GithubError::RateLimited {
            secondary, reset, ..
        } = primary
        else {
            panic!("{primary:?}");
        };
        assert!(!secondary);
        assert_eq!(reset, Some(1762419600));

        let secondary = GithubError::from_response(
            429,
            &headers(&[("retry-after", "60"), ("x-ratelimit-remaining", "4000")]),
            br#"{"message":"You have exceeded a secondary rate limit."}"#,
        );
        let GithubError::RateLimited {
            secondary,
            retry_after,
            ..
        } = secondary
        else {
            panic!("{secondary:?}");
        };
        assert!(secondary);
        assert_eq!(retry_after, Some(Duration::from_secs(60)));
    }

    #[test]
    fn rate_limits_name_the_limit_hit() {
        let used_up = GithubError::from_response(
            403,
            &headers(&[
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-resource", "search"),
            ]),
            br#"{"message":"API rate limit exceeded for user ID 1."}"#,
        );
        assert!(
            matches!(&used_up, GithubError::RateLimited { resource: Some(r), secondary: false, .. } if r == "search"),
            "{used_up:?}"
        );
        let graphql = |remaining: &'static str| {
            GithubError::from_graphql(
                vec![GraphQlError {
                    message: "API rate limit exceeded".into(),
                    r#type: Some("RATE_LIMITED".into()),
                    path: Vec::new(),
                    extensions: None,
                }],
                &headers(&[
                    ("x-ratelimit-remaining", remaining),
                    ("x-ratelimit-resource", "graphql"),
                ]),
            )
        };
        // GraphQL's limit used up, or GitHub's secondary limits with some of it left.
        assert!(
            matches!(graphql("0"), GithubError::RateLimited { resource: Some(r), secondary: false, .. } if r == "graphql")
        );
        assert!(matches!(
            graphql("4000"),
            GithubError::RateLimited {
                secondary: true,
                ..
            }
        ));
    }

    #[test]
    fn other_errors_keep_githubs_message_and_link() {
        let error = GithubError::from_response(
            422,
            &HeaderMap::new(),
            br#"{"message":"Validation Failed","errors":[{"resource":"PullRequest","code":"custom"}],"documentation_url":"https://docs.github.com/rest/pulls"}"#,
        );
        let GithubError::Api {
            status,
            message,
            documentation_url,
            errors,
        } = &error
        else {
            panic!("{error:?}");
        };
        assert_eq!(*status, 422);
        assert_eq!(message, "Validation Failed");
        assert_eq!(
            documentation_url.as_deref(),
            Some("https://docs.github.com/rest/pulls")
        );
        assert_eq!(errors.len(), 1);

        let error = GithubError::from_response(
            404,
            &HeaderMap::new(),
            br#"{"message":"Not Found","status":"404"}"#,
        );
        assert!(matches!(error, GithubError::NotFound { .. }), "{error:?}");

        let error = GithubError::from_response(502, &HeaderMap::new(), b"Bad gateway");
        assert_eq!(error.to_string(), "GitHub answered 502: Bad gateway");

        // A whole web page isn't a message.
        let page = format!(
            "<!DOCTYPE html>\n<html>{}</html>",
            "<p>Unicorn!</p>".repeat(500)
        );
        let error = GithubError::from_response(503, &HeaderMap::new(), page.as_bytes());
        assert_eq!(error.to_string(), "GitHub answered 503: (an HTML page)");
        let error = GithubError::from_response(500, &HeaderMap::new(), "x ".repeat(500).as_bytes());
        assert!(error.to_string().chars().count() < 250, "{error}");
    }

    #[test]
    fn graphql_errors_map_by_type_and_message() {
        let error = |json: &str, headers: &HeaderMap| {
            let errors: Vec<GraphQlError> = serde_json::from_str(json).unwrap();
            GithubError::from_graphql(errors, headers)
        };
        let not_found = error(
            r#"[{"type":"NOT_FOUND","path":["repository"],"message":"Could not resolve to a Repository with the name 'rozed-dev/nope'."}]"#,
            &HeaderMap::new(),
        );
        assert!(
            matches!(not_found, GithubError::NotFound { .. }),
            "{not_found:?}"
        );

        let restricted = error(
            &serde_json::json!([{"type": "FORBIDDEN", "message": RESTRICTED}]).to_string(),
            &HeaderMap::new(),
        );
        assert!(
            matches!(restricted, GithubError::OAuthAppRestricted { ref org, .. } if org.as_deref() == Some("rozed-dev")),
            "{restricted:?}"
        );

        let sso = error(
            r#"[{"type":"FORBIDDEN","message":"Resource protected by organization SAML enforcement. You must grant your OAuth token access to this organization."}]"#,
            &headers(&[(
                "x-github-sso",
                "required; url=https://github.com/orgs/a/sso",
            )]),
        );
        assert!(
            matches!(sso, GithubError::SsoRequired { ref url, .. } if url.as_deref() == Some("https://github.com/orgs/a/sso")),
            "{sso:?}"
        );

        let other = error(
            r#"[{"path":["query","nope"],"extensions":{"code":"undefinedField"},"message":"Field 'nope' doesn't exist on type 'PullRequest'"}]"#,
            &HeaderMap::new(),
        );
        assert_eq!(
            other.to_string(),
            "Field 'nope' doesn't exist on type 'PullRequest'"
        );
    }

    #[test]
    fn graphql_errors_are_classified_by_all_of_them() {
        let error = |json: serde_json::Value, headers: &HeaderMap| {
            let errors: Vec<GraphQlError> = serde_json::from_value(json).unwrap();
            GithubError::from_graphql(errors, headers)
        };
        // An org's restriction on a fork, after an unrelated error, still reads as the
        // restriction, with its link.
        let restricted = error(
            serde_json::json!([
                {"type": "NOT_FOUND", "path": ["repository", "pullRequest", "author"], "message": "gone"},
                {"type": "FORBIDDEN", "path": ["repository", "pullRequest", "headRepository"], "message": RESTRICTED},
            ]),
            &HeaderMap::new(),
        );
        assert!(
            matches!(restricted, GithubError::OAuthAppRestricted { ref org, .. } if org.as_deref() == Some("rozed-dev")),
            "{restricted:?}"
        );
        // Single sign-on comes before a restriction.
        let sso = error(
            serde_json::json!([
                {"type": "FORBIDDEN", "message": RESTRICTED},
                {"type": "FORBIDDEN", "message": "Resource protected by organization SAML enforcement. You must grant your OAuth token access to this organization."},
            ]),
            &HeaderMap::new(),
        );
        assert!(matches!(sso, GithubError::SsoRequired { .. }), "{sso:?}");
        // A rate limit after another error, with how long GitHub asked to wait.
        let limited = error(
            serde_json::json!([
                {"type": "NOT_FOUND", "message": "gone"},
                {"type": "RATE_LIMITED", "message": "API rate limit exceeded"},
            ]),
            &headers(&[("retry-after", "30"), ("x-ratelimit-reset", "1762419600")]),
        );
        let GithubError::RateLimited {
            message,
            retry_after,
            reset,
            ..
        } = &limited
        else {
            panic!("{limited:?}");
        };
        assert_eq!(message, "API rate limit exceeded");
        assert_eq!(*retry_after, Some(Duration::from_secs(30)));
        assert_eq!(*reset, Some(1762419600));
    }

    #[test]
    fn parses_the_sso_header() {
        assert_eq!(
            SsoHeader::parse("partial-results; organizations=21955855,20582480"),
            Some(SsoHeader {
                required: false,
                url: None,
                organizations: vec![21955855, 20582480]
            })
        );
        assert_eq!(SsoHeader::parse("something else"), None);
    }
}
