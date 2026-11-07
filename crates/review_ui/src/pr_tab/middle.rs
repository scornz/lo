//! The middle of the tab: the pull request's diff, or another view in its place, and the keys that
//! move through the diff and change how it shows (design §5 "The diff view", §10 "Window").

use editor::Direction;
use gpui::{AnyElement, App, Context, Div, EntityId, Focusable as _, KeyBinding, Window, actions};
use review::revisions::Revisions;
use settings::DiffViewStyle;
use ui::prelude::*;

use super::keys::{DIFF_CONTEXT, KEY_CONTEXT, diff_and_file_list, reading};
use super::{PartView, PrTab, Status};
use crate::diff_cursor;
use crate::shown_files::{self, Expanded};
use crate::viewed_marks::changed_since_viewed;

actions!(
    review_ui,
    [
        /// Side by side, moves the cursor to the diff's other side.
        SwitchSide,
        /// Shows the diff inline or side by side.
        ToggleDiffLayout,
        /// Hides changes to whitespace alone, or shows them again.
        ToggleWhitespace,
        /// Opens the current file when it's collapsed, loads it when it's too large to have
        /// loaded, and otherwise shows the whole file or only its changes.
        ExpandFile,
        /// Moves to the next file in the diff.
        NextFile,
        /// Moves to the previous file in the diff.
        PreviousFile,
        /// Moves to the next change, in whichever file it is.
        NextChange,
        /// Moves to the previous change, in whichever file it is.
        PreviousChange,
    ]
);

/// What the middle of the tab shows: the diff, or a view in its place, such as the timeline, until
/// it gives the place back. The diff's keys act on it only while it shows.
#[derive(Default)]
pub enum Middle {
    /// The pull request's diff.
    #[default]
    Diff,
    /// A view in the diff's place, which takes focus when it shows.
    View(PartView),
}

impl Middle {
    /// Whether the middle shows the diff.
    pub fn shows_diff(&self) -> bool {
        matches!(self, Self::Diff)
    }

    /// Whether the middle shows `entity`'s view.
    pub fn shows(&self, entity: EntityId) -> bool {
        match self {
            Self::Diff => false,
            Self::View(view) => view.shows(entity),
        }
    }
}

/// The diff's keys.
pub(super) fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("tab", SwitchSide, Some(DIFF_CONTEXT)),
        // Back and forward through where the diff's cursor has been (design §10), beside Zed's
        // ctrl-- and ctrl-_.
        KeyBinding::new("ctrl-o", workspace::pane::GoBack, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-i", workspace::pane::GoForward, Some(KEY_CONTEXT)),
    ]);
    cx.bind_keys(
        [
            diff_and_file_list("alt-v", ToggleDiffLayout),
            diff_and_file_list("alt-w", ToggleWhitespace),
            diff_and_file_list("alt-a", ExpandFile),
            reading("] f", NextFile),
            reading("[ f", PreviousFile),
            reading("] c", NextChange),
            reading("[ c", PreviousChange),
        ]
        .concat(),
    );
}

/// The tab runs the diff's actions wherever their keys work, while the diff shows.
pub(super) fn on_actions(root: Div, tab: &PrTab, cx: &Context<PrTab>) -> Div {
    root.when(tab.middle.shows_diff(), |root| {
        root.on_action(cx.listener(PrTab::switch_side))
            .on_action(cx.listener(PrTab::toggle_diff_layout))
            .on_action(cx.listener(PrTab::toggle_whitespace))
            .on_action(cx.listener(PrTab::expand_file))
            .on_action(cx.listener(PrTab::next_file))
            .on_action(cx.listener(PrTab::previous_file))
            .on_action(cx.listener(PrTab::next_change))
            .on_action(cx.listener(PrTab::previous_change))
    })
}

impl PrTab {
    /// What the middle of the tab shows.
    pub fn middle(&self) -> &Middle {
        &self.middle
    }

    /// Shows `middle` in the middle of the tab, in place of what it showed. When anything in the
    /// tab has focus, focus moves to what now shows.
    pub fn show_middle(&mut self, middle: Middle, window: &mut Window, cx: &mut Context<Self>) {
        let focused = self.focus_handle.contains_focused(window, cx);
        self.middle = middle;
        if focused {
            self.focus_diff(window, cx);
        }
        cx.notify();
    }

    /// Focuses what the middle shows: the diff, once there is one, or the view in its place.
    /// Returns whether anything took focus.
    pub(super) fn focus_middle(&self, window: &mut Window, cx: &mut App) -> bool {
        match &self.middle {
            Middle::Diff => {
                let Some(diff) = &self.diff else {
                    return false;
                };
                window.focus(&diff.focus_handle(cx), cx);
            }
            Middle::View(view) => window.focus(&view.focus, cx),
        }
        true
    }

    /// The middle of the tab, between the file list and the threads panel: what
    /// [`Self::middle`] says.
    pub(super) fn render_middle(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let middle = v_flex().flex_1().min_w_0().h_full();
        match &self.middle {
            Middle::Diff => middle
                .children(self.render_changed_since_viewed(cx))
                .child(self.render_diff(cx)),
            Middle::View(view) => middle.child(view.view.clone()),
        }
    }

    /// The diff, or why the pull request couldn't open.
    fn render_diff(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let diff: AnyElement = match (&self.diff, &self.status) {
            (_, Status::Failed(error)) => v_flex()
                .size_full()
                .p_4()
                .gap_2()
                .child(Label::new("Lo couldn't open this pull request").color(Color::Error))
                .child(Label::new(error.clone()).size(LabelSize::Small))
                .child(h_flex().gap_2().children(self.render_actions(cx)))
                .into_any_element(),
            (Some(diff), _) => diff.clone().into_any_element(),
            (None, _) => div().size_full().into_any_element(),
        };
        div().flex_1().min_h_0().w_full().child(diff)
    }

    /// `tab`: side by side, moves the cursor to the diff's other side.
    fn switch_side(&mut self, _: &SwitchSide, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(diff) = &self.diff {
            shown_files::switch_side(diff, window, cx);
        }
    }

    /// Shows the tab's diffs inline or side by side.
    fn toggle_diff_layout(
        &mut self,
        _: &ToggleDiffLayout,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.diff_style = match self.diff_style {
            DiffViewStyle::Split => DiffViewStyle::Unified,
            DiffViewStyle::Unified => DiffViewStyle::Split,
        };
        self.apply_diff_style(window, cx);
        cx.notify();
    }

    /// Hides changes to whitespace alone, or shows them again.
    fn toggle_whitespace(
        &mut self,
        _: &ToggleWhitespace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.hide_whitespace = !self.hide_whitespace;
        self.sync_whitespace(window, cx);
        cx.notify();
    }

    /// Opens, loads or shows the whole of the file selected in the file list, when it has focus,
    /// or else the file the diff shows, which `e` acts on too.
    fn expand_file(&mut self, _: &ExpandFile, window: &mut Window, cx: &mut Context<Self>) {
        let Some(diff) = self.diff.clone() else {
            return;
        };
        let in_file_list = self.file_list_focus.contains_focused(window, cx);
        let selected = self
            .files
            .selected()
            .filter(|_| in_file_list)
            .map(|entry| entry.path().to_string());
        let Some(path) = selected.or_else(|| self.shown_file(cx).map(|(_, path)| path)) else {
            return;
        };
        if shown_files::expand(&diff, &path, window, cx) == Expanded::Load {
            self.load_file(diff, path, window, cx);
        }
    }

    /// What the diff says about the file it shows when GitHub says it changed since it was viewed:
    /// the file, and since when, from the revision this Mac recorded when it was marked viewed.
    pub fn changed_since_viewed(&self, cx: &mut App) -> Option<(String, String)> {
        let (_, path) = self.shown_file(cx)?;
        let github_path = self.github_path(&path)?;
        let none = Revisions::default();
        let note = changed_since_viewed(
            self.revisions.as_ref().unwrap_or(&none),
            self.marks.state(&github_path),
            self.viewed_at.get(&github_path).map(String::as_str),
        )?;
        Some((path, note))
    }

    /// `]f` and `[f`: the next or previous file in review order, which is the diff's order,
    /// among the files in the diff. They stop at the ends.
    fn move_file(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some((current, _)) = self.shown_file(cx) else {
            return;
        };
        let positions: Vec<usize> = match forward {
            true => (current + 1..self.files.len()).collect(),
            false => (0..current).rev().collect(),
        };
        let target = positions
            .into_iter()
            .filter_map(|position| self.files.at_review_position(position))
            .map(|entry| entry.path().to_string())
            .find(|path| self.location_in_diff(path, cx).is_some());
        if let Some(path) = target {
            self.files.select_path(&path);
            self.selection_moved(window, cx);
        }
    }

    fn next_file(&mut self, _: &NextFile, window: &mut Window, cx: &mut Context<Self>) {
        self.move_file(true, window, cx);
    }

    fn previous_file(&mut self, _: &PreviousFile, window: &mut Window, cx: &mut Context<Self>) {
        self.move_file(false, window, cx);
    }

    /// `]c` and `[c`: the next or previous change, across files. The list follows the cursor.
    fn next_change(&mut self, _: &NextChange, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(diff) = &self.diff {
            diff_cursor::go_to_change(diff, Direction::Next, window, cx);
        }
    }

    fn previous_change(&mut self, _: &PreviousChange, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(diff) = &self.diff {
            diff_cursor::go_to_change(diff, Direction::Prev, window, cx);
        }
    }

    /// The note above the diff while the current file changed since it was viewed.
    fn render_changed_since_viewed(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (path, note) = self.changed_since_viewed(cx)?;
        let colors = cx.theme().colors();
        Some(
            h_flex()
                .id("changed-since-viewed")
                .w_full()
                .flex_shrink_0()
                .px_3()
                .py_1()
                .gap_2()
                .border_b_1()
                .border_color(colors.border_variant)
                .bg(colors.panel_background)
                .child(
                    Icon::new(IconName::FileDiff)
                        .size(IconSize::Small)
                        .color(Color::Warning),
                )
                .child(Label::new(path).size(LabelSize::Small))
                .child(
                    Label::new(note)
                        .size(LabelSize::Small)
                        .color(Color::Muted)
                        .truncate(),
                )
                .into_any_element(),
        )
    }
}
