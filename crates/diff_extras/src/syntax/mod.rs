//! The syntax-aware diff: compares two versions of a file by their syntax trees rather than by
//! their lines, with difftastic's algorithm.

// Until the diff uses them.
#[allow(dead_code)]
mod changes;
#[allow(dead_code)]
mod dijkstra;
#[allow(dead_code)]
mod graph;
mod language;
#[allow(dead_code)]
mod nodes;
#[allow(dead_code)]
mod parse;

pub use language::Language;
