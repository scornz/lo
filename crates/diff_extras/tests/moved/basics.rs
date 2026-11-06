//! Blocks moved unchanged: within a file, across files and re-indented.

use diff_extras::moved::{EditedLine, FileChanges, Moves, Place};

use crate::support::{TOTAL, indented, moves};

#[test]
fn finds_a_block_moved_within_a_file() {
    let file = FileChanges::new("src/cart.rs")
        .delete(10, TOTAL)
        .add(40, TOTAL);

    assert_eq!(moves(&[file]), ["src/cart.rs:10-13 -> src/cart.rs:40-43"]);
}

#[test]
fn finds_a_block_moved_to_another_file() {
    let files = [
        FileChanges::new("src/cart.rs").delete(10, TOTAL),
        FileChanges::new("src/pricing.rs").add(1, TOTAL),
    ];

    let found = Moves::find(&files);

    assert_eq!(found.all().len(), 1);
    let moved = &found.all()[0];
    assert_eq!(
        moved.from,
        Place {
            file: 0,
            path: "src/cart.rs".into(),
            lines: 10..=13,
        }
    );
    assert_eq!(
        moved.to,
        Place {
            file: 1,
            path: "src/pricing.rs".into(),
            lines: 1..=4,
        }
    );
    assert_eq!(moved.edits, Vec::<EditedLine>::new());
}

#[test]
fn names_a_renamed_file_by_its_old_path_where_the_block_came_from() {
    let file = FileChanges::new("src/billing.rs")
        .renamed_from("src/cart.rs")
        .delete(10, TOTAL)
        .add(40, TOTAL);

    assert_eq!(
        moves(&[file]),
        ["src/cart.rs:10-13 -> src/billing.rs:40-43"]
    );
}

#[test]
fn ignores_changes_in_indentation() {
    let files = [
        FileChanges::new("src/cart.rs").delete(10, TOTAL),
        FileChanges::new("src/pricing.rs").add(20, indented(4, &TOTAL)),
    ];

    assert_eq!(moves(&files), ["src/cart.rs:10-13 -> src/pricing.rs:20-23"]);
}

#[test]
fn ignores_trailing_whitespace() {
    let trailing: Vec<String> = TOTAL.iter().map(|line| format!("{line}  \t")).collect();
    let files = [
        FileChanges::new("src/cart.rs").delete(10, trailing),
        FileChanges::new("src/pricing.rs").add(20, TOTAL),
    ];

    assert_eq!(moves(&files), ["src/cart.rs:10-13 -> src/pricing.rs:20-23"]);
}

#[test]
fn finds_only_the_moved_part_of_larger_changes() {
    let mut deleted = vec!["let before = 1;"];
    deleted.extend(TOTAL);
    deleted.push("let after = 2;");
    let mut added = vec!["let other = 3;"];
    added.extend(TOTAL);
    added.push("let another = 4;");
    let files = [
        FileChanges::new("src/cart.rs").delete(10, deleted),
        FileChanges::new("src/pricing.rs").add(20, added),
    ];

    assert_eq!(moves(&files), ["src/cart.rs:11-14 -> src/pricing.rs:21-24"]);
}

#[test]
fn finds_several_moves_in_order_of_where_they_land() {
    let other = ["fn zero() -> u32 {", "    let zero = 0;", "    zero", "}"];
    let files = [
        FileChanges::new("src/a.rs")
            .delete(5, other)
            .delete(30, TOTAL),
        FileChanges::new("src/b.rs").add(1, TOTAL).add(10, other),
    ];

    assert_eq!(
        moves(&files),
        [
            "src/a.rs:30-33 -> src/b.rs:1-4",
            "src/a.rs:5-8 -> src/b.rs:10-13"
        ]
    );
}

#[test]
fn finds_nothing_without_a_match() {
    let files = [
        FileChanges::new("src/a.rs").delete(1, TOTAL),
        FileChanges::new("src/b.rs").add(1, ["fn other() {", "    work();", "    more();", "}"]),
    ];

    assert_eq!(moves(&files), Vec::<String>::new());
}

#[test]
fn takes_lines_in_any_order() {
    let mut deleted = FileChanges::new("src/cart.rs").delete(10, TOTAL).deleted;
    deleted.reverse();
    let mut file = FileChanges::new("src/cart.rs").add(40, TOTAL);
    file.added.swap(0, 2);
    file.deleted = deleted;

    assert_eq!(moves(&[file]), ["src/cart.rs:10-13 -> src/cart.rs:40-43"]);
}
