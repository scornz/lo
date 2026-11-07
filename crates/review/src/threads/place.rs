//! Where a thread shows in the diff on screen (design §8, "In the diff", "Outdated threads").

use super::{DiffSide, Line, Lines, Subject, Thread};

/// The diff on screen, by what each column shows. Revisions are named by head SHA.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Screen {
    /// What the left column shows.
    pub left: Left,
    /// The revision the right column shows.
    pub right: String,
}

/// What the left column of the diff on screen shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Left {
    /// The pull request's base, as of the right column's revision: that revision's whole diff,
    /// GitHub's view of it.
    Base,
    /// Another revision, as it is: what changed between the two.
    Revision(String),
    /// Another revision's changes replayed onto the right column's base (design §6, "Rebase-aware
    /// comparison"). Its lines are no commit's, so no thread shows on them.
    Replayed(String),
}

impl Screen {
    /// The whole diff of the revision with head `head`.
    pub fn full(head: impl Into<String>) -> Self {
        Self {
            left: Left::Base,
            right: head.into(),
        }
    }

    /// What changed from revision `from` to revision `to`.
    pub fn between(from: impl Into<String>, to: impl Into<String>) -> Self {
        Self {
            left: Left::Revision(from.into()),
            right: to.into(),
        }
    }

    /// What changed from revision `from`, replayed onto `to`'s base, to revision `to`.
    pub fn replayed(from: impl Into<String>, to: impl Into<String>) -> Self {
        Self {
            left: Left::Replayed(from.into()),
            right: to.into(),
        }
    }
}

/// Where a thread shows in the diff on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    /// Under `lines.end`, covering the lines from `lines.start`. The sides here are the screen's
    /// columns, not GitHub's: `Left` is the left column, whatever it shows.
    Lines(Lines),
    /// On the file's header: a file-level thread.
    File,
    /// Only in the threads panel, labeled outdated: its lines changed after it was written, and
    /// the revision it was written on isn't on screen.
    Outdated,
    /// Only in the threads panel: it's on revisions that aren't on screen.
    NotOnScreen,
}

/// Where `thread` shows in the diff on `screen`.
///
/// - A thread that isn't outdated shows under its current lines on [`Thread::current_on`], when a
///   column shows that revision. That's usually the latest head, but not always: GitHub moves a
///   thread to each new head only on the next push, so a thread written on an older revision
///   after a newer one was pushed, or one read just before a push, is still on an older revision.
///   GitHub says where a thread is on that one revision only, so on any other it shows only if it
///   was written there.
/// - Any thread shows where it was written, when a column shows that revision. For an outdated
///   thread that's the only place in the diff; elsewhere it's [`Placement::Outdated`].
/// - When both places are on screen, the right column wins, then the current lines: between r1
///   and r3, a thread written on r1 and unchanged since shows once, on r3's line.
/// - A line on GitHub's left side is on the base, so it shows only in a revision's whole diff.
///   When only one end of a range is on screen, the thread shows under that line alone.
/// - A file-level thread shows on the file's header, unless it's outdated and the revision it was
///   written on isn't on screen.
///
/// It doesn't look at paths: the UI looks a file's threads up by the path a column shows. Between
/// revisions of a renamed file, a thread written on the older one under its old path shows in the
/// left column only if the UI looks threads up by the left file's path too. A thread written in
/// GitHub's view of a single commit is on a commit that isn't a revision's head, so it shows only
/// in the panel until GitHub moves it to a later head.
pub fn place(thread: &Thread, screen: &Screen) -> Placement {
    let written_on = thread.written_on.as_deref();
    if thread.subject == Subject::File {
        let on_screen = written_on.is_some_and(|revision| screen.shows(revision));
        return if !thread.outdated || on_screen {
            Placement::File
        } else {
            Placement::Outdated
        };
    }
    let current = (thread.current_lines())
        .filter(|_| !thread.outdated)
        .zip(thread.current_on.as_deref());
    let written = thread.written_lines().zip(written_on);
    for column in [DiffSide::Right, DiffSide::Left] {
        for (lines, revision) in [current, written].into_iter().flatten() {
            if let Some(lines) = screen.lines(lines, revision, column) {
                return Placement::Lines(lines);
            }
        }
    }
    if thread.outdated {
        Placement::Outdated
    } else {
        Placement::NotOnScreen
    }
}

impl Screen {
    /// Whether a column shows the revision with head `revision`.
    fn shows(&self, revision: &str) -> bool {
        self.right == revision || matches!(&self.left, Left::Revision(left) if left == revision)
    }

    /// Where `lines` of `revision`'s diff are on screen, looking only at `column` for the
    /// revision itself. Its base is in the left column of its whole diff.
    fn lines(&self, lines: Lines, revision: &str, column: DiffSide) -> Option<Lines> {
        let on_screen = |line: Line| {
            let side = self.column(line.side, revision, column)?;
            Some(Line::new(side, line.number))
        };
        let start = lines.start.and_then(on_screen);
        match on_screen(lines.end) {
            Some(end) => Some(Lines { start, end }),
            None => start.map(Lines::one),
        }
    }

    /// The column that shows GitHub's `side` of `revision`'s diff, when `column` shows the
    /// revision.
    fn column(&self, side: DiffSide, revision: &str, column: DiffSide) -> Option<DiffSide> {
        let shown = match column {
            DiffSide::Right => self.right == revision,
            DiffSide::Left => matches!(&self.left, Left::Revision(left) if left == revision),
        };
        match (column, side) {
            (_, DiffSide::Right) => shown.then_some(column),
            (DiffSide::Right, DiffSide::Left) => {
                (shown && self.left == Left::Base).then_some(DiffSide::Left)
            }
            (DiffSide::Left, DiffSide::Left) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::super::{CommentState, ThreadComment};
    use super::*;

    /// Revisions r1 to r3 have heads `a1` to `a3`; `a3` is the latest.
    const HEAD: &str = "a3";

    fn comment() -> ThreadComment {
        ThreadComment {
            id: "PRRC_1".to_string(),
            author: Some("octocat".to_string()),
            body: "Off by one?".to_string(),
            state: CommentState::Submitted,
            created_at: SystemTime::UNIX_EPOCH,
            url: "https://github.com/octo/repo/pull/1#discussion_r1".to_string(),
            viewer_can_update: false,
            viewer_can_delete: false,
        }
    }

    /// A thread on one line of the right side, written on `written_on` at `original_line`, and on
    /// `line` of the latest head while it isn't outdated. Like GitHub, it gives its `startLine` as
    /// its `line`, and an outdated thread's first comment stays on the commit it was written on.
    fn thread(written_on: &str, original_line: u32, line: Option<u32>) -> Thread {
        Thread {
            id: "PRRT_1".to_string(),
            path: "src/parse.rs".to_string(),
            line,
            start_line: line,
            original_line: Some(original_line),
            outdated: line.is_none(),
            current_on: Some(if line.is_some() { HEAD } else { written_on }.to_string()),
            written_on: Some(written_on.to_string()),
            comments: vec![comment()],
            ..Default::default()
        }
    }

    fn on(lines: Lines) -> Placement {
        Placement::Lines(lines)
    }

    #[test]
    fn a_thread_shows_under_its_line_on_the_latest_revision() {
        let thread = thread("a3", 12, Some(12));

        assert_eq!(
            place(&thread, &Screen::full("a3")),
            on(Lines::one(Line::right(12)))
        );
    }

    #[test]
    fn a_thread_written_on_r1_whose_line_did_not_change_shows_where_it_is_now_on_r3() {
        // r2 added four lines above it.
        let thread = thread("a1", 10, Some(14));

        assert_eq!(
            place(&thread, &Screen::full("a3")),
            on(Lines::one(Line::right(14)))
        );
    }

    #[test]
    fn a_thread_written_on_r1_whose_line_changed_is_outdated_on_r3() {
        let thread = thread("a1", 10, None);

        assert_eq!(place(&thread, &Screen::full("a3")), Placement::Outdated);
    }

    #[test]
    fn an_outdated_thread_shows_where_it_was_written_when_viewing_that_revision() {
        let thread = thread("a1", 10, None);

        assert_eq!(
            place(&thread, &Screen::full("a1")),
            on(Lines::one(Line::right(10)))
        );
    }

    #[test]
    fn an_outdated_thread_shows_in_the_left_column_when_it_shows_the_revision_it_was_written_on() {
        let thread = thread("a1", 10, None);

        assert_eq!(
            place(&thread, &Screen::between("a1", "a3")),
            on(Lines::one(Line::left(10)))
        );
    }

    #[test]
    fn a_thread_that_is_still_current_shows_in_the_right_column_between_revisions() {
        let thread = thread("a1", 10, Some(14));

        assert_eq!(
            place(&thread, &Screen::between("a1", "a3")),
            on(Lines::one(Line::right(14)))
        );
    }

    #[test]
    fn a_current_thread_is_not_on_an_older_revision_it_was_not_written_on() {
        // Its line on r2 is unknown: GitHub only says where it is on the latest.
        let thread = thread("a1", 10, Some(14));

        assert_eq!(place(&thread, &Screen::full("a2")), Placement::NotOnScreen);
        assert_eq!(
            place(&thread, &Screen::between("a2", "a3")),
            on(Lines::one(Line::right(14)))
        );
    }

    #[test]
    fn an_outdated_thread_is_only_in_the_panel_when_its_revision_is_not_on_screen() {
        let thread = thread("a1", 10, None);

        assert_eq!(
            place(&thread, &Screen::between("a2", "a3")),
            Placement::Outdated
        );
        assert_eq!(place(&thread, &Screen::full("a2")), Placement::Outdated);
    }

    #[test]
    fn an_outdated_thread_does_not_show_on_a_replayed_revision() {
        let thread = thread("a1", 10, None);

        assert_eq!(
            place(&thread, &Screen::replayed("a1", "a3")),
            Placement::Outdated
        );
    }

    #[test]
    fn a_current_thread_shows_in_the_right_column_with_a_replayed_revision() {
        // The left column's lines are no commit's, but the right column is r3.
        let thread = thread("a1", 10, Some(14));

        assert_eq!(
            place(&thread, &Screen::replayed("a1", "a3")),
            on(Lines::one(Line::right(14)))
        );
        assert_eq!(
            place(&thread, &Screen::replayed("a1", "a2")),
            Placement::NotOnScreen
        );
    }

    #[test]
    fn a_thread_on_the_base_shows_in_the_left_column_of_the_whole_diff() {
        let thread = Thread {
            side: DiffSide::Left,
            ..thread("a3", 5, Some(5))
        };

        assert_eq!(
            place(&thread, &Screen::full("a3")),
            on(Lines::one(Line::left(5)))
        );
    }

    #[test]
    fn a_thread_on_the_base_is_not_on_screen_between_revisions() {
        let thread = Thread {
            side: DiffSide::Left,
            ..thread("a1", 5, Some(5))
        };

        assert_eq!(
            place(&thread, &Screen::between("a1", "a3")),
            Placement::NotOnScreen
        );
    }

    #[test]
    fn an_outdated_thread_on_the_base_shows_in_the_whole_diff_of_its_revision() {
        let thread = Thread {
            side: DiffSide::Left,
            ..thread("a1", 5, None)
        };

        assert_eq!(
            place(&thread, &Screen::full("a1")),
            on(Lines::one(Line::left(5)))
        );
        assert_eq!(
            place(&thread, &Screen::between("a1", "a3")),
            Placement::Outdated
        );
    }

    #[test]
    fn a_multi_line_thread_covers_its_range() {
        let thread = Thread {
            start_line: Some(10),
            original_start_line: Some(10),
            ..thread("a3", 14, Some(14))
        };

        assert_eq!(
            place(&thread, &Screen::full("a3")),
            on(Lines::range(Line::right(10), Line::right(14)))
        );
    }

    #[test]
    fn a_multi_line_thread_shows_its_written_range_on_its_revision() {
        let thread = Thread {
            original_start_line: Some(3),
            ..thread("a1", 6, None)
        };

        assert_eq!(
            place(&thread, &Screen::full("a1")),
            on(Lines::range(Line::right(3), Line::right(6)))
        );
        assert_eq!(
            place(&thread, &Screen::between("a1", "a3")),
            on(Lines::range(Line::left(3), Line::left(6)))
        );
    }

    #[test]
    fn a_range_from_a_removed_line_to_an_added_line_covers_both_columns() {
        let thread = Thread {
            start_side: Some(DiffSide::Left),
            start_line: Some(7),
            original_start_line: Some(7),
            ..thread("a3", 9, Some(9))
        };

        assert_eq!(
            place(&thread, &Screen::full("a3")),
            on(Lines::range(Line::left(7), Line::right(9)))
        );
    }

    #[test]
    fn a_range_starting_on_the_base_shows_under_its_last_line_between_revisions() {
        let thread = Thread {
            start_side: Some(DiffSide::Left),
            start_line: Some(7),
            original_start_line: Some(7),
            ..thread("a1", 9, Some(9))
        };

        assert_eq!(
            place(&thread, &Screen::between("a1", "a3")),
            on(Lines::one(Line::right(9)))
        );
    }

    #[test]
    fn a_range_ending_on_the_base_shows_under_its_first_line_when_only_that_is_on_screen() {
        // From an unchanged line to the removed line after it.
        let thread = Thread {
            side: DiffSide::Left,
            start_side: Some(DiffSide::Right),
            start_line: Some(10),
            original_start_line: Some(10),
            ..thread("a1", 11, Some(11))
        };

        assert_eq!(
            place(&thread, &Screen::between("a1", "a3")),
            on(Lines::one(Line::right(10)))
        );
    }

    /// A file-level thread written on `written_on`, as GitHub gives it: on line 1.
    fn file_thread(written_on: &str, outdated: bool) -> Thread {
        Thread {
            subject: Subject::File,
            outdated,
            ..thread(written_on, 1, Some(1))
        }
    }

    #[test]
    fn a_file_thread_shows_on_the_files_header() {
        let thread = file_thread("a1", false);

        assert_eq!(place(&thread, &Screen::full("a2")), Placement::File);
        assert_eq!(
            place(&thread, &Screen::between("a1", "a3")),
            Placement::File
        );
    }

    #[test]
    fn an_outdated_file_thread_shows_on_the_header_only_where_its_revision_is_on_screen() {
        let thread = file_thread("a1", true);

        assert_eq!(place(&thread, &Screen::full("a1")), Placement::File);
        assert_eq!(
            place(&thread, &Screen::between("a1", "a3")),
            Placement::File
        );
        assert_eq!(place(&thread, &Screen::full("a3")), Placement::Outdated);
    }

    #[test]
    fn a_thread_whose_commit_github_lost_shows_only_where_it_is_now() {
        let current = Thread {
            written_on: None,
            ..thread("a1", 10, Some(14))
        };
        let outdated = Thread {
            written_on: None,
            ..thread("a1", 10, None)
        };

        assert_eq!(
            place(&current, &Screen::full("a3")),
            on(Lines::one(Line::right(14)))
        );
        assert_eq!(place(&current, &Screen::full("a1")), Placement::NotOnScreen);
        assert_eq!(place(&outdated, &Screen::full("a1")), Placement::Outdated);
    }

    #[test]
    fn a_thread_github_has_on_an_older_commit_shows_on_that_commit_only() {
        // Written on r2 at line 20 after r3 was pushed: GitHub keeps its `line` on r2, its first
        // comment's `commit`, until the next push, and gives it no `startLine`. r3 added four
        // lines above it, so r3's line 20 is other code.
        let thread = Thread {
            current_on: Some("a2".to_string()),
            start_line: None,
            ..thread("a2", 20, Some(20))
        };

        assert_eq!(place(&thread, &Screen::full("a3")), Placement::NotOnScreen);
        assert_eq!(
            place(&thread, &Screen::full("a2")),
            on(Lines::one(Line::right(20)))
        );
        assert_eq!(
            place(&thread, &Screen::between("a2", "a3")),
            on(Lines::one(Line::left(20)))
        );
    }

    #[test]
    fn a_thread_is_placed_on_its_commit_not_on_a_newer_head() {
        // Read while r3 was the latest; r4 has since been pushed, and GitHub hasn't moved it yet.
        let thread = thread("a1", 10, Some(14));

        assert_eq!(place(&thread, &Screen::full("a4")), Placement::NotOnScreen);
        assert_eq!(
            place(&thread, &Screen::between("a3", "a4")),
            on(Lines::one(Line::left(14)))
        );
    }

    #[test]
    fn a_current_thread_whose_commit_github_lost_shows_only_where_it_was_written() {
        let thread = Thread {
            current_on: None,
            ..thread("a1", 10, Some(14))
        };

        assert_eq!(place(&thread, &Screen::full("a3")), Placement::NotOnScreen);
        assert_eq!(
            place(&thread, &Screen::full("a1")),
            on(Lines::one(Line::right(10)))
        );
    }
}
