//! A symbol index built from tree-sitter tags queries (design §7, "Syntax-based, on every repo").

mod file;
mod generated;
mod grammar;
mod index;
pub mod queries;
mod store;

pub use file::{Definition, FileSymbols, Position, Reference};
pub use grammar::{Grammar, GrammarError};
pub use index::Index;
pub use store::{BlobId, File, Limits, Skip, Store};
