//! A symbol index built from tree-sitter tags queries (design §7, "Syntax-based, on every repo").

mod file;
mod grammar;
pub mod queries;

pub use file::{Definition, FileSymbols, Position, Reference};
pub use grammar::{Grammar, GrammarError};
