//! What rozed decides about a pull request's changes before it draws them (design §5, §6).
//!
//! Pure logic: no UI, network or git. Callers map GitHub's and git's data into the input types
//! here.

pub mod files;
