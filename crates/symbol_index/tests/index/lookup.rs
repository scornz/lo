//! Looking up a name's definitions and references across a commit.

use symbol_index::{Index, Rank};

use crate::support::index;

/// Where `name`'s definitions are, ranked from `from_path`, as `rank path:row`.
fn definitions(index: &Index, name: &str, from_path: &str) -> Vec<String> {
    index
        .definitions(name, from_path)
        .iter()
        .map(|(rank, found)| {
            assert_eq!(&*found.symbol.name, name);
            format!("{rank:?} {}:{}", found.path, found.symbol.position.row)
        })
        .collect()
}

#[test]
fn definitions_rank_the_same_file_then_its_directory_then_the_rest() {
    let index = index(&[
        ("lib.rs", "fn parse() {}"),
        (
            "src/a/main.rs",
            "fn main() {\n    parse();\n}\n\nfn parse() {}",
        ),
        ("src/a/other.rs", "fn parse() {}"),
        ("src/a/sub/nested.rs", "fn parse() {}"),
        ("src/aa/sibling.rs", "fn parse() {}"),
        ("src/b/far.rs", "fn parse() {}"),
        ("src/b/unrelated.rs", "fn other() {}"),
    ]);

    assert_eq!(
        definitions(&index, "parse", "src/a/main.rs"),
        [
            "SameFile src/a/main.rs:4",
            "SameDirectory src/a/other.rs:0",
            // The rest in path order: a subdirectory isn't the same directory.
            "Other lib.rs:0",
            "Other src/a/sub/nested.rs:0",
            "Other src/aa/sibling.rs:0",
            "Other src/b/far.rs:0",
        ]
    );
}

#[test]
fn definitions_in_one_file_keep_their_order() {
    let index = index(&[
        (
            "shapes.ts",
            "class Circle {\n  area() {}\n}\nclass Square {\n  area() {}\n}",
        ),
        ("main.ts", "circle.area();"),
    ]);

    assert_eq!(
        definitions(&index, "area", "main.ts"),
        ["SameDirectory shapes.ts:1", "SameDirectory shapes.ts:4"]
    );
}

#[test]
fn definitions_rank_from_a_file_the_index_doesnt_have() {
    let index = index(&[
        ("src/a.py", "def parse(): pass"),
        ("b.py", "def parse(): pass"),
    ]);

    assert_eq!(
        definitions(&index, "parse", "src/new.py"),
        ["SameDirectory src/a.py:0", "Other b.py:0"]
    );
}

#[test]
fn a_name_nothing_defines_has_no_definitions() {
    let index = index(&[("main.rs", "fn main() { parse(); }")]);

    assert!(index.definitions("parse", "main.rs").is_empty());
    assert!(index.definitions("pars", "main.rs").is_empty());
}

#[test]
fn every_language_is_searched() {
    let index = index(&[
        ("backend/parse.rs", "fn parse() {}"),
        ("web/parse.ts", "export function parse() {}"),
        ("tools/parse.py", "def parse(): pass"),
        ("cli/parse.go", "package cli\n\nfunc parse() {}"),
    ]);

    assert_eq!(
        definitions(&index, "parse", "web/main.ts"),
        [
            "SameDirectory web/parse.ts:0",
            "Other backend/parse.rs:0",
            "Other cli/parse.go:2",
            "Other tools/parse.py:0",
        ]
    );
}

#[test]
fn references_are_every_use_in_path_order() {
    let index = index(&[
        (
            "src/main.rs",
            "fn main() {\n    parse();\n    run(parse);\n    parse();\n}",
        ),
        ("src/lib.rs", "pub fn parse() {}\nfn helper() { parse(); }"),
        ("src/other.rs", "fn other() { parser(); }"),
    ]);

    let references: Vec<String> = index
        .references("parse")
        .iter()
        .map(|found| {
            let enclosing = found.file.enclosing(found.symbol).unwrap();
            format!(
                "{}:{} in {}",
                found.path, found.symbol.position.row, enclosing.name
            )
        })
        .collect();

    // `run(parse)` passes it without calling it, which the query doesn't tag.
    assert_eq!(
        references,
        [
            "src/lib.rs:1 in helper",
            "src/main.rs:1 in main",
            "src/main.rs:3 in main"
        ]
    );
}

#[test]
fn ranks_order_from_nearest() {
    assert!(Rank::SameFile < Rank::SameDirectory);
    assert!(Rank::SameDirectory < Rank::Imported);
    assert!(Rank::Imported < Rank::Other);
}
