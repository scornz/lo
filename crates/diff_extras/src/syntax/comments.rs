//! Which line a comment written in the syntax-aware view goes on.
//!
//! GitHub takes a line comment only on a line inside one of the hunks of the pull request's
//! diff, on the side it's for. The syntax-aware view shows real lines, so a comment goes on the
//! line it's written beside, if GitHub takes one there. Otherwise the view offers a file-level
//! comment instead (design §5).

use super::{Row, Side};

/// A hunk of the pull request's diff, from its header: `@@ -10,5 +12,7 @@` covers old lines 10
/// to 14 and new lines 12 to 18.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
}

/// The lines of a file GitHub takes a line comment on: those inside the hunks of the pull
/// request's diff, on each side.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommentableLines {
    pub hunks: Vec<Hunk>,
}

impl CommentableLines {
    /// Whether GitHub takes a comment on 1-based `line` of `side`.
    pub fn contains(&self, side: Side, line: u32) -> bool {
        self.hunks.iter().any(|hunk| {
            let (start, lines) = match side {
                Side::Old => (hunk.old_start, hunk.old_lines),
                Side::New => (hunk.new_start, hunk.new_lines),
            };
            (start..start + lines).contains(&line)
        })
    }
}

/// Where a line comment goes: a side, and a 1-based line on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommentLine {
    pub side: Side,
    pub line: u32,
}

/// The line a comment written on `row`, in `side`'s column, goes on, or `None` if GitHub won't
/// take it, so the UI can offer a file-level comment instead.
///
/// It's the row's line on `side`, if GitHub takes a comment there. If the row has no line on
/// `side`, the column has a gap beside a line only the other side has, such as a deleted line,
/// and the comment goes on that line. A line GitHub doesn't take a comment on gives `None`, even
/// when the line beside it would: the comment would then sit on a line other than the one it
/// was written beside.
pub fn nearest_commentable_line(
    row: &Row,
    side: Side,
    commentable_lines: &CommentableLines,
) -> Option<CommentLine> {
    let side = if row.line(side).is_some() {
        side
    } else {
        side.other()
    };
    let line = row.line(side)?.number;
    commentable_lines
        .contains(side, line)
        .then_some(CommentLine { side, line })
}

impl Side {
    /// The other side.
    pub fn other(self) -> Self {
        match self {
            Side::Old => Side::New,
            Side::New => Side::Old,
        }
    }
}
