//! Code deleted and added in the same place, between the same two unchanged lines, was changed in
//! place, not moved.

use diff_extras::moved::FileChanges;

use crate::support::{TOTAL, indented, moves};

#[test]
fn code_re_indented_in_place_is_not_a_move() {
    let file = FileChanges::new("a.rs")
        .delete(10, TOTAL)
        .add(10, indented(4, &TOTAL));

    assert_eq!(moves(&[file]), Vec::<String>::new());
}

#[test]
fn code_wrapped_in_a_block_in_place_is_not_a_move() {
    let mut wrapped = vec!["if enabled {".to_string()];
    wrapped.extend(indented(4, &TOTAL));
    wrapped.push("}".into());
    let file = FileChanges::new("a.rs").delete(10, TOTAL).add(10, wrapped);

    assert_eq!(moves(&[file]), Vec::<String>::new());
}

#[test]
fn code_in_place_is_found_after_earlier_changes_shift_the_numbers() {
    // Deleting line 2 moves every later line up one, so old line 10 is new line 9.
    let file = FileChanges::new("a.rs")
        .delete(2, ["let unused = 0;"])
        .delete(10, TOTAL)
        .add(9, indented(4, &TOTAL));

    assert_eq!(moves(&[file]), Vec::<String>::new());
}

#[test]
fn code_in_place_in_a_renamed_file_is_not_a_move() {
    let file = FileChanges::new("b.rs")
        .renamed_from("a.rs")
        .delete(10, TOTAL)
        .add(10, indented(4, &TOTAL));

    assert_eq!(moves(&[file]), Vec::<String>::new());
}

#[test]
fn code_moved_past_an_unchanged_line_is_a_move() {
    // Old lines 14 to 18 stay, as new lines 10 to 14.
    let file = FileChanges::new("a.rs").delete(10, TOTAL).add(15, TOTAL);

    assert_eq!(moves(&[file]), ["a.rs:10-13 -> a.rs:15-18"]);
}

#[test]
fn code_at_the_same_line_numbers_in_another_file_is_a_move() {
    let files = [
        FileChanges::new("a.rs").delete(10, TOTAL),
        FileChanges::new("b.rs").add(10, TOTAL),
    ];

    assert_eq!(moves(&files), ["a.rs:10-13 -> b.rs:10-13"]);
}
