//! Small edits inside a moved block keep it a move, and are reported.

use diff_extras::moved::FileChanges;

use crate::support::moves;

/// `count` distinct lines, `let line_1 = 1;` and so on.
fn lines(count: usize) -> Vec<String> {
    (1..=count)
        .map(|n| format!("let line_{n} = {n};"))
        .collect()
}

/// `lines` with the lines at the 1-based `numbers` edited.
fn edit(lines: &[String], numbers: &[usize]) -> Vec<String> {
    let mut edited = lines.to_vec();
    for &number in numbers {
        edited[number - 1] = format!("let line_{number} = changed();");
    }
    edited
}

/// A file that deletes `old` at line 1 and one that adds `new` at line 1.
fn moved(old: &[String], new: &[String]) -> Vec<String> {
    moves(&[
        FileChanges::new("a.rs").delete(1, old),
        FileChanges::new("b.rs").add(1, new),
    ])
}

#[test]
fn an_edited_line_inside_a_block_keeps_it_a_move() {
    let old = lines(6);

    assert_eq!(
        moved(&old, &edit(&old, &[3])),
        ["a.rs:1-6 -> b.rs:1-6 edited 3/3"]
    );
}

#[test]
fn two_edited_lines_in_a_row_keep_it_a_move() {
    let old = lines(8);

    assert_eq!(
        moved(&old, &edit(&old, &[4, 5])),
        ["a.rs:1-8 -> b.rs:1-8 edited 4/4 5/5"]
    );
}

#[test]
fn three_edited_lines_in_a_row_split_the_block() {
    let old = lines(10);

    assert_eq!(
        moved(&old, &edit(&old, &[4, 5, 6])),
        ["a.rs:1-3 -> b.rs:1-3", "a.rs:7-10 -> b.rs:7-10"]
    );
}

#[test]
fn exactly_two_thirds_matching_is_enough() {
    let old = lines(6);

    assert_eq!(
        moved(&old, &edit(&old, &[3, 5])),
        ["a.rs:1-6 -> b.rs:1-6 edited 3/3 5/5"]
    );
}

#[test]
fn less_than_two_thirds_matching_is_not_a_move() {
    let old = lines(5);

    assert_eq!(moved(&old, &edit(&old, &[2, 4])), Vec::<String>::new());
}

#[test]
fn a_block_runs_to_the_furthest_line_that_keeps_two_thirds() {
    // Matches: yes, yes, yes, no, no, yes, no, no, yes, no, no, yes.
    let old = lines(12);

    assert_eq!(
        moved(&old, &edit(&old, &[4, 5, 7, 8, 10, 11])),
        ["a.rs:1-6 -> b.rs:1-6 edited 4/4 5/5"]
    );
}

#[test]
fn a_block_starts_past_a_weak_start() {
    // Matches: yes, no, no, yes, no, no, yes, yes, yes, yes. From line 1, no stretch keeps two
    // thirds past line 1, so the block starts at line 4.
    let old = lines(10);

    assert_eq!(
        moved(&old, &edit(&old, &[2, 3, 5, 6])),
        ["a.rs:4-10 -> b.rs:4-10 edited 5/5 6/6"]
    );
}

#[test]
fn a_block_starts_and_ends_with_matching_lines() {
    let old = lines(5);

    assert_eq!(moved(&old, &edit(&old, &[1, 5])), ["a.rs:2-4 -> b.rs:2-4"]);
}

#[test]
fn edits_need_three_matching_lines_around_them() {
    let old = lines(3);

    assert_eq!(moved(&old, &edit(&old, &[2])), Vec::<String>::new());
}

#[test]
fn edited_lines_pair_up_across_blank_lines() {
    let old = ["let a = 1;", "", "let b = 2;", "let c = 3;", "let d = 4;"];
    let new = ["let a = 1;", "let b = 2;", "let c = 30;", "", "let d = 4;"];
    let files = [
        FileChanges::new("a.rs").delete(1, old),
        FileChanges::new("b.rs").add(1, new),
    ];

    assert_eq!(moves(&files), ["a.rs:1-5 -> b.rs:1-5 edited 4/3"]);
}

#[test]
fn a_line_added_inside_a_block_splits_it_in_two() {
    let old = lines(6);
    let mut new = old.clone();
    new.insert(3, "log::info!(\"added\");".into());

    assert_eq!(
        moved(&old, &new),
        ["a.rs:1-3 -> b.rs:1-3", "a.rs:4-6 -> b.rs:5-7"]
    );
}

#[test]
fn a_line_removed_inside_a_block_splits_it_in_two() {
    let old = lines(7);
    let mut new = old.clone();
    new.remove(3);

    assert_eq!(
        moved(&old, &new),
        ["a.rs:1-3 -> b.rs:1-3", "a.rs:5-7 -> b.rs:4-6"]
    );
}

#[test]
fn a_longer_block_with_an_edit_beats_a_shorter_one_without() {
    let old = lines(6);
    let files = [
        FileChanges::new("a.rs").delete(1, &old),
        FileChanges::new("b.rs").add(1, &old[..4]),
        FileChanges::new("c.rs").add(1, edit(&old, &[3])),
    ];

    assert_eq!(moves(&files), ["a.rs:1-6 -> c.rs:1-6 edited 3/3"]);
}

#[test]
fn of_two_blocks_as_long_the_one_with_fewer_edits_wins() {
    let old = lines(6);
    let files = [
        FileChanges::new("a.rs").delete(1, &old),
        FileChanges::new("b.rs").add(1, edit(&old, &[3])),
        FileChanges::new("c.rs").add(1, edit(&old, &[6])),
    ];

    // Both match 5 lines, but c.rs's block ends before its edited line, so it has no edits.
    assert_eq!(moves(&files), ["a.rs:1-5 -> c.rs:1-5"]);
}
