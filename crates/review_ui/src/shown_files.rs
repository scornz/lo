//! Each file in a pull request's diff as it shows, and what the diff's keys change about it
//! (design §5, "The diff view"): opening a collapsed file, loading a large one, showing a whole
//! file, hiding whitespace changes, the side the cursor is on, inline or side by side, and the
//! before and after of a changed image.
//!
//! The diff's right-hand editor keeps a [`ShownFiles`] addon, so each diff, GitHub's or the local
//! copy's, has its own.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use buffer_diff::BufferDiff;
use editor::display_map::{
    BlockContext, BlockPlacement, BlockProperties, BlockStyle, CustomBlockId,
};
use editor::scroll::Autoscroll;
use editor::{Addon, Editor, SplittableEditor, ToggleSplitDiff, multibuffer_context_lines};
use gpui::StyledImage as _;
use gpui::{
    AnyElement, App, AppContext as _, AsyncWindowContext, Entity, EntityId, Focusable as _,
    ObjectFit, RenderImage, Task, WeakEntity, Window, checkerboard, img, size,
};
use language::{Buffer, BufferId, OffsetRangeExt as _, Point};
use multi_buffer::PathKey;
use settings::DiffViewStyle;
use ui::{Label, LabelSize, prelude::*};

use crate::file_display::{Collapsed, ImageSide, Images, without_whitespace_changes};
use crate::image_preview;
use crate::pr_diff::{Checkout, FileHeaders, LocalPullRequest};

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
    /// `images`, decoded to draw once the file first shows, which the image blocks read.
    pub(crate) drawn: Arc<OnceLock<Drawn>>,
    /// Decodes `images`, while it does.
    decoding: Option<Task<()>>,
    /// Why the file shows collapsed, if it does or did.
    pub(crate) collapsed: Option<Collapsed>,
    /// Whether the whole file shows, rather than its changes with the lines around them.
    pub(crate) whole: bool,
    /// Whether it was folded when it was added, so a fold the reviewer changed shows.
    initial_folded: bool,
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
            drawn: Arc::default(),
            decoding: None,
            collapsed: None,
            whole: false,
            initial_folded: false,
            whitespace_hidden: false,
            image_blocks: Vec::new(),
        }
    }
}

/// One side of a changed image, decoded to a preview to draw, or why it can't be.
pub(crate) type DrawnSide = Option<Result<Arc<RenderImage>, SharedString>>;

/// The two sides of a changed image, decoded to draw. A side is `None` when the pull request adds
/// or deletes the image.
#[derive(Clone)]
pub(crate) struct Drawn {
    pub(crate) old: DrawnSide,
    pub(crate) new: DrawnSide,
}

impl Drawn {
    fn images(&self) -> impl Iterator<Item = &Arc<RenderImage>> {
        [&self.old, &self.new]
            .into_iter()
            .filter_map(|side| side.as_ref()?.as_ref().ok())
    }
}

/// Decodes one side to a preview, in the background.
fn decode_side(side: Option<ImageSide>) -> DrawnSide {
    let side = side?;
    Some(
        image_preview::preview(side.format, &side.bytes, side.dimensions)
            .map(|preview| Arc::new(RenderImage::new(vec![image::Frame::new(preview)])))
            .map_err(SharedString::from),
    )
}

/// Starts decoding `path`'s images, both sides at once in the background, unless they're decoded
/// or on their way. Their blocks draw them when they're ready.
fn start_decode(diff: &Entity<SplittableEditor>, path: &str, window: &mut Window, cx: &mut App) {
    let Some((images, drawn)) = read(diff, cx, |files| {
        let file = files.get(path)?;
        let pending = file.drawn.get().is_none() && file.decoding.is_none();
        pending.then(|| Some((file.images.clone()?, file.drawn.clone())))?
    })
    .flatten() else {
        return;
    };
    let weak_diff = diff.downgrade();
    let task = window.spawn(cx, async move |cx| {
        let Images { old, new } = images;
        let old = cx.background_spawn(async move { decode_side(old) });
        let new = cx.background_spawn(async move { decode_side(new) });
        drawn
            .set(Drawn {
                old: old.await,
                new: new.await,
            })
            .ok();
        weak_diff
            .update(cx, |diff, cx| diff.update_editors(cx, |_, cx| cx.notify()))
            .ok();
    });
    update(diff, cx, |files, _| {
        if let Some(file) = files.files.get_mut(path) {
            file.decoding = Some(task);
        }
    });
}

/// The files among `buffers` now show: their images start decoding, and a large file from
/// GitHub's diff, now open, no longer says how to open it.
pub(crate) fn opened(
    diff: &Entity<SplittableEditor>,
    buffers: &[BufferId],
    window: &mut Window,
    cx: &mut App,
) {
    let paths = update(diff, cx, |files, headers| {
        let mut images = Vec::new();
        for (path, file) in &mut files.files {
            if !buffers.contains(&file.buffer_id) {
                continue;
            }
            if let Some(Collapsed::LargePatch { generated, .. }) = &file.collapsed {
                file.collapsed = generated.map(Collapsed::Generated);
                headers.set_collapsed(file.buffer_id, file.collapsed.clone());
            }
            if file.images.is_some() {
                images.push(path.clone());
            }
        }
        images
    })
    .unwrap_or_default();
    for path in paths {
        start_decode(diff, &path, window, cx);
    }
}

/// Lets go of the textures `diff`'s images were drawn with, in every window, once the diff no
/// longer shows. Their pixels go with the diff.
pub(crate) fn release_images(
    diff: &Entity<SplittableEditor>,
    mut window: Option<&mut Window>,
    cx: &mut App,
) {
    let images: Vec<Arc<RenderImage>> = read(diff, cx, |files| {
        files
            .files
            .values()
            .filter_map(|file| file.drawn.get())
            .flat_map(|drawn| drawn.images().cloned().collect::<Vec<_>>())
            .collect()
    })
    .unwrap_or_default();
    for image in images {
        cx.drop_image(image, window.as_deref_mut());
    }
}

/// The diff's files, by path as shown.
#[derive(Default)]
pub(crate) struct ShownFiles {
    files: HashMap<String, ShownFile>,
    /// The local copy the files come from, which loads a large file on a key and keeps the clone
    /// open while the diff shows. `None` for GitHub's diff.
    pub(crate) local: Option<Arc<LocalPullRequest>>,
    /// The trusted checkout the local copy's files show from, once it's ready, which a large
    /// file loaded on a key shows from too.
    pub(crate) checkout: Option<Checkout>,
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
    let mut file = file;
    file.initial_folded = fold;
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
        if let Some(drawn) = replaced.drawn.get() {
            for image in drawn.images() {
                cx.drop_image(image.clone(), Some(window));
            }
        }
    }
    if fold {
        let rhs = diff.read(cx).rhs_editor().clone();
        rhs.update(cx, |rhs, cx| rhs.fold_buffers([buffer_id], cx));
    } else {
        // A folded file's images decode once it opens.
        start_decode(diff, &path, window, cx);
    }
    show_images(diff, &path, window, cx);
}

/// Opens or folds each of `to`'s files as the reviewer left it in `from`, the diff it replaces,
/// and shows it whole if they asked: what they chose wins over `to`'s own rules, which apply only
/// to the files they didn't touch. The file at `cursor` never folds under them.
pub(crate) fn carry_over(
    from: &Entity<SplittableEditor>,
    to: &Entity<SplittableEditor>,
    cursor: Option<&str>,
    window: &mut Window,
    cx: &mut App,
) {
    let from_rhs = from.read(cx).rhs_editor().clone();
    let chosen: HashMap<String, (bool, bool)> = read(from, cx, |files| {
        files
            .files
            .iter()
            .filter_map(|(path, file)| {
                let folded = from_rhs.read(cx).is_buffer_folded(file.buffer_id, cx);
                let touched = folded != file.initial_folded || file.whole;
                let under_cursor = cursor == Some(path.as_str()) && !folded;
                (touched || under_cursor).then(|| (path.clone(), (folded, file.whole)))
            })
            .collect()
    })
    .unwrap_or_default();
    let to_rhs = to.read(cx).rhs_editor().clone();
    for (path, (folded, whole)) in chosen {
        let Some((buffer_id, was_whole)) = read(to, cx, |files| {
            files.get(&path).map(|file| (file.buffer_id, file.whole))
        })
        .flatten() else {
            continue;
        };
        if whole != was_whole {
            update(to, cx, |files, _| {
                if let Some(file) = files.files.get_mut(&path) {
                    file.whole = whole;
                }
            });
            show_rows(to, &path, Some(folded), window, cx);
        } else if folded {
            to_rhs.update(cx, |rhs, cx| rhs.fold_buffers([buffer_id], cx));
        } else {
            to_rhs.update(cx, |rhs, cx| rhs.unfold_buffer(buffer_id, cx));
        }
    }
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
/// It ends folded when `fold` says so, or as it was when `fold` is `None`.
pub(crate) fn show_rows(
    diff: &Entity<SplittableEditor>,
    path: &str,
    fold: Option<bool>,
    window: &mut Window,
    cx: &mut App,
) {
    let Some((ranges, bare, key, buffer, buffer_id, file_diff)) = read(diff, cx, |files| {
        let file = files.get(path)?;
        let (ranges, bare) = ranges(file, cx);
        Some((
            ranges,
            bare,
            file.key.clone(),
            file.buffer.clone(),
            file.buffer_id,
            file.diff.clone(),
        ))
    })
    .flatten() else {
        return;
    };
    let rhs = diff.read(cx).rhs_editor().clone();
    let fold = fold.unwrap_or_else(|| rhs.read(cx).is_buffer_folded(buffer_id, cx));
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
    // Zed unfolds a file whose excerpts were removed once it hears of it, after this, so the fold
    // the file ends with comes after that.
    let rhs = rhs.downgrade();
    window.defer(cx, move |_, cx| {
        rhs.update(cx, |rhs, cx| {
            if fold {
                rhs.fold_buffers([buffer_id], cx);
            } else {
                rhs.unfold_buffer(buffer_id, cx);
            }
        })
        .ok();
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
        let large = file.collapsed.as_ref().is_some_and(Collapsed::is_large);
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
    show_rows(diff, path, Some(false), window, cx);
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
        let current = update(diff, cx, |files, headers| {
            let file = files.files.get_mut(path)?;
            // Loaded again since: that file follows the setting itself.
            if file.diff != file_diff {
                return None;
            }
            file.whitespace_hidden = hide;
            if let Some(collapsed) = &collapse {
                file.collapsed = collapsed.clone();
                headers.set_collapsed(file.buffer_id, collapsed.clone());
            }
            Some(())
        })
        .flatten();
        if current.is_none() {
            return;
        }
        // A file that's whitespace only folds, one that no longer is opens, and any other keeps
        // its fold.
        let fold = collapse.map(|collapsed| collapsed.is_some());
        show_rows(diff, path, fold, window, cx);
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

/// How tall an image block is, in lines of `line_height`, the editor's: room for its images up to
/// 240 pixels high, and a label.
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
            file.images
                .clone()
                .map(|images| (images, file.drawn.clone())),
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
    let rhs_line_height = {
        let rhs = diff.read(cx).rhs_editor().clone();
        rhs.update(cx, |rhs, cx| {
            rhs.style(cx).text.line_height_in_pixels(window.rem_size())
        })
    };
    let rows = image_rows(&images, rhs_line_height);
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
                        match drawn.get() {
                            Some(drawn) => render_images(drawn, side, cx),
                            None => render_decoding(cx),
                        }
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

/// An image block's place, kept while its images decode.
fn render_decoding(cx: &mut BlockContext) -> AnyElement {
    let height = cx.line_height * cx.height as f32;
    let width = (cx.max_width - cx.em_width * 4.).max(px(120.));
    div()
        .id(cx.block_id)
        .w(width)
        .h(height)
        .child(note("(decoding the images…)".to_string()))
        .into_any_element()
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
                        Some(Err(error)) if error.starts_with("too large") => {
                            note(format!("({error})"))
                        }
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
    use project::{FakeFs, Project, ProjectPath};
    use util::rel_path::RelPath;
    use workspace::{MultiWorkspace, Workspace};

    use crate::file_display::tests::png;
    use crate::image_preview;
    use crate::pr_diff::tests::fixtures::{Entry, Fixture};
    use crate::pr_diff::{self, Trust};
    use crate::pr_tab::{self, PrTab};
    use gpui::size;

    const PULL: &str = "https://github.com/owner/repo/pull/1";

    /// rozed's window with a tab for [`PULL`], wide enough to show its diff side by side.
    async fn tab(cx: &mut TestAppContext) -> (Entity<PrTab>, &mut VisualTestContext) {
        crate::trust::tests::init_test(cx);
        cx.update(|cx| {
            editor::init(cx);
            crate::init_workspaces(cx);
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
        show_copy(tab, pr_diff::tests::copy(base, head, false), cx)
    }

    /// [`show`], from `copied`.
    fn show_copy(
        tab: &Entity<PrTab>,
        copied: pr_diff::tests::Copied,
        cx: &mut VisualTestContext,
    ) -> (Entity<SplittableEditor>, (Fixture, tempfile::TempDir)) {
        let pr_diff::tests::Copied {
            _fixture: fixture,
            _cache: cache,
            local,
            ..
        } = copied;
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
        cx.update(|window, cx| put_cursor(diff, path, window, cx));
        cx.run_until_parked();
    }

    /// [`select`], leaving whatever it starts to run later.
    fn put_cursor(diff: &Entity<SplittableEditor>, path: &str, window: &mut Window, cx: &mut App) {
        let key = read(diff, cx, |files| files.get(path).unwrap().key.clone()).unwrap();
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
    }

    /// Presses `action`'s key in `diff` with the cursor in `path`, leaving what it starts to run
    /// later, as a second key pressed before the first one's work is done would.
    fn press_in(
        diff: &Entity<SplittableEditor>,
        path: &str,
        action: impl gpui::Action,
        cx: &mut VisualTestContext,
    ) {
        cx.update(|window, cx| {
            put_cursor(diff, path, window, cx);
            window.dispatch_action(Box::new(action), cx);
        });
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
    async fn alt_a_loads_each_large_file_it_is_pressed_on(cx: &mut TestAppContext) {
        let (tab, cx) = tab(cx).await;
        let (big, huge) = (lines("big", 5_001), lines("huge", 5_001));
        let (diff, _copy) = show(
            &tab,
            &[],
            &[
                Entry::File("big.txt", big.as_bytes()),
                Entry::File("huge.txt", huge.as_bytes()),
            ],
            cx,
        );
        // The second key comes before the first file has loaded.
        press_in(&diff, "big.txt", pr_tab::ExpandFile, cx);
        press_in(&diff, "huge.txt", pr_tab::ExpandFile, cx);
        cx.run_until_parked();
        for path in ["big.txt", "huge.txt"] {
            assert!(!is_folded(&diff, path, cx), "{path}");
            assert_eq!(collapsed_label(&diff, path, cx), None, "{path}");
        }
        let text = text(&diff, cx);
        assert!(text.contains("big 5001") && text.contains("huge 5001"));
    }

    #[gpui::test]
    async fn whitespace_hidden_while_a_large_file_loads_is_hidden_in_it(cx: &mut TestAppContext) {
        let (tab, cx) = tab(cx).await;
        let base = lines("big", 5_001);
        let head = base.replace('\n', " \n");
        let (diff, _copy) = show(
            &tab,
            &[Entry::File("big.txt", base.as_bytes())],
            &[Entry::File("big.txt", head.as_bytes())],
            cx,
        );
        press_in(&diff, "big.txt", pr_tab::ExpandFile, cx);
        press_in(&diff, "big.txt", pr_tab::ToggleWhitespace, cx);
        cx.run_until_parked();
        assert!(file(&diff, "big.txt", cx, |file, _| file.whitespace_hidden));
        assert_eq!(
            collapsed_label(&diff, "big.txt", cx).as_deref(),
            Some("whitespace only")
        );
    }

    #[gpui::test]
    async fn a_file_with_a_line_over_a_mebibyte_is_too_large_to_show(cx: &mut TestAppContext) {
        let (tab, cx) = tab(cx).await;
        let long = format!("{}\n", "x".repeat(crate::file_display::MAX_LINE_BYTES + 1));
        let (diff, _copy) = show(&tab, &[], &[Entry::File("data.txt", long.as_bytes())], cx);
        assert_eq!(
            collapsed_label(&diff, "data.txt", cx).as_deref(),
            Some("large: load with ⌥A")
        );

        select(&diff, "data.txt", cx);
        cx.simulate_keystrokes("alt-a");
        cx.run_until_parked();
        assert_eq!(
            collapsed_label(&diff, "data.txt", cx).as_deref(),
            Some("too large to show")
        );
        assert!(is_folded(&diff, "data.txt", cx));
        // Opened, it says why, and the key does nothing more.
        cx.simulate_keystrokes("alt-a");
        cx.run_until_parked();
        cx.simulate_keystrokes("alt-a");
        cx.run_until_parked();
        let text = text(&diff, cx);
        assert!(
            text.contains("(too large to show: it has a line over 1 MiB long)")
                && !text.contains("xxx"),
            "{}",
            &text[..text.len().min(200)]
        );
    }

    #[gpui::test]
    async fn a_large_file_loaded_on_a_key_shows_from_the_trusted_checkout(cx: &mut TestAppContext) {
        let (tab, cx) = tab(cx).await;
        let big = lines("big", 5_001);
        let copied = pr_diff::tests::copy(&[], &[Entry::File("big.txt", big.as_bytes())], true);
        let dir = copied.checkout.clone().unwrap();
        let (diff, _copy) = show_copy(&tab, copied, cx);
        // The checkout, opened in the project as a trusted pull request's is.
        let project = tab.read_with(cx, |tab, cx| {
            tab.workspace()
                .upgrade()
                .unwrap()
                .read(cx)
                .project()
                .clone()
        });
        let fs = project.read_with(cx, |project, _| project.fs().as_fake());
        fs.insert_tree(&dir, serde_json::json!({ "big.txt": big }))
            .await;
        let (worktree, _) = project
            .update(cx, |project, cx| {
                project.find_or_create_worktree(&dir, true, cx)
            })
            .await
            .unwrap();
        let worktree_id = worktree.read_with(cx, |worktree, _| worktree.id());
        cx.update(|_, cx| {
            let rhs = diff.read(cx).rhs_editor().clone();
            rhs.update(cx, |rhs, _| {
                rhs.addon_mut::<ShownFiles>().unwrap().checkout =
                    Some(pr_diff::Checkout { dir, worktree_id });
            })
        });

        select(&diff, "big.txt", cx);
        cx.simulate_keystrokes("alt-a");
        cx.run_until_parked();
        let shown = file(&diff, "big.txt", cx, |file, _| file.buffer.clone());
        let opened = project.read_with(cx, |project, cx| {
            project.get_open_buffer(
                &ProjectPath {
                    worktree_id,
                    path: RelPath::from_unix_str("big.txt").unwrap().into_arc(),
                },
                cx,
            )
        });
        assert_eq!(opened, Some(shown), "not the checkout's buffer");
    }

    #[gpui::test]
    async fn a_large_file_in_githubs_diff_says_its_key_opens_it_until_it_does(
        cx: &mut TestAppContext,
    ) {
        let (tab, cx) = tab(cx).await;
        let large = |path, name| github::files::PullFile {
            additions: 6_000,
            changes: 6_001,
            ..github_file(path, name, 20)
        };
        let diff = show_github(
            &tab,
            vec![large("Cargo.lock", "lock"), large("big.txt", "big")],
            cx,
        );
        assert_eq!(
            collapsed_label(&diff, "Cargo.lock", cx).as_deref(),
            Some("lockfile, large: open with ⌥A")
        );
        assert_eq!(
            collapsed_label(&diff, "big.txt", cx).as_deref(),
            Some("large: open with ⌥A")
        );

        for path in ["Cargo.lock", "big.txt"] {
            assert!(is_folded(&diff, path, cx), "{path}");
            select(&diff, path, cx);
            cx.simulate_keystrokes("alt-a");
            cx.run_until_parked();
            assert!(!is_folded(&diff, path, cx), "{path}");
        }
        assert_eq!(
            collapsed_label(&diff, "Cargo.lock", cx).as_deref(),
            Some("lockfile")
        );
        assert_eq!(collapsed_label(&diff, "big.txt", cx), None);
    }

    #[gpui::test]
    async fn alt_a_in_the_file_list_acts_on_its_selected_file(cx: &mut TestAppContext) {
        let (tab, cx) = tab(cx).await;
        let (diff, _copy) = show(
            &tab,
            &[
                Entry::File("Cargo.lock", b"a = 1\n"),
                Entry::File("b.txt", lines("b", 20).as_bytes()),
            ],
            &[
                Entry::File("Cargo.lock", b"a = 2\n"),
                Entry::File("b.txt", changed("b", 20, 10).as_bytes()),
            ],
            cx,
        );
        let list = tab.read_with(cx, |tab, _| tab.file_list_focus_handle().clone());
        let selected = |cx: &mut VisualTestContext| {
            tab.read_with(cx, |tab, _| {
                tab.files().selected().map(|entry| entry.path().to_string())
            })
        };
        // The list selects the lockfile, and the diff's cursor then moves to b.txt.
        cx.update(|window, cx| window.focus(&list, cx));
        for _ in 0..2 {
            if selected(cx).as_deref() != Some("Cargo.lock") {
                cx.simulate_keystrokes("j");
            }
        }
        assert_eq!(selected(cx).as_deref(), Some("Cargo.lock"));
        select(&diff, "b.txt", cx);
        cx.update(|window, cx| window.focus(&list, cx));
        cx.run_until_parked();

        cx.simulate_keystrokes("alt-a");
        assert!(!is_folded(&diff, "Cargo.lock", cx));
        assert!(!file(&diff, "b.txt", cx, |file, _| file.whole));
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

    /// GitHub's entry for `path`, holding [`lines`] of `name` with line `line` changed, with
    /// three lines around it.
    fn github_file(path: &str, name: &str, line: u32) -> github::files::PullFile {
        let start = line - 3;
        let mut patch = format!("@@ -{start},7 +{start},7 @@");
        for unchanged in start..line {
            patch.push_str(&format!("\n {name} {unchanged}"));
        }
        patch.push_str(&format!("\n-{name} {line}\n+{name} {line} changed"));
        for unchanged in line + 1..=line + 3 {
            patch.push_str(&format!("\n {name} {unchanged}"));
        }
        github::files::PullFile {
            filename: path.to_string(),
            status: github::files::FileStatus::Modified,
            additions: 1,
            deletions: 1,
            changes: 2,
            patch: Some(patch),
            previous_filename: None,
            blob_url: None,
            contents_url: None,
        }
    }

    /// Shows GitHub's diff of `files` in `tab`, and returns it.
    fn show_github(
        tab: &Entity<PrTab>,
        files: Vec<github::files::PullFile>,
        cx: &mut VisualTestContext,
    ) -> Entity<SplittableEditor> {
        tab.update_in(cx, |tab, window, cx| {
            let source = crate::api_diff::GithubDiff {
                link: PULL.parse().unwrap(),
                title: format!("{PULL}: test"),
                repo_name: "repo".into(),
                head: None,
            };
            tab.show_github_files(source, files, window, cx)
        });
        cx.run_until_parked();
        let diff = tab.read_with(cx, |tab, _| tab.diff().unwrap().clone());
        cx.update(|window, cx| window.focus(&diff.focus_handle(cx), cx));
        cx.run_until_parked();
        diff
    }

    #[gpui::test]
    async fn the_local_copys_diff_keeps_what_the_reviewer_opened_and_folded(
        cx: &mut TestAppContext,
    ) {
        let (tab, cx) = tab(cx).await;
        let names = [
            ("Cargo.lock", "lock"),
            ("api.pb.go", "api"),
            ("gen.pb.go", "gen"),
            ("b.txt", "b"),
        ];
        let github = show_github(
            &tab,
            names
                .iter()
                .map(|(path, name)| github_file(path, name, 20))
                .collect(),
            cx,
        );
        let folded = |diff: &Entity<SplittableEditor>, cx: &mut VisualTestContext| {
            names.map(|(path, _)| is_folded(diff, path, cx))
        };
        // GitHub's diff knows lockfiles by name, but not what .gitattributes says.
        assert_eq!(folded(&github, cx), [true, false, false, false]);
        // The reviewer opens the lockfile, and reads api.pb.go.
        select(&github, "Cargo.lock", cx);
        cx.simulate_keystrokes("alt-a");
        select(&github, "api.pb.go", cx);

        let base: Vec<String> = names.iter().map(|(_, name)| lines(name, 40)).collect();
        let head: Vec<String> = names
            .iter()
            .map(|(_, name)| changed(name, 40, 20))
            .collect();
        let entries = |texts: &[String]| -> Vec<(&str, Vec<u8>)> {
            names
                .iter()
                .zip(texts)
                .map(|((path, _), text)| (*path, text.clone().into_bytes()))
                .collect()
        };
        let (base, head) = (entries(&base), entries(&head));
        let mut head: Vec<Entry> = head
            .iter()
            .map(|(path, text)| Entry::File(path, text))
            .collect();
        head.push(Entry::File(
            ".gitattributes",
            b"*.pb.go linguist-generated\n",
        ));
        let base: Vec<Entry> = base
            .iter()
            .map(|(path, text)| Entry::File(path, text))
            .collect();
        let (local, _copy) = show(&tab, &base, &head, cx);
        assert_ne!(local, github);

        // The lockfile stays open, and api.pb.go, with the cursor in it, doesn't fold; gen.pb.go,
        // which the reviewer didn't touch, follows .gitattributes.
        assert_eq!(folded(&local, cx), [false, false, true, false]);
    }

    #[gpui::test]
    async fn alt_w_keeps_the_left_sides_lines_without_a_final_newline(cx: &mut TestAppContext) {
        let (tab, cx) = tab(cx).await;
        let base = "one\ntwo\nthree\nfour\n";
        let (diff, _copy) = show(
            &tab,
            &[Entry::File("a.txt", base.as_bytes())],
            &[Entry::File("a.txt", b"one\n  two\nthree")],
            cx,
        );
        cx.simulate_keystrokes("alt-w");
        cx.run_until_parked();

        let left = file(&diff, "a.txt", cx, |file, cx| {
            file.diff.read(cx).base_text_buffer().read(cx).text()
        });
        // Two's indentation reads as the head's; four is still the base's own line, numbered 4.
        assert_eq!(left, "one\n  two\nthree\nfour\n");
        assert_eq!(left.lines().count(), base.lines().count());
        let lhs = diff.read_with(cx, |diff, cx| diff.lhs_editor().unwrap().read(cx).text(cx));
        assert!(lhs.lines().any(|line| line == "four"), "{lhs:?}");
        assert!(!lhs.contains("threefour"), "{lhs:?}");
    }

    #[gpui::test]
    async fn alt_w_still_shows_a_change_to_a_line_separator(cx: &mut TestAppContext) {
        let (tab, cx) = tab(cx).await;
        let (diff, _copy) = show(
            &tab,
            &[Entry::File(
                "a.js",
                "// TODO: call cleanup() later\nrun();\n".as_bytes(),
            )],
            &[Entry::File(
                "a.js",
                "// TODO: call\u{2028}cleanup() later\nrun();\n".as_bytes(),
            )],
            cx,
        );
        cx.simulate_keystrokes("alt-w");
        cx.run_until_parked();
        assert!(!is_folded(&diff, "a.js", cx));
        assert_eq!(collapsed_label(&diff, "a.js", cx), None);
    }

    #[gpui::test]
    async fn alt_w_keeps_collapsed_and_folded_files_folded(cx: &mut TestAppContext) {
        let (tab, cx) = tab(cx).await;
        let (diff, _copy) = show(
            &tab,
            &[
                Entry::File("Cargo.lock", lines("lock", 40).as_bytes()),
                Entry::File("api.pb.go", lines("gen", 40).as_bytes()),
                Entry::File("a.txt", lines("a", 40).as_bytes()),
                Entry::File("b.txt", lines("b", 40).as_bytes()),
            ],
            &[
                Entry::File(".gitattributes", b"*.pb.go linguist-generated\n"),
                Entry::File(
                    "Cargo.lock",
                    changed("lock", 40, 20)
                        .replace("lock 3\n", "  lock 3\n")
                        .as_bytes(),
                ),
                Entry::File("api.pb.go", changed("gen", 40, 20).as_bytes()),
                Entry::File(
                    "a.txt",
                    changed("a", 40, 20).replace("a 3\n", "  a 3\n").as_bytes(),
                ),
                Entry::File("b.txt", changed("b", 40, 30).as_bytes()),
            ],
            cx,
        );
        // The reviewer folds a.txt by hand.
        let a = file(&diff, "a.txt", cx, |file, _| file.buffer_id);
        diff.update(cx, |diff, cx| {
            diff.rhs_editor()
                .update(cx, |rhs, cx| rhs.fold_buffers([a], cx))
        });
        cx.run_until_parked();
        let folded = |cx: &mut VisualTestContext| {
            ["Cargo.lock", "api.pb.go", "a.txt", "b.txt"].map(|path| is_folded(&diff, path, cx))
        };
        assert_eq!(folded(cx), [true, true, true, false]);

        for press in 1..=2 {
            select(&diff, "b.txt", cx);
            cx.simulate_keystrokes("alt-w");
            cx.run_until_parked();
            assert_eq!(
                folded(cx),
                [true, true, true, false],
                "after {press} presses"
            );
        }
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

    /// Zed binds `tab` to indent in every editor, and rozed binds it after.
    #[gpui::test]
    async fn tab_switches_sides_with_zeds_keymap_too(cx: &mut TestAppContext) {
        let (tab, cx) = tab(cx).await;
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
        let drawn = file(&diff, "logo.png", cx, |file, _| {
            file.drawn.get().cloned().unwrap()
        });
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

    /// The decoded preview of `path`'s new side.
    fn new_preview(
        diff: &Entity<SplittableEditor>,
        path: &str,
        cx: &mut VisualTestContext,
    ) -> Result<Arc<RenderImage>, SharedString> {
        file(diff, path, cx, |file, _| {
            file.drawn
                .get()
                .expect("decoded")
                .new
                .clone()
                .expect("a new side")
        })
    }

    #[gpui::test]
    async fn a_pull_requests_svg_draws_nothing_from_disk(cx: &mut TestAppContext) {
        let (tab, cx) = tab(cx).await;
        let dir = tempfile::TempDir::new().unwrap();
        let secret = dir.path().join("secret.png");
        std::fs::write(&secret, image_preview::tests::png_of(20, 20)).unwrap();
        let svg = |href: &str| {
            format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="20" height="20"><image xlink:href="{href}" width="20" height="20"/></svg>"#
            )
        };
        let (local, zero) = (svg(secret.to_str().unwrap()), svg("/dev/zero"));
        let started = Instant::now();
        let (diff, _copy) = show(
            &tab,
            &[],
            &[
                Entry::File("local.svg", local.as_bytes()),
                Entry::File("zero.svg", zero.as_bytes()),
            ],
            cx,
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        for path in ["local.svg", "zero.svg"] {
            let preview = new_preview(&diff, path, cx).unwrap();
            let bytes = preview.as_bytes(0).unwrap();
            // The PNG on disk is opaque red; nothing of it is drawn.
            assert!(
                bytes.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 0),
                "{path}"
            );
        }
    }

    #[gpui::test]
    async fn images_that_declare_huge_canvases_are_never_decoded_in_full(cx: &mut TestAppContext) {
        let (tab, cx) = tab(cx).await;
        // 2048 × 2048, six frames, under 200 bytes; and a PNG header of 16385 × 64.
        let gif = image_preview::tests::gif_of(2048, 2048, 6);
        let wide = image_preview::tests::png_of(16385, 64);
        let (diff, _copy) = show(
            &tab,
            &[],
            &[
                Entry::File("anim.gif", &gif),
                Entry::File("wide.png", &wide),
            ],
            cx,
        );

        let gif = new_preview(&diff, "anim.gif", cx).unwrap();
        assert_eq!(gif.frame_count(), 1);
        assert_eq!(
            gif.size(0),
            size(
                (image_preview::PREVIEW_SIDE as i32).into(),
                (image_preview::PREVIEW_SIDE as i32).into()
            )
        );
        assert!(text(&diff, cx).contains("GIF image, 2048 × 2048, animated, 6 frames"));
        assert_eq!(
            new_preview(&diff, "wide.png", cx).unwrap_err(),
            "too large to preview: 16385 × 64"
        );
    }

    #[gpui::test]
    async fn a_collapsed_files_images_decode_only_once_it_opens(cx: &mut TestAppContext) {
        let (tab, cx) = tab(cx).await;
        let (diff, _copy) = show(
            &tab,
            &[],
            &[
                Entry::File(".gitattributes", b"gen/** linguist-generated\n"),
                Entry::File("gen/chart.png", &png()),
            ],
            cx,
        );
        assert!(is_folded(&diff, "gen/chart.png", cx));
        assert!(file(&diff, "gen/chart.png", cx, |file, _| file
            .drawn
            .get()
            .is_none()));

        select(&diff, "gen/chart.png", cx);
        cx.simulate_keystrokes("alt-a");
        cx.run_until_parked();
        assert!(!is_folded(&diff, "gen/chart.png", cx));
        assert!(new_preview(&diff, "gen/chart.png", cx).is_ok());
    }

    #[gpui::test]
    async fn a_diff_replaced_lets_its_images_go(cx: &mut TestAppContext) {
        let (tab, cx) = tab(cx).await;
        let png = png();
        let base = [Entry::File("logo.png", png.as_slice())];
        let changed = [png.as_slice(), b"\0"].concat();
        let head = [Entry::File("logo.png", changed.as_slice())];
        let (first, first_copy) = show(&tab, &base, &head, cx);
        let first_images: Vec<_> = file(&first, "logo.png", cx, |file, _| {
            file.drawn
                .get()
                .unwrap()
                .images()
                .map(Arc::downgrade)
                .collect()
        });
        assert_eq!(first_images.len(), 2);

        let (second, _second_copy) = show(&tab, &base, &head, cx);
        let first_diff = first.downgrade();
        drop((first, first_copy));
        // The last frame drawn holds what it drew until the next.
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        assert!(
            first_diff.upgrade().is_none(),
            "the first diff is still held"
        );
        assert!(
            first_images.iter().all(|image| image.upgrade().is_none()),
            "the first diff's images are still held"
        );
        let live = file(&second, "logo.png", cx, |file, _| {
            file.drawn.get().unwrap().images().count()
        });
        assert_eq!(live, 2);
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
