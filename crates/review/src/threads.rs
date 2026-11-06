//! Review threads: where each one shows in the diff on screen, and which ones the threads panel
//! lists (design §8, "Comments", "Threads panel").
//!
//! The GitHub layer fills a [`Thread`] from GraphQL's `PullRequestReviewThread`. [`place`] says
//! where it shows in the diff on screen, a [`Screen`]: under a line of either column, on the file's
//! header, or only in the threads panel. GitHub keeps a thread on the pull request's latest head
//! while its lines are unchanged, and marks it outdated once they change. An outdated thread still
//! shows where it was written whenever the diff on screen shows that revision.

mod place;

use std::time::SystemTime;

use crate::github::github_enum;

pub use place::{Left, Placement, Screen, place};

github_enum! {
    /// A side of a pull request's diff: GraphQL's `DiffSide`. `Left` is the base's version of a
    /// file and `Right` the head's.
    #[derive(Default, PartialOrd, Ord)]
    pub enum DiffSide {
        Left = "LEFT",
        #[default]
        Right = "RIGHT",
    }
}

github_enum! {
    /// What a thread is on: GraphQL's `PullRequestReviewThreadSubjectType`.
    #[derive(Default)]
    pub enum Subject {
        /// One line, or a range of lines.
        #[default]
        Line = "LINE",
        /// The whole file.
        File = "FILE",
    }
}

github_enum! {
    /// Whether a comment is posted: GraphQL's `PullRequestReviewCommentState`.
    #[derive(Default)]
    pub enum CommentState {
        /// In your pending review. Only you see it.
        Pending = "PENDING",
        /// Posted.
        #[default]
        Submitted = "SUBMITTED",
    }
}

/// A 1-based line on one side of a diff.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Line {
    pub side: DiffSide,
    pub number: u32,
}

impl Line {
    pub fn new(side: DiffSide, number: u32) -> Self {
        Self { side, number }
    }

    /// A line of the base's version of the file.
    pub fn left(number: u32) -> Self {
        Self::new(DiffSide::Left, number)
    }

    /// A line of the head's version of the file.
    pub fn right(number: u32) -> Self {
        Self::new(DiffSide::Right, number)
    }
}

/// The lines a comment is on: one line, or a range that can start on the other side, as GitHub's
/// `startLine` and `startDiffSide` allow.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Lines {
    /// The first line of a range, or `None` for one line.
    pub start: Option<Line>,
    /// The last line, which a thread shows under.
    pub end: Line,
}

impl Lines {
    /// One line.
    pub fn one(line: Line) -> Self {
        Self {
            start: None,
            end: line,
        }
    }

    /// The lines from `start` to `end`. A range of one line is that line.
    pub fn range(start: Line, end: Line) -> Self {
        Self {
            start: (start != end).then_some(start),
            end,
        }
    }

    /// The first line.
    pub fn first(&self) -> Line {
        self.start.unwrap_or(self.end)
    }

    /// Whether the lines are a range of more than one line.
    pub fn is_range(&self) -> bool {
        self.start.is_some()
    }
}

/// A review thread, as GraphQL's `PullRequestReviewThread` describes it. Field docs name the
/// GraphQL field each one comes from.
///
/// GitHub keeps two places for a line thread: where it is on the pull request's latest head
/// (`line`, `startLine`), while its lines are unchanged since, and where it was written
/// (`originalLine`, `originalStartLine`, on `written_on`). Both use the same sides.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Thread {
    /// `id`.
    pub id: String,
    /// `path`.
    pub path: String,
    /// `subjectType`.
    pub subject: Subject,
    /// `diffSide`: the side of its last line.
    pub side: DiffSide,
    /// `startDiffSide`: the side of its first line, for a range. When it's `None` and a start line
    /// is set, the range starts on `side`.
    pub start_side: Option<DiffSide>,
    /// `line`: its last line on the pull request's latest head. `None` for a file-level thread,
    /// and once it's outdated.
    pub line: Option<u32>,
    /// `startLine`: its first line on the latest head, for a range.
    pub start_line: Option<u32>,
    /// `originalLine`: its last line on the revision it was written on.
    pub original_line: Option<u32>,
    /// `originalStartLine`: its first line on the revision it was written on, for a range.
    pub original_start_line: Option<u32>,
    /// `isOutdated`: its lines changed after it was written.
    pub outdated: bool,
    /// `isResolved`.
    pub resolved: bool,
    /// The revision it was written on, by head SHA: its first comment's `originalCommit.oid`.
    /// `None` when GitHub no longer has the commit.
    pub written_on: Option<String>,
    /// `comments`, oldest first.
    pub comments: Vec<ThreadComment>,
}

impl Thread {
    /// Its lines on the pull request's latest head: `None` for a file-level thread, and once it's
    /// outdated.
    pub fn current_lines(&self) -> Option<Lines> {
        self.lines(self.start_line, self.line)
    }

    /// Its lines on the revision it was written on, `written_on`. `None` for a file-level thread.
    pub fn written_lines(&self) -> Option<Lines> {
        self.lines(self.original_start_line, self.original_line)
    }

    /// Whether it starts open in the diff: unresolved threads do, and resolved ones start
    /// collapsed.
    pub fn starts_open(&self) -> bool {
        !self.resolved
    }

    fn lines(&self, start: Option<u32>, end: Option<u32>) -> Option<Lines> {
        if self.subject == Subject::File {
            return None;
        }
        let end = Line::new(self.side, end?);
        Some(match start {
            Some(start) => {
                Lines::range(Line::new(self.start_side.unwrap_or(self.side), start), end)
            }
            None => Lines::one(end),
        })
    }
}

/// A comment in a thread, as GraphQL's `PullRequestReviewComment` describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadComment {
    /// `id`.
    pub id: String,
    /// `author.login`. `None` for a deleted account, which GitHub shows as "ghost".
    pub author: Option<String>,
    /// `body`, in Markdown.
    pub body: String,
    /// `state`.
    pub state: CommentState,
    /// `createdAt`.
    pub created_at: SystemTime,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thread_on_one_line_is_on_that_line() {
        let thread = Thread {
            line: Some(14),
            original_line: Some(10),
            ..Default::default()
        };

        assert_eq!(thread.current_lines(), Some(Lines::one(Line::right(14))));
        assert_eq!(thread.written_lines(), Some(Lines::one(Line::right(10))));
    }

    #[test]
    fn a_range_starts_on_its_start_side() {
        let thread = Thread {
            side: DiffSide::Right,
            start_side: Some(DiffSide::Left),
            line: Some(9),
            start_line: Some(7),
            original_line: Some(9),
            original_start_line: Some(6),
            ..Default::default()
        };

        assert_eq!(
            thread.current_lines(),
            Some(Lines::range(Line::left(7), Line::right(9)))
        );
        assert_eq!(
            thread.written_lines(),
            Some(Lines::range(Line::left(6), Line::right(9)))
        );
    }

    #[test]
    fn a_range_without_a_start_side_starts_on_its_side() {
        let thread = Thread {
            side: DiffSide::Left,
            line: Some(9),
            start_line: Some(7),
            ..Default::default()
        };

        assert_eq!(
            thread.current_lines(),
            Some(Lines::range(Line::left(7), Line::left(9)))
        );
    }

    #[test]
    fn an_outdated_thread_has_no_current_lines() {
        let thread = Thread {
            line: None,
            start_line: Some(7),
            original_line: Some(9),
            outdated: true,
            ..Default::default()
        };

        assert_eq!(thread.current_lines(), None);
        assert_eq!(thread.written_lines(), Some(Lines::one(Line::right(9))));
    }

    #[test]
    fn a_file_thread_has_no_lines() {
        let thread = Thread {
            subject: Subject::File,
            ..Default::default()
        };

        assert_eq!(thread.current_lines(), None);
        assert_eq!(thread.written_lines(), None);
    }

    #[test]
    fn unresolved_threads_start_open_and_resolved_ones_collapsed() {
        let unresolved = Thread::default();
        let resolved = Thread {
            resolved: true,
            ..Default::default()
        };

        assert!(unresolved.starts_open());
        assert!(!resolved.starts_open());
    }

    #[test]
    fn a_range_of_one_line_is_that_line() {
        assert_eq!(
            Lines::range(Line::right(4), Line::right(4)),
            Lines::one(Line::right(4))
        );
        assert!(Lines::range(Line::left(4), Line::right(4)).is_range());
        assert_eq!(
            Lines::range(Line::left(2), Line::right(4)).first(),
            Line::left(2)
        );
    }

    #[test]
    fn graphql_values_parse() {
        assert_eq!("LEFT".parse(), Ok(DiffSide::Left));
        assert_eq!("FILE".parse(), Ok(Subject::File));
        assert_eq!("PENDING".parse(), Ok(CommentState::Pending));
        assert_eq!(DiffSide::Right.as_str(), "RIGHT");
    }
}
