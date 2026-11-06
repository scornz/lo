//! Grammars for each language, built once per test binary.

use std::sync::LazyLock;

use symbol_index::{FileSymbols, Grammar, queries};

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

/// A file's definitions as `kind name`, with `< parent` when one encloses it.
pub fn definitions(symbols: &FileSymbols) -> Vec<String> {
    symbols
        .definitions()
        .iter()
        .map(|definition| {
            let mut line = format!("{} {}", definition.kind, definition.name);
            if let Some(parent) = symbols.parent(definition) {
                line.push_str(&format!(" < {}", parent.name));
            }
            line
        })
        .collect()
}

/// A file's references as `kind name`, with `in definition` when one encloses it.
pub fn references(symbols: &FileSymbols) -> Vec<String> {
    symbols
        .references()
        .iter()
        .map(|reference| {
            let mut line = format!("{} {}", reference.kind, reference.name);
            if let Some(enclosing) = symbols.enclosing(reference) {
                line.push_str(&format!(" in {}", enclosing.name));
            }
            line
        })
        .collect()
}
