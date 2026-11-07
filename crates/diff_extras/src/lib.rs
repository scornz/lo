//! What Lo's diff view adds to Zed's line diff. [`moved`] finds blocks of code a pull request
//! moves, so the view can show them as moves rather than as unrelated deletions and additions.
//! [`syntax`] compares a file's two versions by their syntax trees, for the syntax-aware view.

pub mod moved;
pub mod syntax;
