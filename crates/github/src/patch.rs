//! Reading a file's patch from GitHub's list of a pull request's files: the hunks of its unified
//! diff, without the `diff --git`, `---` and `+++` lines.

use std::ops::Range;

/// A file's patch, read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Patch {
    pub hunks: Vec<Hunk>,
    /// Whether the base's version ends without a newline: GitHub's patch then follows the base's
    /// last line with `\ No newline at end of file`.
    pub old_missing_newline: bool,
    /// The same for the head's version.
    pub new_missing_newline: bool,
}

/// One hunk: a run of changed lines with the unchanged lines around them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    /// The rows, counted from 0, the hunk covers in the base's version of the file: its unchanged
    /// and deleted lines. When it has none, the empty range sits where its lines would go.
    pub old: Range<u32>,
    /// The same in the head's version: its unchanged and added lines.
    pub new: Range<u32>,
    /// The hunk's lines, in order, without their `+`, `-` or ` ` and without line endings.
    pub lines: Vec<HunkLine>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HunkLine {
    /// On both sides.
    Unchanged(String),
    /// Only in the base.
    Deleted(String),
    /// Only in the head.
    Added(String),
}

impl HunkLine {
    pub fn text(&self) -> &str {
        match self {
            Self::Unchanged(text) | Self::Deleted(text) | Self::Added(text) => text,
        }
    }

    /// Whether the base has the line.
    pub fn is_old(&self) -> bool {
        !matches!(self, Self::Added(_))
    }

    /// Whether the head has the line.
    pub fn is_new(&self) -> bool {
        !matches!(self, Self::Deleted(_))
    }
}

/// Why a patch couldn't be read.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("line {line} of the patch: {problem}")]
pub struct PatchError {
    /// The patch's line, from 1.
    pub line: usize,
    pub problem: String,
}

/// Reads a patch: hunks that each start with a header such as `@@ -12,7 +12,8 @@ fn main()`.
/// Lines may end in `\r\n`, which reads as `\n`, as rozed's buffers have them.
pub fn parse_patch(patch: &str) -> Result<Patch, PatchError> {
    let mut parsed = Patch::default();
    // The lines each side still expects in the current hunk.
    let mut remaining = (0u32, 0u32);
    // The side the previous line belonged to, for `\ No newline at end of file`.
    let mut last = None;
    let count = patch.split('\n').count();
    for (index, line) in patch.split('\n').enumerate() {
        let number = index + 1;
        let error = |problem: String| PatchError {
            line: number,
            problem,
        };
        let line = line.strip_suffix('\r').unwrap_or(line);
        if remaining == (0, 0) {
            if let Some(header) = line.strip_prefix("@@ ") {
                let (old, new) = parse_header(header).map_err(error)?;
                if let Some(previous) = parsed.hunks.last()
                    && (old.start < previous.old.end || new.start < previous.new.end)
                {
                    return Err(error("the hunk overlaps the one before it".into()));
                }
                remaining = (old.end - old.start, new.end - new.start);
                parsed.hunks.push(Hunk {
                    old,
                    new,
                    lines: Vec::new(),
                });
                last = None;
                continue;
            }
            if line.starts_with('\\') {
                no_newline(&mut parsed, last).map_err(error)?;
                continue;
            }
            // GitHub's patch has no newline after its last line.
            if line.is_empty() && index + 1 == count {
                continue;
            }
            return Err(error(if parsed.hunks.is_empty() {
                format!("expected a hunk header, found {line:?}")
            } else {
                format!("the hunk has more lines than its header says: {line:?}")
            }));
        }
        let hunk = parsed.hunks.last_mut().expect("a header came first");
        let (kind, text) = match line.chars().next() {
            Some(kind @ (' ' | '-' | '+')) => (kind, &line[1..]),
            // Some tools drop the space before an empty unchanged line.
            None => (' ', ""),
            Some('\\') => {
                no_newline(&mut parsed, last).map_err(error)?;
                continue;
            }
            Some(_) => return Err(error(format!("a hunk line starts with {line:?}"))),
        };
        let (old_left, new_left) = &mut remaining;
        let fits = match kind {
            ' ' => *old_left > 0 && *new_left > 0,
            '-' => *old_left > 0,
            _ => *new_left > 0,
        };
        if !fits {
            return Err(error(
                "the hunk has more lines than its header says".to_string(),
            ));
        }
        let text = text.to_string();
        let (line, side) = match kind {
            ' ' => {
                *old_left -= 1;
                *new_left -= 1;
                (HunkLine::Unchanged(text), Side::Both)
            }
            '-' => {
                *old_left -= 1;
                (HunkLine::Deleted(text), Side::Old)
            }
            _ => {
                *new_left -= 1;
                (HunkLine::Added(text), Side::New)
            }
        };
        hunk.lines.push(line);
        last = Some(side);
    }
    if remaining != (0, 0) {
        return Err(PatchError {
            line: count,
            problem: format!(
                "the patch ends {} lines of the base and {} of the head short of its last hunk",
                remaining.0, remaining.1
            ),
        });
    }
    Ok(parsed)
}

/// Which side a hunk line was on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    Old,
    New,
    Both,
}

/// Notes `\ No newline at end of file` for the side of the line before it.
fn no_newline(patch: &mut Patch, last: Option<Side>) -> Result<(), String> {
    match last {
        Some(Side::Old) => patch.old_missing_newline = true,
        Some(Side::New) => patch.new_missing_newline = true,
        Some(Side::Both) => {
            patch.old_missing_newline = true;
            patch.new_missing_newline = true;
        }
        None => return Err("`\\ No newline at end of file` follows no line".into()),
    }
    Ok(())
}

/// Reads `-12,7 +12,8 @@ fn main()` into the rows each side covers.
fn parse_header(header: &str) -> Result<(Range<u32>, Range<u32>), String> {
    let invalid = || format!("{:?} isn't a hunk header", format!("@@ {header}"));
    let (ranges, _section) = header.split_once(" @@").ok_or_else(invalid)?;
    let (old, new) = ranges.split_once(' ').ok_or_else(invalid)?;
    let old = old.strip_prefix('-').ok_or_else(invalid)?;
    let new = new.strip_prefix('+').ok_or_else(invalid)?;
    Ok((
        rows(old).ok_or_else(invalid)?,
        rows(new).ok_or_else(invalid)?,
    ))
}

/// The rows `start,count` covers. `start` counts lines from 1; with a count of 0 it's the line the
/// empty range follows, 0 at the start of the file. A missing count means 1.
fn rows(range: &str) -> Option<Range<u32>> {
    let (start, count) = match range.split_once(',') {
        Some((start, count)) => (start.parse::<u32>().ok()?, count.parse::<u32>().ok()?),
        None => (range.parse::<u32>().ok()?, 1),
    };
    if count == 0 {
        return Some(start..start);
    }
    let first = start.checked_sub(1)?;
    Some(first..first.checked_add(count)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    use HunkLine::{Added, Deleted, Unchanged};

    fn lines(lines: &[HunkLine]) -> Vec<HunkLine> {
        lines.to_vec()
    }

    fn unchanged(text: &str) -> HunkLine {
        Unchanged(text.into())
    }

    fn deleted(text: &str) -> HunkLine {
        Deleted(text.into())
    }

    fn added(text: &str) -> HunkLine {
        Added(text.into())
    }

    #[test]
    fn hunks_give_the_rows_each_side_covers() {
        let patch = "@@ -2,4 +2,5 @@ fn main() {\n     let a = 1;\n-    let b = 2;\n+    let b = 3;\n+    let c = 4;\n     println!();\n }\n@@ -40,3 +41,2 @@ fn other() {\n     one();\n-    two();\n     three();";
        let parsed = parse_patch(patch).unwrap();
        assert_eq!(
            parsed,
            Patch {
                hunks: vec![
                    Hunk {
                        old: 1..5,
                        new: 1..6,
                        lines: lines(&[
                            unchanged("    let a = 1;"),
                            deleted("    let b = 2;"),
                            added("    let b = 3;"),
                            added("    let c = 4;"),
                            unchanged("    println!();"),
                            unchanged("}"),
                        ]),
                    },
                    Hunk {
                        old: 39..42,
                        new: 40..42,
                        lines: lines(&[
                            unchanged("    one();"),
                            deleted("    two();"),
                            unchanged("    three();"),
                        ]),
                    },
                ],
                old_missing_newline: false,
                new_missing_newline: false,
            }
        );
    }

    #[test]
    fn an_added_file_covers_no_rows_of_the_base() {
        let parsed = parse_patch("@@ -0,0 +1,2 @@\n+one\n+two").unwrap();
        assert_eq!(parsed.hunks[0].old, 0..0);
        assert_eq!(parsed.hunks[0].new, 0..2);
    }

    #[test]
    fn a_deleted_file_covers_no_rows_of_the_head() {
        let parsed = parse_patch("@@ -1,2 +0,0 @@\n-one\n-two").unwrap();
        assert_eq!(parsed.hunks[0].old, 0..2);
        assert_eq!(parsed.hunks[0].new, 0..0);
    }

    #[test]
    fn a_missing_count_means_one_line() {
        let parsed = parse_patch("@@ -0,0 +1 @@\n+Layer A of the rozed stack fixture.").unwrap();
        assert_eq!(parsed.hunks[0].new, 0..1);
        let parsed = parse_patch("@@ -3 +3 @@\n-old\n+new").unwrap();
        assert_eq!(
            (parsed.hunks[0].old.clone(), parsed.hunks[0].new.clone()),
            (2..3, 2..3)
        );
    }

    #[test]
    fn lines_inserted_after_a_line_sit_after_it() {
        // Two lines added after line 5 of the base, with no lines around them.
        let parsed = parse_patch("@@ -5,0 +6,2 @@\n+one\n+two").unwrap();
        assert_eq!(parsed.hunks[0].old, 5..5);
        assert_eq!(parsed.hunks[0].new, 5..7);
    }

    #[test]
    fn a_missing_newline_at_the_end_is_noted_for_its_side() {
        let patch = "@@ -1 +1 @@\n-old\n\\ No newline at end of file\n+new";
        let parsed = parse_patch(patch).unwrap();
        assert!(parsed.old_missing_newline);
        assert!(!parsed.new_missing_newline);
        assert_eq!(parsed.hunks[0].lines, [deleted("old"), added("new")]);

        let patch = "@@ -1,2 +1,2 @@\n-a\n+b\n same\n\\ No newline at end of file";
        let parsed = parse_patch(patch).unwrap();
        assert!(parsed.old_missing_newline && parsed.new_missing_newline);
    }

    #[test]
    fn carriage_returns_and_empty_unchanged_lines_read_as_rozeds_buffers_have_them() {
        let patch = "@@ -1,3 +1,3 @@\r\n a\r\n\n-b\r\n+c\r\n";
        let parsed = parse_patch(patch).unwrap();
        assert_eq!(
            parsed.hunks[0].lines,
            [unchanged("a"), unchanged(""), deleted("b"), added("c")]
        );
    }

    #[test]
    fn a_patch_that_doesnt_add_up_is_an_error() {
        let short = parse_patch("@@ -1,3 +1,3 @@\n a\n-b").unwrap_err();
        assert!(short.problem.contains("short of its last hunk"), "{short}");

        let long = parse_patch("@@ -1 +1 @@\n-a\n+b\n+c").unwrap_err();
        assert_eq!(long.line, 4);
        assert!(long.problem.contains("more lines"), "{long}");

        let headless = parse_patch("Binary files differ").unwrap_err();
        assert!(headless.problem.contains("hunk header"), "{headless}");

        for header in ["@@ -a +1 @@", "@@ -1 1 @@", "@@ -1 +1", "@@ -0,1 +1 @@"] {
            let error = parse_patch(&format!("{header}\n x")).unwrap_err();
            assert!(error.problem.contains("hunk header"), "{header}: {error}");
        }

        let overlapping = parse_patch("@@ -1,2 +1,2 @@\n a\n b\n@@ -2 +2 @@\n b").unwrap_err();
        assert!(overlapping.problem.contains("overlaps"), "{overlapping}");

        let stray = parse_patch("@@ -1 +1 @@\n*a").unwrap_err();
        assert!(stray.problem.contains("starts with"), "{stray}");
    }

    #[test]
    fn an_empty_patch_has_no_hunks() {
        assert_eq!(parse_patch("").unwrap(), Patch::default());
    }
}
