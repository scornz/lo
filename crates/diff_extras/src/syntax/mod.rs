//! The syntax-aware diff: compares two versions of a file by their syntax trees rather than by
//! their lines, with difftastic's algorithm.

mod language;
// Until the diff uses them.
#[allow(dead_code)]
mod nodes;
#[allow(dead_code)]
mod parse;

pub use language::Language;
