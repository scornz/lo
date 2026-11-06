//! Tags queries for the languages rozed indexes, to pass to [`Grammar::new`](crate::Grammar::new)
//! with the matching tree-sitter grammar.
//!
//! Each starts with its grammar's own `queries/tags.scm`, vendored unchanged at the grammar
//! version Zed uses (`THIRD_PARTY_NOTICES.md` names each source and its license). Those are thin
//! in places, such as Rust's, which has no constants, trait methods or type references, so each
//! is followed by rozed's own patterns for what it leaves out (`queries/<language>/lo.scm`).
//! A name tagged by both keeps the grammar's tag, because the earlier pattern wins.

/// Rust, from `tree-sitter-rust` 0.24.2.
pub const RUST: &str = concat!(
    include_str!("../queries/rust/tags.scm"),
    include_str!("../queries/rust/lo.scm"),
);

/// TypeScript, for the TypeScript grammar of the `tree-sitter-typescript` fork Zed pins.
///
/// As in that grammar's `tree-sitter.json`, it's TypeScript's query followed by JavaScript's
/// (`tree-sitter-javascript` 0.23.1, the version `tree-sitter-typescript` builds on).
pub const TYPESCRIPT: &str = concat!(
    include_str!("../queries/typescript/tags.scm"),
    include_str!("../queries/javascript/tags.scm"),
    include_str!("../queries/typescript/lo.scm"),
);

/// TSX, for the TSX grammar: [`TYPESCRIPT`] and JSX components.
pub const TSX: &str = concat!(
    include_str!("../queries/typescript/tags.scm"),
    include_str!("../queries/javascript/tags.scm"),
    include_str!("../queries/typescript/lo.scm"),
    include_str!("../queries/tsx/lo.scm"),
);

/// JavaScript. Zed reads JavaScript with the TSX grammar, so this is [`TSX`]: pass it with the
/// TSX grammar.
pub const JAVASCRIPT: &str = TSX;

/// Python, from `tree-sitter-python` 0.25.
pub const PYTHON: &str = concat!(
    include_str!("../queries/python/tags.scm"),
    include_str!("../queries/python/lo.scm"),
);

/// Go, from the `tree-sitter-go` fork Zed pins.
pub const GO: &str = concat!(
    include_str!("../queries/go/tags.scm"),
    include_str!("../queries/go/lo.scm"),
);
