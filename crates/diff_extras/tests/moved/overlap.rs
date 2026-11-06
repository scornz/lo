//! Each deleted or added line belongs to at most one move, and longer matches win.

use diff_extras::moved::FileChanges;

use crate::support::{TOTAL, moves};

/// Ten distinct lines, `let line_1 = 1;` to `let line_10 = 10;`.
fn ten() -> Vec<String> {
    (1..=10).map(|n| format!("let line_{n} = {n};")).collect()
}

#[test]
fn the_longer_of_two_destinations_wins() {
    let files = [
        FileChanges::new("a.rs").delete(1, TOTAL),
        FileChanges::new("b.rs").add(1, &TOTAL[..3]),
        FileChanges::new("c.rs").add(1, TOTAL),
    ];

    assert_eq!(moves(&files), ["a.rs:1-4 -> c.rs:1-4"]);
}

#[test]
fn the_longer_of_two_sources_wins() {
    let files = [
        FileChanges::new("a.rs").delete(1, &TOTAL[..3]),
        FileChanges::new("b.rs").delete(1, TOTAL),
        FileChanges::new("c.rs").add(1, TOTAL),
    ];

    assert_eq!(moves(&files), ["b.rs:1-4 -> c.rs:1-4"]);
}

#[test]
fn a_block_added_twice_is_one_move_to_the_first_place() {
    let files = [
        FileChanges::new("a.rs").delete(1, TOTAL),
        FileChanges::new("b.rs").add(1, TOTAL),
        FileChanges::new("c.rs").add(1, TOTAL),
    ];

    assert_eq!(moves(&files), ["a.rs:1-4 -> b.rs:1-4"]);
}

#[test]
fn a_block_added_twice_in_one_file_is_one_move() {
    let file = FileChanges::new("a.rs")
        .delete(1, TOTAL)
        .add(20, TOTAL)
        .add(40, TOTAL);

    assert_eq!(moves(&[file]), ["a.rs:1-4 -> a.rs:20-23"]);
}

#[test]
fn a_block_deleted_twice_is_one_move_from_the_first_place() {
    let files = [
        FileChanges::new("a.rs").delete(1, TOTAL),
        FileChanges::new("b.rs").delete(1, TOTAL),
        FileChanges::new("c.rs").add(1, TOTAL),
    ];

    assert_eq!(moves(&files), ["a.rs:1-4 -> c.rs:1-4"]);
}

#[test]
fn what_is_left_of_a_shorter_match_can_still_be_a_move() {
    let ten = ten();
    let files = [
        FileChanges::new("a.rs").delete(1, &ten),
        // Lines 1 to 7 win, so lines 8 to 10 are all that's left to move here.
        FileChanges::new("b.rs").add(1, &ten[..7]),
        FileChanges::new("c.rs").add(1, &ten[5..]),
    ];

    assert_eq!(
        moves(&files),
        ["a.rs:1-7 -> b.rs:1-7", "a.rs:8-10 -> c.rs:3-5"]
    );
}

#[test]
fn what_is_left_of_a_shorter_match_must_still_be_three_lines() {
    let ten = ten();
    let files = [
        FileChanges::new("a.rs").delete(1, &ten),
        FileChanges::new("b.rs").add(1, &ten[..7]),
        FileChanges::new("c.rs").add(1, &ten[5..9]),
    ];

    assert_eq!(moves(&files), ["a.rs:1-7 -> b.rs:1-7"]);
}

#[test]
fn a_block_gathered_from_two_places_is_two_moves() {
    let ten = ten();
    let files = [
        FileChanges::new("a.rs").delete(1, &ten[..6]),
        FileChanges::new("b.rs").delete(1, &ten[3..]),
        FileChanges::new("c.rs").add(1, &ten),
    ];

    assert_eq!(
        moves(&files),
        ["a.rs:1-3 -> c.rs:1-3", "b.rs:1-7 -> c.rs:4-10"]
    );
}

#[test]
fn repeated_lines_line_up_once() {
    let pattern = ["let a = 1;", "let b = 2;", "let c = 3;"];
    let deleted: Vec<&str> = pattern.iter().cycle().take(9).copied().collect();
    let files = [
        FileChanges::new("a.rs").delete(1, &deleted),
        FileChanges::new("b.rs").add(1, &deleted[..6]),
    ];

    assert_eq!(moves(&files), ["a.rs:1-6 -> b.rs:1-6"]);
}
