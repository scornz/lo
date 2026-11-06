//! The box `cmd-o` opens: paste a pull request's link, press enter, and it opens in a tab, or its
//! tab comes to the front if it's already open.

use editor::Editor;
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, KeyBinding, Render,
    SharedString, WeakEntity, Window, actions,
};
use ui::prelude::*;
use workspace::{ModalView, Workspace};

use crate::pr_diff::Trust;
use crate::pr_link::PrLink;
use crate::pr_tab;

actions!(
    review_ui,
    [
        /// Opens a pull request from its link.
        OpenPullRequest,
    ]
);

const KEY_CONTEXT: &str = "OpenPullRequest";

/// The box's keys. In its line, enter opens the link and escape closes the box, as in a picker.
pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-o", OpenPullRequest, Some("Workspace")),
        KeyBinding::new("enter", menu::Confirm, Some("OpenPullRequest > Editor")),
        KeyBinding::new("escape", menu::Cancel, Some("OpenPullRequest > Editor")),
    ]);
}

/// The box: a line for the link and why the last one couldn't open.
pub struct OpenBox {
    input: Entity<Editor>,
    error: Option<SharedString>,
    workspace: WeakEntity<Workspace>,
}

impl OpenBox {
    /// Opens the box in `workspace`, or closes it if it's open.
    pub fn toggle(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
        let weak = workspace.weak_handle();
        workspace.toggle_modal(window, cx, |window, cx| Self::new(weak, window, cx));
    }

    fn new(workspace: WeakEntity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("https://github.com/owner/repo/pull/123", window, cx);
            editor
        });
        Self {
            input,
            error: None,
            workspace,
        }
    }

    pub fn input(&self) -> &Entity<Editor> {
        &self.input
    }

    pub fn error(&self) -> Option<&SharedString> {
        self.error.as_ref()
    }

    fn confirm(&mut self, _: &menu::Confirm, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).text(cx);
        match text.parse::<PrLink>() {
            Ok(link) => {
                cx.emit(DismissEvent);
                let workspace = self.workspace.clone();
                window.defer(cx, move |window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            pr_tab::open(workspace, link, Trust::Untrusted, window, cx);
                        })
                        .ok();
                });
            }
            Err(error) => {
                self.error = Some(format!("Can't open that: {error}.").into());
                cx.notify();
            }
        }
    }

    fn cancel(&mut self, _: &menu::Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }
}

impl EventEmitter<DismissEvent> for OpenBox {}

impl ModalView for OpenBox {}

impl Focusable for OpenBox {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl Render for OpenBox {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::cancel))
            .w(rems(36.))
            .elevation_3(cx)
            .p_2()
            .gap_1p5()
            .child(
                Label::new("Open a pull request")
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .child(
                div()
                    .px_2()
                    .py_1()
                    .rounded_sm()
                    .border_1()
                    .border_color(cx.theme().colors().border)
                    .child(self.input.clone()),
            )
            .children(
                self.error
                    .clone()
                    .map(|error| Label::new(error).size(LabelSize::Small).color(Color::Error)),
            )
    }
}
