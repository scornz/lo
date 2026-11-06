//! rozed's GitHub layer: signing in, storing accounts and calling GitHub's REST and GraphQL APIs
//! (design §3).
//!
//! Everything goes through an [`http_client::HttpClient`]: in the app, the shared client that
//! refuses Zed's hosts; in tests, a [`replay::ReplayHttpClient`] serving recorded responses.

pub mod accounts;
#[cfg(any(test, feature = "test-support"))]
pub mod cassette;
mod client;
pub mod credentials;
pub mod db;
pub mod device_flow;
mod error;
pub mod gh_import;
mod graphql;
pub mod poll;
pub mod queries;
mod rate_limit;
#[cfg(any(test, feature = "test-support"))]
pub mod record;
#[cfg(any(test, feature = "test-support"))]
pub mod replay;
mod token;
mod user;

pub use client::{
    API_URL, API_VERSION, ClientState, Conditional, GithubClient, RestResponse, user_agent,
};
pub use error::{CLIENT_ID, GithubError, HelpLink, SsoHeader, approval_url};
pub use graphql::{GraphQlError, GraphQlResponse};
pub use rate_limit::{GraphQlRateLimit, RateLimit, RateLimits};
pub use token::{Token, TokenKind};
pub use user::{AuthenticatedUser, SCOPES, User, missing_scopes, parse_scopes};
