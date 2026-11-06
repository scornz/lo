//! Asking which account to open pull requests with, when several accounts can see a repository and
//! none of them was chosen for it (design §3 "Which account").
//!
//! Tabs' questions wait in one queue, and the picker shows one question at a time: which account
//! rozed should use for an owner, or for one repository whose owner's account can't see it. The
//! answer is remembered for that owner or that repository alone, and goes to every tab waiting
//! on the same question whose accounts include it; any other tab is asked again with its own.
//! The picker closes only when it's answered or escape dismisses it: another dialog or a click
//! elsewhere leaves it open, and a question waits while another dialog is open.

use std::time::Duration;

use futures::channel::oneshot;
use github::accounts::Accounts;
use gpui::{
    AnyWindowHandle, App, AppContext as _, Context, DismissEvent, Entity, EntityId, EventEmitter,
    FocusHandle, Focusable, Global, KeyBinding, Render, Task, WeakEntity, Window,
};
use ui::{ListItem, ListItemSpacing, prelude::*};
use workspace::{DismissDecision, ModalView, Workspace};

use crate::github_layer;

const KEY_CONTEXT: &str = "AccountPicker";

/// How often a question waiting for another dialog to close looks again.
const WAIT_FOR_DIALOG: Duration = Duration::from_millis(250);

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

/// What the picker asks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Question {
    /// The repository's owner and name, as GitHub names them now.
    pub owner: String,
    pub repo: String,
    /// The account rozed uses for the owner, which can't see `repo`: the answer is then for
    /// `repo` alone, and the owner keeps it.
    pub owners_account: Option<String>,
    /// The accounts that can see `repo`, to choose from.
    pub logins: Vec<String>,
}

impl Question {
    /// What the answer is for: the owner, or the repository alone.
    pub fn subject(&self) -> String {
        match self.owners_account {
            Some(_) => format!("{}/{}", self.owner, self.repo),
            None => self.owner.clone(),
        }
    }

    /// Whether this question's answer is for `owner/repo` too.
    fn covers(&self, owner: &str, repo: &str) -> bool {
        self.owner.eq_ignore_ascii_case(owner)
            && (self.owners_account.is_none() || self.repo.eq_ignore_ascii_case(repo))
    }
}

/// No account was chosen for `subject`: the picker was dismissed, or no account that can see
/// the repository is left.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotChosen {
    pub subject: String,
}

/// The login chosen for a tab, or why there's none.
pub type Answer = oneshot::Receiver<Result<String, NotChosen>>;

/// Asks which of `logins`, the accounts that can see `owner/repo`, to open it with, in the
/// workspace in `window`. The answer comes at once when an account among `logins` is
/// remembered for the repository by then, as when another tab's question was just answered;
/// otherwise once the picker asking it, or the same question for another tab, is answered.
pub fn ask(
    tab: EntityId,
    window: AnyWindowHandle,
    workspace: WeakEntity<Workspace>,
    owner: &str,
    repo: &str,
    logins: Vec<String>,
    cx: &mut App,
) -> Answer {
    let (reply, answer) = oneshot::channel();
    let questions = Questions::global(cx);
    questions.update(cx, |questions, _| {
        questions.waiting.push(Waiter {
            tab,
            window,
            workspace,
            owner: owner.to_string(),
            repo: repo.to_string(),
            logins,
            reply,
        });
    });
    // Once the caller lets go of the window, where the picker shows.
    cx.defer(move |cx| questions.update(cx, |questions, cx| questions.settle(cx)));
    answer
}

/// `tab` closed, so the questions it waited on may be moot.
pub(crate) fn tab_closed(tab: EntityId, cx: &mut App) {
    if let Some(questions) = cx
        .try_global::<GlobalQuestions>()
        .map(|global| global.0.clone())
    {
        cx.defer(move |cx| {
            questions.update(cx, |questions, cx| {
                questions.waiting.retain(|waiter| waiter.tab != tab);
                questions.settle(cx);
            })
        });
    }
}

/// A tab waiting for an account.
struct Waiter {
    tab: EntityId,
    window: AnyWindowHandle,
    workspace: WeakEntity<Workspace>,
    owner: String,
    repo: String,
    logins: Vec<String>,
    reply: oneshot::Sender<Result<String, NotChosen>>,
}

impl Waiter {
    fn answer(self, answer: Result<String, NotChosen>) {
        self.reply.send(answer).ok();
    }
}

/// The question on screen, or answered and being remembered.
struct Shown {
    question: Question,
    picker: WeakEntity<AccountPicker>,
    /// Whether the answer is being written down, so the picker has closed.
    answered: bool,
    _answer: Task<()>,
}

/// The questions waiting for an answer, oldest first, and the one the picker shows.
pub struct Questions {
    waiting: Vec<Waiter>,
    shown: Option<Shown>,
    /// Looking again once another dialog may have closed.
    retry: Option<Task<()>>,
}

struct GlobalQuestions(Entity<Questions>);

impl Global for GlobalQuestions {}

impl Questions {
    fn global(cx: &mut App) -> Entity<Self> {
        if let Some(global) = cx.try_global::<GlobalQuestions>() {
            return global.0.clone();
        }
        let questions = cx.new(|_| Self {
            waiting: Vec::new(),
            shown: None,
            retry: None,
        });
        cx.set_global(GlobalQuestions(questions.clone()));
        questions
    }

    /// Answers the tabs that can be answered without asking, drops the ones that closed, and
    /// shows the oldest question left when the picker is free.
    fn settle(&mut self, cx: &mut Context<Self>) {
        let accounts = github_layer::loaded(cx).map(|layer| layer.read(cx).accounts().clone());
        for waiter in std::mem::take(&mut self.waiting) {
            if waiter.reply.is_canceled() {
                continue;
            }
            let Some(accounts) = &accounts else {
                self.waiting.push(waiter);
                continue;
            };
            match known(waiter, accounts, cx) {
                Known::Answer(waiter, login) => waiter.answer(Ok(login)),
                Known::NoneLeft(waiter) => {
                    let subject = waiter.owner.clone();
                    waiter.answer(Err(NotChosen { subject }));
                }
                Known::Ask(waiter) => self.waiting.push(waiter),
            }
        }
        if let Some(shown) = &self.shown {
            // Every tab that asked closed, so nobody needs the answer.
            let wanted = self
                .waiting
                .iter()
                .any(|waiter| shown.question.covers(&waiter.owner, &waiter.repo));
            if !wanted
                && !shown.answered
                && let Some(picker) = shown.picker.upgrade()
            {
                picker.update(cx, |picker, cx| picker.close(cx));
            }
            return;
        }
        let Some(accounts) = accounts else {
            return;
        };
        while !self.waiting.is_empty() {
            let waiter = &self.waiting[0];
            let owners_account = accounts
                .read(cx)
                .remembered(&waiter.owner, &waiter.repo)
                .inspect_err(|error| {
                    log::warn!("reading the account for {}: {error:#}", waiter.owner)
                })
                .ok()
                .and_then(|remembered| remembered.owner);
            let question = Question {
                owner: waiter.owner.clone(),
                repo: waiter.repo.clone(),
                owners_account,
                logins: waiter.logins.clone(),
            };
            let (window, workspace) = (waiter.window, waiter.workspace.clone());
            match self.show(question, window, workspace, cx) {
                Showing::Shown(shown) => {
                    self.shown = Some(shown);
                    return;
                }
                Showing::Busy => {
                    self.retry_later(cx);
                    return;
                }
                Showing::Gone => {
                    let waiter = self.waiting.remove(0);
                    let subject = waiter.owner.clone();
                    waiter.answer(Err(NotChosen { subject }));
                }
            }
        }
    }

    /// Shows the picker asking `question` in `workspace`, unless another dialog is open there.
    fn show(
        &mut self,
        question: Question,
        window: AnyWindowHandle,
        workspace: WeakEntity<Workspace>,
        cx: &mut Context<Self>,
    ) -> Showing {
        let Some(workspace) = workspace.upgrade() else {
            return Showing::Gone;
        };
        let (reply, answer) = oneshot::channel();
        let shown = cx.update_window(window, |_, window, cx| {
            workspace.update(cx, |workspace, cx| {
                if workspace.has_active_modal(window, cx) {
                    return None;
                }
                workspace.toggle_modal(window, cx, {
                    let question = question.clone();
                    move |_, cx| AccountPicker::new(question, reply, cx)
                });
                workspace.active_modal::<AccountPicker>(cx)
            })
        });
        let picker = match shown {
            Ok(Some(picker)) => picker,
            Ok(None) => return Showing::Busy,
            Err(_) => return Showing::Gone,
        };
        log::info!(
            "asking which account to use for {} ({})",
            question.subject(),
            question.logins.join(", ")
        );
        let answer = cx.spawn({
            let question = question.clone();
            async move |this, cx| {
                let reply = answer.await;
                this.update(cx, |this, cx| match reply {
                    Ok(Reply::Chose(login)) => this.answered(question, login, cx),
                    Ok(Reply::Dismissed) => this.dismissed(&question, cx),
                    // The picker went without an answer, as when its window closed.
                    Err(_) => {
                        this.shown = None;
                        this.settle(cx);
                    }
                })
                .ok();
            }
        });
        Showing::Shown(Shown {
            question,
            picker: picker.downgrade(),
            answered: false,
            _answer: answer,
        })
    }

    /// Remembers `login` as the answer to `question`, then gives it to the tabs waiting on the
    /// same question whose accounts include it.
    fn answered(&mut self, question: Question, login: String, cx: &mut Context<Self>) {
        let Some(accounts) =
            github_layer::loaded(cx).map(|layer| layer.read(cx).accounts().clone())
        else {
            self.shown = None;
            return;
        };
        // An account removed while the picker listed it is asked about again, without it.
        if accounts.read(cx).account(&login).is_none() {
            log::info!(
                "{login} was chosen for {}, but it's gone",
                question.subject()
            );
            self.shown = None;
            self.settle(cx);
            return;
        }
        let remember = accounts.update(cx, |accounts, cx| match &question.owners_account {
            Some(_) => {
                accounts.remember_account_for_repo(&question.owner, &question.repo, &login, cx)
            }
            None => accounts.remember_account_for_owner(&question.owner, &login, cx),
        });
        let subject = question.subject();
        let write = cx.spawn(async move |this, cx| {
            match remember.await {
                Ok(()) => log::info!("rozed uses {login} for {subject}"),
                Err(error) => log::warn!("remembering {login} for {subject}: {error:#}"),
            }
            this.update(cx, |this, cx| {
                this.shown = None;
                for waiter in std::mem::take(&mut this.waiting) {
                    let can_use = waiter
                        .logins
                        .iter()
                        .any(|other| other.eq_ignore_ascii_case(&login));
                    if can_use && question.covers(&waiter.owner, &waiter.repo) {
                        waiter.answer(Ok(login.clone()));
                    } else {
                        this.waiting.push(waiter);
                    }
                }
                this.settle(cx);
            })
            .ok();
        });
        if let Some(shown) = &mut self.shown {
            shown.answered = true;
            shown._answer = write;
        }
    }

    /// The picker was dismissed: the tabs waiting on its question open nothing.
    fn dismissed(&mut self, question: &Question, cx: &mut Context<Self>) {
        log::info!("no account was chosen for {}", question.subject());
        self.shown = None;
        for waiter in std::mem::take(&mut self.waiting) {
            if question.covers(&waiter.owner, &waiter.repo) {
                waiter.answer(Err(NotChosen {
                    subject: question.subject(),
                }));
            } else {
                self.waiting.push(waiter);
            }
        }
        self.settle(cx);
    }

    fn retry_later(&mut self, cx: &mut Context<Self>) {
        if self.retry.is_some() {
            return;
        }
        self.retry = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(WAIT_FOR_DIALOG).await;
            this.update(cx, |this, cx| {
                this.retry = None;
                this.settle(cx);
            })
            .ok();
        }));
    }
}

enum Showing {
    Shown(Shown),
    /// Another dialog is open.
    Busy,
    /// The tab's window or workspace closed.
    Gone,
}

/// What can be said for a tab without asking.
enum Known {
    /// An account remembered for its repository by now is among its accounts, or only one of
    /// them is still signed in.
    Answer(Waiter, String),
    /// None of its accounts is signed in any more.
    NoneLeft(Waiter),
    Ask(Waiter),
}

/// What can be said for `waiter` from what's remembered now, without asking.
fn known(mut waiter: Waiter, accounts: &Entity<Accounts>, cx: &App) -> Known {
    let accounts = accounts.read(cx);
    waiter
        .logins
        .retain(|login| accounts.account(login).is_some() && !accounts.is_signed_out(login));
    let remembered = accounts
        .remembered(&waiter.owner, &waiter.repo)
        .inspect_err(|error| log::warn!("reading the account for {}: {error:#}", waiter.owner))
        .unwrap_or_default();
    if let Some(login) = remembered.pick(&waiter.logins).cloned() {
        return Known::Answer(waiter, login);
    }
    match waiter.logins.as_slice() {
        [] => Known::NoneLeft(waiter),
        [only] => {
            let only = only.clone();
            Known::Answer(waiter, only)
        }
        _ => Known::Ask(waiter),
    }
}

/// What the picker says when it closes.
enum Reply {
    Chose(String),
    Dismissed,
}

/// The picker: which of several accounts to use for an owner's repositories, or for one
/// repository.
pub struct AccountPicker {
    question: Question,
    selected: usize,
    focus_handle: FocusHandle,
    /// Where the answer goes, until it's given.
    reply: Option<oneshot::Sender<Reply>>,
}

impl AccountPicker {
    fn new(question: Question, reply: oneshot::Sender<Reply>, cx: &mut Context<Self>) -> Self {
        Self {
            question,
            selected: 0,
            focus_handle: cx.focus_handle(),
            reply: Some(reply),
        }
    }

    pub fn question(&self) -> &Question {
        &self.question
    }

    pub fn owner(&self) -> &str {
        &self.question.owner
    }

    pub fn logins(&self) -> &[String] {
        &self.question.logins
    }

    pub fn selected(&self) -> Option<&str> {
        self.question.logins.get(self.selected).map(String::as_str)
    }

    fn select_next(&mut self, _: &menu::SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected + 1 < self.question.logins.len() {
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
            reply.send(Reply::Chose(login.to_string())).ok();
        }
        cx.emit(DismissEvent);
    }

    fn cancel(&mut self, _: &menu::Cancel, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(reply) = self.reply.take() {
            reply.send(Reply::Dismissed).ok();
        }
        cx.emit(DismissEvent);
    }

    /// Closes the picker without an answer, when no tab needs one any more.
    fn close(&mut self, cx: &mut Context<Self>) {
        self.reply = None;
        cx.emit(DismissEvent);
    }
}

impl EventEmitter<DismissEvent> for AccountPicker {}

impl ModalView for AccountPicker {
    /// Only an answer or escape closes the picker: the tabs waiting on it keep waiting while
    /// another dialog tries to open or a click lands elsewhere.
    fn on_before_dismiss(&mut self, _: &mut Window, _: &mut Context<Self>) -> DismissDecision {
        DismissDecision::Dismiss(self.reply.is_none())
    }
}

impl Focusable for AccountPicker {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for AccountPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Question {
            owner,
            repo,
            owners_account,
            ..
        } = &self.question;
        let subject = self.question.subject();
        let explanation = match owners_account {
            None => format!(
                "More than one of your accounts can see {owner}/{repo}. rozed uses the one you \
                 choose for every {owner} repository."
            ),
            Some(owners_account) => format!(
                "{owners_account}, the account rozed uses for {owner}, can't see {owner}/{repo}. \
                 rozed uses the one you choose for {owner}/{repo} only, and {owners_account} for \
                 {owner}'s other repositories."
            ),
        };
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
                        "Which account should rozed use for {subject}?"
                    )))
                    .child(
                        Label::new(explanation)
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
            )
            .children(
                self.question
                    .logins
                    .iter()
                    .enumerate()
                    .map(|(index, login)| {
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
                    }),
            )
    }
}
