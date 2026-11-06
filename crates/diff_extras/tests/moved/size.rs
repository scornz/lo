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

#[test]
fn lines_of_only_punctuation_are_not_a_move() {
    let braces = ["        }", "    });", "}"];
    let files = [
        FileChanges::new("a.rs").delete(1, braces),
        FileChanges::new("b.rs").add(1, braces),
    ];

    assert_eq!(moves(&files), Vec::<String>::new());
}

#[test]
fn lines_of_only_punctuation_do_not_count_toward_three() {
    let call = ["run(", "    first,", ");"];
    let files = [
        FileChanges::new("a.rs").delete(1, call),
        FileChanges::new("b.rs").add(1, call),
    ];

    assert_eq!(moves(&files), Vec::<String>::new());
}

#[test]
fn lines_of_only_punctuation_sit_inside_a_block() {
    let block = ["let list = vec![", "    1,", "];", "let total = 2;"];
    let files = [
        FileChanges::new("a.rs").delete(1, block),
        FileChanges::new("b.rs").add(1, block),
    ];

    assert_eq!(moves(&files), ["a.rs:1-4 -> b.rs:1-4"]);
}

#[test]
fn lines_with_letters_or_digits_decide_which_match_is_longer() {
    let parts = ["let p1 = 1;", "let p2 = 2;", "let p3 = 3;", "let p4 = 4;"];
    let mut old = parts.to_vec();
    old.extend(["}"; 6]);
    let files = [
        FileChanges::new("a.rs").delete(1, &old),
        // Nine matching lines, but only three with letters or digits.
        FileChanges::new("b.rs").add(1, &old[1..]),
        FileChanges::new("c.rs").add(1, parts),
    ];

    assert_eq!(moves(&files), ["a.rs:1-4 -> c.rs:1-4"]);
}
