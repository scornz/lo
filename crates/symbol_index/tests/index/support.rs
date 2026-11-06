//! Grammars for each language, built once per test binary.

use std::sync::LazyLock;

use symbol_index::{Grammar, queries};

pub static RUST: LazyLock<Grammar> =
    LazyLock::new(|| Grammar::new(tree_sitter_rust::LANGUAGE.into(), queries::RUST).unwrap());

pub static TYPESCRIPT: LazyLock<Grammar> = LazyLock::new(|| {
    Grammar::new(
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        queries::TYPESCRIPT,
    )
    .unwrap()
});

pub static TSX: LazyLock<Grammar> = LazyLock::new(|| {
    Grammar::new(
        tree_sitter_typescript::LANGUAGE_TSX.into(),
        queries::TYPESCRIPT,
    )
    .unwrap()
});

/// Zed reads JavaScript with the TSX grammar.
pub static JAVASCRIPT: LazyLock<Grammar> = LazyLock::new(|| {
    Grammar::new(
        tree_sitter_typescript::LANGUAGE_TSX.into(),
        queries::JAVASCRIPT,
    )
    .unwrap()
});

pub static PYTHON: LazyLock<Grammar> =
    LazyLock::new(|| Grammar::new(tree_sitter_python::LANGUAGE.into(), queries::PYTHON).unwrap());

pub static GO: LazyLock<Grammar> =
    LazyLock::new(|| Grammar::new(tree_sitter_go::LANGUAGE.into(), queries::GO).unwrap());
