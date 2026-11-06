//! Which lines GitHub takes a comment on (design §8, "Comments").
//!
//! GitHub takes a line comment only on a line of the pull request's diff: a line of one of a
//! file's hunks, on the side it's on. A removed line takes one on the left, the base's side, and
//! an added line on the right. An unchanged line is on both sides; rozed puts its comments on the
//! right, as GitHub's docs advise, so they show between revisions and can suggest a change. Each
//! end of a range must be a line of the diff, but the two may be in different hunks, and a range
//! may start on the other side, as a range from a removed line to the added line that replaces it
//! does. Anywhere else, the UI offers a file-level comment.
//!
//! Checked against GitHub: its API refuses a line outside the hunks. REST answers "could not be
//! resolved", `addPullRequestReview` "Line could not be resolved", and `addPullRequestReviewThread`
//! returns a null thread with no error. GitHub's newer Files changed page also takes comments on
//! unchanged lines outside the hunks of a changed file, but its API doesn't, so rozed keeps to the
//! hunks.

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
    /// output for one file. Anything outside a hunk's counted lines, such as git's `diff --git`,
    /// `---` and `+++` lines, is skipped, and so is `\ No newline at end of file`. A second file's
    /// header, a second `diff` line or a second `---` and `+++` pair, is an error, since its hunks'
    /// lines are another file's.
    pub fn parse(patch: &str) -> Result<Vec<Self>, PatchError> {
        let error = |line, reason| PatchError { line, reason };
        let second_file = "a second file's diff starts here, and a patch is one file's";
        let mut hunks = Vec::new();
        let (mut diff_header, mut file_header) = (false, false);
        let mut lines = patch.lines().zip(1..).peekable();
        while let Some((text, header_line)) = lines.next() {
            if text.starts_with("diff ") {
                if diff_header || file_header || !hunks.is_empty() {
                    return Err(error(header_line, second_file));
                }
                diff_header = true;
                continue;
            }
            if text.starts_with("--- ")
                && lines
                    .peek()
                    .is_some_and(|(next, _)| next.starts_with("+++ "))
            {
                if file_header || !hunks.is_empty() {
                    return Err(error(header_line, second_file));
                }
                file_header = true;
                continue;
            }
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

    /// The right side's number of the line at `position`, if it's on the right: an added or
    /// unchanged line.
    fn right_number(&self, position: usize) -> Option<u32> {
        let index = self.new.binary_search(&position).ok()?;
        self.new_start.checked_add(u32::try_from(index).ok()?)
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

/// Which comments GitHub takes on one file on screen: line comments on some lines, only a
/// file-level comment, or none at all.
///
/// - A file in the pull request's diff at the right column's revision takes line comments on the
///   lines of its hunks, and a file-level comment.
/// - One whose hunks have no lines in a column that takes comments, such as a binary file, a pure
///   rename, or a deleted file between revisions, takes only a file-level comment.
/// - One outside that diff, such as a file a later revision put back as it was, takes no comment
///   at all, though a diff between revisions lists it.
///
/// Comments go on the right column's revision, so the left column takes them only in a revision's
/// whole diff, where it shows the base. Between revisions it shows another revision, whose lines
/// GitHub's left side can't name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommentableLines {
    /// The file's hunks, or `None` when it isn't in the pull request's diff.
    hunks: Option<Vec<Hunk>>,
    left: bool,
}

/// Why GitHub won't take a comment on a range of lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeError {
    /// GitHub takes no comment on this end of the range: it's outside the diff's hunks, or in a
    /// column that doesn't take comments.
    NotCommentable(Line),
}

impl CommentableLines {
    /// The comments a file on `screen` takes, when it's in the pull request's diff at the right
    /// column's revision.
    ///
    /// `hunks` are the file's hunks in that diff, in order: from the revision's base to its head,
    /// with the three lines of context GitHub uses. They aren't the hunks of a diff between
    /// revisions, nor the latest head's `patch` when an older revision is on screen. The GitHub
    /// layer parses them from GitHub's `patch` for the file at the latest head. GitHub leaves
    /// `patch` out for binary files and pure renames, which have no hunks, but also for some
    /// ordinary text files, apparently by size. For those, and for older revisions, it takes them
    /// from the local copy with `git diff -U3`, ignoring the user's git config
    /// (`GIT_CONFIG_GLOBAL=/dev/null`, `GIT_CONFIG_NOSYSTEM=1`), whose `diff.context`,
    /// `diff.interHunkContext` or `diff.algorithm` would make other hunks than GitHub's.
    pub fn new(screen: &Screen, hunks: Vec<Hunk>) -> Self {
        Self {
            hunks: Some(hunks),
            left: screen.left == Left::Base,
        }
    }

    /// A file that isn't in the pull request's diff at the right column's revision, which takes no
    /// comment: GitHub refuses even a file-level one there.
    pub fn not_in_diff() -> Self {
        Self {
            hunks: None,
            left: false,
        }
    }

    /// Whether the file takes a file-level comment: it's in the pull request's diff.
    pub fn takes_file_comment(&self) -> bool {
        self.hunks.is_some()
    }

    /// Whether any line of the file takes a comment. When none does, it takes only a file-level
    /// comment, if [`takes_file_comment`](Self::takes_file_comment).
    pub fn takes_line_comment(&self) -> bool {
        (self.hunks.iter().flatten())
            .any(|hunk| !hunk.new.is_empty() || (self.left && !hunk.old.is_empty()))
    }

    /// The lines a comment on `line` goes on, or `None` when GitHub won't take one there, so the UI
    /// offers a file-level comment. `line`'s side is its column on screen, which is GitHub's side
    /// wherever a comment can go, except for an unchanged line picked in the left column: that
    /// goes on the right, the side GitHub's docs give unchanged lines, so it still shows between
    /// revisions and can suggest a change.
    pub fn line(&self, line: Line) -> Option<Lines> {
        self.find(line).map(|(_, line)| Lines::one(line))
    }

    /// The lines a comment on a selection from `from` to `to` goes on, in either order: from the
    /// one higher in the diff to the other. Both ends must take a comment, but they may be in
    /// different hunks, and the lines between them needn't. An unchanged end goes on the right, as
    /// in [`line`](Self::line).
    pub fn range(&self, from: Line, to: Line) -> Result<Lines, RangeError> {
        let (from_at, from) = self.find(from).ok_or(RangeError::NotCommentable(from))?;
        let (to_at, to) = self.find(to).ok_or(RangeError::NotCommentable(to))?;
        Ok(match from_at.cmp(&to_at) {
            std::cmp::Ordering::Less => Lines::range(from, to),
            std::cmp::Ordering::Equal => Lines::one(to),
            std::cmp::Ordering::Greater => Lines::range(to, from),
        })
    }

    /// Where `line` is in the diff, if it takes a comment (its hunk, and its position there), and
    /// the line a comment on it goes on.
    fn find(&self, line: Line) -> Option<((usize, usize), Line)> {
        if line.side == DiffSide::Left && !self.left {
            return None;
        }
        (self.hunks.iter().flatten().enumerate()).find_map(|(index, hunk)| {
            let position = hunk.position(line)?;
            let line = match line.side {
                DiffSide::Left => hunk.right_number(position).map_or(line, Line::right),
                DiffSide::Right => line,
            };
            Some(((index, position), line))
        })
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
    fn a_second_files_diff_is_an_error() {
        let second_file = |line| {
            Err(PatchError {
                line,
                reason: "a second file's diff starts here, and a patch is one file's",
            })
        };
        // `git diff` output for two files.
        let git = "\
diff --git a/a.rs b/a.rs
--- a/a.rs
+++ b/a.rs
@@ -1 +1 @@
-a
+b
diff --git a/c.rs b/c.rs
--- a/c.rs
+++ b/c.rs
@@ -40,3 +40,4 @@
 x
+y
 z
 w
";
        assert_eq!(Hunk::parse(git), second_file(7));
        // A plain unified diff for two files: the next file's `---` and `+++` follow a hunk.
        let plain = "@@ -1 +1 @@\n-a\n+b\n--- a/c\n+++ b/c\n@@ -4,0 +5 @@\n+d\n";
        assert_eq!(Hunk::parse(plain), second_file(4));
        // A pure rename has no hunks, so the next file follows its header.
        let rename = "\
diff --git a/x.rs b/y.rs
similarity index 100%
rename from x.rs
rename to y.rs
diff --git a/c.rs b/c.rs
--- a/c.rs
+++ b/c.rs
@@ -1 +1 @@
-a
+b
";
        assert_eq!(Hunk::parse(rename), second_file(5));
    }

    #[test]
    fn a_removed_line_that_looks_like_a_header_is_a_removed_line() {
        // `-- a` removed, and `++ b` added: inside a hunk's counted lines, they're lines.
        let patch = "@@ -1,2 +1,2 @@\n--- a\n+++ b\n x\n";

        assert_eq!(
            Hunk::parse(patch),
            Ok(vec![Hunk::new(1, 1, [Removed, Added, Context])])
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
    fn an_unchanged_line_in_a_hunk_takes_a_comment_in_either_column_on_the_right() {
        // On the right, GitHub's side for unchanged lines: the thread then shows between
        // revisions, and takes a suggestion.
        assert_eq!(full().line(Line::left(1)), Some(Lines::one(Line::right(1))));
        assert_eq!(
            full().line(Line::right(1)),
            Some(Lines::one(Line::right(1)))
        );
        // Line 20 on the left is line 20 on the right, and line 21 is 23: the hunk adds two
        // lines between them.
        assert_eq!(
            full().line(Line::left(20)),
            Some(Lines::one(Line::right(20)))
        );
        assert_eq!(
            full().line(Line::left(21)),
            Some(Lines::one(Line::right(23)))
        );
        assert_eq!(
            full().line(Line::right(24)),
            Some(Lines::one(Line::right(24)))
        );
        assert!(full().line(Line::left(22)).unwrap().takes_suggestion());
    }

    #[test]
    fn a_range_picked_in_the_left_column_puts_its_unchanged_ends_on_the_right() {
        // Unchanged lines 4 to 6, picked on the left.
        assert_eq!(
            full().range(Line::left(4), Line::left(6)),
            Ok(Lines::range(Line::right(4), Line::right(6)))
        );
        // The same unchanged line picked in both columns is one line.
        assert_eq!(
            full().range(Line::left(1), Line::right(1)),
            Ok(Lines::one(Line::right(1)))
        );
        // A removed line keeps the left side.
        assert_eq!(
            full().range(Line::left(1), Line::left(3)),
            Ok(Lines::range(Line::right(1), Line::left(3)))
        );
    }

    #[test]
    fn ranges_over_a_replaced_line_between_unchanged_ones() {
        let hunks = Hunk::parse("@@ -1,3 +1,3 @@\n a\n-b\n+c\n d\n").unwrap();
        let lines = CommentableLines::new(&Screen::full("a2"), hunks);

        assert_eq!(
            lines.range(Line::left(1), Line::right(1)),
            Ok(Lines::one(Line::right(1)))
        );
        assert_eq!(
            lines.range(Line::right(1), Line::left(2)),
            Ok(Lines::range(Line::right(1), Line::left(2)))
        );
        assert_eq!(
            lines.range(Line::right(3), Line::left(1)),
            Ok(Lines::range(Line::right(1), Line::right(3)))
        );
        assert_eq!(
            lines.range(Line::left(3), Line::left(2)),
            Ok(Lines::range(Line::left(2), Line::right(3)))
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
        // A binary file, or a pure rename.
        let binary = CommentableLines::new(&Screen::full("a2"), Vec::new());

        assert!(binary.takes_file_comment());
        assert!(!binary.takes_line_comment());
        assert_eq!(binary.line(Line::right(1)), None);
        assert!(full().takes_file_comment());
        assert!(full().takes_line_comment());
        // Between revisions, a deleted file has no right column.
        let deleted = Hunk::parse("@@ -1,2 +0,0 @@\n-a\n-b\n").unwrap();
        let deleted = CommentableLines::new(&Screen::between("a1", "a2"), deleted);
        assert!(deleted.takes_file_comment());
        assert!(!deleted.takes_line_comment());
    }

    #[test]
    fn a_file_outside_the_pull_requests_diff_takes_no_comment() {
        // r1 edited it and r2 put it back, so it's between r1 and r2 but not in r2's diff.
        let reverted = CommentableLines::not_in_diff();

        assert!(!reverted.takes_file_comment());
        assert!(!reverted.takes_line_comment());
        assert_eq!(reverted.line(Line::right(1)), None);
        assert_eq!(
            reverted.range(Line::right(1), Line::right(2)),
            Err(RangeError::NotCommentable(Line::right(1)))
        );
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
        // The unchanged line before them is above both, and on the right.
        assert_eq!(
            full().range(Line::right(3), Line::left(2)),
            Ok(Lines::range(Line::right(2), Line::right(3)))
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
    fn a_range_can_span_hunks() {
        assert_eq!(
            full().range(Line::right(5), Line::right(19)),
            Ok(Lines::range(Line::right(5), Line::right(19)))
        );
        // Selected upwards: the later hunk's line is the range's end.
        assert_eq!(
            full().range(Line::right(22), Line::right(4)),
            Ok(Lines::range(Line::right(4), Line::right(22)))
        );
        // From a removed line in the first hunk to an added line in the second.
        assert_eq!(
            full().range(Line::right(21), Line::left(3)),
            Ok(Lines::range(Line::left(3), Line::right(21)))
        );
    }

    #[test]
    fn a_range_spanning_hunks_needs_both_ends_in_one() {
        // Line 10 is between the hunks.
        assert_eq!(
            full().range(Line::right(5), Line::right(10)),
            Err(RangeError::NotCommentable(Line::right(10)))
        );
        assert_eq!(
            full().range(Line::right(10), Line::right(19)),
            Err(RangeError::NotCommentable(Line::right(10)))
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
