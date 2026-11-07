//! Asking which account to open pull requests with, when several accounts can see a repository and
//! none of them was chosen for it (design §3 "Which account").
//!
//! Tabs' questions wait in one queue, and the picker shows one question at a time, for the active
//! tab: which account Lo should use for an owner, or for one repository whose owner's account
//! can't see it. A question from another tab waits until that tab is active, so a tab loading in
//! the background never puts the picker over other work. The answer is remembered for that owner
//! or that repository alone, and goes to every tab waiting on the same question whose accounts
//! include it; any other tab is asked again with its own. The picker closes only when it's
//! answered or escape dismisses it: another dialog or a click elsewhere leaves it open, a new tab
//! doesn't take its keyboard focus, and a question waits while another dialog is open. Accounts…
//! sets the picker aside, to show again once the accounts dialog closes, and an account removed or
//! signed out meanwhile drops out of it.

use std::time::Duration;

use futures::channel::oneshot;
use github::accounts::Accounts;
use gpui::{
    AnyWindowHandle, App, AppContext as _, Context, DismissEvent, Entity, EntityId, EventEmitter,
    FocusHandle, Focusable, Global, KeyBinding, Render, Subscription, Task, WeakEntity, Window,
};
use ui::{ListItem, ListItemSpacing, prelude::*};
use workspace::{DismissDecision, ModalView, Workspace};

use crate::github_layer;
use crate::pr_tab::PullHandle;

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
    /// The account Lo uses for the owner, which can't see `repo`: the answer is then for
    /// `repo` alone, and the owner keeps it.
    pub owners_account: Option<String>,
    /// The accounts that can see `repo`, to choose from.
    pub logins: Vec<String>,
}

impl Question {
    /// What the answer is for: the owner, or the repository alone.
    pub fn subject(&self) -> String {
        subject(&self.owner, &self.repo, self.owners_account.is_some())
    }

    /// Whether this question's answer is for `owner/repo` too.
    fn covers(&self, owner: &str, repo: &str) -> bool {
        self.owner.eq_ignore_ascii_case(owner)
            && (self.owners_account.is_none() || self.repo.eq_ignore_ascii_case(repo))
    }
}

/// What a question about `owner/repo` is for: the owner, or the repository alone when it's
/// `for_repo`.
fn subject(owner: &str, repo: &str, for_repo: bool) -> String {
    if for_repo {
        format!("{owner}/{repo}")
    } else {
        owner.to_string()
    }
}

/// Why no account was chosen for `subject`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotChosen {
    /// Escape dismissed the picker.
    Dismissed { subject: String },
    /// None of the accounts that can see the repository is signed in any more.
    NoneLeft { subject: String },
    /// An account was added or signed in again while the tab waited, so it should check which
    /// accounts can see the repository again: the new one may be among them.
    AccountsChanged,
}

/// The login chosen for a tab, or why there's none.
pub type Answer = oneshot::Receiver<Result<String, NotChosen>>;

/// Asks which of `logins`, the accounts that can see `owner/repo`, to open it with in `tab`, in
/// the workspace in `window`. The answer comes at once when an account among `logins` is
/// remembered for the repository by then, as when another tab's question was just answered;
/// otherwise once the picker asking it, or the same question for another tab, is answered. The
/// picker asks only while `tab` is the active tab.
pub fn ask(
    tab: PullHandle,
    window: AnyWindowHandle,
    workspace: WeakEntity<Workspace>,
    owner: &str,
    repo: &str,
    logins: Vec<String>,
    cx: &mut App,
) -> Answer {
    let (reply, answer) = oneshot::channel();
    let questions = Questions::global(cx);
    questions.update(cx, |questions, cx| {
        questions.watch_accounts(cx);
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
    settle_soon(cx);
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
                questions
                    .waiting
                    .retain(|waiter| waiter.tab.tab().entity_id() != tab);
                questions.settle(cx);
            })
        });
    }
}

/// The active tab changed, so its question can be asked.
pub(crate) fn active_item_changed(cx: &mut App) {
    settle_soon(cx);
}

/// Sets the open picker aside, keeping its question for once the dialog about to open closes,
/// as Accounts… does. An account signed in there makes the waiting tabs check again which
/// accounts can see their repositories, so the picker then lists it. Returns whether there was a
/// picker to set aside.
pub fn set_aside(workspace: &mut Workspace, cx: &mut App) -> bool {
    let Some(picker) = workspace.active_modal::<AccountPicker>(cx) else {
        return false;
    };
    picker.update(cx, |picker, cx| picker.reply(Reply::SetAside, cx));
    true
}

/// Looks at the questions once whatever's being updated now is done, as the window where the
/// picker shows may be.
fn settle_soon(cx: &mut App) {
    if let Some(questions) = cx
        .try_global::<GlobalQuestions>()
        .map(|global| global.0.clone())
    {
        cx.defer(move |cx| questions.update(cx, |questions, cx| questions.settle(cx)));
    }
}

/// A tab's pull request waiting for an account.
struct Waiter {
    tab: PullHandle,
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

    /// Whether the waiting tab is the pull request its window is about, as the window's title
    /// has it: the active tab, or the one last active beside an empty pane. Only then may the
    /// picker ask.
    fn is_active(&self, cx: &App) -> bool {
        let Some(workspace) = self.workspace.upgrade() else {
            return false;
        };
        crate::window_title::shown_pull_request(workspace.read(cx), cx)
            .is_some_and(|tab| tab.entity_id() == self.tab.tab().entity_id())
            && self.tab.is_shown(cx)
    }

    /// Tells the tab whether the picker asks its question now, or it waits.
    fn show_status(&self, subject: &str, asking: bool, cx: &mut App) {
        self.tab
            .update(cx, |tab, cx| tab.waiting_for_account(subject, asking, cx))
            .ok();
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
    /// Accounts removed or signed out drop out of the questions, and accounts added or signed in
    /// again send the waiting tabs back to check.
    accounts: Option<Subscription>,
    /// The accounts signed in when the questions were last looked at, lowercased.
    signed_in: Vec<String>,
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
            accounts: None,
            signed_in: Vec::new(),
        });
        cx.set_global(GlobalQuestions(questions.clone()));
        questions
    }

    /// Looks at the questions again whenever an account is added, removed or signed out.
    fn watch_accounts(&mut self, cx: &mut Context<Self>) {
        if self.accounts.is_some() {
            return;
        }
        if let Some(layer) = github_layer::loaded(cx) {
            let accounts = layer.read(cx).accounts().clone();
            self.signed_in = signed_in(&accounts, cx);
            self.accounts = Some(cx.observe(&accounts, |this, _, cx| this.settle(cx)));
        }
    }

    /// Answers the tabs that can be answered without asking, drops the ones that closed, and
    /// shows the oldest question of an active tab left when the picker is free.
    fn settle(&mut self, cx: &mut Context<Self>) {
        let accounts = github_layer::loaded(cx).map(|layer| layer.read(cx).accounts().clone());
        // An account added or signed in again may see the waiting tabs' repositories, which
        // only checking again tells: each tab checks and asks anew.
        if let Some(accounts) = &accounts {
            let now = signed_in(accounts, cx);
            let gained = now.iter().any(|login| !self.signed_in.contains(login));
            self.signed_in = now;
            if gained {
                for waiter in std::mem::take(&mut self.waiting) {
                    waiter.answer(Err(NotChosen::AccountsChanged));
                }
            }
        }
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
                Known::NoneLeft(waiter, subject) => {
                    waiter.answer(Err(NotChosen::NoneLeft { subject }))
                }
                Known::Ask(waiter) => self.waiting.push(waiter),
            }
        }
        // Each waiting tab says which question it waits on, as asked now.
        for waiter in &self.waiting {
            let shown = self.shown.as_ref().filter(|shown| {
                !shown.answered && shown.question.covers(&waiter.owner, &waiter.repo)
            });
            match shown {
                Some(shown) => waiter.show_status(&shown.question.subject(), true, cx),
                None => {
                    let for_repo = accounts.as_ref().is_some_and(|accounts| {
                        accounts
                            .read(cx)
                            .remembered(&waiter.owner, &waiter.repo)
                            .is_ok_and(|remembered| remembered.owner.is_some())
                    });
                    waiter.show_status(&subject(&waiter.owner, &waiter.repo, for_repo), false, cx);
                }
            }
        }
        if let Some(shown) = &self.shown {
            if shown.answered {
                return;
            }
            let wanted = self
                .waiting
                .iter()
                .any(|waiter| shown.question.covers(&waiter.owner, &waiter.repo));
            // An account it lists was removed or signed out: asked again without it.
            let gone =
                accounts.as_ref().is_some_and(|accounts| {
                    let accounts = accounts.read(cx);
                    shown.question.logins.iter().any(|login| {
                        accounts.account(login).is_none() || accounts.is_signed_out(login)
                    })
                });
            if let Some(picker) = shown.picker.upgrade() {
                if !wanted {
                    // Every tab that asked closed, so nobody needs the answer.
                    picker.update(cx, |picker, cx| picker.reply(Reply::Moot, cx));
                } else if gone {
                    picker.update(cx, |picker, cx| picker.reply(Reply::SetAside, cx));
                }
            }
            return;
        }
        let Some(accounts) = accounts else {
            return;
        };
        let mut index = 0;
        while index < self.waiting.len() {
            let waiter = &self.waiting[index];
            if waiter.workspace.upgrade().is_none() {
                let waiter = self.waiting.remove(index);
                let subject = waiter.owner.clone();
                waiter.answer(Err(NotChosen::Dismissed { subject }));
                continue;
            }
            if !waiter.is_active(cx) {
                index += 1;
                continue;
            }
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
                    let subject = shown.question.subject();
                    for waiter in &self.waiting {
                        if shown.question.covers(&waiter.owner, &waiter.repo) {
                            waiter.show_status(&subject, true, cx);
                        }
                    }
                    self.shown = Some(shown);
                    return;
                }
                Showing::Busy => {
                    self.retry_later(cx);
                    return;
                }
                Showing::Gone => {
                    let waiter = self.waiting.remove(index);
                    let subject = waiter.owner.clone();
                    waiter.answer(Err(NotChosen::Dismissed { subject }));
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
                    move |window, cx| AccountPicker::new(question, reply, window, cx)
                });
                workspace.active_modal::<AccountPicker>(cx)
            })
        });
        // A window that can't be updated just now is tried again too.
        let Ok(Some(picker)) = shown else {
            return Showing::Busy;
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
                    // Set aside, or nobody needs the answer, or the picker went without one,
                    // as when its window closed: the question waits again.
                    Ok(Reply::SetAside | Reply::Moot) | Err(_) => {
                        this.shown = None;
                        let subject = question.subject();
                        for waiter in &this.waiting {
                            if question.covers(&waiter.owner, &waiter.repo) {
                                waiter.show_status(&subject, false, cx);
                            }
                        }
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
        // An account removed or signed out while the picker listed it is asked about again,
        // without it.
        if !is_usable(&accounts, &login, cx) {
            log::info!(
                "{login} was chosen for {}, but it's gone or signed out",
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
                Ok(()) => log::info!("Lo uses {login} for {subject}"),
                Err(error) => log::warn!("remembering {login} for {subject}: {error:#}"),
            }
            this.update(cx, |this, cx| {
                this.shown = None;
                // It may have gone while its answer was written down.
                if is_usable(&accounts, &login, cx) {
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
                waiter.answer(Err(NotChosen::Dismissed {
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

/// The accounts signed in now, lowercased.
fn signed_in(accounts: &Entity<Accounts>, cx: &App) -> Vec<String> {
    let accounts = accounts.read(cx);
    accounts
        .accounts()
        .iter()
        .filter(|account| !accounts.is_signed_out(&account.login))
        .map(|account| account.login.to_ascii_lowercase())
        .collect()
}

/// Whether `login` is an account that's still signed in.
fn is_usable(accounts: &Entity<Accounts>, login: &str, cx: &App) -> bool {
    let accounts = accounts.read(cx);
    accounts.account(login).is_some() && !accounts.is_signed_out(login)
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
    /// An account remembered for its repository by now is among its accounts.
    Answer(Waiter, String),
    /// None of its accounts is signed in any more, so nothing can be chosen for the subject.
    NoneLeft(Waiter, String),
    Ask(Waiter),
}

/// What can be said for `waiter` from what's remembered now, without asking. Its accounts that
/// were removed or signed out since are dropped.
fn known(mut waiter: Waiter, accounts: &Entity<Accounts>, cx: &App) -> Known {
    waiter.logins.retain(|login| is_usable(accounts, login, cx));
    let remembered = accounts
        .read(cx)
        .remembered(&waiter.owner, &waiter.repo)
        .inspect_err(|error| log::warn!("reading the account for {}: {error:#}", waiter.owner))
        .unwrap_or_default();
    if let Some(login) = remembered.pick(&waiter.logins).cloned() {
        return Known::Answer(waiter, login);
    }
    // Even with one account left, it's asked rather than taken: the tab never quietly opens as
    // another account than the one chosen.
    if waiter.logins.is_empty() {
        let subject = subject(&waiter.owner, &waiter.repo, remembered.owner.is_some());
        Known::NoneLeft(waiter, subject)
    } else {
        Known::Ask(waiter)
    }
}

/// What the picker says when it closes.
enum Reply {
    Chose(String),
    Dismissed,
    /// Accounts… or an account dropping out of it set it aside, to ask again later.
    SetAside,
    /// No tab needs the answer any more.
    Moot,
}

/// The picker: which of several accounts to use for an owner's repositories, or for one
/// repository.
pub struct AccountPicker {
    question: Question,
    selected: usize,
    focus_handle: FocusHandle,
    /// Where the answer goes, until it's given.
    reply: Option<oneshot::Sender<Reply>>,
    _keep_focus: Subscription,
}

impl AccountPicker {
    fn new(
        question: Question,
        reply: oneshot::Sender<Reply>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        // A tab opening while it waits for an answer, as from a link, takes the keyboard back
        // from nothing: the picker keeps it until it's answered.
        let keep_focus = cx.on_focus_out(&focus_handle, window, |this, _, window, cx| {
            if this.reply.is_some() {
                cx.defer_in(window, |this, window, cx| {
                    if this.reply.is_some() {
                        window.focus(&this.focus_handle, cx);
                    }
                });
            }
        });
        Self {
            question,
            selected: 0,
            focus_handle,
            reply: Some(reply),
            _keep_focus: keep_focus,
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

    /// Sends `reply`, unless one was sent, and closes.
    fn reply(&mut self, reply: Reply, cx: &mut Context<Self>) {
        if let Some(sender) = self.reply.take() {
            sender.send(reply).ok();
        }
        cx.emit(DismissEvent);
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
        match self.selected().map(str::to_string) {
            Some(login) => self.reply(Reply::Chose(login), cx),
            None => self.reply(Reply::SetAside, cx),
        }
    }

    fn cancel(&mut self, _: &menu::Cancel, _: &mut Window, cx: &mut Context<Self>) {
        self.reply(Reply::Dismissed, cx);
    }
}

impl EventEmitter<DismissEvent> for AccountPicker {}

impl ModalView for AccountPicker {
    /// Only an answer, escape or being set aside closes the picker: the tabs waiting on it keep
    /// waiting while another dialog tries to open or a click lands elsewhere.
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
                "More than one of your accounts can see {owner}/{repo}. Lo uses the one you \
                 choose for every {owner} repository."
            ),
            Some(owners_account) => format!(
                "{owners_account}, the account Lo uses for {owner}, can't see {owner}/{repo}. \
                 Lo uses the one you choose for {owner}/{repo} only, and {owners_account} for \
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
                        "Which account should Lo use for {subject}?"
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
