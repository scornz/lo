//! How long a block has to be, and how blank lines count.

use diff_extras::moved::FileChanges;

use crate::support::moves;

#[test]
fn a_move_needs_three_lines() {
    let three = ["let a = 1;", "let b = 2;", "let c = 3;"];
    let files = [
        FileChanges::new("a.rs").delete(1, three),
        FileChanges::new("b.rs").add(1, three),
    ];

    assert_eq!(moves(&files), ["a.rs:1-3 -> b.rs:1-3"]);
}

#[test]
fn two_lines_are_too_short() {
    let two = ["let a = 1;", "let b = 2;"];
    let files = [
        FileChanges::new("a.rs").delete(1, two),
        FileChanges::new("b.rs").add(1, two),
    ];

    assert_eq!(moves(&files), Vec::<String>::new());
}

#[test]
fn blank_lines_do_not_count_toward_three() {
    let two = ["let a = 1;", "", "  ", "let b = 2;", ""];
    let files = [
        FileChanges::new("a.rs").delete(1, two),
        FileChanges::new("b.rs").add(1, two),
    ];

    assert_eq!(moves(&files), Vec::<String>::new());
}

#[test]
fn blank_lines_sit_inside_a_block() {
    let files = [
        FileChanges::new("a.rs").delete(1, ["let a = 1;", "", "let b = 2;", "", "let c = 3;"]),
        FileChanges::new("b.rs").add(1, ["let a = 1;", "let b = 2;", "", "", "", "let c = 3;"]),
    ];

    assert_eq!(moves(&files), ["a.rs:1-5 -> b.rs:1-6"]);
}

#[test]
fn a_block_starts_and_ends_on_a_line_that_is_not_blank() {
    let files = [
        FileChanges::new("a.rs").delete(1, ["", "let a = 1;", "let b = 2;", "let c = 3;", ""]),
        FileChanges::new("b.rs").add(1, ["", "", "let a = 1;", "let b = 2;", "let c = 3;"]),
    ];

    assert_eq!(moves(&files), ["a.rs:2-4 -> b.rs:3-5"]);
}

#[test]
fn a_block_is_consecutive_lines() {
    // The same three lines, but an unchanged line splits the deletion in two.
    let files = [
        FileChanges::new("a.rs")
            .delete(1, ["let a = 1;", "let b = 2;"])
            .delete(4, ["let c = 3;"]),
        FileChanges::new("b.rs").add(1, ["let a = 1;", "let b = 2;", "let c = 3;"]),
    ];

    assert_eq!(moves(&files), Vec::<String>::new());
}
