//! When the syntax-aware diff gives up, so the caller keeps the line diff.

use diff_extras::syntax::{Fallback, Language, Limits, Source, compare};

use crate::support::{compare_with, tree};

#[test]
fn a_file_without_a_grammar() {
    let text = "plain text\n";

    let result = compare(
        None,
        Source { text, tree: None },
        Source { text, tree: None },
        &Limits::default(),
    );

    assert_eq!(result, Err(Fallback::NoGrammar));
}

#[test]
fn a_grammar_without_settings() {
    let text = "{}\n";
    let tree = tree("json", text);
    let source = Source {
        text,
        tree: Some(&tree),
    };

    assert_eq!(Language::for_grammar("markdown"), None);
    assert_eq!(
        compare(
            Language::for_grammar("markdown"),
            source,
            source,
            &Limits::default()
        ),
        Err(Fallback::NoGrammar)
    );
}

#[test]
fn a_version_without_a_tree() {
    let text = "{}\n";
    let tree = tree("json", text);

    let result = compare(
        Language::for_grammar("json"),
        Source { text, tree: None },
        Source {
            text,
            tree: Some(&tree),
        },
        &Limits::default(),
    );

    assert_eq!(result, Err(Fallback::NoGrammar));
}

#[test]
fn a_file_past_the_byte_limit() {
    let old = "[1, 2, 3]\n";
    let new = "[1, 2, 3, 4]\n";
    let limits = Limits {
        bytes: old.len(),
        ..Limits::default()
    };

    assert_eq!(
        compare_with("json", old, new, &limits),
        Err(Fallback::TooLarge)
    );
    assert!(compare_with("json", old, old, &limits).is_ok());
}

#[test]
fn a_diff_past_the_graph_limit() {
    let old = "[1, 2, 3, 4, 5, 6, 7, 8]\n";
    let new = "[\"a\", \"b\", \"c\", \"d\", \"e\", \"f\", \"g\", \"h\"]\n";
    let limits = Limits {
        graph: 20,
        ..Limits::default()
    };

    assert_eq!(
        compare_with("json", old, new, &limits),
        Err(Fallback::TooLarge)
    );
    assert!(compare_with("json", old, new, &Limits::default()).is_ok());
}

#[test]
fn the_default_limits_are_difftastics() {
    assert_eq!(
        Limits::default(),
        Limits {
            bytes: 1_000_000,
            graph: 3_000_000,
            parse_errors: 0,
        }
    );
}

#[test]
fn trees_with_parse_errors() {
    let old = "fn f() {}\n";
    let new = "fn f( {\n";

    let result = compare_with("rust", old, new, &Limits::default());

    assert!(
        matches!(result, Err(Fallback::ParseErrors { count }) if count > 0),
        "{result:?}"
    );
    let lenient = Limits {
        parse_errors: 10,
        ..Limits::default()
    };
    assert!(compare_with("rust", old, new, &lenient).is_ok());
}
