//! The tab's right side: the threads panel (design §8 "Threads panel", §10 "Window").

use gpui::{App, Context, Div, KeyBinding, Window, actions, px};
use ui::prelude::*;

use super::PrTab;
use super::keys::KEY_CONTEXT;

actions!(
    review_ui,
    [
        /// Shows or hides the threads panel.
        ToggleThreads,
    ]
);

/// The threads panel's keys.
pub(super) fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-alt-b", ToggleThreads, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-r", ToggleThreads, Some(KEY_CONTEXT)),
    ]);
}

/// The tab runs the threads panel's actions wherever their keys work.
pub(super) fn on_actions(tab: Div, cx: &Context<PrTab>) -> Div {
    tab.on_action(cx.listener(PrTab::toggle_threads))
}

impl PrTab {
    fn toggle_threads(&mut self, _: &ToggleThreads, _: &mut Window, cx: &mut Context<Self>) {
        self.show_threads = !self.show_threads;
        cx.notify();
    }

    pub(super) fn render_threads(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        v_flex()
            .id("pr-threads")
            .w(px(280.))
            .h_full()
            .flex_shrink_0()
            .p_2()
            .gap_1()
            .border_l_1()
            .border_color(colors.border)
            .bg(colors.panel_background)
            .child(
                Label::new("Threads")
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .child(
                Label::new("No threads yet")
                    .size(LabelSize::Small)
                    .color(Color::Disabled),
            )
    }
}
