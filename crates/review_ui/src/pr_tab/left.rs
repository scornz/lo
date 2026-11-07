//! The tab's left side: a slot for the pull request's stack tree, above its files, in review order
//! or by path (design §4 "The stack tree", §5 "File order", §10 "Window").

use std::ops::Range;

use gpui::{AnyElement, AnyView, App, Context, Div, KeyBinding, Window, actions, px, uniform_list};
use review::revisions::ViewedState;
use ui::{ListItem, ListItemSpacing, Tooltip, prelude::*};

use super::PrTab;
use super::keys::{FILE_LIST_CONTEXT, KEY_CONTEXT, READING_CONTEXT};
use crate::file_list::{FileChange, FileOrder};

actions!(
    review_ui,
    [
        /// Shows or hides the file list.
        ToggleFileList,
        /// Switches the file list between review order and path order.
        ToggleFileOrder,
    ]
);

/// The file list's keys.
pub(super) fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-b", ToggleFileList, Some(KEY_CONTEXT)),
        // alt-o types ø.
        KeyBinding::new("alt-o", ToggleFileOrder, Some(READING_CONTEXT)),
        KeyBinding::new("o", ToggleFileOrder, Some(FILE_LIST_CONTEXT)),
        // The workspace binds escape to stop following a collaborator.
        KeyBinding::new("escape", menu::Cancel, Some(FILE_LIST_CONTEXT)),
        KeyBinding::new("enter", menu::Confirm, Some(FILE_LIST_CONTEXT)),
        KeyBinding::new("j", menu::SelectNext, Some(FILE_LIST_CONTEXT)),
        KeyBinding::new("k", menu::SelectPrevious, Some(FILE_LIST_CONTEXT)),
    ]);
}

/// The tab runs the file list's actions wherever their keys work.
pub(super) fn on_actions(tab: Div, cx: &Context<PrTab>) -> Div {
    tab.on_action(cx.listener(PrTab::toggle_file_list))
        .on_action(cx.listener(PrTab::toggle_file_order))
}

impl PrTab {
    /// Shows `tree`, the pull request's stack tree, above the file list, or takes it away.
    pub fn set_stack_tree(&mut self, tree: Option<AnyView>, cx: &mut Context<Self>) {
        self.stack_tree = tree;
        cx.notify();
    }

    /// The tab's left side, unless it's empty: the stack tree, if there is one, above the file
    /// list while it's shown.
    pub(super) fn render_left(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if self.stack_tree.is_none() && !self.show_file_list {
            return None;
        }
        let colors = cx.theme().colors();
        Some(
            v_flex()
                .w(px(300.))
                .h_full()
                .flex_shrink_0()
                .border_r_1()
                .border_color(colors.border)
                .bg(colors.panel_background)
                .children(self.stack_tree.clone())
                .when(self.show_file_list, |this| {
                    this.child(self.render_file_list(cx))
                }),
        )
    }

    fn toggle_file_list(
        &mut self,
        _: &ToggleFileList,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_file_list = !self.show_file_list;
        if self.show_file_list {
            window.focus(&self.file_list_focus, cx);
        } else {
            self.focus_diff(window, cx);
        }
        cx.notify();
    }

    fn toggle_file_order(&mut self, _: &ToggleFileOrder, _: &mut Window, cx: &mut Context<Self>) {
        self.files.toggle_order();
        self.scroll_to_selected();
        cx.notify();
    }

    fn select_next(&mut self, _: &menu::SelectNext, window: &mut Window, cx: &mut Context<Self>) {
        self.files.select_next();
        self.selection_moved(window, cx);
    }

    fn select_previous(
        &mut self,
        _: &menu::SelectPrevious,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.files.select_previous();
        self.selection_moved(window, cx);
    }

    fn confirm(&mut self, _: &menu::Confirm, window: &mut Window, cx: &mut Context<Self>) {
        self.reveal_selected(window, cx);
        self.focus_diff(window, cx);
    }

    fn cancel(&mut self, _: &menu::Cancel, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_diff(window, cx);
    }

    fn render_file_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let order = match self.files.order() {
            FileOrder::Review => "Review order",
            FileOrder::Path => "Path order",
        };
        let count = match self.files.len() {
            1 => "1 file".to_string(),
            count => format!("{count} files"),
        };
        v_flex()
            .id("pr-file-list")
            .key_context(FILE_LIST_CONTEXT)
            .track_focus(&self.file_list_focus)
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::cancel))
            .flex_1()
            .min_h_0()
            .child(
                h_flex()
                    .px_2()
                    .py_1()
                    .justify_between()
                    .border_b_1()
                    .border_color(colors.border_variant)
                    .child(Label::new(count).size(LabelSize::Small).color(Color::Muted))
                    .child(
                        Label::new(order)
                            .size(LabelSize::XSmall)
                            .color(Color::Muted),
                    ),
            )
            .child(
                uniform_list(
                    "pr-files",
                    self.files.len(),
                    cx.processor(|this, range: Range<usize>, _window, cx| {
                        range
                            .filter_map(|row| this.render_file_row(row, cx))
                            .collect::<Vec<_>>()
                    }),
                )
                .track_scroll(&self.file_list_scroll)
                .flex_1(),
            )
    }

    fn render_file_row(&self, row: usize, cx: &mut Context<Self>) -> Option<AnyElement> {
        let entry = self.files.row(row)?;
        let selected = self.files.selected_row() == Some(row);
        let (dir, name) = match entry.path().rsplit_once('/') {
            Some((dir, name)) => (Some(dir.to_string()), name.to_string()),
            None => (None, entry.path().to_string()),
        };
        let change_color = match entry.change {
            FileChange::Added => Color::Created,
            FileChange::Deleted => Color::Deleted,
            FileChange::Modified | FileChange::Renamed | FileChange::TypeChanged => Color::Modified,
        };
        let file = &entry.file;
        Some(
            ListItem::new(("pr-file", row))
                .spacing(ListItemSpacing::Dense)
                .toggle_state(selected)
                .start_slot(
                    Label::new(entry.change.letter())
                        .size(LabelSize::Small)
                        .color(change_color),
                )
                .child(
                    h_flex()
                        .min_w_0()
                        .gap_1p5()
                        .child(Label::new(name).size(LabelSize::Small).truncate())
                        .children(dir.map(|dir| {
                            Label::new(dir)
                                .size(LabelSize::XSmall)
                                .color(Color::Muted)
                                .truncate()
                        })),
                )
                .end_slot(
                    h_flex()
                        .gap_1()
                        .when(!file.binary, |this| {
                            this.child(
                                Label::new(format!("+{}", file.additions))
                                    .size(LabelSize::XSmall)
                                    .color(Color::Created),
                            )
                            .child(
                                Label::new(format!("−{}", file.deletions))
                                    .size(LabelSize::XSmall)
                                    .color(Color::Deleted),
                            )
                        })
                        .children(viewed_mark(row, entry.viewed)),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.files.select_row(row);
                    this.reveal_selected(window, cx);
                    cx.notify();
                }))
                .into_any_element(),
        )
    }
}

/// How the file list shows the viewed mark of the file at `row`.
fn viewed_mark(row: usize, viewed: Option<ViewedState>) -> Option<AnyElement> {
    match viewed? {
        ViewedState::Viewed => Some(
            Icon::new(IconName::Check)
                .size(IconSize::XSmall)
                .color(Color::Success)
                .into_any_element(),
        ),
        ViewedState::Dismissed => Some(
            div()
                .id(("pr-file-changed", row))
                .child(
                    Label::new("changed")
                        .size(LabelSize::XSmall)
                        .color(Color::Warning),
                )
                .tooltip(Tooltip::text(
                    "Changed since viewed: a push changed it after it was marked viewed",
                ))
                .into_any_element(),
        ),
        ViewedState::Unviewed => None,
    }
}
