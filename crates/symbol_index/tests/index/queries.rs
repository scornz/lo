//! Each vendored query fits its grammar.

use std::sync::LazyLock;

use symbol_index::{Grammar, queries};

use crate::support::{GO, JAVASCRIPT, PYTHON, RUST, TSX, TYPESCRIPT};

#[test]
fn every_query_fits_its_grammar() {
    // Each grammar unwraps `Grammar::new` when first used.
    for grammar in [&RUST, &TYPESCRIPT, &TSX, &JAVASCRIPT, &PYTHON, &GO] {
        LazyLock::force(grammar);
    }
}

#[test]
fn a_query_for_another_grammar_is_an_error() {
    let error = Grammar::new(tree_sitter_python::LANGUAGE.into(), queries::RUST).unwrap_err();
    assert!(
        error.to_string().contains("can't use the tags query"),
        "{error}"
    );
}

#[test]
fn the_javascript_query_also_fits_typescript() {
    Grammar::new(
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        queries::JAVASCRIPT,
    )
    .unwrap();
}
