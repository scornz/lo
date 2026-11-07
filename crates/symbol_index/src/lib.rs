//! A symbol index built from tree-sitter tags queries (design §7, "Syntax-based, on every repo"):
//! go-to-definition, find-references and the outline by name, on any repository, without
//! language servers.
//!
//! - [`Grammar`] pairs a tree-sitter grammar with a tags query from [`queries`]. The caller
//!   passes both, so the app can use Zed's grammars.
//! - [`FileSymbols::read`] reads one file's definitions and references.
//! - [`Store`] keeps each blob's symbols across commits. [`Store::index`] indexes a commit's
//!   files, reading and parsing only the blobs it hasn't seen, on every core, and returns an
//!   [`Index`].
//! - [`Index::definitions`] ranks a name's definitions from the asking file,
//!   [`Index::references`] lists its uses and [`Index::symbols_in`] gives a file's outline.
//!
//! Where the design is silent, the index:
//!
//! - Runs each grammar's own tags query and then Lo's additions for what it leaves out
//!   ([`queries`]). It passes no locals query, since none of these tags queries consults one.
//! - Treats a Rust `impl` block as no definition, as tree-sitter-rust's query does, so a method in
//!   one has no parent.
//! - Gives references their kind and enclosing definition too, and positions with UTF-16 columns,
//!   which language servers count in.
//! - Keeps a blob's symbols per grammar and never evicts them.
//! - Skips files over 1 MB and generated files ([`Skip`]).
//! - Searches every language for a name. A definition in another language ranks with the rest.
//! - Reads imports with a small heuristic per language that knows nothing of `Cargo.toml`,
//!   `tsconfig.json`, `PYTHONPATH` or `go.mod`. Definitions an import names rank below the asking
//!   file's directory and above the rest ([`Rank`]).

mod file;
mod generated;
mod grammar;
mod imports;
mod index;
pub mod queries;
mod store;

pub use file::{Definition, FileSymbols, Position, Reference};
pub use grammar::{Grammar, GrammarError};
pub use index::{Found, Index, Rank};
pub use store::{BlobId, File, Limits, Skip, Store};
