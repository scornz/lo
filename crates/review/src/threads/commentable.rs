//! Which lines GitHub takes a comment on (design §8, "Comments").
//!
//! GitHub takes a line comment only on a line of the pull request's diff: a line of one of a
//! file's hunks, on the side it's on. A removed or unchanged line takes one on the left, the base's
//! side, and an added or unchanged line on the right. A range must start and end in the same hunk
//! (GitHub refuses others with "start line must be part of the same hunk as the line"), and may
//! start on the other side, as a range from a removed line to the added line that replaces it
//! does. Anywhere else, the UI offers a file-level comment.
//!
//! GitHub's newer Files changed page also takes comments on unchanged lines outside the hunks of a
//! changed file, but its API documents only lines of the diff and refuses others with "line must
//! be part of the diff", so rozed keeps to them.

use std::fmt;

use super::{DiffSide, Left, Line, Lines, Screen};

/// A line of a hunk, by its prefix in a unified diff.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HunkLine {
    /// ` `: unchanged, on both sides.
    Context,
    /// `-`: on the left only.
    Removed,
    /// `+`: on the right only.
    Added,
}

/// A hunk of a file's diff: changed lines and the unchanged lines around them, as one `@@` header
/// and the lines under it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    old_start: u32,
    new_start: u32,
    /// The position in the hunk of each of its left lines, from `old_start` on.
    old: Vec<usize>,
    /// The position in the hunk of each of its right lines, from `new_start` on.
    new: Vec<usize>,
}

impl Hunk {
    /// A hunk whose first left line is `old_start` and first right line is `new_start`, as its
    /// header says, with `lines` in order.
    pub fn new(old_start: u32, new_start: u32, lines: impl IntoIterator<Item = HunkLine>) -> Self {
        let (mut old, mut new) = (Vec::new(), Vec::new());
        for (position, line) in lines.into_iter().enumerate() {
            if line != HunkLine::Added {
                old.push(position);
            }
            if line != HunkLine::Removed {
                new.push(position);
            }
        }
        Self {
            old_start,
            new_start,
            old,
            new,
        }
    }

    /// The hunks of one file's unified diff: GitHub's `patch` for a changed file, or `git diff`'s
    /// output for one file. Anything before a hunk's `@@` header, such as git's `diff --git`, `---`
    /// and `+++` lines, is skipped, and so is `\ No newline at end of file`.
    pub fn parse(patch: &str) -> Result<Vec<Self>, PatchError> {
        let error = |line, reason| PatchError { line, reason };
        let mut hunks = Vec::new();
        let mut lines = patch.lines().zip(1..);
        while let Some((text, header_line)) = lines.next() {
            if !text.starts_with("@@") {
                continue;
            }
            let ((old_start, mut old_left), (new_start, mut new_left)) =
                header(text).ok_or(error(
                    header_line,
                    "the hunk header isn't `@@ -start,count +start,count @@`",
                ))?;
            let mut kinds = Vec::new();
            let mut last_line = header_line;
            while old_left > 0 || new_left > 0 {
                let Some((text, line)) = lines.next() else {
                    return Err(error(last_line, "the patch ends inside a hunk"));
                };
                last_line = line;
                let kind = match text.as_bytes().first() {
                    // Some tools trim the space before an unchanged blank line.
                    Some(b' ') | None => HunkLine::Context,
                    Some(b'-') => HunkLine::Removed,
                    Some(b'+') => HunkLine::Added,
                    Some(b'\\') => continue,
                    Some(_) => {
                        return Err(error(
                            line,
                            "a hunk line starts with something other than ` `, `-`, `+` or `\\`",
                        ));
                    }
                };
                let (old, new) = match kind {
                    HunkLine::Context => (1, 1),
                    HunkLine::Removed => (1, 0),
                    HunkLine::Added => (0, 1),
                };
                let too_many = || error(line, "the hunk has more lines than its header says");
                old_left = old_left.checked_sub(old).ok_or_else(too_many)?;
                new_left = new_left.checked_sub(new).ok_or_else(too_many)?;
                kinds.push(kind);
            }
            hunks.push(Self::new(old_start, new_start, kinds));
        }
        Ok(hunks)
    }

    /// The position in the hunk of `line`, if the hunk has it.
    fn position(&self, line: Line) -> Option<usize> {
        let (start, positions) = match line.side {
            DiffSide::Left => (self.old_start, &self.old),
            DiffSide::Right => (self.new_start, &self.new),
        };
        let index = line.number.checked_sub(start)?;
        positions.get(usize::try_from(index).ok()?).copied()
    }
}

/// A unified diff [`Hunk::parse`] can't read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatchError {
    /// The 1-based line of the patch where reading failed.
    pub line: usize,
    /// What's wrong there.
    pub reason: &'static str,
}

impl fmt::Display for PatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "line {} of the patch: {}",
            self.line, self.reason
        )
    }
}

impl std::error::Error for PatchError {}

/// The lines of one file on screen that GitHub takes a line comment on.
///
/// Comments go on the right column's revision, so the left column takes them only in a revision's
/// whole diff, where it shows the base. Between revisions it shows another revision, whose lines
/// GitHub's left side can't name. An empty set, for a binary file or one too large for GitHub to
/// diff, takes only file-level comments.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommentableLines {
    hunks: Vec<Hunk>,
    left: bool,
}

/// Why GitHub won't take a comment on a range of lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeError {
    /// GitHub takes no comment on this line: it's outside the diff's hunks, or in a column that
    /// doesn't take comments.
    NotCommentable(Line),
    /// The lines are in different hunks. GitHub takes a range only within one.
    AcrossHunks,
}

impl CommentableLines {
    /// The lines of a file on `screen` that take a comment. `hunks` are the file's hunks in the pull
    /// request's diff at the right column's revision, from that revision's base to its head, with
    /// the three lines of context GitHub uses: the right column's hunks in the revision's whole
    /// diff, but not the hunks of a diff between revisions.
    pub fn new(screen: &Screen, hunks: Vec<Hunk>) -> Self {
        Self {
            hunks,
            left: screen.left == Left::Base,
        }
    }

    /// Whether no line takes a comment, so the file takes only file-level comments.
    pub fn is_empty(&self) -> bool {
        !self
            .hunks
            .iter()
            .any(|hunk| !hunk.new.is_empty() || (self.left && !hunk.old.is_empty()))
    }

    /// The lines a comment on `line` goes on, or `None` when GitHub won't take one there, so the UI
    /// offers a file-level comment. `line`'s side is its column on screen, which is GitHub's side
    /// wherever a comment can go.
    pub fn line(&self, line: Line) -> Option<Lines> {
        self.find(line).map(|_| Lines::one(line))
    }

    /// The lines a comment on a selection from `from` to `to` goes on, in either order: from the
    /// one higher in the hunk to the other.
    pub fn range(&self, from: Line, to: Line) -> Result<Lines, RangeError> {
        let (from_hunk, from_position) = self.find(from).ok_or(RangeError::NotCommentable(from))?;
        let (to_hunk, to_position) = self.find(to).ok_or(RangeError::NotCommentable(to))?;
        if from_hunk != to_hunk {
            return Err(RangeError::AcrossHunks);
        }
        Ok(match from_position.cmp(&to_position) {
            std::cmp::Ordering::Less => Lines::range(from, to),
            std::cmp::Ordering::Equal => Lines::one(to),
            std::cmp::Ordering::Greater => Lines::range(to, from),
        })
    }

    /// The hunk with `line`, and its position there, if `line` takes a comment.
    fn find(&self, line: Line) -> Option<(usize, usize)> {
        if line.side == DiffSide::Left && !self.left {
            return None;
        }
        (self.hunks.iter().enumerate())
            .find_map(|(index, hunk)| Some((index, hunk.position(line)?)))
    }
}

/// A hunk header's left and right ranges, each a start and a count: `@@ -10,5 +12,7 @@` is
/// `((10, 5), (12, 7))`. A missing count is 1.
fn header(text: &str) -> Option<((u32, u32), (u32, u32))> {
    let mut fields = text.strip_prefix("@@ ")?.split(' ');
    let old = range(fields.next()?.strip_prefix('-')?)?;
    let new = range(fields.next()?.strip_prefix('+')?)?;
    (fields.next()? == "@@").then_some((old, new))
}

/// A hunk header's `start,count`, or `start` for a count of 1.
fn range(text: &str) -> Option<(u32, u32)> {
    match text.split_once(',') {
        Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
        None => Some((text.parse().ok()?, 1)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GitHub's `patch` for a file with two hunks: line 3 edited, and two lines added after line 20.
    /// `\x20` is the space before an unchanged blank line.
    const PATCH: &str = "\
@@ -1,6 +1,6 @@
 use std::fmt;
\x20
-fn parse(input: &str) -> u32 {
+fn parse(input: &str) -> Option<u32> {
     let trimmed = input.trim();
     trimmed.len()
 }
@@ -18,5 +18,7 @@ impl Parser {
     fn next(&mut self) {
         self.at += 1;
     }
+
+    fn peek(&self) {}
 }
\x20
";

    fn hunks() -> Vec<Hunk> {
        Hunk::parse(PATCH).unwrap()
    }

    fn full() -> CommentableLines {
        CommentableLines::new(&Screen::full("a2"), hunks())
    }

    fn between() -> CommentableLines {
        CommentableLines::new(&Screen::between("a1", "a2"), hunks())
    }

    use HunkLine::{Added, Context, Removed};

    #[test]
    fn a_patch_parses_into_its_hunks() {
        assert_eq!(
            hunks(),
            [
                Hunk::new(
                    1,
                    1,
                    [Context, Context, Removed, Added, Context, Context, Context]
                ),
                Hunk::new(
                    18,
                    18,
                    [Context, Context, Context, Added, Added, Context, Context]
                ),
            ]
        );
    }

    #[test]
    fn git_headers_and_missing_newline_markers_are_skipped() {
        let patch = "\
diff --git a/README.md b/README.md
index 3b18e51..a7c3d2f 100644
--- a/README.md
+++ b/README.md
@@ -1 +1 @@
-rozed
\\ No newline at end of file
+rozed reads pull requests.
\\ No newline at end of file
";

        assert_eq!(
            Hunk::parse(patch),
            Ok(vec![Hunk::new(1, 1, [Removed, Added])])
        );
    }

    #[test]
    fn lines_after_a_hunks_counted_lines_are_skipped() {
        // In `git diff` output for several files, the next file's `---` and `+++` follow a hunk.
        let patch = "@@ -1 +1 @@\n-a\n+b\n--- a/c\n+++ b/c\n@@ -4,0 +5 @@\n+d\n";

        assert_eq!(
            Hunk::parse(patch),
            Ok(vec![
                Hunk::new(1, 1, [Removed, Added]),
                Hunk::new(4, 5, [Added])
            ])
        );
    }

    #[test]
    fn a_new_file_is_one_hunk_of_added_lines() {
        let patch = "@@ -0,0 +1,2 @@\n+fn main() {}\n+\n";

        assert_eq!(
            Hunk::parse(patch),
            Ok(vec![Hunk::new(0, 1, [Added, Added])])
        );
    }

    #[test]
    fn an_unchanged_line_whose_space_was_trimmed_is_still_context() {
        let patch = "@@ -1,3 +1,3 @@\n a\n\n-b\n+c\n";

        assert_eq!(
            Hunk::parse(patch),
            Ok(vec![Hunk::new(1, 1, [Context, Context, Removed, Added])])
        );
    }

    #[test]
    fn a_patch_that_is_not_a_unified_diff_is_an_error() {
        assert_eq!(
            Hunk::parse("@@ -1,2 +1,2\n a\n"),
            Err(PatchError {
                line: 1,
                reason: "the hunk header isn't `@@ -start,count +start,count @@`",
            })
        );
        assert_eq!(
            Hunk::parse("@@ -1,2 +1,2 @@\n a\n"),
            Err(PatchError {
                line: 2,
                reason: "the patch ends inside a hunk",
            })
        );
        assert_eq!(
            Hunk::parse("@@ -1,2 +1,1 @@\n+a\n+b\n"),
            Err(PatchError {
                line: 3,
                reason: "the hunk has more lines than its header says",
            })
        );
        assert_eq!(
            Hunk::parse("@@ -1,2 +1,2 @@\n a\n*b\n"),
            Err(PatchError {
                line: 3,
                reason: "a hunk line starts with something other than ` `, `-`, `+` or `\\`",
            })
        );
    }

    #[test]
    fn an_added_line_takes_a_comment_on_the_right() {
        assert_eq!(
            full().line(Line::right(3)),
            Some(Lines::one(Line::right(3)))
        );
        assert_eq!(
            full().line(Line::right(21)),
            Some(Lines::one(Line::right(21)))
        );
    }

    #[test]
    fn a_removed_line_takes_a_comment_on_the_left() {
        assert_eq!(full().line(Line::left(3)), Some(Lines::one(Line::left(3))));
    }

    #[test]
    fn an_unchanged_line_in_a_hunk_takes_a_comment_on_either_side() {
        assert_eq!(full().line(Line::left(1)), Some(Lines::one(Line::left(1))));
        assert_eq!(
            full().line(Line::right(1)),
            Some(Lines::one(Line::right(1)))
        );
        // Line 20 on the left is line 20 on the right: the hunk adds lines after it.
        assert_eq!(
            full().line(Line::left(20)),
            Some(Lines::one(Line::left(20)))
        );
        assert_eq!(
            full().line(Line::right(24)),
            Some(Lines::one(Line::right(24)))
        );
    }

    #[test]
    fn a_line_outside_the_hunks_takes_no_comment() {
        assert_eq!(full().line(Line::right(7)), None);
        assert_eq!(full().line(Line::right(17)), None);
        assert_eq!(full().line(Line::right(25)), None);
        assert_eq!(full().line(Line::left(23)), None);
        assert_eq!(full().line(Line::right(0)), None);
    }

    #[test]
    fn between_revisions_only_the_right_column_takes_comments() {
        assert_eq!(between().line(Line::left(3)), None);
        assert_eq!(between().line(Line::left(1)), None);
        assert_eq!(
            between().line(Line::right(3)),
            Some(Lines::one(Line::right(3)))
        );

        let replayed = CommentableLines::new(&Screen::replayed("a1", "a2"), hunks());
        assert_eq!(replayed.line(Line::left(3)), None);
        assert_eq!(
            replayed.line(Line::right(3)),
            Some(Lines::one(Line::right(3)))
        );
    }

    #[test]
    fn a_file_with_no_hunks_takes_only_file_comments() {
        let binary = CommentableLines::new(&Screen::full("a2"), Vec::new());

        assert!(binary.is_empty());
        assert_eq!(binary.line(Line::right(1)), None);
        assert!(!full().is_empty());
        // Between revisions, a deleted file has no right column.
        let deleted = Hunk::parse("@@ -1,2 +0,0 @@\n-a\n-b\n").unwrap();
        assert!(CommentableLines::new(&Screen::between("a1", "a2"), deleted).is_empty());
    }

    #[test]
    fn a_range_within_a_hunk_takes_a_comment() {
        assert_eq!(
            full().range(Line::right(19), Line::right(22)),
            Ok(Lines::range(Line::right(19), Line::right(22)))
        );
    }

    #[test]
    fn a_range_selected_upwards_starts_at_its_top() {
        assert_eq!(
            full().range(Line::right(22), Line::right(19)),
            Ok(Lines::range(Line::right(19), Line::right(22)))
        );
    }

    #[test]
    fn a_range_can_run_from_a_removed_line_to_the_added_line_after_it() {
        assert_eq!(
            full().range(Line::left(3), Line::right(3)),
            Ok(Lines::range(Line::left(3), Line::right(3)))
        );
        // Selected upwards in the inline view: the added line comes after the removed one.
        assert_eq!(
            full().range(Line::right(4), Line::left(3)),
            Ok(Lines::range(Line::left(3), Line::right(4)))
        );
        // The unchanged line before them is above both.
        assert_eq!(
            full().range(Line::right(3), Line::left(2)),
            Ok(Lines::range(Line::left(2), Line::right(3)))
        );
    }

    #[test]
    fn a_range_of_one_line_is_that_line() {
        assert_eq!(
            full().range(Line::right(21), Line::right(21)),
            Ok(Lines::one(Line::right(21)))
        );
    }

    #[test]
    fn a_range_across_hunks_takes_no_comment() {
        assert_eq!(
            full().range(Line::right(5), Line::right(19)),
            Err(RangeError::AcrossHunks)
        );
    }

    #[test]
    fn a_range_ending_outside_the_hunks_takes_no_comment() {
        assert_eq!(
            full().range(Line::right(5), Line::right(8)),
            Err(RangeError::NotCommentable(Line::right(8)))
        );
        assert_eq!(
            between().range(Line::left(1), Line::right(3)),
            Err(RangeError::NotCommentable(Line::left(1)))
        );
    }
}
