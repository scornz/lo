//! Looking up the move a line belongs to, and whether it was edited.

use diff_extras::moved::{EditedLine, FileChanges, Move, Moves};

use crate::support::describe;

fn lines(prefix: &str, count: usize) -> Vec<String> {
    (1..=count)
        .map(|n| format!("let {prefix}_{n} = {n};"))
        .collect()
}

/// Two blocks moved from a.rs to b.rs, and one within b.rs, with blank lines and an edit.
fn changes() -> Vec<FileChanges> {
    let mut first = lines("first", 4);
    first.insert(2, String::new());
    let second = lines("second", 3);
    let third = lines("third", 5);
    let mut edited = third.clone();
    edited[2] = "let third_3 = changed();".into();
    vec![
        FileChanges::new("a.rs")
            .delete(1, &first)
            .delete(20, &second),
        FileChanges::new("b.rs")
            .add(1, &second)
            .add(10, &first)
            .delete(30, &third)
            .add(50, &edited),
    ]
}

fn at(moves: &Moves, found: Option<&Move>) -> Option<usize> {
    found.map(|found| {
        moves
            .all()
            .iter()
            .position(|moved| std::ptr::eq(moved, found))
            .unwrap()
    })
}

#[test]
fn finds_the_moves() {
    let moves = Moves::find(&changes());

    let described: Vec<String> = moves.all().iter().map(describe).collect();
    assert_eq!(
        described,
        [
            "a.rs:20-22 -> b.rs:1-3",
            "a.rs:1-5 -> b.rs:10-14",
            "b.rs:30-34 -> b.rs:50-54 edited 32/52",
        ]
    );
}

#[test]
fn finds_the_move_of_each_deleted_line() {
    let moves = Moves::find(&changes());

    let of_deleted = |file, line| at(&moves, moves.of_deleted(file, line));
    assert_eq!(of_deleted(0, 1), Some(1));
    // The blank line inside the block.
    assert_eq!(of_deleted(0, 3), Some(1));
    assert_eq!(of_deleted(0, 5), Some(1));
    assert_eq!(of_deleted(0, 6), None);
    assert_eq!(of_deleted(0, 19), None);
    assert_eq!(of_deleted(0, 20), Some(0));
    assert_eq!(of_deleted(0, 22), Some(0));
    assert_eq!(of_deleted(0, 23), None);
    assert_eq!(of_deleted(1, 1), None);
    assert_eq!(of_deleted(1, 32), Some(2));
    assert_eq!(of_deleted(2, 1), None);
}

#[test]
fn finds_the_move_of_each_added_line() {
    let moves = Moves::find(&changes());

    let of_added = |file, line| at(&moves, moves.of_added(file, line));
    assert_eq!(of_added(0, 1), None);
    assert_eq!(of_added(1, 1), Some(0));
    assert_eq!(of_added(1, 3), Some(0));
    assert_eq!(of_added(1, 4), None);
    assert_eq!(of_added(1, 9), None);
    assert_eq!(of_added(1, 10), Some(1));
    assert_eq!(of_added(1, 12), Some(1));
    assert_eq!(of_added(1, 14), Some(1));
    assert_eq!(of_added(1, 15), None);
    assert_eq!(of_added(1, 50), Some(2));
    assert_eq!(of_added(1, 54), Some(2));
    assert_eq!(of_added(1, 55), None);
}

#[test]
fn finds_the_edit_of_a_line() {
    let moves = Moves::find(&changes());
    let edited = &moves.all()[2];
    let edit = Some(EditedLine { old: 32, new: 52 });

    assert_eq!(edited.edit_of_old(32), edit);
    assert_eq!(edited.edit_of_new(52), edit);
    assert_eq!(edited.edit_of_old(31), None);
    assert_eq!(edited.edit_of_old(52), None);
    assert_eq!(edited.edit_of_new(53), None);
    assert_eq!(moves.all()[0].edit_of_new(1), None);
}

#[test]
fn finds_nothing_among_no_moves() {
    let moves = Moves::find(&[]);

    assert!(moves.all().is_empty());
    assert!(moves.of_deleted(0, 1).is_none());
    assert!(moves.of_added(0, 1).is_none());
}
