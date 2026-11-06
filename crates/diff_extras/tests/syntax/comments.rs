//! Mapping a row of the syntax-aware view to a line GitHub takes a comment on.

use diff_extras::syntax::{
    CommentLine, CommentableLines, Hunk, Line, Row, Side, nearest_commentable_line,
};

use crate::support::compare_with;

fn line(number: u32) -> Option<Line> {
    Some(Line {
        number,
        novel: Vec::new(),
    })
}

fn hunk(old_start: u32, old_lines: u32, new_start: u32, new_lines: u32) -> Hunk {
    Hunk {
        old_start,
        old_lines,
        new_start,
        new_lines,
    }
}

/// One hunk, `@@ -10,5 +12,7 @@`: old lines 10 to 14, new lines 12 to 18.
fn one_hunk() -> CommentableLines {
    CommentableLines {
        hunks: vec![hunk(10, 5, 12, 7)],
    }
}

#[test]
fn a_line_in_a_hunk_takes_the_comment() {
    let row = Row {
        old: line(11),
        new: line(13),
    };

    assert_eq!(
        nearest_commentable_line(&row, Side::New, &one_hunk()),
        Some(CommentLine {
            side: Side::New,
            line: 13
        })
    );
    assert_eq!(
        nearest_commentable_line(&row, Side::Old, &one_hunk()),
        Some(CommentLine {
            side: Side::Old,
            line: 11
        })
    );
}

#[test]
fn a_line_outside_the_hunks_takes_none() {
    // GitHub has no hunk at new line 30, so the UI offers a file-level comment, even though
    // the old line beside it is in one.
    let row = Row {
        old: line(12),
        new: line(30),
    };

    assert_eq!(nearest_commentable_line(&row, Side::New, &one_hunk()), None);
}

#[test]
fn a_gap_beside_a_line_comments_on_that_line() {
    // A deleted line has a gap beside it in the new column.
    let row = Row {
        old: line(12),
        new: None,
    };

    assert_eq!(
        nearest_commentable_line(&row, Side::New, &one_hunk()),
        Some(CommentLine {
            side: Side::Old,
            line: 12
        })
    );
}

#[test]
fn a_gap_beside_a_line_outside_the_hunks_takes_none() {
    let row = Row {
        old: None,
        new: line(40),
    };

    assert_eq!(nearest_commentable_line(&row, Side::Old, &one_hunk()), None);
}

#[test]
fn a_hunk_that_only_adds_takes_no_old_line() {
    // `@@ -0,0 +1,3 @@`: a new file.
    let commentable = CommentableLines {
        hunks: vec![hunk(0, 0, 1, 3)],
    };

    assert!(!commentable.contains(Side::Old, 0));
    assert!(!commentable.contains(Side::Old, 1));
    assert!(commentable.contains(Side::New, 3));
}

#[test]
fn checks_every_hunk() {
    let commentable = CommentableLines {
        hunks: vec![hunk(1, 4, 1, 5), hunk(20, 7, 21, 7)],
    };
    let row = Row {
        old: line(22),
        new: line(23),
    };

    assert_eq!(
        nearest_commentable_line(&row, Side::New, &commentable),
        Some(CommentLine {
            side: Side::New,
            line: 23
        })
    );
    assert!(commentable.contains(Side::Old, 4));
    assert!(!commentable.contains(Side::Old, 5));
    assert!(commentable.contains(Side::New, 27));
    assert!(!commentable.contains(Side::New, 28));
}

#[test]
fn maps_the_rows_of_a_real_diff() {
    // GitHub's diff of these has one hunk, `@@ -1,4 +1,5 @@`, as a line diff with three lines of
    // context: every line of both files.
    let old = "fn f() {\n    a();\n    c();\n}\n";
    let new = "fn f() {\n    a();\n    b();\n    c();\n}\n";
    let commentable = CommentableLines {
        hunks: vec![hunk(1, 4, 1, 5)],
    };

    let diff = compare_with("rust", old, new, &Default::default()).unwrap();
    let targets: Vec<Option<CommentLine>> = diff
        .rows
        .iter()
        .map(|row| nearest_commentable_line(row, Side::Old, &commentable))
        .collect();

    let old_line = |line| {
        Some(CommentLine {
            side: Side::Old,
            line,
        })
    };
    // The added `b();` has no old line, so it takes the comment on the new side.
    assert_eq!(
        targets,
        [
            old_line(1),
            old_line(2),
            Some(CommentLine {
                side: Side::New,
                line: 3
            }),
            old_line(3),
            old_line(4),
        ]
    );
}
