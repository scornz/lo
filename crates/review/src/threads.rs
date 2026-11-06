//! Review threads: where each one shows in the diff on screen, and which ones the threads panel
//! lists (design §8, "Comments", "Threads panel").
//!
//! The GitHub layer fills a [`Thread`] from GraphQL's `PullRequestReviewThread`. [`place`] says
//! where it shows in the diff on screen, a [`Screen`]: under a line of either column, on the file's
//! header, or only in the threads panel. GitHub reports a thread's current lines on its first
//! comment's commit. On each push it moves the thread to the new head while its lines are
//! unchanged, and marks it outdated once they change. An outdated thread still shows where it was
//! written whenever the diff on screen shows that revision. [`panel`] lists the threads the threads
//! panel shows, by [`PanelFilter`].
//!
//! [`CommentableLines`] says which lines of a file on screen GitHub takes a new comment on, from
//! the file's [`Hunk`]s, and whether it takes a file-level comment, which the UI offers elsewhere.
//! [`suggestion_block`] starts a suggested change.
//!
//! ```
//! use review::threads::{CommentableLines, Hunk, Line, Lines, Placement, Screen, Thread, place};
//!
//! // Written on r1 (head `a1`) at line 10. r2 added four lines above it, and it's unchanged
//! // since, so GitHub has it on line 14 of the latest head, `a3`.
//! let thread = Thread {
//!     path: "src/parse.rs".into(),
//!     side: "RIGHT".parse().unwrap(),
//!     line: Some(14),
//!     start_line: Some(14),
//!     original_line: Some(10),
//!     current_on: Some("a3".into()),
//!     written_on: Some("a1".into()),
//!     ..Default::default()
//! };
//! let under = |line| Placement::Lines(Lines::one(line));
//! assert_eq!(place(&thread, &Screen::full("a3")), under(Line::right(14)));
//! assert_eq!(place(&thread, &Screen::full("a1")), under(Line::right(10)));
//! assert_eq!(place(&thread, &Screen::full("a2")), Placement::NotOnScreen);
//! assert!(thread.starts_open());
//!
//! // GitHub takes comments on the lines of the pull request's hunks, and nowhere else.
//! let hunks = Hunk::parse("@@ -1,2 +1,3 @@\n fn main() {\n+    run();\n }\n").unwrap();
//! let commentable = CommentableLines::new(&Screen::full("a3"), hunks);
//! assert_eq!(commentable.line(Line::right(2)), Some(Lines::one(Line::right(2))));
//! assert_eq!(commentable.line(Line::right(9)), None);
//! ```

mod commentable;
mod panel;
mod place;
mod suggestion;

use std::time::SystemTime;

use crate::github::github_enum;

pub use commentable::{CommentableLines, Hunk, HunkLine, PatchError, RangeError};
pub use panel::{PanelFilter, panel};
pub use place::{Left, Placement, Screen, place};
pub use suggestion::suggestion_block;

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
/// GitHub keeps two places for a line thread: where it is now (`line`, `startLine`, on
/// `current_on`), while its lines are unchanged since it was written, and where it was written
/// (`originalLine`, `originalStartLine`, on `written_on`). Both use the same sides.
///
/// GitHub gives a one-line thread a `startLine` equal to its `line`, or none, and never a
/// `startDiffSide`; it gives a file-level thread `line: 1`. Neither makes a range.
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
    /// `startDiffSide`: the side of its first line, for a range. GitHub sets it for every range;
    /// when it's `None` and a start line is set, the range starts on `side`.
    pub start_side: Option<DiffSide>,
    /// `line`: its last line on `current_on`. `None` once it's outdated. A file-level thread has
    /// no lines, whatever GitHub gives here.
    pub line: Option<u32>,
    /// `startLine`: its first line on `current_on`, for a range.
    pub start_line: Option<u32>,
    /// `originalLine`: its last line on the revision it was written on.
    pub original_line: Option<u32>,
    /// `originalStartLine`: its first line on the revision it was written on, for a range.
    pub original_start_line: Option<u32>,
    /// `isOutdated`: its lines changed after it was written.
    pub outdated: bool,
    /// `isResolved`.
    pub resolved: bool,
    /// The revision `line` and `startLine` are on, by head SHA: its first comment's `commit.oid`.
    /// GitHub moves a thread to each new head when it's pushed, while the thread's lines are
    /// unchanged, so this is usually the latest head. A thread written on an older revision after
    /// a newer one was pushed stays on the older one until the next push, and one read just before
    /// a push is on the head before it. Once outdated it's `written_on`. `None` when GitHub no
    /// longer has the commit.
    pub current_on: Option<String>,
    /// The revision it was written on, by head SHA: its first comment's `originalCommit.oid`.
    /// `None` when GitHub no longer has the commit.
    pub written_on: Option<String>,
    /// `comments`, oldest first.
    pub comments: Vec<ThreadComment>,
}

impl Thread {
    /// Its lines on `current_on`: `None` for a file-level thread, and once it's outdated.
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

    /// Whether the person with login `login` started it or replied in it. Logins ignore case, as
    /// GitHub's do.
    pub fn involves(&self, login: &str) -> bool {
        self.comments.iter().any(|comment| {
            (comment.author.as_deref()).is_some_and(|author| author.eq_ignore_ascii_case(login))
        })
    }

    /// Whether it's in your pending review: its first comment isn't posted yet, so only you see it.
    pub fn is_pending(&self) -> bool {
        (self.comments.first()).is_some_and(|comment| comment.state == CommentState::Pending)
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
    fn a_one_line_thread_whose_start_line_github_gives_as_its_line_is_one_line() {
        // GitHub's shape: `startLine` equal to `line`, with no `startDiffSide`.
        for side in [DiffSide::Right, DiffSide::Left] {
            let thread = Thread {
                side,
                line: Some(9),
                start_line: Some(9),
                original_line: Some(9),
                ..Default::default()
            };

            assert_eq!(thread.current_lines(), Some(Lines::one(Line::new(side, 9))));
            assert!(!thread.current_lines().unwrap().is_range());
        }
    }

    #[test]
    fn a_range_from_a_removed_line_to_the_line_that_replaced_it_keeps_both_sides() {
        // `startLine` equals `line`, but `startDiffSide` differs from `diffSide`.
        let thread = Thread {
            side: DiffSide::Right,
            start_side: Some(DiffSide::Left),
            line: Some(10),
            start_line: Some(10),
            ..Default::default()
        };

        assert_eq!(
            thread.current_lines(),
            Some(Lines::range(Line::left(10), Line::right(10)))
        );
    }

    #[test]
    fn an_outdated_thread_has_no_current_lines() {
        // GitHub's shape: `line` and `startLine` null.
        let thread = Thread {
            line: None,
            start_line: None,
            original_line: Some(9),
            outdated: true,
            ..Default::default()
        };

        assert_eq!(thread.current_lines(), None);
        assert_eq!(thread.written_lines(), Some(Lines::one(Line::right(9))));
    }

    #[test]
    fn a_file_thread_has_no_lines() {
        // GitHub gives a file-level thread `line: 1` and `originalLine: 1`.
        let thread = Thread {
            subject: Subject::File,
            line: Some(1),
            original_line: Some(1),
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
    fn a_thread_whose_first_comment_is_pending_is_in_your_pending_review() {
        let comment = |state| ThreadComment {
            id: "PRRC_1".to_string(),
            author: Some("alice".to_string()),
            body: "Typo".to_string(),
            state,
            created_at: SystemTime::UNIX_EPOCH,
        };
        let pending = Thread {
            comments: vec![comment(CommentState::Pending)],
            ..Default::default()
        };
        let replied_to = Thread {
            comments: vec![
                comment(CommentState::Submitted),
                comment(CommentState::Pending),
            ],
            ..Default::default()
        };

        assert!(pending.is_pending());
        assert!(!replied_to.is_pending());
        assert!(!Thread::default().is_pending());
    }

    #[test]
    fn graphql_values_parse() {
        assert_eq!("LEFT".parse(), Ok(DiffSide::Left));
        assert_eq!("FILE".parse(), Ok(Subject::File));
        assert_eq!("PENDING".parse(), Ok(CommentState::Pending));
        assert_eq!(DiffSide::Right.as_str(), "RIGHT");
    }
}
