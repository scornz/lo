//! A symbol index built from tree-sitter tags queries (design §7, "Syntax-based, on every repo").

mod grammar;
pub mod queries;

pub use grammar::{Grammar, GrammarError};
