//! The diff's cursor, by file: which file it's in, and moving it to the next or previous change
//! across files (design §10 "Keys").
//!
//! The right side's editor holds the cursor; in the side-by-side view Zed keeps the left side's
//! cursor beside it. Each file's excerpts sit under [`crate::pr_diff::path_key`], whose sort
//! prefix is the file's position in review order.

use editor::{Direction, SplittableEditor};
use gpui::{App, Entity, Window};
use language::Point;

/// The file the diff's cursor is in: its position in review order and its path.
pub fn file_at_cursor(diff: &Entity<SplittableEditor>, cx: &App) -> Option<(usize, String)> {
    let editor = diff.read(cx).rhs_editor().read(cx);
    let snapshot = editor.buffer().read(cx).snapshot(cx);
    let head = editor.selections.newest_anchor().head();
    let (buffer, _) = snapshot.excerpt_containing(head..head)?;
    let key = snapshot.path_for_buffer(buffer.remote_id())?;
    let position = usize::try_from(key.sort_prefix?).ok()?;
    Some((position, key.path.as_unix_str().to_string()))
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
