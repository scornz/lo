//! The tab's right side, which hosts the threads panel (design §8 "Threads panel", §10 "Window").

use gpui::{App, Context, Div, KeyBinding, Window, actions, px};
use ui::prelude::*;

use super::keys::KEY_CONTEXT;
use super::{PartView, PrTab};

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
pub(super) fn on_actions(root: Div, _: &PrTab, cx: &Context<PrTab>) -> Div {
    root.on_action(cx.listener(PrTab::toggle_threads))
}

impl PrTab {
    fn toggle_threads(&mut self, _: &ToggleThreads, window: &mut Window, cx: &mut Context<Self>) {
        self.show_threads = !self.show_threads;
        if !self.show_threads {
            self.focus_diff_from(self.threads_panel.as_ref(), window, cx);
        }
        cx.notify();
    }

    /// Puts `panel`, the threads panel, on the tab's right side, or takes it away. The panel draws
    /// all of itself, its heading included.
    pub fn set_threads_panel(
        &mut self,
        panel: Option<PartView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_diff_from(self.threads_panel.as_ref(), window, cx);
        self.threads_panel = panel;
        cx.notify();
    }

    /// The tab's right side, while it's shown.
    pub(super) fn render_right(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        self.show_threads.then(|| self.render_threads(cx))
    }

    /// The threads panel, or until there is one, a note that there are no threads.
    fn render_threads(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        v_flex()
            .id("pr-threads")
            .w(px(280.))
            .h_full()
            .flex_shrink_0()
            .border_l_1()
            .border_color(colors.border)
            .bg(colors.panel_background)
            .map(|this| match &self.threads_panel {
                Some(panel) => this.child(panel.view.clone()),
                None => this
                    .p_2()
                    .gap_1()
                    .child(
                        Label::new("Threads")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(
                        Label::new("No threads yet")
                            .size(LabelSize::Small)
                            .color(Color::Disabled),
                    ),
            })
    }
}
