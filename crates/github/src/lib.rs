//! rozed's GitHub layer: signing in, storing accounts and calling GitHub's REST and GraphQL APIs
//! (design §3).
//!
//! Everything goes through an [`http_client::HttpClient`]: in the app, the shared client that
//! refuses Zed's hosts; in tests, a [`replay::ReplayHttpClient`] serving recorded responses.

#[cfg(any(test, feature = "test-support"))]
pub mod cassette;
#[cfg(any(test, feature = "test-support"))]
pub mod record;
#[cfg(any(test, feature = "test-support"))]
pub mod replay;
mod token;

pub use token::{Token, TokenKind};
