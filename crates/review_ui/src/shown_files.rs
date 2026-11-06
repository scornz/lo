//! Each file in a pull request's diff as it shows, and what the diff's keys change about it
//! (design §5, "The diff view"): opening a collapsed file, loading a large one, showing a whole
//! file, hiding whitespace changes, the side the cursor is on, inline or side by side, and the
//! before and after of a changed image.
//!
//! The diff's right-hand editor keeps a [`ShownFiles`] addon, so each diff, GitHub's or the local
//! copy's, has its own.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use buffer_diff::BufferDiff;
use editor::display_map::{
    BlockContext, BlockPlacement, BlockProperties, BlockStyle, CustomBlockId,
};
use editor::scroll::Autoscroll;
use editor::{Addon, Editor, SplittableEditor, ToggleSplitDiff, multibuffer_context_lines};
use gpui::StyledImage as _;
use gpui::{
    AnyElement, App, AppContext as _, AsyncWindowContext, Entity, EntityId, Focusable as _,
    ObjectFit, RenderImage, WeakEntity, Window, checkerboard, img, size,
};
use language::{Buffer, BufferId, OffsetRangeExt as _, Point};
use multi_buffer::PathKey;
use settings::DiffViewStyle;
use ui::{Label, LabelSize, prelude::*};

use crate::file_display::{Collapsed, ImageSide, Images, without_whitespace_changes};
use crate::pr_diff::{FileHeaders, LocalPullRequest};

/// One file in the diff.
pub(crate) struct ShownFile {
    /// Where its excerpts sit in the diff: its position in review order, then its path.
    pub(crate) key: PathKey,
    pub(crate) buffer: Entity<Buffer>,
    pub(crate) buffer_id: BufferId,
    pub(crate) diff: Entity<BufferDiff>,
    /// The base's text, for a file compared as text: `Some(None)` when the pull request adds it.
    /// Hiding whitespace changes compares the head with another base, and showing them puts this
    /// back. `None` for a file shown by a placeholder or as images.
    pub(crate) base: Option<Option<Arc<str>>>,
    /// The head's rows that show, when only they're known (GitHub's diff).
    pub(crate) rows: Option<Vec<Range<u32>>>,
    /// The before and after of a changed image, which show under the file's last line.
    pub(crate) images: Option<Images>,
    /// `images`, decoded to draw.
    pub(crate) drawn: Option<Drawn>,
    /// Why the file shows collapsed, if it does or did.
    pub(crate) collapsed: Option<Collapsed>,
    /// Whether the whole file shows, rather than its changes with the lines around them.
    pub(crate) whole: bool,
    /// Whether its diff hides changes to whitespace alone.
    pub(crate) whitespace_hidden: bool,
    /// The image blocks shown for it, in the right-hand editor and the left-hand one.
    image_blocks: Vec<(WeakEntity<Editor>, CustomBlockId)>,
}

impl ShownFile {
    pub(crate) fn new(
        key: PathKey,
        buffer: Entity<Buffer>,
        buffer_id: BufferId,
        diff: Entity<BufferDiff>,
        base: Option<Option<Arc<str>>>,
    ) -> Self {
        Self {
            key,
            buffer,
            buffer_id,
            diff,
            base,
            rows: None,
            images: None,
            drawn: None,
            collapsed: None,
            whole: false,
            whitespace_hidden: false,
            image_blocks: Vec::new(),
        }
    }
}

/// One side of a changed image, decoded to draw, or why it can't be.
pub(crate) type DrawnSide = Option<Result<Arc<RenderImage>, SharedString>>;

/// The two sides of a changed image, decoded to draw. A side is `None` when the pull request adds
/// or deletes the image.
#[derive(Clone)]
pub(crate) struct Drawn {
    pub(crate) old: DrawnSide,
    pub(crate) new: DrawnSide,
}

/// Decodes `images` in the background, so drawing them needs no more work.
pub(crate) async fn decode(images: &Images, cx: &mut AsyncWindowContext) -> Drawn {
    let Ok(renderer) = cx.update(|_, cx| cx.svg_renderer()) else {
        let gone = || Some(Err(SharedString::from("the window closed")));
        return Drawn {
            old: gone(),
            new: gone(),
        };
    };
    let decode = |side: &Option<ImageSide>| {
        let image = side.as_ref().map(|side| side.image.clone());
        let renderer = renderer.clone();
        cx.background_spawn(async move {
            image.map(|image| {
                image
                    .to_image_data(renderer)
                    .map_err(|error| SharedString::from(format!("{error:#}")))
            })
        })
    };
    let (old, new) = (decode(&images.old), decode(&images.new));
    Drawn {
        old: old.await,
        new: new.await,
    }
}

/// The diff's files, by path as shown.
#[derive(Default)]
pub(crate) struct ShownFiles {
    files: HashMap<String, ShownFile>,
    /// The local copy the files come from, which loads a large file on a key and keeps the clone
    /// open while the diff shows. `None` for GitHub's diff.
    pub(crate) local: Option<Arc<LocalPullRequest>>,
    /// Whether the diff shows side by side, for the image blocks, which show both images when it
    /// doesn't and one each side when it does.
    split: Arc<AtomicBool>,
    /// The left-hand editor the image blocks were last added to.
    lhs: Option<EntityId>,
}

impl Addon for ShownFiles {
    fn to_any(&self) -> &dyn std::any::Any {
        self
    }

    fn to_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

impl ShownFiles {
    pub(crate) fn get(&self, path: &str) -> Option<&ShownFile> {
        self.files.get(path)
    }

    pub(crate) fn paths(&self) -> Vec<String> {
        self.files.keys().cloned().collect()
    }
}

/// Reads `diff`'s files.
pub(crate) fn read<R>(
    diff: &Entity<SplittableEditor>,
    cx: &App,
    f: impl FnOnce(&ShownFiles) -> R,
) -> Option<R> {
    let rhs = diff.read(cx).rhs_editor().read(cx);
    rhs.addon::<ShownFiles>().map(f)
}

/// Changes `diff`'s files, and the headers that say how they show.
fn update<R>(
    diff: &Entity<SplittableEditor>,
    cx: &mut App,
    f: impl FnOnce(&mut ShownFiles, &mut FileHeaders) -> R,
) -> Option<R> {
    let rhs = diff.read(cx).rhs_editor().clone();
    rhs.update(cx, |rhs, cx| {
        let mut files = rhs.addon_mut::<ShownFiles>().map(std::mem::take)?;
        let result = rhs
            .addon_mut::<FileHeaders>()
            .map(|headers| f(&mut files, headers));
        if let Some(addon) = rhs.addon_mut::<ShownFiles>() {
            *addon = files;
        }
        cx.notify();
        result
    })
}

/// Adds `file`, at `path`, to `diff`'s files and shows its images, folding it when `fold` says.
/// A file that was there before, such as the placeholder of a large file now loaded, makes way
/// for it.
pub(crate) fn add(
    diff: &Entity<SplittableEditor>,
    path: String,
    file: ShownFile,
    fold: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let buffer_id = file.buffer_id;
    let collapsed = file.collapsed.clone();
    let replaced = update(diff, cx, |files, headers| {
        headers.set_collapsed(buffer_id, collapsed);
        let replaced = files.files.insert(path.clone(), file)?;
        if replaced.buffer_id != buffer_id {
            headers.forget(replaced.buffer_id);
        }
        Some(replaced)
    })
    .flatten();
    if let Some(replaced) = replaced {
        remove_image_blocks(replaced.image_blocks, cx);
    }
    if fold {
        let rhs = diff.read(cx).rhs_editor().clone();
        rhs.update(cx, |rhs, cx| rhs.fold_buffers([buffer_id], cx));
    }
    show_images(diff, &path, window, cx);
}

/// The path of the file the diff's cursor is in. The right-hand editor holds the cursor; side by
/// side, Zed keeps it beside the left-hand one's.
pub(crate) fn file_at_cursor(diff: &Entity<SplittableEditor>, cx: &App) -> Option<String> {
    let editor = diff.read(cx).rhs_editor().read(cx);
    let snapshot = editor.buffer().read(cx).snapshot(cx);
    let head = editor.selections.newest_anchor().head();
    let (buffer, _) = snapshot.excerpt_containing(head..head)?;
    let key = snapshot.path_for_buffer(buffer.remote_id())?;
    Some(key.path.as_unix_str().to_string())
}

/// The rows of a file that show: its fixed rows, its whole text, or each change (with the lines
/// around it, which the diff adds). A file with images shows its last row too, which they sit
/// under. A file with no changes shows whole, collapsed. Also returns whether those are bare
/// rows, shown without lines around them.
fn ranges(file: &ShownFile, cx: &App) -> (Vec<Range<Point>>, bool) {
    let snapshot = file.buffer.read(cx).snapshot();
    let whole = || vec![Point::zero()..snapshot.max_point()];
    if let Some(rows) = &file.rows {
        let ranges = rows
            .iter()
            .map(|rows| {
                let last = rows.end.saturating_sub(1).max(rows.start);
                Point::new(rows.start, 0)..Point::new(last, 0)
            })
            .collect::<Vec<_>>();
        return (if ranges.is_empty() { whole() } else { ranges }, true);
    }
    if file.whole {
        return (whole(), true);
    }
    let mut ranges: Vec<_> = file
        .diff
        .read(cx)
        .snapshot(cx)
        .hunks(&snapshot)
        .map(|hunk| hunk.buffer_range.to_point(&snapshot))
        .collect();
    if ranges.is_empty() {
        return (whole(), true);
    }
    if file.images.is_some() {
        let last = snapshot.max_point();
        ranges.push(Point::new(last.row, 0)..last);
    }
    (ranges, false)
}

/// Shows the rows of `path` that [`ranges`] says, and its images under them.
pub(crate) fn show_rows(
    diff: &Entity<SplittableEditor>,
    path: &str,
    window: &mut Window,
    cx: &mut App,
) {
    let Some((ranges, bare, key, buffer, file_diff)) = read(diff, cx, |files| {
        let file = files.get(path)?;
        let (ranges, bare) = ranges(file, cx);
        Some((
            ranges,
            bare,
            file.key.clone(),
            file.buffer.clone(),
            file.diff.clone(),
        ))
    })
    .flatten() else {
        return;
    };
    let whole = buffer.read(cx).max_point();
    diff.update(cx, |diff, cx| {
        let context = if bare {
            0
        } else {
            multibuffer_context_lines(cx)
        };
        // Updating alone would keep the rows shown before, merged with the new ones. The whole
        // file needs no removing, and a file about to fold mustn't be removed first.
        if ranges != [Point::zero()..whole] {
            diff.remove_excerpts_for_path(key.clone(), cx);
        }
        diff.update_excerpts_for_path(key, buffer, ranges, context, file_diff, cx);
    });
    show_images(diff, path, window, cx);
}

/// Shows the rows a newly built file shows, as [`show_rows`] does, before it's added.
pub(crate) fn initial_ranges(file: &ShownFile, cx: &App) -> (Vec<Range<Point>>, u32) {
    let (ranges, bare) = ranges(file, cx);
    let context = if bare {
        0
    } else {
        multibuffer_context_lines(cx)
    };
    (ranges, context)
}

/// What the expand key did to a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Expanded {
    /// It was collapsed, and now shows.
    Opened,
    /// It shows whole now.
    Whole,
    /// It shows its changes again.
    Changes,
    /// It's too large to have been loaded: the caller loads it.
    Load,
    /// Nothing: a file whose rows are fixed, as in GitHub's diff.
    Nothing,
}

/// The expand key on `path`: opens it when it's collapsed, says to load it when it's too large to
/// have been loaded, and otherwise switches between the whole file and its changes.
pub(crate) fn expand(
    diff: &Entity<SplittableEditor>,
    path: &str,
    window: &mut Window,
    cx: &mut App,
) -> Expanded {
    let Some((buffer_id, large, fixed)) = read(diff, cx, |files| {
        let file = files.get(path)?;
        // GitHub's diff has nothing more to load: its large files only open.
        let large =
            files.local.is_some() && file.collapsed.as_ref().is_some_and(Collapsed::is_large);
        Some((file.buffer_id, large, file.rows.is_some()))
    })
    .flatten() else {
        return Expanded::Nothing;
    };
    if large {
        return Expanded::Load;
    }
    let rhs = diff.read(cx).rhs_editor().clone();
    if rhs.read(cx).is_buffer_folded(buffer_id, cx) {
        rhs.update(cx, |rhs, cx| rhs.unfold_buffer(buffer_id, cx));
        return Expanded::Opened;
    }
    if fixed {
        return Expanded::Nothing;
    }
    let whole = update(diff, cx, |files, _| {
        let file = files.files.get_mut(path)?;
        file.whole = !file.whole;
        Some(file.whole)
    })
    .flatten();
    let Some(whole) = whole else {
        return Expanded::Nothing;
    };
    show_rows(diff, path, window, cx);
    if whole {
        Expanded::Whole
    } else {
        Expanded::Changes
    }
}

/// Hides changes to whitespace alone in each of `diff`'s files, or shows them again, where it
/// doesn't already. A file whose every change is to whitespace collapses while they're hidden.
/// Dropping the future stops it, leaving each file as it got to it.
pub(crate) async fn hide_whitespace(
    diff: Entity<SplittableEditor>,
    hide: bool,
    cx: &mut AsyncWindowContext,
) {
    let paths = cx
        .update(|_, cx| read(&diff, cx, ShownFiles::paths))
        .ok()
        .flatten()
        .unwrap_or_default();
    for path in paths {
        if let Err(error) = set_whitespace_hidden(&diff, &path, hide, cx).await {
            log::warn!("{path}: can't change how whitespace changes show: {error:#}");
            return;
        }
    }
}

/// [`hide_whitespace`] for one file.
async fn set_whitespace_hidden(
    diff: &Entity<SplittableEditor>,
    path: &str,
    hide: bool,
    cx: &mut AsyncWindowContext,
) -> anyhow::Result<()> {
    let found = cx.update(|_, cx| {
        read(diff, cx, |files| {
            let file = files.get(path)?;
            if file.whitespace_hidden == hide {
                return None;
            }
            let Some(Some(base)) = file.base.clone() else {
                return None;
            };
            Some((
                base,
                file.buffer.read(cx).text_snapshot(),
                file.diff.clone(),
                file.collapsed.clone(),
            ))
        })
        .flatten()
    })?;
    let Some((base, snapshot, file_diff, collapsed)) = found else {
        return Ok(());
    };
    let head = snapshot.text();
    let compared: Arc<str> = if hide {
        cx.background_spawn({
            let (base, head) = (base.clone(), head.clone());
            async move { without_whitespace_changes(&base, &head).into() }
        })
        .await
    } else {
        base.clone()
    };
    let whitespace_only = hide && *compared == *head && *base != *head;
    file_diff
        .update(cx, |file_diff, cx| {
            file_diff.set_base_text(Some(compared), snapshot, cx)
        })
        .await;

    cx.update(|window, cx| {
        let collapse = match (&collapsed, whitespace_only) {
            (None, true) => Some(Some(Collapsed::WhitespaceOnly)),
            (Some(Collapsed::WhitespaceOnly), false) => Some(None),
            _ => None,
        };
        let buffer_id = update(diff, cx, |files, headers| {
            let file = files.files.get_mut(path)?;
            // Loaded again since: that file follows the setting itself.
            if file.diff != file_diff {
                return None;
            }
            file.whitespace_hidden = hide;
            let buffer_id = file.buffer_id;
            if let Some(collapsed) = &collapse {
                file.collapsed = collapsed.clone();
                headers.set_collapsed(buffer_id, collapsed.clone());
            }
            Some(buffer_id)
        })
        .flatten();
        let Some(buffer_id) = buffer_id else {
            return;
        };
        show_rows(diff, path, window, cx);
        let rhs = diff.read(cx).rhs_editor().clone();
        match collapse {
            Some(Some(_)) => rhs.update(cx, |rhs, cx| rhs.fold_buffers([buffer_id], cx)),
            Some(None) => rhs.update(cx, |rhs, cx| rhs.unfold_buffer(buffer_id, cx)),
            None => {}
        }
    })
}

/// Moves the cursor to the diff's other side, side by side. Returns whether it moved.
pub(crate) fn switch_side(
    diff: &Entity<SplittableEditor>,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    let (Some(lhs), rhs) = (
        diff.read(cx).lhs_editor().cloned(),
        diff.read(cx).rhs_editor().clone(),
    ) else {
        return false;
    };
    let target = if lhs.focus_handle(cx).contains_focused(window, cx) {
        rhs
    } else {
        lhs
    };
    window.focus(&target.focus_handle(cx), cx);
    target.update(cx, |editor, cx| {
        editor.request_autoscroll(Autoscroll::fit(), cx)
    });
    true
}

/// Shows `diff` inline or side by side.
pub(crate) fn set_style(
    diff: &Entity<SplittableEditor>,
    style: DiffViewStyle,
    window: &mut Window,
    cx: &mut App,
) {
    diff.update(cx, |diff, cx| {
        if diff.diff_view_style() != style {
            diff.toggle_split(&ToggleSplitDiff, window, cx);
        }
    });
}

/// Keeps the image blocks right when the diff goes side by side or inline: one image a side, or
/// both on the right.
pub(crate) fn sync_side_by_side(
    diff: &Entity<SplittableEditor>,
    window: &mut Window,
    cx: &mut App,
) {
    let lhs = diff.read(cx).lhs_editor().map(|lhs| lhs.entity_id());
    // This runs on every change to the diff, so it changes nothing unless the side changed.
    let unchanged = read(diff, cx, |files| {
        files.split.store(lhs.is_some(), Ordering::Relaxed);
        files.lhs == lhs
    });
    if unchanged != Some(false) {
        return;
    }
    let paths = update(diff, cx, |files, _| {
        files.lhs = lhs;
        files.paths()
    })
    .unwrap_or_default();
    for path in paths {
        show_images(diff, &path, window, cx);
    }
}

/// How tall an image block is, in lines: room for its images up to 240 pixels high, and a label.
fn image_rows(images: &Images, line_height: Pixels) -> u32 {
    const MAX_HEIGHT: f32 = 240.;
    let tallest = [&images.old, &images.new]
        .into_iter()
        .flatten()
        .map(|side| {
            side.dimensions
                .map_or(MAX_HEIGHT, |(_, height)| height as f32)
        })
        .fold(0., f32::max)
        .clamp(16., MAX_HEIGHT);
    let line_height = f32::from(line_height).max(1.);
    (tallest / line_height).ceil() as u32 + 2
}

/// Shows `path`'s images under its last line: both on the right inline, or one a side side by
/// side. Replaces any it showed before.
fn show_images(diff: &Entity<SplittableEditor>, path: &str, window: &mut Window, cx: &mut App) {
    let Some((images, buffer, base, split)) = read(diff, cx, |files| {
        let file = files.get(path)?;
        let base = file.diff.read(cx).base_text_buffer().clone();
        Some((
            file.images.clone().zip(file.drawn.clone()),
            file.buffer.clone(),
            base,
            files.split.clone(),
        ))
    })
    .flatten() else {
        return;
    };
    let old_blocks = update(diff, cx, |files, _| {
        files
            .files
            .get_mut(path)
            .map(|file| std::mem::take(&mut file.image_blocks))
    })
    .flatten()
    .unwrap_or_default();
    remove_image_blocks(old_blocks, cx);
    let Some((images, drawn)) = images else {
        return;
    };
    let rows = image_rows(&images, window.line_height());
    let mut blocks = Vec::new();
    let rhs = diff.read(cx).rhs_editor().clone();
    let lhs = diff.read(cx).lhs_editor().cloned();
    let sides = match &lhs {
        Some(lhs) => vec![
            (rhs.clone(), buffer, Side::After),
            (lhs.clone(), base, Side::Before),
        ],
        None => vec![(rhs.clone(), buffer, Side::Both)],
    };
    for (editor, buffer, side) in sides {
        let anchor = {
            let multi_buffer = editor.read(cx).buffer().clone();
            let last_row = buffer.read(cx).max_point().row;
            multi_buffer
                .read(cx)
                .buffer_point_to_anchor(&buffer, Point::new(last_row, 0), cx)
        };
        let Some(anchor) = anchor else {
            continue;
        };
        let (drawn, split) = (drawn.clone(), split.clone());
        let ids = editor.update(cx, |editor, cx| {
            editor.insert_blocks(
                [BlockProperties {
                    // Near isn't matched with a spacer on the other side, as Above and Below
                    // are, so the two sides' blocks sit level.
                    placement: BlockPlacement::Near(anchor),
                    height: Some(rows),
                    style: BlockStyle::Fixed,
                    render: Arc::new(move |cx| {
                        let side = match (side, split.load(Ordering::Relaxed)) {
                            (Side::Both, true) => Side::After,
                            (side, _) => side,
                        };
                        render_images(&drawn, side, cx)
                    }),
                    priority: 0,
                }],
                None,
                cx,
            )
        });
        blocks.extend(ids.into_iter().map(|id| (editor.downgrade(), id)));
    }
    let lhs_id = lhs.map(|lhs| lhs.entity_id());
    update(diff, cx, |files, _| {
        // What the blocks were drawn for, which `sync_side_by_side` compares with.
        files.lhs = lhs_id;
        files.split.store(lhs_id.is_some(), Ordering::Relaxed);
        if let Some(file) = files.files.get_mut(path) {
            file.image_blocks = blocks;
        }
    });
}

fn remove_image_blocks(blocks: Vec<(WeakEntity<Editor>, CustomBlockId)>, cx: &mut App) {
    let mut by_editor: HashMap<EntityId, (WeakEntity<Editor>, HashSet<CustomBlockId>)> =
        HashMap::new();
    for (editor, id) in blocks {
        by_editor
            .entry(editor.entity_id())
            .or_insert_with(|| (editor, HashSet::new()))
            .1
            .insert(id);
    }
    for (editor, ids) in by_editor.into_values() {
        editor
            .update(cx, |editor, cx| {
                editor.remove_blocks(ids.into_iter().collect(), None, cx)
            })
            .ok();
    }
}

/// A line of text in the middle of an image's panel, in place of the image.
fn note(text: String) -> AnyElement {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .child(Label::new(text).size(LabelSize::Small).color(Color::Muted))
        .into_any_element()
}

/// Which images a block shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    Before,
    After,
    Both,
}

fn render_images(drawn: &Drawn, side: Side, cx: &mut BlockContext) -> AnyElement {
    // Every size is explicit: the block is laid out on its own, and an image left to size itself
    // would grow to its own height and spill over the lines below.
    let (height, label) = (cx.line_height * cx.height as f32, cx.line_height);
    let width = (cx.max_width - cx.em_width * 4.).max(px(120.));
    let (gap, inset) = (px(8.), px(12.));
    let panels = if side == Side::Both { 2. } else { 1. };
    let panel_width = ((width - gap * (panels - 1.)) / panels).max(px(60.));
    let image_size = size(
        (panel_width - inset).max(px(16.)),
        (height - label - inset - px(8.)).max(px(16.)),
    );
    let colors = cx.theme().colors();
    let (border, background, square) = (colors.border_variant, colors.panel_background, 8.);
    let panel = |name: &'static str, image: &DrawnSide| {
        v_flex()
            .w(panel_width)
            .flex_shrink_0()
            .child(
                div()
                    .h(label)
                    .child(Label::new(name).size(LabelSize::XSmall).color(Color::Muted)),
            )
            .child(
                div()
                    .w(image_size.width + inset)
                    .h(image_size.height + inset)
                    .p_1()
                    .overflow_hidden()
                    .rounded_sm()
                    .border_1()
                    .border_color(border)
                    .bg(checkerboard(background, square))
                    .child(match image {
                        // The box's own ratio, or the image's would decide one of its sides.
                        Some(Ok(image)) => img(image.clone())
                            .w(image_size.width)
                            .h(image_size.height)
                            .aspect_ratio(image_size.width / image_size.height)
                            .object_fit(ObjectFit::ScaleDown)
                            .into_any_element(),
                        Some(Err(error)) => note(format!("(rozed can't draw this image: {error})")),
                        None => note("(no image)".to_string()),
                    }),
            )
    };
    let before = || panel("Before", &drawn.old);
    let after = || panel("After", &drawn.new);
    h_flex()
        .id(cx.block_id)
        .w(width)
        .h(height)
        .items_start()
        .py_1()
        .gap(gap)
        .map(|this| match side {
            Side::Before => this.child(before()),
            Side::After => this.child(after()),
            Side::Both => this.child(before()).child(after()),
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Instant;

    use editor::SelectionEffects;
    use gpui::{TestAppContext, VisualTestContext};
    use project::{FakeFs, Project};
    use workspace::{MultiWorkspace, Workspace};

    use crate::file_display::tests::png;
    use crate::pr_diff::tests::fixtures::{Entry, Fixture};
    use crate::pr_diff::{self, Trust};
    use crate::pr_tab::{self, PrTab};

    const PULL: &str = "https://github.com/owner/repo/pull/1";

    /// rozed's window with a tab for [`PULL`], wide enough to show its diff side by side.
    async fn tab(cx: &mut TestAppContext) -> (Entity<PrTab>, &mut VisualTestContext) {
        tab_with_keymap(cx, |_| {}).await
    }

    /// [`tab`], with `keymap` bound after rozed's keys, as rozed binds Zed's.
    async fn tab_with_keymap(
        cx: &mut TestAppContext,
        keymap: impl FnOnce(&mut App),
    ) -> (Entity<PrTab>, &mut VisualTestContext) {
        crate::trust::tests::init_test(cx);
        cx.update(|cx| {
            editor::init(cx);
            crate::init_workspaces(cx);
            keymap(cx);
        });
        let project = Project::test(FakeFs::new(cx.executor()), [], cx).await;
        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project, window, cx));
        cx.simulate_resize(size(px(2400.), px(1400.)));
        let workspace: Entity<Workspace> =
            multi_workspace.read_with(cx, |multi_workspace, _| multi_workspace.workspace().clone());
        let tab = workspace.update_in(cx, |workspace, window, cx| {
            pr_tab::open_with(
                workspace,
                PULL.parse().unwrap(),
                Trust::Untrusted,
                window,
                cx,
                |_, _, _| {},
            )
        });
        cx.run_until_parked();
        (tab, cx)
    }

    /// Shows the local copy's diff of a pull request from `base` to `head` in `tab`, and returns
    /// the diff. The local copy lasts as long as what's returned.
    fn show(
        tab: &Entity<PrTab>,
        base: &[Entry],
        head: &[Entry],
        cx: &mut VisualTestContext,
    ) -> (Entity<SplittableEditor>, (Fixture, tempfile::TempDir)) {
        let pr_diff::tests::Copied {
            _fixture: fixture,
            _cache: cache,
            local,
            ..
        } = pr_diff::tests::copy(base, head, false);
        let local = Arc::new(local);
        tab.update_in(cx, |tab, window, cx| {
            let (workspace, link) = (tab.workspace().clone(), tab.link().clone());
            cx.spawn_in(window, async move |tab, cx| {
                let title = format!("{link}: test");
                pr_diff::show_local_copy(
                    tab,
                    workspace,
                    link,
                    title,
                    local,
                    Trust::Untrusted,
                    Instant::now(),
                    cx,
                )
                .await
                .unwrap()
            })
            .detach();
        });
        cx.run_until_parked();
        let diff = tab.read_with(cx, |tab, _| tab.diff().unwrap().clone());
        // Focus the diff, as opening a pull request does.
        cx.update(|window, cx| window.focus(&diff.focus_handle(cx), cx));
        cx.run_until_parked();
        (diff, (fixture, cache))
    }

    /// `count` lines, "`name` 1" to "`name` `count`".
    fn lines(name: &str, count: u32) -> String {
        (1..=count).map(|line| format!("{name} {line}\n")).collect()
    }

    /// [`lines`], with line `changed` changed.
    fn changed(name: &str, count: u32, changed: u32) -> String {
        lines(name, count).replace(
            &format!("{name} {changed}\n"),
            &format!("{name} {changed} changed\n"),
        )
    }

    fn file<R>(
        diff: &Entity<SplittableEditor>,
        path: &str,
        cx: &mut VisualTestContext,
        f: impl FnOnce(&ShownFile, &App) -> R,
    ) -> R {
        cx.update(|_, cx| read(diff, cx, |files| f(files.get(path).unwrap(), cx)).unwrap())
    }

    fn is_folded(diff: &Entity<SplittableEditor>, path: &str, cx: &mut VisualTestContext) -> bool {
        let buffer_id = file(diff, path, cx, |file, _| file.buffer_id);
        cx.update(|_, cx| {
            diff.read(cx)
                .rhs_editor()
                .read(cx)
                .is_buffer_folded(buffer_id, cx)
        })
    }

    /// What `path`'s header says it's collapsed for, with ⌥A as the load key.
    fn collapsed_label(
        diff: &Entity<SplittableEditor>,
        path: &str,
        cx: &mut VisualTestContext,
    ) -> Option<String> {
        let buffer_id = file(diff, path, cx, |file, _| file.buffer_id);
        cx.update(|_, cx| {
            let rhs = diff.read(cx).rhs_editor().read(cx);
            let headers = rhs.addon::<FileHeaders>().unwrap();
            headers
                .collapsed
                .get(&buffer_id)
                .map(|collapsed| collapsed.label("⌥A"))
        })
    }

    /// Puts the diff's cursor at the start of `path`, as choosing it in the file list does.
    fn select(diff: &Entity<SplittableEditor>, path: &str, cx: &mut VisualTestContext) {
        let key = file(diff, path, cx, |file, _| file.key.clone());
        cx.update(|window, cx| {
            let rhs = diff.read(cx).rhs_editor().clone();
            let location = rhs
                .read(cx)
                .buffer()
                .read(cx)
                .location_for_path(&key, cx)
                .unwrap();
            rhs.update(cx, |rhs, cx| {
                rhs.change_selections(SelectionEffects::no_scroll(), window, cx, |selections| {
                    selections.select_ranges([location..location])
                })
            });
            window.focus(&rhs.focus_handle(cx), cx);
        });
        cx.run_until_parked();
    }

    fn text(diff: &Entity<SplittableEditor>, cx: &mut VisualTestContext) -> String {
        diff.read_with(cx, |diff, cx| diff.rhs_editor().read(cx).text(cx))
    }

    #[gpui::test]
    async fn generated_lockfile_and_large_files_start_collapsed_saying_why(
        cx: &mut TestAppContext,
    ) {
        let (tab, cx) = tab(cx).await;
        let big = lines("big", 5_001);
        let (diff, _copy) = show(
            &tab,
            &[
                Entry::File("Cargo.lock", b"a = 1\n"),
                Entry::File("api.pb.go", b"package a\n"),
                Entry::File("src/lib.rs", b"fn a() {}\n"),
            ],
            &[
                Entry::File(".gitattributes", b"*.pb.go linguist-generated\n"),
                Entry::File("Cargo.lock", b"a = 2\n"),
                Entry::File("api.pb.go", b"package b\n"),
                Entry::File("data/big.txt", big.as_bytes()),
                Entry::File("src/lib.rs", b"fn b() {}\n"),
            ],
            cx,
        );

        for (path, label) in [
            ("Cargo.lock", Some("lockfile")),
            ("api.pb.go", Some("generated")),
            ("data/big.txt", Some("large: load with ⌥A")),
            ("src/lib.rs", None),
            (".gitattributes", None),
        ] {
            assert_eq!(collapsed_label(&diff, path, cx).as_deref(), label, "{path}");
            assert_eq!(is_folded(&diff, path, cx), label.is_some(), "{path}");
        }
        // The large file isn't loaded.
        assert!(!text(&diff, cx).contains("big 4999"));
        // Generated files list last.
        let paths: Vec<_> = tab.read_with(cx, |tab, _| {
            tab.files()
                .rows()
                .map(|entry| entry.path().to_string())
                .collect()
        });
        assert_eq!(
            paths,
            [
                "data/big.txt",
                "src/lib.rs",
                ".gitattributes",
                "Cargo.lock",
                "api.pb.go"
            ]
        );
    }

    #[gpui::test]
    async fn alt_a_opens_a_collapsed_file_and_loads_a_large_one(cx: &mut TestAppContext) {
        let (tab, cx) = tab(cx).await;
        let big = lines("big", 5_001);
        let (diff, _copy) = show(
            &tab,
            &[Entry::File("Cargo.lock", b"a = 1\n")],
            &[
                Entry::File("Cargo.lock", b"a = 2\n"),
                Entry::File("big.txt", big.as_bytes()),
            ],
            cx,
        );

        select(&diff, "Cargo.lock", cx);
        cx.simulate_keystrokes("alt-a");
        assert!(!is_folded(&diff, "Cargo.lock", cx));
        // Still a lockfile, as its header says.
        assert_eq!(
            collapsed_label(&diff, "Cargo.lock", cx).as_deref(),
            Some("lockfile")
        );

        select(&diff, "big.txt", cx);
        cx.simulate_keystrokes("alt-a");
        cx.run_until_parked();
        assert!(!is_folded(&diff, "big.txt", cx));
        assert_eq!(collapsed_label(&diff, "big.txt", cx), None);
        let text = text(&diff, cx);
        assert!(
            text.contains("big 1\n") && text.contains("big 5001"),
            "loaded"
        );
    }

    #[gpui::test]
    async fn alt_a_shows_the_whole_file_and_then_its_changes_again(cx: &mut TestAppContext) {
        let (tab, cx) = tab(cx).await;
        let (diff, _copy) = show(
            &tab,
            &[Entry::File("a.txt", lines("a", 100).as_bytes())],
            &[Entry::File("a.txt", changed("a", 100, 50).as_bytes())],
            cx,
        );
        let changes = text(&diff, cx);
        assert!(changes.contains("a 50 changed") && !changes.contains("a 1\n"));

        select(&diff, "a.txt", cx);
        cx.simulate_keystrokes("alt-a");
        let whole = text(&diff, cx);
        assert!(
            whole.contains("a 1\n") && whole.contains("a 100\n"),
            "{whole}"
        );
        assert!(file(&diff, "a.txt", cx, |file, _| file.whole));

        cx.simulate_keystrokes("alt-a");
        assert_eq!(text(&diff, cx), changes);
    }

    #[gpui::test]
    async fn alt_w_hides_changes_to_whitespace_alone(cx: &mut TestAppContext) {
        let (tab, cx) = tab(cx).await;
        let indented = lines("a", 20).replace("a 3\n", "    a 3\n");
        // b.txt: line 3 indented, line 15 changed.
        let b = changed("b", 20, 15).replace("b 3\n", "\tb 3\n");
        let (diff, _copy) = show(
            &tab,
            &[
                Entry::File("a.txt", lines("a", 20).as_bytes()),
                Entry::File("b.txt", lines("b", 20).as_bytes()),
            ],
            &[
                Entry::File("a.txt", indented.as_bytes()),
                Entry::File("b.txt", b.as_bytes()),
            ],
            cx,
        );
        let hunks = |path: &str, cx: &mut VisualTestContext| {
            file(&diff, path, cx, |file, cx| {
                let snapshot = file.buffer.read(cx).snapshot();
                file.diff.read(cx).snapshot(cx).hunks(&snapshot).count()
            })
        };
        assert_eq!((hunks("a.txt", cx), hunks("b.txt", cx)), (1, 2));

        cx.simulate_keystrokes("alt-w");
        cx.run_until_parked();
        assert!(tab.read_with(cx, |tab, _| tab.hides_whitespace()));
        assert_eq!((hunks("a.txt", cx), hunks("b.txt", cx)), (0, 1));
        assert!(is_folded(&diff, "a.txt", cx));
        assert_eq!(
            collapsed_label(&diff, "a.txt", cx).as_deref(),
            Some("whitespace only")
        );
        assert!(!is_folded(&diff, "b.txt", cx));

        cx.simulate_keystrokes("alt-w");
        cx.run_until_parked();
        assert_eq!((hunks("a.txt", cx), hunks("b.txt", cx)), (1, 2));
        assert!(!is_folded(&diff, "a.txt", cx));
        assert_eq!(collapsed_label(&diff, "a.txt", cx), None);
    }

    #[gpui::test]
    async fn alt_v_shows_the_diff_inline_or_side_by_side_and_tab_switches_sides(
        cx: &mut TestAppContext,
    ) {
        let (tab, cx) = tab(cx).await;
        let (diff, _copy) = show(
            &tab,
            &[Entry::File("a.txt", lines("a", 20).as_bytes())],
            &[Entry::File("a.txt", changed("a", 20, 10).as_bytes())],
            cx,
        );
        let split = |cx: &mut VisualTestContext| diff.read_with(cx, |diff, _| diff.is_split());
        assert!(split(cx));
        select(&diff, "a.txt", cx);

        let focused_side = |cx: &mut VisualTestContext| {
            cx.update(|window, cx| {
                let diff = diff.read(cx);
                if diff.rhs_editor().focus_handle(cx).is_focused(window) {
                    "right"
                } else if diff
                    .lhs_editor()
                    .is_some_and(|lhs| lhs.focus_handle(cx).is_focused(window))
                {
                    "left"
                } else {
                    "neither"
                }
            })
        };
        assert_eq!(focused_side(cx), "right");
        cx.simulate_keystrokes("tab");
        assert_eq!(focused_side(cx), "left");
        cx.simulate_keystrokes("tab");
        assert_eq!(focused_side(cx), "right");

        cx.simulate_keystrokes("alt-v");
        assert!(!split(cx));
        assert_eq!(
            tab.read_with(cx, |tab, _| tab.diff_style()),
            DiffViewStyle::Unified
        );
        // Inline, there's no other side.
        cx.simulate_keystrokes("tab");
        assert_eq!(focused_side(cx), "right");

        cx.simulate_keystrokes("alt-v");
        assert!(split(cx));
    }

    /// Zed's default macOS keymap, which binds `tab` in every editor.
    fn zeds_keymap(cx: &mut App) {
        let content = util::asset_str::<settings::SettingsAssets>(settings::DEFAULT_KEYMAP_PATH);
        let bindings = match settings::KeymapFile::load(&content, cx) {
            settings::KeymapFileLoadResult::Success { key_bindings }
            | settings::KeymapFileLoadResult::SomeFailedToLoad { key_bindings, .. } => key_bindings,
            settings::KeymapFileLoadResult::JsonParseFailure { error } => panic!("{error:#}"),
        };
        cx.bind_keys(bindings);
    }

    /// Zed binds `tab` to indent in every editor, after rozed's keys, but a read-only editor
    /// doesn't handle indenting, so `tab` reaches rozed's binding.
    #[gpui::test]
    async fn tab_switches_sides_with_zeds_keymap_too(cx: &mut TestAppContext) {
        let (tab, cx) = tab_with_keymap(cx, zeds_keymap).await;
        let (diff, _copy) = show(
            &tab,
            &[Entry::File("a.txt", lines("a", 20).as_bytes())],
            &[Entry::File("a.txt", changed("a", 20, 10).as_bytes())],
            cx,
        );
        select(&diff, "a.txt", cx);
        let lhs_focused = |cx: &mut VisualTestContext| {
            cx.update(|window, cx| {
                diff.read(cx)
                    .lhs_editor()
                    .is_some_and(|lhs| lhs.focus_handle(cx).is_focused(window))
            })
        };
        assert!(!lhs_focused(cx));
        cx.simulate_keystrokes("tab");
        assert!(lhs_focused(cx));
    }

    #[gpui::test]
    async fn the_next_diff_keeps_the_tabs_layout(cx: &mut TestAppContext) {
        let (tab, cx) = tab(cx).await;
        let base = [Entry::File("a.txt", b"a\n" as &[u8])];
        let head = [Entry::File("a.txt", b"b\n" as &[u8])];
        let (first, _copy) = show(&tab, &base, &head, cx);
        cx.simulate_keystrokes("alt-v");
        assert!(!first.read_with(cx, |diff, _| diff.is_split()));

        let (second, _copy) = show(&tab, &base, &head, cx);
        assert_ne!(first, second);
        second.read_with(cx, |diff, _| {
            assert_eq!(diff.diff_view_style(), DiffViewStyle::Unified);
            assert!(!diff.is_split());
        });
    }

    fn image_blocks(
        diff: &Entity<SplittableEditor>,
        path: &str,
        cx: &mut VisualTestContext,
    ) -> usize {
        file(diff, path, cx, |file, _| file.image_blocks.len())
    }

    #[gpui::test]
    async fn images_show_one_a_side_or_both_inline(cx: &mut TestAppContext) {
        let (tab, cx) = tab(cx).await;
        let png = png();
        let (diff, _copy) = show(
            &tab,
            &[Entry::File("logo.png", &png)],
            &[Entry::File("logo.png", &[png.as_slice(), b"\0"].concat())],
            cx,
        );
        assert!(diff.read_with(cx, |diff, _| diff.is_split()));
        assert!(file(&diff, "logo.png", cx, |file, _| file.images.is_some()));
        // Decoded, ready to draw.
        let drawn = file(&diff, "logo.png", cx, |file, _| file.drawn.clone().unwrap());
        for side in [drawn.old, drawn.new] {
            let image = side.unwrap().unwrap();
            assert_eq!(image.size(0), size(3.into(), 2.into()));
        }
        // One a side.
        assert_eq!(image_blocks(&diff, "logo.png", cx), 2);
        assert!(!is_folded(&diff, "logo.png", cx));
        let text = text(&diff, cx);
        assert!(text.contains("PNG image, 3 × 2"), "{text}");

        cx.simulate_keystrokes("alt-v");
        cx.run_until_parked();
        // Both on the right.
        assert_eq!(image_blocks(&diff, "logo.png", cx), 1);
        cx.simulate_keystrokes("alt-v");
        cx.run_until_parked();
        assert_eq!(image_blocks(&diff, "logo.png", cx), 2);
    }

    #[gpui::test]
    async fn a_rename_shows_its_old_path_and_its_changes(cx: &mut TestAppContext) {
        let (tab, cx) = tab(cx).await;
        let source = lines("line", 30);
        let (diff, _copy) = show(
            &tab,
            &[
                Entry::File("old/a.txt", source.as_bytes()),
                Entry::File("old/b.txt", source.as_bytes()),
            ],
            &[
                Entry::File("new/a.txt", changed("line", 30, 15).as_bytes()),
                Entry::File("new/b.txt", source.as_bytes()),
            ],
            cx,
        );
        let renamed_from = |path: &str, cx: &mut VisualTestContext| {
            let buffer_id = file(&diff, path, cx, |file, _| file.buffer_id);
            cx.update(|_, cx| {
                let rhs = diff.read(cx).rhs_editor().read(cx);
                let headers = rhs.addon::<FileHeaders>().unwrap();
                headers
                    .renamed_from
                    .get(&buffer_id)
                    .map(|old| old.to_string())
            })
        };
        assert_eq!(renamed_from("new/a.txt", cx).as_deref(), Some("old/a.txt"));
        assert_eq!(renamed_from("new/b.txt", cx).as_deref(), Some("old/b.txt"));
        // Its changes show; a rename alone starts collapsed, with nothing changed to show.
        assert!(text(&diff, cx).contains("line 15 changed"));
        assert!(!is_folded(&diff, "new/a.txt", cx));
        assert!(is_folded(&diff, "new/b.txt", cx));
        assert_eq!(
            collapsed_label(&diff, "new/b.txt", cx).as_deref(),
            Some("no changes")
        );
    }
}
