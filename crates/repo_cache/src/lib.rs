//! rozed's local copy of each repository it shows, built on the git CLI.
//!
//! [`Git`] finds git and runs it in an environment of rozed's own, so the user's git config is
//! never read or written (see [`git`](crate::git) for what that environment holds).

mod askpass;
mod error;
mod git;

pub use error::Error;
pub use git::{Git, GitVersion, Token};
