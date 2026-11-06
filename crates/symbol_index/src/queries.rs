//! Tags queries for the languages rozed indexes, to pass to [`Grammar::new`](crate::Grammar::new)
//! with the matching tree-sitter grammar.
//!
//! Each is vendored from its grammar's own `queries/tags.scm`, at the grammar version Zed uses
//! (`THIRD_PARTY_NOTICES.md` names each source and its license).

/// Rust, from `tree-sitter-rust` 0.24.2.
pub const RUST: &str = include_str!("../queries/rust/tags.scm");

/// TypeScript, TSX and JavaScript, for the TypeScript and TSX grammars of the
/// `tree-sitter-typescript` fork Zed pins.
///
/// As in that grammar's `tree-sitter.json`, it's TypeScript's query followed by JavaScript's
/// (`tree-sitter-javascript` 0.23.1, the version `tree-sitter-typescript` builds on).
pub const TYPESCRIPT: &str = concat!(
    include_str!("../queries/typescript/tags.scm"),
    include_str!("../queries/javascript/tags.scm"),
);

/// JavaScript. Zed reads JavaScript with the TSX grammar, so this is [`TYPESCRIPT`]: pass it with
/// the TSX grammar.
pub const JAVASCRIPT: &str = TYPESCRIPT;

/// Python, from `tree-sitter-python` 0.25.
pub const PYTHON: &str = include_str!("../queries/python/tags.scm");

/// Go, from the `tree-sitter-go` fork Zed pins.
pub const GO: &str = include_str!("../queries/go/tags.scm");
