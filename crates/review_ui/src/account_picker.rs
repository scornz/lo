//! The small picker that asks which account to open an owner's pull requests with, when several
//! accounts can see the repository and none was chosen for its owner yet (design §3 "Which
//! account"). The tab that asked remembers the answer for the owner.

use futures::FutureExt as _;
use futures::channel::oneshot;
use futures::future::Shared;
use gpui::{
    App, Context, DismissEvent, EventEmitter, FocusHandle, Focusable, KeyBinding, Render, Window,
};
use ui::{ListItem, ListItemSpacing, prelude::*};
use workspace::{ModalView, Workspace};

const KEY_CONTEXT: &str = "AccountPicker";

/// The picker's keys: `j`/`k` or the arrows move, enter chooses, escape closes it without
/// choosing.
pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("j", menu::SelectNext, Some(KEY_CONTEXT)),
        KeyBinding::new("k", menu::SelectPrevious, Some(KEY_CONTEXT)),
        KeyBinding::new("down", menu::SelectNext, Some(KEY_CONTEXT)),
        KeyBinding::new("up", menu::SelectPrevious, Some(KEY_CONTEXT)),
        KeyBinding::new("enter", menu::Confirm, Some(KEY_CONTEXT)),
        KeyBinding::new("escape", menu::Cancel, Some(KEY_CONTEXT)),
    ]);
}

/// The login chosen, or an error once the picker closed without a choice.
pub type Answer = Shared<oneshot::Receiver<String>>;

/// Which of several accounts to use for an owner's repositories.
pub struct AccountPicker {
    owner: String,
    repo: String,
    logins: Vec<String>,
    selected: usize,
    focus_handle: FocusHandle,
    /// Where the choice goes. Dropped without one when the picker closes.
    reply: Option<oneshot::Sender<String>>,
    answer: Answer,
}

impl AccountPicker {
    /// Asks in `workspace` which of `logins` to open `owner/repo` with. When the picker already
    /// asks about the same owner, the answer is that one's. A picker asking about another owner
    /// closes without an answer.
    pub fn ask(
        workspace: &mut Workspace,
        owner: &str,
        repo: &str,
        logins: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Answer {
        if let Some(open) = workspace.active_modal::<Self>(cx) {
            let open = open.read(cx);
            if open.owner.eq_ignore_ascii_case(owner) {
                return open.answer.clone();
            }
            workspace.hide_modal(window, cx);
        }
        let (reply, answer) = oneshot::channel();
        let answer = answer.shared();
        let (owner, repo) = (owner.to_string(), repo.to_string());
        workspace.toggle_modal(window, cx, {
            let answer = answer.clone();
            move |_, cx| Self {
                owner,
                repo,
                logins,
                selected: 0,
                focus_handle: cx.focus_handle(),
                reply: Some(reply),
                answer,
            }
        });
        answer
    }

    pub fn owner(&self) -> &str {
        &self.owner
    }

    pub fn logins(&self) -> &[String] {
        &self.logins
    }

    pub fn selected(&self) -> Option<&str> {
        self.logins.get(self.selected).map(String::as_str)
    }

    fn select_next(&mut self, _: &menu::SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected + 1 < self.logins.len() {
            self.selected += 1;
            cx.notify();
        }
    }

    fn select_previous(
        &mut self,
        _: &menu::SelectPrevious,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected > 0 {
            self.selected -= 1;
            cx.notify();
        }
    }

    fn confirm(&mut self, _: &menu::Confirm, _: &mut Window, cx: &mut Context<Self>) {
        if let (Some(reply), Some(login)) = (self.reply.take(), self.selected()) {
            reply.send(login.to_string()).ok();
        }
        cx.emit(DismissEvent);
    }

    fn cancel(&mut self, _: &menu::Cancel, _: &mut Window, cx: &mut Context<Self>) {
        self.reply = None;
        cx.emit(DismissEvent);
    }
}

impl EventEmitter<DismissEvent> for AccountPicker {}

impl ModalView for AccountPicker {}

impl Focusable for AccountPicker {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for AccountPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let owner = &self.owner;
        v_flex()
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::cancel))
            .w(rems(30.))
            .elevation_3(cx)
            .p_2()
            .gap_1()
            .child(
                v_flex()
                    .px_1()
                    .pb_1()
                    .gap_0p5()
                    .child(Label::new(format!(
                        "Which account should rozed use for {owner}?"
                    )))
                    .child(
                        Label::new(format!(
                            "More than one of your accounts can see {owner}/{}. rozed uses the \
                             one you choose for every {owner} repository.",
                            self.repo
                        ))
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                    ),
            )
            .children(self.logins.iter().enumerate().map(|(index, login)| {
                ListItem::new(("account", index))
                    .spacing(ListItemSpacing::Sparse)
                    .toggle_state(index == self.selected)
                    .start_slot(
                        Icon::new(IconName::Person)
                            .size(IconSize::Small)
                            .color(Color::Muted),
                    )
                    .child(Label::new(login.clone()))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.selected = index;
                        this.confirm(&menu::Confirm, window, cx);
                    }))
            }))
    }
}
