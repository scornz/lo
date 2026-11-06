//! What rozed decides about a pull request's changes before it draws them.
//!
//! Pure logic: no UI, network or git. Callers map GitHub's and git's data into the input types
//! here.

mod github;

pub mod drafts;
pub mod files;
pub mod merge;
pub mod pending;
pub mod revisions;
pub mod threads;

pub use github::UnknownValue;
