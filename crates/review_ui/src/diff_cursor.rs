//! The diff's cursor, by file: which file it's in, and moving it to the next or previous change
//! across files (design §10 "Keys").
//!
//! The right side's editor holds the cursor; in the side-by-side view Zed keeps the left side's
//! cursor beside it. Each file's excerpts sit under [`crate::pr_diff::path_key`], whose sort
//! prefix is the file's position in review order.

use editor::display_map::{DisplayRow, ToDisplayPoint as _};
use editor::{Bias, Direction, DisplayPoint, SelectionEffects, SplittableEditor, ToPoint as _};
use gpui::{App, Entity, Window};
use language::Point;
use multi_buffer::MultiBufferSnapshot;

/// The file the diff's cursor is in: its position in review order and its path.
pub fn file_at_cursor(diff: &Entity<SplittableEditor>, cx: &App) -> Option<(usize, String)> {
    let editor = diff.read(cx).rhs_editor().read(cx);
    let snapshot = editor.buffer().read(cx).snapshot(cx);
    let head = editor.selections.newest_anchor().head().to_point(&snapshot);
    file_at(&snapshot, head)
}

/// The file the diff shows, which `e` and the movement keys act on: the one the cursor is in
/// while it's in view, or about to be scrolled into view, else the one at the top of the view.
/// Scrolling doesn't move the cursor. Its position in review order and its path.
pub fn shown_file(diff: &Entity<SplittableEditor>, cx: &mut App) -> Option<(usize, String)> {
    let editor = diff.read(cx).rhs_editor().clone();
    editor.update(cx, |editor, cx| {
        let display = editor.display_snapshot(cx);
        let head = editor.selections.newest_anchor().head();
        let top = editor.scroll_position(cx).y.max(0.);
        let cursor_row = f64::from(head.to_display_point(&display).row().0);
        let in_view = editor.has_autoscroll_request()
            || editor
                .visible_line_count()
                .is_none_or(|lines| cursor_row >= top.floor() && cursor_row < top + lines);
        let point = if in_view {
            head.to_point(display.buffer_snapshot())
        } else {
            let top_row = DisplayPoint::new(DisplayRow(top.floor() as u32), 0);
            display.display_point_to_point(top_row, Bias::Right)
        };
        file_at(display.buffer_snapshot(), point)
    })
}

/// The file at `point` in the diff: its position in review order and its path.
fn file_at(snapshot: &MultiBufferSnapshot, point: Point) -> Option<(usize, String)> {
    let (buffer, _) = snapshot.point_to_buffer_point(point)?;
    let key = snapshot.path_for_buffer(buffer.remote_id())?;
    let position = usize::try_from(key.sort_prefix?).ok()?;
    Some((position, key.path.as_unix_str().to_string()))
}

/// Where the diff's cursor is: its file, and the point in the file's head version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CursorPosition {
    /// The file's path in the pull request.
    pub path: String,
    pub point: Point,
}

/// Where the diff's cursor is, if it's in a file.
pub fn cursor_position(diff: &Entity<SplittableEditor>, cx: &App) -> Option<CursorPosition> {
    let editor = diff.read(cx).rhs_editor().read(cx);
    let snapshot = editor.buffer().read(cx).snapshot(cx);
    let head = editor.selections.newest_anchor().head().to_point(&snapshot);
    let (buffer, point) = snapshot.point_to_buffer_point(head)?;
    let key = snapshot.path_for_buffer(buffer.remote_id())?;
    Some(CursorPosition {
        path: key.path.as_unix_str().to_string(),
        point,
    })
}

/// Puts the diff's cursor at `position`, or at the nearest line its file shows, without
/// scrolling. Returns whether the diff has the file.
pub fn move_cursor_to(
    diff: &Entity<SplittableEditor>,
    position: &CursorPosition,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    let editor = diff.read(cx).rhs_editor().clone();
    let anchor = {
        let multi_buffer = editor.read(cx).buffer().read(cx);
        let snapshot = multi_buffer.snapshot(cx);
        multi_buffer
            .all_buffers_iter()
            .find(|buffer| {
                snapshot
                    .path_for_buffer(buffer.read(cx).remote_id())
                    .is_some_and(|key| key.path.as_unix_str() == position.path)
            })
            .and_then(|buffer| multi_buffer.buffer_point_to_anchor(&buffer, position.point, cx))
    };
    let Some(anchor) = anchor else {
        return false;
    };
    editor.update(cx, |editor, cx| {
        editor.change_selections(SelectionEffects::no_scroll(), window, cx, |selections| {
            selections.select_anchor_ranges([anchor..anchor])
        })
    });
    true
}

/// Moves the diff's cursor to the start of the next change after it, or the previous one before
/// it, in whichever file that is. At the last change, or the first, it stays.
pub fn go_to_change(
    diff: &Entity<SplittableEditor>,
    direction: Direction,
    window: &mut Window,
    cx: &mut App,
) {
    let editor = diff.read(cx).rhs_editor().clone();
    editor.update(cx, |editor, cx| {
        let snapshot = editor.snapshot(window, cx);
        let head = editor
            .selections
            .newest::<Point>(&snapshot.display_snapshot)
            .head();
        editor.go_to_hunk_before_or_after_position(&snapshot, head, direction, false, window, cx);
    });
}
