//! The Accounts modal: the GitHub accounts rozed is signed in to, signing in with GitHub's device
//! flow, and importing accounts from the `gh` CLI.
//!
//! Opened from the command palette (`accounts: open accounts`, `accounts: sign in`,
//! `accounts: import from gh`) and the app menu. Up and down move the selection, enter acts on
//! it, space picks accounts to import, cmd-backspace removes the selected account, and escape
//! goes back or closes.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::anyhow;
use github::accounts::Accounts;
use github::db::{Account, AccountSource, RozedDb};
use github::device_flow::{self, DeviceFlowError};
use github::gh_import::{self, GhAccount};
use github::{GithubError, HelpLink};
use gpui::{
    App, AppContext as _, ClipboardItem, Context, DismissEvent, Entity, EventEmitter, FocusHandle,
    Focusable, Global, KeyBinding, Render, SharedString, Subscription, Task, Window, actions,
};
use http_client::HttpClient;
use ui::{ListItem, ToggleState, prelude::*};
use util::ResultExt as _;
use workspace::{ModalView, MultiWorkspace, Workspace};

actions!(
    accounts,
    [
        /// Shows the GitHub accounts rozed is signed in to.
        OpenAccounts,
        /// Signs in to GitHub with a code entered on github.com.
        SignIn,
        /// Imports accounts from the gh CLI.
        ImportFromGh,
        /// Removes the selected account and its token.
        RemoveSelected,
        /// Picks or unpicks the selected gh account for import.
        ToggleChosen,
    ]
);

const KEY_CONTEXT: &str = "AccountsModal";

/// Set when rozed's database couldn't be opened, so the modal can say why accounts are missing.
struct AccountsUnavailable(SharedString);

impl Global for AccountsUnavailable {}

/// Opens rozed's database, loads the accounts and registers the modal's actions. Call it after
/// the app's HTTP client is set.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("space", ToggleChosen, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-backspace", RemoveSelected, Some(KEY_CONTEXT)),
    ]);
    // The app menu enables an item only when something on the focused element's path handles
    // its action, and before anything in the window is focused nothing is. This handler keeps
    // "Accounts…" working then. It runs while the window that dispatched the action is being
    // updated, so it defers opening the modal until that's done.
    cx.on_action(|_: &OpenAccounts, cx| cx.defer(|cx| open_in_any_window(Start::List, cx)));
    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace
            .register_action(|workspace, _: &OpenAccounts, window, cx| {
                AccountsModal::open(workspace, Start::List, window, cx)
            })
            .register_action(|workspace, _: &SignIn, window, cx| {
                AccountsModal::open(workspace, Start::SignIn, window, cx)
            })
            .register_action(|workspace, _: &ImportFromGh, window, cx| {
                AccountsModal::open(workspace, Start::Import, window, cx)
            });
    })
    .detach();

    let http = cx.http_client();
    let store = github::credentials::token_store();
    let path = github::db::default_path();
    cx.spawn(async move |cx| {
        let loaded = match RozedDb::open(&path).await {
            Ok(db) => Accounts::new(db, store, http),
            Err(error) => Err(error),
        };
        cx.update(|cx| match loaded {
            Ok(accounts) => {
                let accounts = cx.new(|_| accounts);
                Accounts::set_global(accounts, cx);
            }
            Err(error) => {
                log::error!("accounts are unavailable: {error:#}");
                cx.set_global(AccountsUnavailable(format!("{error:#}").into()));
            }
        })
    })
    .detach();
}

/// Opens the modal in the active rozed window, or the first one.
fn open_in_any_window(start: Start, cx: &mut App) {
    let window = cx
        .active_window()
        .and_then(|window| window.downcast::<MultiWorkspace>())
        .or_else(|| {
            cx.windows()
                .into_iter()
                .find_map(|window| window.downcast::<MultiWorkspace>())
        });
    let Some(window) = window else {
        log::info!("no rozed window to show accounts in");
        return;
    };
    window
        .update(cx, |multi_workspace, window, cx| {
            window.activate_window();
            multi_workspace
                .workspace()
                .clone()
                .update(cx, |workspace, cx| {
                    AccountsModal::open(workspace, start, window, cx)
                });
        })
        .log_err();
}

/// What the modal shows first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Start {
    List,
    SignIn,
    Import,
}

/// What the modal is doing.
enum Mode {
    List,
    SignIn(SignInStep),
    Import(ImportStep),
}

enum SignInStep {
    RequestingCode,
    WaitingForUser {
        user_code: SharedString,
        verification_uri: String,
        expires_at: Instant,
    },
    Finishing,
}

enum ImportStep {
    Loading,
    Choose {
        gh: PathBuf,
        accounts: Vec<GhAccount>,
        chosen: BTreeSet<String>,
    },
    Importing,
}

/// The outcome of the last sign-in or import, shown under the list.
#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    pub message: SharedString,
    pub is_error: bool,
    pub link: Option<HelpLink>,
}

impl Notice {
    fn info(message: impl Into<SharedString>) -> Self {
        Self {
            message: message.into(),
            is_error: false,
            link: None,
        }
    }

    /// An error, with GitHub's link for fixing it when it has one: authorizing single sign-on,
    /// or asking an organization to approve rozed.
    pub fn error(error: &anyhow::Error) -> Self {
        let github = error.downcast_ref::<GithubError>().or_else(|| {
            match error.downcast_ref::<DeviceFlowError>() {
                Some(DeviceFlowError::Github(github)) => Some(github),
                _ => None,
            }
        });
        let message = match github {
            Some(GithubError::OAuthAppRestricted { org: Some(org), .. }) => {
                format!("{org} hasn't approved rozed. Request approval on GitHub.")
            }
            Some(GithubError::SsoRequired { .. }) => {
                "This organization requires single sign-on. Authorize rozed's token on GitHub."
                    .to_string()
            }
            _ => format!("{error:#}"),
        };
        Self {
            message: message.into(),
            is_error: true,
            link: github.and_then(GithubError::help_link),
        }
    }
}

pub struct AccountsModal {
    accounts: Option<Entity<Accounts>>,
    unavailable: Option<SharedString>,
    http: Arc<dyn HttpClient>,
    focus_handle: FocusHandle,
    mode: Mode,
    selected: usize,
    notice: Option<Notice>,
    /// Where to find `gh`, when not searched for (tests).
    gh_override: Option<PathBuf>,
    /// The sign-in or import in progress. Dropping it cancels it.
    task: Option<Task<()>>,
    /// Redraws the sign-in countdown every second.
    ticker: Option<Task<()>>,
    _observe_accounts: Option<Subscription>,
}

impl EventEmitter<DismissEvent> for AccountsModal {}

impl ModalView for AccountsModal {}

impl Focusable for AccountsModal {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl AccountsModal {
    pub fn open(
        workspace: &mut Workspace,
        start: Start,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let accounts = Accounts::global(cx);
        let unavailable = cx
            .try_global::<AccountsUnavailable>()
            .map(|unavailable| unavailable.0.clone());
        let http = cx.http_client();
        workspace.toggle_modal(window, cx, move |window, cx| {
            let mut modal = Self::new(accounts, unavailable, http, cx);
            window.focus(&modal.focus_handle, cx);
            modal.start(start, cx);
            modal
        });
    }

    pub fn new(
        accounts: Option<Entity<Accounts>>,
        unavailable: Option<SharedString>,
        http: Arc<dyn HttpClient>,
        cx: &mut Context<Self>,
    ) -> Self {
        let observe_accounts = accounts.as_ref().map(|accounts| {
            cx.observe(accounts, |this, _, cx| {
                this.clamp_selection(cx);
                cx.notify();
            })
        });
        Self {
            accounts,
            unavailable,
            http,
            focus_handle: cx.focus_handle(),
            mode: Mode::List,
            selected: 0,
            notice: None,
            gh_override: None,
            task: None,
            ticker: None,
            _observe_accounts: observe_accounts,
        }
    }

    pub fn start(&mut self, start: Start, cx: &mut Context<Self>) {
        match start {
            Start::List => self.show_list(cx),
            Start::SignIn => self.start_sign_in(cx),
            Start::Import => self.start_import(cx),
        }
    }

    fn account_list(&self, cx: &App) -> Vec<Account> {
        self.accounts
            .as_ref()
            .map(|accounts| accounts.read(cx).accounts().to_vec())
            .unwrap_or_default()
    }

    /// How many rows the selection moves over: accounts then the two actions in the list, gh's
    /// accounts when importing.
    fn row_count(&self, cx: &App) -> usize {
        match &self.mode {
            Mode::List => self.account_list(cx).len() + 2,
            Mode::Import(ImportStep::Choose { accounts, .. }) => accounts.len(),
            _ => 0,
        }
    }

    fn clamp_selection(&mut self, cx: &App) {
        self.selected = self.selected.min(self.row_count(cx).saturating_sub(1));
    }

    fn show_list(&mut self, cx: &mut Context<Self>) {
        self.task = None;
        self.ticker = None;
        self.mode = Mode::List;
        self.clamp_selection(cx);
        cx.notify();
    }

    fn finish(&mut self, notice: Notice, cx: &mut Context<Self>) {
        self.notice = Some(notice);
        self.show_list(cx);
    }

    fn require_accounts(&mut self, cx: &mut Context<Self>) -> Option<Entity<Accounts>> {
        if self.accounts.is_none() {
            let reason = self
                .unavailable
                .clone()
                .unwrap_or_else(|| "rozed's database isn't open yet".into());
            self.finish(
                Notice {
                    message: format!("Accounts are unavailable: {reason}").into(),
                    is_error: true,
                    link: None,
                },
                cx,
            );
        }
        self.accounts.clone()
    }

    pub fn start_sign_in(&mut self, cx: &mut Context<Self>) {
        let Some(accounts) = self.require_accounts(cx) else {
            return;
        };
        self.notice = None;
        self.mode = Mode::SignIn(SignInStep::RequestingCode);
        cx.notify();
        let http = self.http.clone();
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = async {
                let codes = device_flow::request_codes(&http).await?;
                this.update(cx, |this, cx| {
                    // Copy the code and open GitHub's page, so the user only has to paste.
                    cx.write_to_clipboard(ClipboardItem::new_string(codes.user_code.clone()));
                    cx.open_url(&codes.verification_uri);
                    this.mode = Mode::SignIn(SignInStep::WaitingForUser {
                        user_code: codes.user_code.clone().into(),
                        verification_uri: codes.verification_uri.clone(),
                        expires_at: Instant::now() + codes.expires_in,
                    });
                    this.start_ticker(cx);
                    cx.notify();
                })?;
                let executor = cx.background_executor().clone();
                let token = device_flow::wait_for_token(&http, &codes, &executor).await?;
                this.update(cx, |this, cx| {
                    this.ticker = None;
                    this.mode = Mode::SignIn(SignInStep::Finishing);
                    cx.notify();
                })?;
                let user = device_flow::check_token(&http, token.clone()).await?;
                accounts
                    .update(cx, |accounts, cx| {
                        accounts.add(user, token, AccountSource::DeviceFlow, cx)
                    })
                    .await
            }
            .await;
            this.update(cx, |this, cx| {
                let notice = match result {
                    Ok(account) => Notice::info(format!("Signed in as {}.", account.login)),
                    Err(error) => Notice::error(&error),
                };
                this.finish(notice, cx);
            })
            .ok();
        }));
    }

    fn start_ticker(&mut self, cx: &mut Context<Self>) {
        self.ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        }));
    }

    pub fn start_import(&mut self, cx: &mut Context<Self>) {
        let Some(accounts) = self.require_accounts(cx) else {
            return;
        };
        self.notice = None;
        self.mode = Mode::Import(ImportStep::Loading);
        cx.notify();
        let gh_override = self.gh_override.clone();
        self.task = Some(cx.spawn(async move |this, cx| {
            let listed = cx
                .background_spawn(async move {
                    let gh = gh_override.or_else(gh_import::find_gh).ok_or_else(|| {
                        anyhow!("rozed couldn't find gh. Install it, or set GH_PATH to its path.")
                    })?;
                    let accounts = gh_import::list_accounts(&gh)?;
                    anyhow::Ok((gh, accounts))
                })
                .await;
            this.update(cx, |this, cx| match listed {
                Ok((_, gh_accounts)) if gh_accounts.is_empty() => {
                    this.finish(
                        Notice::info("gh isn't signed in to any github.com account."),
                        cx,
                    );
                }
                Ok((gh, gh_accounts)) => {
                    // Pick the usable accounts rozed doesn't have yet.
                    let known = accounts.read(cx);
                    let chosen = gh_accounts
                        .iter()
                        .filter(|account| {
                            account.is_usable() && known.account(&account.login).is_none()
                        })
                        .map(|account| account.login.clone())
                        .collect();
                    this.mode = Mode::Import(ImportStep::Choose {
                        gh,
                        accounts: gh_accounts,
                        chosen,
                    });
                    this.selected = 0;
                    this.task = None;
                    cx.notify();
                }
                Err(error) => this.finish(Notice::error(&error), cx),
            })
            .ok();
        }));
    }

    fn toggle_chosen(&mut self, _: &ToggleChosen, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_choice(self.selected, cx);
    }

    pub fn toggle_choice(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Mode::Import(ImportStep::Choose {
            accounts, chosen, ..
        }) = &mut self.mode
            && let Some(account) = accounts.get(index)
            && account.is_usable()
            && !chosen.remove(&account.login)
        {
            chosen.insert(account.login.clone());
        }
        self.selected = index;
        cx.notify();
    }

    pub fn import_chosen(&mut self, cx: &mut Context<Self>) {
        let Some(accounts) = self.accounts.clone() else {
            return;
        };
        let Mode::Import(ImportStep::Choose { gh, chosen, .. }) = &self.mode else {
            return;
        };
        if chosen.is_empty() {
            return;
        }
        let (gh, logins) = (gh.clone(), chosen.iter().cloned().collect::<Vec<_>>());
        self.mode = Mode::Import(ImportStep::Importing);
        cx.notify();
        let http = self.http.clone();
        self.task = Some(cx.spawn(async move |this, cx| {
            let mut imported = Vec::new();
            let mut failure = None;
            for login in logins {
                let executor = cx.background_executor().clone();
                let result = async {
                    let (user, token) = gh_import::import_account(
                        http.clone(),
                        gh.clone(),
                        login.clone(),
                        executor,
                    )
                    .await?;
                    accounts
                        .update(cx, |accounts, cx| {
                            accounts.add(user, token, AccountSource::Gh, cx)
                        })
                        .await
                }
                .await;
                match result {
                    Ok(account) => imported.push(account.login),
                    Err(error) => {
                        failure = Some(error.context(format!("importing {login}")));
                        break;
                    }
                }
            }
            this.update(cx, |this, cx| {
                let notice = match failure {
                    Some(error) => Notice::error(&error),
                    None => Notice::info(format!("Imported {}.", imported.join(", "))),
                };
                this.finish(notice, cx);
            })
            .ok();
        }));
    }

    pub fn remove(&mut self, login: &str, cx: &mut Context<Self>) {
        let Some(accounts) = self.accounts.clone() else {
            return;
        };
        let task = accounts.update(cx, |accounts, cx| accounts.remove(login, cx));
        let login = login.to_string();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.notice = Some(match result {
                    Ok(()) => Notice::info(format!("Removed {login}.")),
                    Err(error) => Notice::error(&error),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn remove_selected(&mut self, _: &RemoveSelected, _: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.mode, Mode::List)
            && let Some(account) = self.account_list(cx).get(self.selected)
        {
            self.remove(&account.login.clone(), cx);
        }
    }

    fn select_next(&mut self, _: &menu::SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        let count = self.row_count(cx);
        if count > 0 {
            self.selected = (self.selected + 1) % count;
            cx.notify();
        }
    }

    fn select_previous(
        &mut self,
        _: &menu::SelectPrevious,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let count = self.row_count(cx);
        if count > 0 {
            self.selected = (self.selected + count - 1) % count;
            cx.notify();
        }
    }

    fn confirm(&mut self, _: &menu::Confirm, _: &mut Window, cx: &mut Context<Self>) {
        match &self.mode {
            Mode::List => {
                let account_count = self.account_list(cx).len();
                if self.selected == account_count {
                    self.start_sign_in(cx);
                } else if self.selected == account_count + 1 {
                    self.start_import(cx);
                }
            }
            Mode::SignIn(SignInStep::WaitingForUser {
                user_code,
                verification_uri,
                ..
            }) => {
                cx.write_to_clipboard(ClipboardItem::new_string(user_code.to_string()));
                cx.open_url(verification_uri);
            }
            Mode::Import(ImportStep::Choose { .. }) => self.import_chosen(cx),
            _ => {}
        }
    }

    fn cancel(&mut self, _: &menu::Cancel, _: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.mode, Mode::List) {
            cx.emit(DismissEvent);
        } else {
            self.show_list(cx);
        }
    }
}

/// What tests look at.
#[cfg(test)]
impl AccountsModal {
    /// Uses this `gh` instead of searching for one.
    pub fn with_gh(mut self, gh: PathBuf) -> Self {
        self.gh_override = Some(gh);
        self
    }

    pub fn notice(&self) -> Option<&Notice> {
        self.notice.as_ref()
    }

    /// The code being shown for the device flow, if any.
    pub fn user_code(&self) -> Option<&str> {
        match &self.mode {
            Mode::SignIn(SignInStep::WaitingForUser { user_code, .. }) => Some(user_code),
            _ => None,
        }
    }

    /// The gh accounts offered for import, with whether each is picked.
    pub fn import_choices(&self) -> Vec<(String, bool)> {
        match &self.mode {
            Mode::Import(ImportStep::Choose {
                accounts, chosen, ..
            }) => accounts
                .iter()
                .map(|account| (account.login.clone(), chosen.contains(&account.login)))
                .collect(),
            _ => Vec::new(),
        }
    }

    pub fn is_listing(&self) -> bool {
        matches!(self.mode, Mode::List)
    }
}

fn source_label(source: AccountSource) -> &'static str {
    match source {
        AccountSource::DeviceFlow => "Signed in with GitHub",
        AccountSource::Gh => "Imported from gh",
    }
}

fn format_remaining(remaining: Duration) -> String {
    let seconds = remaining.as_secs();
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

impl AccountsModal {
    fn render_account(&self, index: usize, account: &Account, cx: &mut Context<Self>) -> ListItem {
        let signed_out = self
            .accounts
            .as_ref()
            .is_some_and(|accounts| accounts.read(cx).is_signed_out(&account.login));
        let missing = github::missing_scopes(&account.scopes);
        let mut detail = vec![source_label(account.source).to_string()];
        if let Some(name) = &account.name {
            detail.insert(0, name.clone());
        }
        let warning = if signed_out {
            Some("Signed out: GitHub refused its token. Sign in again.".to_string())
        } else if !missing.is_empty() {
            Some(format!("Missing scopes: {}", missing.join(", ")))
        } else {
            None
        };
        let login = account.login.clone();
        ListItem::new(("account", index))
            .inset(true)
            .toggle_state(self.selected == index)
            .start_slot(Icon::new(IconName::Person).color(Color::Muted))
            .child(
                v_flex()
                    .child(Label::new(account.login.clone()))
                    .child(
                        Label::new(detail.join(" · "))
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .children(warning.map(|warning| {
                        Label::new(warning)
                            .size(LabelSize::Small)
                            .color(Color::Warning)
                    })),
            )
            .end_slot(
                Button::new(("remove", index), "Remove")
                    .style(ButtonStyle::Subtle)
                    .on_click(cx.listener(move |this, _, _, cx| this.remove(&login, cx))),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.selected = index;
                cx.notify();
            }))
    }

    fn render_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let accounts = self.account_list(cx);
        let count = accounts.len();
        let rows = accounts
            .iter()
            .enumerate()
            .map(|(index, account)| self.render_account(index, account, cx))
            .collect::<Vec<_>>();
        v_flex()
            .gap_1()
            .when(count == 0, |this| {
                this.child(
                    div().px_2().py_1().child(
                        Label::new(
                            "No accounts yet. Sign in with GitHub, or import gh's accounts.",
                        )
                        .color(Color::Muted),
                    ),
                )
            })
            .children(rows)
            .child(
                ListItem::new("sign-in")
                    .inset(true)
                    .toggle_state(self.selected == count)
                    .start_slot(Icon::new(IconName::Github))
                    .child(Label::new("Sign in with GitHub"))
                    .on_click(cx.listener(|this, _, _, cx| this.start_sign_in(cx))),
            )
            .child(
                ListItem::new("import")
                    .inset(true)
                    .toggle_state(self.selected == count + 1)
                    .start_slot(Icon::new(IconName::Download))
                    .child(Label::new("Import from gh"))
                    .on_click(cx.listener(|this, _, _, cx| this.start_import(cx))),
            )
            .into_any_element()
    }

    fn render_sign_in(&self, step: &SignInStep, cx: &mut Context<Self>) -> AnyElement {
        match step {
            SignInStep::RequestingCode => Label::new("Asking GitHub for a code…")
                .color(Color::Muted)
                .into_any_element(),
            SignInStep::Finishing => Label::new("Signed in. Checking the account…")
                .color(Color::Muted)
                .into_any_element(),
            SignInStep::WaitingForUser {
                user_code,
                verification_uri,
                expires_at,
            } => {
                let remaining = expires_at.saturating_duration_since(Instant::now());
                let copied = cx
                    .read_from_clipboard()
                    .and_then(|item| item.text())
                    .is_some_and(|text| text == user_code.as_ref());
                let (code, uri) = (user_code.clone(), verification_uri.clone());
                v_flex()
                    .gap_2()
                    .items_center()
                    .child(Label::new("Enter this code on GitHub:").color(Color::Muted))
                    .child(
                        div()
                            .px_4()
                            .py_2()
                            .rounded_md()
                            .bg(cx.theme().colors().element_background)
                            .border_1()
                            .border_color(cx.theme().colors().border)
                            .child(Headline::new(user_code.clone()).size(HeadlineSize::XLarge)),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new(
                                    "copy-code",
                                    if copied { "Copied" } else { "Copy code" },
                                )
                                .start_icon(Icon::new(if copied {
                                    IconName::Check
                                } else {
                                    IconName::Copy
                                }))
                                .on_click(move |_, window, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        code.to_string(),
                                    ));
                                    window.refresh();
                                }),
                            )
                            .child(
                                Button::new("open-github", "Open github.com/login/device")
                                    .end_icon(Icon::new(IconName::ArrowUpRight))
                                    .on_click(move |_, _, cx| cx.open_url(&uri)),
                            ),
                    )
                    .child(
                        Label::new(format!(
                            "Waiting for GitHub… The code expires in {}.",
                            format_remaining(remaining)
                        ))
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                    )
                    .into_any_element()
            }
        }
    }

    fn render_import(&self, step: &ImportStep, cx: &mut Context<Self>) -> AnyElement {
        match step {
            ImportStep::Loading => Label::new("Asking gh for its accounts…")
                .color(Color::Muted)
                .into_any_element(),
            ImportStep::Importing => Label::new("Importing…")
                .color(Color::Muted)
                .into_any_element(),
            ImportStep::Choose {
                gh,
                accounts,
                chosen,
            } => v_flex()
                .gap_1()
                .child(
                    div().px_2().child(
                        Label::new(format!("Accounts in {}", gh.display()))
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
                )
                .children(accounts.iter().enumerate().map(|(index, account)| {
                    let mut notes = Vec::new();
                    if account.active {
                        notes.push("active in gh".to_string());
                    }
                    if !account.is_usable() {
                        notes.push(format!("gh reports {}", account.state));
                    }
                    if account.refreshable {
                        notes.push("short-lived token".to_string());
                    }
                    let missing = account.missing_scopes();
                    if !missing.is_empty() {
                        notes.push(format!(
                            "no {} scope: can't merge PRs that change workflows",
                            missing.join(", ")
                        ));
                    }
                    let already = self
                        .accounts
                        .as_ref()
                        .is_some_and(|a| a.read(cx).account(&account.login).is_some());
                    if already {
                        notes.push("already in rozed; importing replaces its token".to_string());
                    }
                    let checked = if chosen.contains(&account.login) {
                        ToggleState::Selected
                    } else {
                        ToggleState::Unselected
                    };
                    ListItem::new(("gh-account", index))
                        .inset(true)
                        .toggle_state(self.selected == index)
                        .start_slot(
                            ui::Checkbox::new(("gh-check", index), checked)
                                .disabled(!account.is_usable()),
                        )
                        .child(v_flex().child(Label::new(account.login.clone())).when(
                            !notes.is_empty(),
                            |this| {
                                this.child(
                                    Label::new(notes.join(" · ")).size(LabelSize::Small).color(
                                        if missing.is_empty() && account.is_usable() {
                                            Color::Muted
                                        } else {
                                            Color::Warning
                                        },
                                    ),
                                )
                            },
                        ))
                        .on_click(cx.listener(move |this, _, _, cx| this.toggle_choice(index, cx)))
                }))
                .child(
                    h_flex().pt_2().px_2().justify_end().child(
                        Button::new("import-chosen", "Import")
                            .style(ButtonStyle::Filled)
                            .disabled(chosen.is_empty())
                            .on_click(cx.listener(|this, _, _, cx| this.import_chosen(cx))),
                    ),
                )
                .into_any_element(),
        }
    }

    fn render_notice(&self, notice: &Notice) -> AnyElement {
        h_flex()
            .px_2()
            .gap_2()
            .flex_wrap()
            .child(
                Icon::new(if notice.is_error {
                    IconName::Warning
                } else {
                    IconName::Check
                })
                .size(IconSize::Small)
                .color(if notice.is_error {
                    Color::Error
                } else {
                    Color::Success
                }),
            )
            .child(Label::new(notice.message.clone()).size(LabelSize::Small))
            .children(notice.link.clone().map(|link| {
                Button::new("help-link", link.label)
                    .end_icon(Icon::new(IconName::ArrowUpRight))
                    .on_click(move |_, _, cx| cx.open_url(&link.url))
            }))
            .into_any_element()
    }
}

impl Render for AccountsModal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (title, body) = match &self.mode {
            Mode::List => ("GitHub accounts", self.render_list(cx)),
            Mode::SignIn(step) => ("Sign in with GitHub", self.render_sign_in(step, cx)),
            Mode::Import(step) => ("Import from gh", self.render_import(step, cx)),
        };
        let hint = match &self.mode {
            Mode::List => "↑↓ select · enter open · ⌘⌫ remove · esc close",
            Mode::SignIn(_) => "enter copy the code and open GitHub · esc cancel",
            Mode::Import(_) => "↑↓ select · space pick · enter import · esc back",
        };
        v_flex()
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::toggle_chosen))
            .on_action(cx.listener(Self::remove_selected))
            .w(rems(34.))
            .elevation_3(cx)
            .overflow_hidden()
            .child(
                h_flex()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .border_b_1()
                    .border_color(cx.theme().colors().border_variant)
                    .child(Icon::new(IconName::Github))
                    .child(Headline::new(title).size(HeadlineSize::Small)),
            )
            .child(div().p_2().child(body))
            .children(
                self.notice
                    .as_ref()
                    .map(|notice| div().pb_2().child(self.render_notice(notice))),
            )
            .child(
                div()
                    .px_3()
                    .py_1()
                    .border_t_1()
                    .border_color(cx.theme().colors().border_variant)
                    .child(Label::new(hint).size(LabelSize::XSmall).color(Color::Muted)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::os::unix::fs::PermissionsExt as _;

    use credentials_provider::test_support::FakeCredentialsProvider;
    use github::replay::ReplayHttpClient;
    use gpui::TestAppContext;

    async fn accounts(
        name: &str,
        http: Arc<dyn HttpClient>,
        cx: &mut TestAppContext,
    ) -> Entity<Accounts> {
        let db = RozedDb::open_test(name).await;
        let store = Arc::new(FakeCredentialsProvider::default());
        cx.new(|_| Accounts::new(db, store, http).unwrap())
    }

    /// Runs what's ready without moving the fake clock, so a sign-in stops at its next poll.
    fn run_ready(cx: &mut TestAppContext) {
        while cx.executor().tick() {}
    }

    /// Moves the fake clock on a second at a time, running what comes due.
    fn wait(seconds: u64, cx: &mut TestAppContext) {
        for _ in 0..seconds {
            cx.executor().advance_clock(Duration::from_secs(1));
            cx.run_until_parked();
        }
    }

    #[gpui::test]
    async fn signing_in_shows_the_code_then_adds_the_account(cx: &mut TestAppContext) {
        let http: Arc<dyn HttpClient> = ReplayHttpClient::cassette("device_flow_success");
        let accounts = accounts("ui_sign_in", http.clone(), cx).await;
        let modal = cx.new(|cx| AccountsModal::new(Some(accounts.clone()), None, http, cx));

        modal.update(cx, |modal, cx| modal.start(Start::SignIn, cx));
        run_ready(cx);
        modal.read_with(cx, |modal, _| {
            assert_eq!(modal.user_code(), Some("FAKE-1234"))
        });
        // The code is on the clipboard and GitHub's page is open.
        assert_eq!(
            cx.read_from_clipboard()
                .and_then(|item| item.text())
                .as_deref(),
            Some("FAKE-1234")
        );
        assert_eq!(
            cx.opened_url().as_deref(),
            Some("https://github.com/login/device")
        );

        // Polls go out 6, 12 and 18 s in; the third gets the token.
        wait(10, cx);
        modal.read_with(cx, |modal, _| {
            assert_eq!(modal.user_code(), Some("FAKE-1234"))
        });
        wait(10, cx);
        modal.read_with(cx, |modal, _| {
            assert!(modal.is_listing());
            assert_eq!(
                modal.notice().map(|n| n.message.as_ref()),
                Some("Signed in as rozed-bot.")
            );
        });
        accounts.read_with(cx, |accounts, _| {
            let account = &accounts.accounts()[0];
            assert_eq!(account.login, "rozed-bot");
            assert_eq!(account.source, AccountSource::DeviceFlow);
        });
    }

    #[gpui::test]
    async fn a_failed_sign_in_says_why(cx: &mut TestAppContext) {
        let http: Arc<dyn HttpClient> = ReplayHttpClient::cassette("device_flow_denied");
        let accounts = accounts("ui_sign_in_denied", http.clone(), cx).await;
        let modal = cx.new(|cx| AccountsModal::new(Some(accounts.clone()), None, http, cx));
        modal.update(cx, |modal, cx| modal.start(Start::SignIn, cx));
        wait(15, cx);
        modal.read_with(cx, |modal, _| {
            let notice = modal.notice().unwrap();
            assert!(notice.is_error);
            assert_eq!(
                notice.message.as_ref(),
                "The sign-in was cancelled on GitHub."
            );
        });
        accounts.read_with(cx, |accounts, _| assert!(accounts.accounts().is_empty()));
    }

    const FAKE_GH: &str = r#"#!/bin/sh
case "$*" in
  "auth status --json hosts --hostname github.com")
    echo '{"hosts":{"github.com":[{"state":"success","active":true,"login":"rozed-bot","tokenSource":"keyring","scopes":"read:org, repo, workflow"},{"state":"error","active":false,"login":"broken","tokenSource":"keyring"}]}}' ;;
  "auth token --hostname github.com --user rozed-bot")
    echo gho_FAKE_FROM_GH ;;
  *) exit 1 ;;
esac
"#;

    #[gpui::test]
    async fn importing_lists_ghs_accounts_and_adds_the_chosen_ones(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let gh = dir.path().join("gh");
        std::fs::write(&gh, FAKE_GH).unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();

        let http: Arc<dyn HttpClient> = ReplayHttpClient::cassette("user");
        let accounts = accounts("ui_import", http.clone(), cx).await;
        let modal = cx.new(|cx| {
            AccountsModal::new(Some(accounts.clone()), None, http, cx).with_gh(gh.clone())
        });
        modal.update(cx, |modal, cx| modal.start(Start::Import, cx));
        cx.run_until_parked();
        // Usable accounts are picked; one gh can't use isn't, and can't be.
        modal.read_with(cx, |modal, _| {
            assert_eq!(
                modal.import_choices(),
                [
                    ("rozed-bot".to_string(), true),
                    ("broken".to_string(), false)
                ]
            );
        });
        modal.update(cx, |modal, cx| modal.toggle_choice(1, cx));
        modal.read_with(cx, |modal, _| assert!(!modal.import_choices()[1].1));

        modal.update(cx, |modal, cx| modal.import_chosen(cx));
        cx.run_until_parked();
        modal.read_with(cx, |modal, _| {
            assert_eq!(
                modal.notice().map(|n| n.message.as_ref()),
                Some("Imported rozed-bot.")
            );
        });
        accounts.read_with(cx, |accounts, _| {
            assert_eq!(accounts.accounts()[0].source, AccountSource::Gh);
        });

        // Removing it from the list forgets it.
        modal.update(cx, |modal, cx| modal.remove("rozed-bot", cx));
        cx.run_until_parked();
        accounts.read_with(cx, |accounts, _| assert!(accounts.accounts().is_empty()));
    }

    #[gpui::test]
    async fn without_a_database_the_modal_says_so(cx: &mut TestAppContext) {
        let http: Arc<dyn HttpClient> = ReplayHttpClient::cassette("user");
        let modal = cx.new(|cx| {
            AccountsModal::new(None, Some("couldn't open rozed.sqlite".into()), http, cx)
        });
        modal.update(cx, |modal, cx| modal.start(Start::SignIn, cx));
        modal.read_with(cx, |modal, _| {
            assert!(modal.is_listing());
            assert_eq!(
                modal.notice().unwrap().message.as_ref(),
                "Accounts are unavailable: couldn't open rozed.sqlite"
            );
        });
    }

    #[test]
    fn errors_link_to_githubs_fix() {
        let restricted = anyhow::Error::new(GithubError::OAuthAppRestricted {
            message: "Although you appear to have the correct authorization credentials, the `acme` organization has enabled OAuth App access restrictions".into(),
            org: Some("acme".into()),
            documentation_url: None,
        });
        let notice = Notice::error(&restricted.context("importing someone"));
        assert_eq!(
            notice.message.as_ref(),
            "acme hasn't approved rozed. Request approval on GitHub."
        );
        assert_eq!(
            notice.link.unwrap().url,
            "https://github.com/settings/connections/applications/Ov23liBORqwRdiPzqsPP"
        );

        let sso = anyhow::Error::new(DeviceFlowError::Github(GithubError::SsoRequired {
            message: "Resource protected by organization SAML enforcement.".into(),
            url: Some("https://github.com/orgs/acme/sso?authorization_request=X".into()),
        }));
        let notice = Notice::error(&sso);
        assert!(notice.is_error);
        assert_eq!(
            notice.link,
            Some(HelpLink {
                label: "Authorize on GitHub",
                url: "https://github.com/orgs/acme/sso?authorization_request=X".into()
            })
        );

        let plain = Notice::error(&anyhow!("gh couldn't list its accounts: boom"));
        assert_eq!(plain.link, None);
    }

    #[test]
    fn the_countdown_reads_as_minutes_and_seconds() {
        assert_eq!(format_remaining(Duration::from_secs(899)), "14:59");
        assert_eq!(format_remaining(Duration::from_secs(5)), "0:05");
    }
}
