//! Helpers that parse both versions of a file and describe the rows compactly.

use diff_extras::syntax::{Fallback, Language, Limits, Line, Source, SyntaxDiff, compare};
use tree_sitter::{Parser, Tree};

pub fn tree(grammar: &str, text: &str) -> Tree {
    let language: tree_sitter::Language = match grammar {
        "json" => tree_sitter_json::LANGUAGE.into(),
        "rust" => tree_sitter_rust::LANGUAGE.into(),
        "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        _ => panic!("no grammar {grammar} in the tests"),
    };
    let mut parser = Parser::new();
    parser.set_language(&language).unwrap();
    parser.parse(text, None).unwrap()
}

/// Compares `old` and `new` in `grammar` with `limits`.
pub fn compare_with(
    grammar: &str,
    old: &str,
    new: &str,
    limits: &Limits,
) -> Result<SyntaxDiff, Fallback> {
    let (old_tree, new_tree) = (tree(grammar, old), tree(grammar, new));
    compare(
        Language::for_grammar(grammar),
        Source {
            text: old,
            tree: Some(&old_tree),
        },
        Source {
            text: new,
            tree: Some(&new_tree),
        },
        limits,
    )
}

/// Each row as `old | new`: a side is its line number and text with novel ranges in brackets,
/// such as `2 let [x] = 1;`, or `-` when the row has no line there.
pub fn rows(grammar: &str, old: &str, new: &str) -> Vec<String> {
    let diff = compare_with(grammar, old, new, &Limits::default()).unwrap();
    assert_every_line_once(&diff, old, new);
    diff.rows
        .iter()
        .map(|row| {
            format!(
                "{} | {}",
                describe(row.old.as_ref(), old),
                describe(row.new.as_ref(), new)
            )
        })
        .collect()
}

fn describe(line: Option<&Line>, text: &str) -> String {
    let Some(line) = line else {
        return "-".to_string();
    };
    let content = text.lines().nth(line.number as usize - 1).unwrap();
    let mut out = format!("{} ", line.number);
    let mut at = 0;
    for range in &line.novel {
        out.push_str(&content[at..range.start]);
        out.push('[');
        out.push_str(&content[range.clone()]);
        out.push(']');
        at = range.end;
    }
    out.push_str(&content[at..]);
    out.trim_end().to_string()
}

/// Every line of each version is in exactly one row, in order.
pub fn assert_every_line_once(diff: &SyntaxDiff, old: &str, new: &str) {
    let old_numbers: Vec<u32> = diff
        .rows
        .iter()
        .filter_map(|row| row.old.as_ref())
        .map(|line| line.number)
        .collect();
    let new_numbers: Vec<u32> = diff
        .rows
        .iter()
        .filter_map(|row| row.new.as_ref())
        .map(|line| line.number)
        .collect();
    assert_eq!(
        old_numbers,
        (1..=old.lines().count() as u32).collect::<Vec<_>>()
    );
    assert_eq!(
        new_numbers,
        (1..=new.lines().count() as u32).collect::<Vec<_>>()
    );
}
