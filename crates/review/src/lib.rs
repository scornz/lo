//! What rozed decides about a pull request's changes before it draws them.
//!
//! Pure logic: no UI, network or git. Callers map GitHub's and git's data into the input types
//! here.

mod github;

pub mod files;
pub mod merge;
pub mod revisions;

pub use github::UnknownValue;
