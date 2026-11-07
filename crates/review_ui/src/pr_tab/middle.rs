//! The middle of the tab: the pull request's diff, and the keys that move through it and change how
//! it shows (design §5 "The diff view", §10 "Window").

use editor::Direction;
use gpui::{AnyElement, App, Context, Div, KeyBinding, Window, actions};
use review::revisions::Revisions;
use settings::DiffViewStyle;
use ui::prelude::*;

use super::PrTab;
use super::keys::{DIFF_CONTEXT, FILE_LIST_CONTEXT, KEY_CONTEXT, READING_CONTEXT};
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

/// The diff's keys.
pub(super) fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("tab", SwitchSide, Some(DIFF_CONTEXT)),
        // Option and a letter types a character where there's text to type, so these work only
        // in the diff and the file list.
        KeyBinding::new("alt-v", ToggleDiffLayout, Some(DIFF_CONTEXT)),
        KeyBinding::new("alt-v", ToggleDiffLayout, Some(FILE_LIST_CONTEXT)),
        KeyBinding::new("alt-w", ToggleWhitespace, Some(DIFF_CONTEXT)),
        KeyBinding::new("alt-w", ToggleWhitespace, Some(FILE_LIST_CONTEXT)),
        KeyBinding::new("alt-a", ExpandFile, Some(DIFF_CONTEXT)),
        KeyBinding::new("alt-a", ExpandFile, Some(FILE_LIST_CONTEXT)),
        KeyBinding::new("] f", NextFile, Some(READING_CONTEXT)),
        KeyBinding::new("[ f", PreviousFile, Some(READING_CONTEXT)),
        KeyBinding::new("] c", NextChange, Some(READING_CONTEXT)),
        KeyBinding::new("[ c", PreviousChange, Some(READING_CONTEXT)),
        // Back and forward through where the diff's cursor has been (design §10), beside Zed's
        // ctrl-- and ctrl-_.
        KeyBinding::new("ctrl-o", workspace::pane::GoBack, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-i", workspace::pane::GoForward, Some(KEY_CONTEXT)),
    ]);
}

/// The tab runs the diff's actions wherever their keys work.
pub(super) fn on_actions(tab: Div, cx: &Context<PrTab>) -> Div {
    tab.on_action(cx.listener(PrTab::switch_side))
        .on_action(cx.listener(PrTab::toggle_diff_layout))
        .on_action(cx.listener(PrTab::toggle_whitespace))
        .on_action(cx.listener(PrTab::expand_file))
        .on_action(cx.listener(PrTab::next_file))
        .on_action(cx.listener(PrTab::previous_file))
        .on_action(cx.listener(PrTab::next_change))
        .on_action(cx.listener(PrTab::previous_change))
}

impl PrTab {
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
    pub(super) fn render_changed_since_viewed(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
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
