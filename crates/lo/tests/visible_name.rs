//! Text a person reads names the app "Lo", with a capital L: a lowercase l reads as a capital I in
//! sans-serif fonts. Lowercase `lo` is only for names that are code, such as `lo://`, `refs/lo/`,
//! `~/.config/lo`, `lo.sqlite` or the `lo` command.
//!
//! This reads every string literal in the workspace's Rust files and fails on a lowercase `lo` used
//! as a word, as in "lo couldn't find gh". A `lo` joined to code by `/`, `.`, `-`, `_`, `:`, `=` or
//! a backtick passes, and so does the usage line's command. A literal that's only a code name, such
//! as a scheme, says so with a `code name, not text` comment on its line or the line above.

use std::fs;
use std::path::{Path, PathBuf};

use tree_sitter::{Node, Parser};

/// What marks a literal that's a code name.
const CODE_NAME: &str = "code name, not text";

#[test]
fn text_a_person_reads_says_lo_with_a_capital_l() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut files = Vec::new();
    rust_files(crates, &mut files);
    // This file's own examples.
    files.retain(|file| !file.ends_with(file!()));
    assert!(files.len() > 100, "found only {} Rust files", files.len());

    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .unwrap();
    let mut found = Vec::new();
    for file in &files {
        let source = fs::read_to_string(file).unwrap();
        let tree = parser.parse(&source, None).unwrap();
        let lines: Vec<&str> = source.lines().collect();
        let mut literals = Vec::new();
        string_literals(tree.root_node(), &mut literals);
        for literal in literals {
            let text = &source[literal.byte_range()];
            if !lowercase_lo_as_a_word(content(text)) {
                continue;
            }
            let row = literal.start_position().row;
            let marked = lines[row].contains(CODE_NAME)
                || row
                    .checked_sub(1)
                    .is_some_and(|above| lines[above].contains(CODE_NAME));
            if !marked {
                let path = file.strip_prefix(crates).unwrap_or(file);
                found.push(format!("crates/{}:{}: {text}", path.display(), row + 1));
            }
        }
    }
    assert!(
        found.is_empty(),
        "These literals say lowercase \"lo\" where text a person reads says \"Lo\" (or, for a \
         code name, mark it `// {CODE_NAME}`):\n{}",
        found.join("\n")
    );
}

#[test]
fn code_names_pass_and_prose_fails() {
    for code in [
        "lo://github.com/a",
        "lo-dev://github.com/a",
        "refs/lo/pull/1/head",
        ".config/lo",
        "lo/0.1.0 (macOS; aarch64)",
        "lo.sqlite",
        "lo-last-used",
        "info,lo=debug",
        "lo: can't open the log",
        "usage: lo [--trust] [<pull request link>]",
        "example: lo https://github.com/sharkdp/fd/pull/2212",
        "running `lo --printenv`",
        "leave_lo_s_memory_flat",
        "\\nLO-PREVIEW-REPLY 1\\n",
        "Lo couldn't find gh",
        "Quit Lo",
        "hello, lots of logs",
    ] {
        assert!(!lowercase_lo_as_a_word(code), "{code}");
    }
    for prose in [
        "lo",
        "lo couldn't find gh",
        "Quit lo",
        "the account lo uses",
        "remove it from lo.",
        "lo's database isn't open yet",
        "(lo couldn't show this file)",
    ] {
        assert!(lowercase_lo_as_a_word(prose), "{prose}");
    }
}

/// Every `.rs` file under `dir`, leaving out build output.
fn rust_files(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy();
        if path.is_dir() {
            if name != "target" && !name.starts_with('.') {
                rust_files(&path, files);
            }
        } else if name.ends_with(".rs") {
            files.push(path);
        }
    }
}

/// The string literals in `node`'s tree, byte strings and raw strings included.
fn string_literals<'tree>(node: Node<'tree>, literals: &mut Vec<Node<'tree>>) {
    if matches!(node.kind(), "string_literal" | "raw_string_literal") {
        literals.push(node);
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        string_literals(child, literals);
    }
}

/// A literal's text between its quotes, as written: escapes stay escaped.
fn content(literal: &str) -> &str {
    let start = literal.find('"').map_or(0, |quote| quote + 1);
    let end = literal
        .rfind('"')
        .filter(|&end| end >= start)
        .unwrap_or(literal.len());
    &literal[start..end]
}

/// Whether `text` has a lowercase `lo` that reads as a word rather than as part of a code name.
fn lowercase_lo_as_a_word(text: &str) -> bool {
    let bytes = text.as_bytes();
    text.match_indices("lo").any(|(at, _)| {
        let before = at.checked_sub(1).map(|i| bytes[i]);
        let after = bytes.get(at + 2).copied();
        let joined_before =
            before.is_some_and(|b| b.is_ascii_alphanumeric() || b"_/.-$`{\\".contains(&b));
        let joined_after = match after {
            None => false,
            Some(b'.') => bytes.get(at + 3).is_some_and(u8::is_ascii_alphanumeric),
            Some(b) => b.is_ascii_alphanumeric() || b"_/-:=`}\\".contains(&b),
        };
        let command = text[..at].ends_with("usage: ") || text[..at].ends_with("example: ");
        !joined_before && !joined_after && !command
    })
}
