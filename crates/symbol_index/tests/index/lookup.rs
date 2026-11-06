//! Looking up a name's definitions and references across a commit.

use symbol_index::{Index, Rank};

use crate::support::index;

/// Where `name`'s definitions are, ranked from `from_path` with no imports, as `rank path:row`.
fn definitions(index: &Index, name: &str, from_path: &str) -> Vec<String> {
    ranked(index, name, from_path, "")
}

/// Where `name`'s definitions are, ranked from `from_path` holding `from_text`.
fn ranked(index: &Index, name: &str, from_path: &str, from_text: &str) -> Vec<String> {
    index
        .definitions(name, from_path, from_text)
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

    assert!(index.definitions("parse", "main.rs", "").is_empty());
    assert!(index.definitions("pars", "main.rs", "").is_empty());
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

#[test]
fn rust_imports_rank_the_files_they_name() {
    let index = index(&[
        (
            "crates/app/src/stacks/detect.rs",
            "pub struct Stack;\npub struct Layer;",
        ),
        ("crates/app/src/stacks/legacy.rs", "pub struct Stack;"),
        ("crates/app/src/ui/view.rs", "struct Stack;"),
        ("crates/app/src/ui/widgets.rs", "pub struct Stack;"),
        ("crates/review/src/stacks/detect.rs", "pub struct Stack;"),
        ("crates/review/src/lib.rs", "pub struct Review;"),
        ("crates/other/src/lib.rs", "pub struct Review;"),
    ]);
    let from = "crates/app/src/ui/view.rs";
    let text = "use crate::stacks::detect::Stack;\nuse review::Review;\nstruct Stack;";

    assert_eq!(
        ranked(&index, "Stack", from, text),
        [
            "SameFile crates/app/src/ui/view.rs:0",
            "SameDirectory crates/app/src/ui/widgets.rs:0",
            "Imported crates/app/src/stacks/detect.rs:0",
            // Another crate's module at the same path isn't the one `crate::` names.
            "Other crates/app/src/stacks/legacy.rs:0",
            "Other crates/review/src/stacks/detect.rs:0",
        ]
    );
    // Another crate in the repository, by its directory's name.
    assert_eq!(
        ranked(&index, "Review", from, text),
        [
            "Imported crates/review/src/lib.rs:0",
            "Other crates/other/src/lib.rs:0",
        ]
    );
    // The import names `Stack`, not every name in its file.
    assert_eq!(
        ranked(&index, "Layer", from, text),
        ["Other crates/app/src/stacks/detect.rs:1"]
    );
}

#[test]
fn typescript_imports_rank_the_files_they_name() {
    let index = index(&[
        (
            "web/src/components/button.tsx",
            "export function Button() {}",
        ),
        ("web/src/legacy/button.tsx", "export function Button() {}"),
        ("web/src/lib/utils/index.ts", "export function format() {}"),
        ("web/src/old/format.ts", "export function format() {}"),
        (
            "web/src/lib/date.js",
            "function parse() {}\nmodule.exports = { parse };",
        ),
        ("web/src/other/date.js", "function parse() {}"),
    ]);
    let from = "web/src/app/page.tsx";
    let text = r#"
import { Button } from "../components/button";
import * as utils from "@/lib/utils";
const { parse } = require("../lib/date.js");
"#;

    assert_eq!(
        ranked(&index, "Button", from, text),
        [
            "Imported web/src/components/button.tsx:0",
            "Other web/src/legacy/button.tsx:0",
        ]
    );
    // A namespace import, through a path alias, brings in any name.
    assert_eq!(
        ranked(&index, "format", from, text),
        [
            "Imported web/src/lib/utils/index.ts:0",
            "Other web/src/old/format.ts:0",
        ]
    );
    assert_eq!(
        ranked(&index, "parse", from, text),
        [
            "Imported web/src/lib/date.js:0",
            "Other web/src/other/date.js:0",
        ]
    );
}

#[test]
fn python_imports_rank_the_files_they_name() {
    let index = index(&[
        ("app/models.py", "class User: pass"),
        ("legacy/models.py", "class User: pass"),
        ("app/services/billing.py", "def charge(): pass"),
        ("legacy/billing.py", "def charge(): pass"),
    ]);
    let from = "app/views/home.py";
    let text = "from app.models import User\nfrom ..services import billing\n";

    assert_eq!(
        ranked(&index, "User", from, text),
        ["Imported app/models.py:0", "Other legacy/models.py:0"]
    );
    // `billing` is a module, used as `billing.charge()`.
    assert_eq!(
        ranked(&index, "charge", from, text),
        [
            "Imported app/services/billing.py:0",
            "Other legacy/billing.py:0"
        ]
    );
}

#[test]
fn go_imports_rank_the_packages_they_name() {
    let index = index(&[
        (
            "internal/store/store.go",
            "package store\n\nfunc Open() {}\nfunc open() {}",
        ),
        (
            "internal/cache/cache.go",
            "package cache\n\nfunc Open() {}\nfunc open() {}",
        ),
    ]);
    let from = "cmd/server/main.go";
    let text = "package main\n\nimport \"github.com/acme/app/internal/store\"\n";

    assert_eq!(
        ranked(&index, "Open", from, text),
        [
            "Imported internal/store/store.go:2",
            "Other internal/cache/cache.go:2",
        ]
    );
    // Only exported names come from another package.
    assert_eq!(
        ranked(&index, "open", from, text),
        [
            "Other internal/cache/cache.go:3",
            "Other internal/store/store.go:3",
        ]
    );
}

#[test]
fn the_same_directory_outranks_an_import() {
    let index = index(&[
        ("src/a/main.rs", ""),
        ("src/a/local.rs", "pub fn parse() {}"),
        ("src/b/parse.rs", "pub fn parse() {}"),
    ]);

    assert_eq!(
        ranked(
            &index,
            "parse",
            "src/a/main.rs",
            "use crate::b::parse::parse;"
        ),
        [
            "SameDirectory src/a/local.rs:0",
            "Imported src/b/parse.rs:0"
        ]
    );
}
