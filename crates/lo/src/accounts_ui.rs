//! The Accounts modal: the GitHub accounts rozed is signed in to, signing in with GitHub's device
//! flow, and importing accounts from the `gh` CLI.
//!
//! Opened from the command palette (`accounts: open accounts`, `accounts: sign in`,
//! `accounts: import from gh`) and the app menu. Up and down move the selection, enter acts on
//! it, space picks accounts to import, cmd-backspace pressed twice removes the selected account,
//! and escape goes back or closes.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::anyhow;
use github::accounts::{Accounts, SignedOut};
use github::db::{Account, AccountSource, LoDb};
use github::device_flow::{self, DeviceFlowError};
use github::gh_import::{self, GhAccount};
use github::{GithubError, HelpLink};
use gpui::{
    App, AppContext as _, ClipboardItem, Context, DismissEvent, Entity, EventEmitter, FocusHandle,
    Focusable, Global, KeyBinding, Render, SharedString, Subscription, Task, Window, actions,
};
use http_client::HttpClient;
use ui::{ListItem, TintColor, ToggleState, prelude::*};
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
        /// Removes the selected account and its token, on a second press.
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
/// the app's HTTP client is set. The task resolves to the loaded accounts and their database, or
/// to `None` when the database couldn't be opened.
pub fn init(cx: &mut App) -> Task<Option<(Entity<Accounts>, LoDb)>> {
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
        let db = LoDb::open(&path).await;
        cx.update(|cx| {
            match db.and_then(|db| Ok((Accounts::load(db.clone(), store, http, cx)?, db))) {
                Ok((accounts, db)) => {
                    Accounts::set_global(accounts.clone(), cx);
                    Some((accounts, db))
                }
                Err(error) => {
                    log::error!("accounts are unavailable: {error:#}");
                    cx.set_global(AccountsUnavailable(format!("{error:#}").into()));
                    None
                }
            }
        })
    })
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
        expires_at: Instant,
        /// Whether rozed put the code on the clipboard. The modal never reads the clipboard to
        /// find out, since it may hold something else the user copied, such as a password.
        copied: bool,
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
    /// The account a first press of Remove asked about; a second press removes it.
    confirming_remove: Option<String>,
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
        // A question about which account to use waits until this closes. An account signed in
        // here sends the waiting tabs back to check which accounts can see their repositories,
        // so the picker then lists it.
        review_ui::account_picker::set_aside(workspace, cx);
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
            confirming_remove: None,
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
        self.confirming_remove = None;
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
                    this.mode = Mode::SignIn(SignInStep::WaitingForUser {
                        user_code: codes.user_code.clone().into(),
                        expires_at: Instant::now() + codes.expires_in,
                        copied: false,
                    });
                    // Copy the code and open GitHub's page, so the user only has to paste.
                    this.copy_code(cx);
                    cx.open_url(device_flow::VERIFICATION_URL);
                    this.start_ticker(cx);
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

    /// Puts the sign-in code on the clipboard.
    fn copy_code(&mut self, cx: &mut Context<Self>) {
        if let Mode::SignIn(SignInStep::WaitingForUser {
            user_code, copied, ..
        }) = &mut self.mode
        {
            cx.write_to_clipboard(ClipboardItem::new_string(user_code.to_string()));
            *copied = true;
            cx.notify();
        }
    }

    /// The copy button's label: "Copied" once rozed has put the code on the clipboard.
    pub fn copy_label(&self) -> &'static str {
        match &self.mode {
            Mode::SignIn(SignInStep::WaitingForUser { copied: true, .. }) => "Copied",
            _ => "Copy code",
        }
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
        let source = accounts
            .read(cx)
            .account(login)
            .map(|account| account.source);
        let task = accounts.update(cx, |accounts, cx| accounts.remove(login, cx));
        let login = login.to_string();
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.notice = Some(match result {
                    Ok(()) => removed_notice(&login, source),
                    Err(error) => Notice::error(&error),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Removes the account on a second press for it; the first only asks.
    pub fn request_remove(&mut self, login: &str, cx: &mut Context<Self>) {
        if self
            .confirming_remove
            .as_deref()
            .is_some_and(|asked| asked.eq_ignore_ascii_case(login))
        {
            self.confirming_remove = None;
            self.remove(login, cx);
        } else {
            self.confirming_remove = Some(login.to_string());
            self.notice = None;
        }
        cx.notify();
    }

    fn remove_selected(&mut self, _: &RemoveSelected, _: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.mode, Mode::List)
            && let Some(account) = self.account_list(cx).get(self.selected)
        {
            self.request_remove(&account.login.clone(), cx);
        }
    }

    fn select_next(&mut self, _: &menu::SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        let count = self.row_count(cx);
        if count > 0 {
            self.selected = (self.selected + 1) % count;
            self.confirming_remove = None;
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
            self.confirming_remove = None;
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
            Mode::SignIn(SignInStep::WaitingForUser { .. }) => {
                self.copy_code(cx);
                cx.open_url(device_flow::VERIFICATION_URL);
            }
            Mode::Import(ImportStep::Choose { .. }) => self.import_chosen(cx),
            _ => {}
        }
    }

    fn cancel(&mut self, _: &menu::Cancel, _: &mut Window, cx: &mut Context<Self>) {
        self.go_back(cx);
    }

    /// Escape: takes back a pending removal, else goes back to the list, else closes.
    pub fn go_back(&mut self, cx: &mut Context<Self>) {
        if self.confirming_remove.take().is_some() {
            cx.notify();
        } else if matches!(self.mode, Mode::List) {
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

    /// The account a first press of Remove asked about.
    pub fn confirming_remove(&self) -> Option<&str> {
        self.confirming_remove.as_deref()
    }
}

/// Where the user revokes an OAuth app's access to their account.
const AUTHORIZED_APPS_URL: &str = "https://github.com/settings/applications";

/// What removing an account did and didn't do. Removing deletes rozed's copy of the token, but
/// rozed has no client secret, so only GitHub's settings can revoke the grant behind it. An
/// imported token is gh's, and revoking it would sign gh out too.
fn removed_notice(login: &str, source: Option<AccountSource>) -> Notice {
    match source {
        Some(AccountSource::Gh) => Notice::info(format!(
            "Removed {login}. Its token is gh's, so gh stays signed in."
        )),
        _ => Notice {
            message: format!(
                "Removed {login}. rozed stays authorized on GitHub until you revoke it there.",
            )
            .into(),
            is_error: false,
            link: HelpLink::new("Revoke on GitHub", AUTHORIZED_APPS_URL),
        },
    }
}

/// What needs the user's attention about an account: why it's signed out, or the scopes its
/// token lacks.
fn account_warning(signed_out: Option<SignedOut>, scopes: &[String]) -> Option<String> {
    match signed_out {
        Some(SignedOut::Refused) => {
            return Some("Signed out: GitHub refused its token. Sign in again.".into());
        }
        Some(SignedOut::TokenMissing) => {
            return Some("Signed out: its token is missing. Sign in again.".into());
        }
        None => {}
    }
    // GitHub lists scopes only for OAuth and classic tokens. A GitHub App's user token or a
    // fine-grained one has permissions instead, which GitHub doesn't report.
    if scopes.is_empty() {
        return Some(
            "A token without OAuth scopes: what it can do depends on its permissions.".into(),
        );
    }
    let missing = github::missing_scopes(scopes);
    (!missing.is_empty()).then(|| format!("Missing scopes: {}", missing.join(", ")))
}

/// The notes beside an account in gh's list.
fn gh_account_notes(account: &GhAccount, already_in_lo: bool) -> Vec<String> {
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
    if account.is_usable() && account.scopes.is_empty() {
        notes.push("a token without OAuth scopes, such as a GitHub App's".to_string());
    } else {
        match account.missing_scopes().as_slice() {
            [] => {}
            ["workflow"] => {
                notes.push("no workflow scope: can't merge PRs that change workflows".to_string())
            }
            missing => notes.push(format!("missing scopes: {}", missing.join(", "))),
        }
    }
    if already_in_lo {
        notes.push("already in rozed; importing replaces its token".to_string());
    }
    notes
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
            .and_then(|accounts| accounts.read(cx).signed_out(&account.login));
        let mut detail = vec![source_label(account.source).to_string()];
        if let Some(name) = &account.name {
            detail.insert(0, name.clone());
        }
        let confirming = self
            .confirming_remove
            .as_deref()
            .is_some_and(|asked| asked.eq_ignore_ascii_case(&account.login));
        let warning = if confirming {
            Some("Press ⌘⌫ or Remove again to remove it from rozed.".to_string())
        } else {
            account_warning(signed_out, &account.scopes)
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
                Button::new(
                    ("remove", index),
                    if confirming { "Remove?" } else { "Remove" },
                )
                .style(if confirming {
                    ButtonStyle::Tinted(TintColor::Error)
                } else {
                    ButtonStyle::Subtle
                })
                .on_click(cx.listener(move |this, _, _, cx| this.request_remove(&login, cx))),
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
                expires_at,
                copied,
            } => {
                let remaining = expires_at.saturating_duration_since(Instant::now());
                let copied = *copied;
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
                                Button::new("copy-code", self.copy_label())
                                    .start_icon(Icon::new(if copied {
                                        IconName::Check
                                    } else {
                                        IconName::Copy
                                    }))
                                    .on_click(cx.listener(|this, _, _, cx| this.copy_code(cx))),
                            )
                            .child(
                                Button::new("open-github", "Open github.com/login/device")
                                    .end_icon(Icon::new(IconName::ArrowUpRight))
                                    .on_click(move |_, _, cx| {
                                        cx.open_url(device_flow::VERIFICATION_URL)
                                    }),
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
                    let already = self
                        .accounts
                        .as_ref()
                        .is_some_and(|a| a.read(cx).account(&account.login).is_some());
                    let notes = gh_account_notes(account, already);
                    let missing = account.missing_scopes();
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
                Button::new("help-link", link.label())
                    .end_icon(Icon::new(IconName::ArrowUpRight))
                    .on_click(move |_, _, cx| cx.open_url(link.url()))
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
        let db = LoDb::open_test(name).await;
        let store = Arc::new(FakeCredentialsProvider::default());
        cx.update(|cx| Accounts::load(db, store, http, cx)).unwrap()
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

    /// The successful sign-in, with GitHub's answer naming another page to enter the code on.
    fn sign_in_naming(verification_uri: &str) -> Arc<dyn HttpClient> {
        let dir = github::cassette::fixtures_dir().join("device_flow_success");
        let (_, interactions) = github::cassette::read_cassette(&dir).unwrap();
        let interactions = interactions.into_iter().map(|(_, mut interaction)| {
            if interaction.request.url == device_flow::DEVICE_CODE_URL {
                interaction.response.body.as_mut().unwrap()["verification_uri"] =
                    verification_uri.into();
            }
            interaction
        });
        Arc::new(ReplayHttpClient::from_interactions("sign_in", interactions).unwrap())
    }

    #[gpui::test]
    async fn signing_in_shows_the_code_then_adds_the_account(cx: &mut TestAppContext) {
        // Whatever page GitHub's answer names, rozed opens GitHub's own.
        let http = sign_in_naming("file:///Applications/Calculator.app");
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
        // "Copied" comes from what rozed did, not from reading the clipboard, which may hold
        // something the user copied since, such as a password.
        modal.read_with(cx, |modal, _| assert_eq!(modal.copy_label(), "Copied"));
        cx.write_to_clipboard(ClipboardItem::new_string("hunter2".into()));
        modal.read_with(cx, |modal, _| assert_eq!(modal.copy_label(), "Copied"));

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
    async fn removing_says_revoking_happens_on_github(cx: &mut TestAppContext) {
        let http: Arc<dyn HttpClient> = ReplayHttpClient::cassette("user");
        let accounts = accounts("ui_remove", http.clone(), cx).await;
        for (login, source) in [
            ("signed-in", AccountSource::DeviceFlow),
            ("from-gh", AccountSource::Gh),
        ] {
            let user = github::AuthenticatedUser {
                user: github::User {
                    login: login.into(),
                    id: 1,
                    name: None,
                    avatar_url: None,
                },
                scopes: vec!["read:org".into(), "repo".into(), "workflow".into()],
            };
            accounts
                .update(cx, |accounts, cx| {
                    accounts.add(user, github::Token::new("gho_FAKE"), source, cx)
                })
                .await
                .unwrap();
        }
        let modal = cx.new(|cx| AccountsModal::new(Some(accounts.clone()), None, http, cx));

        // rozed has no client secret, so it can't revoke its own grant: GitHub's settings can.
        modal.update(cx, |modal, cx| modal.remove("signed-in", cx));
        cx.run_until_parked();
        modal.read_with(cx, |modal, _| {
            let notice = modal.notice().unwrap();
            assert!(!notice.is_error);
            assert_eq!(
                notice.message.as_ref(),
                "Removed signed-in. rozed stays authorized on GitHub until you revoke it there."
            );
            assert_eq!(
                notice.link.as_ref().map(HelpLink::url),
                Some("https://github.com/settings/applications")
            );
        });

        // An imported token is gh's: revoking it would sign gh out too.
        modal.update(cx, |modal, cx| modal.remove("from-gh", cx));
        cx.run_until_parked();
        modal.read_with(cx, |modal, _| {
            let notice = modal.notice().unwrap();
            assert_eq!(
                notice.message.as_ref(),
                "Removed from-gh. Its token is gh's, so gh stays signed in."
            );
            assert_eq!(notice.link, None);
        });
        accounts.read_with(cx, |accounts, _| assert!(accounts.accounts().is_empty()));
    }

    #[gpui::test]
    async fn removing_takes_a_second_press(cx: &mut TestAppContext) {
        let http: Arc<dyn HttpClient> = ReplayHttpClient::cassette("user");
        let accounts = accounts("ui_remove_confirm", http.clone(), cx).await;
        for login in ["a", "b"] {
            let user = github::AuthenticatedUser {
                user: github::User {
                    login: login.into(),
                    id: 1,
                    name: None,
                    avatar_url: None,
                },
                scopes: vec!["read:org".into(), "repo".into(), "workflow".into()],
            };
            accounts
                .update(cx, |accounts, cx| {
                    accounts.add(user, github::Token::new("gho_FAKE"), AccountSource::Gh, cx)
                })
                .await
                .unwrap();
        }
        let modal = cx.new(|cx| AccountsModal::new(Some(accounts.clone()), None, http, cx));
        let count = |cx: &mut TestAppContext| accounts.read_with(cx, |a, _| a.accounts().len());

        // The first press asks; escape then takes it back without closing the modal.
        modal.update(cx, |modal, cx| modal.request_remove("a", cx));
        cx.run_until_parked();
        assert_eq!(count(cx), 2);
        modal.read_with(cx, |modal, _| {
            assert_eq!(modal.confirming_remove(), Some("a"))
        });
        modal.update(cx, |modal, cx| modal.go_back(cx));
        modal.read_with(cx, |modal, _| {
            assert_eq!(modal.confirming_remove(), None);
            assert!(modal.is_listing());
        });

        // Pressing for another account asks about that one instead.
        modal.update(cx, |modal, cx| {
            modal.request_remove("a", cx);
            modal.request_remove("b", cx);
        });
        cx.run_until_parked();
        assert_eq!(count(cx), 2);

        // A second press removes it.
        modal.update(cx, |modal, cx| modal.request_remove("b", cx));
        cx.run_until_parked();
        assert_eq!(count(cx), 1);
        modal.read_with(cx, |modal, _| assert_eq!(modal.confirming_remove(), None));
    }

    #[gpui::test]
    async fn without_a_database_the_modal_says_so(cx: &mut TestAppContext) {
        let http: Arc<dyn HttpClient> = ReplayHttpClient::cassette("user");
        let modal =
            cx.new(|cx| AccountsModal::new(None, Some("couldn't open lo.sqlite".into()), http, cx));
        modal.update(cx, |modal, cx| modal.start(Start::SignIn, cx));
        modal.read_with(cx, |modal, _| {
            assert!(modal.is_listing());
            assert_eq!(
                modal.notice().unwrap().message.as_ref(),
                "Accounts are unavailable: couldn't open lo.sqlite"
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
            notice.link.unwrap().url(),
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
            HelpLink::new(
                "Authorize on GitHub",
                "https://github.com/orgs/acme/sso?authorization_request=X"
            )
        );

        let plain = Notice::error(&anyhow!("gh couldn't list its accounts: boom"));
        assert_eq!(plain.link, None);
    }

    #[test]
    fn an_account_says_why_it_needs_attention() {
        let scopes = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let all = scopes(&["read:org", "repo", "workflow"]);
        assert_eq!(account_warning(None, &all), None);
        assert_eq!(
            account_warning(Some(SignedOut::Refused), &all).as_deref(),
            Some("Signed out: GitHub refused its token. Sign in again.")
        );
        assert_eq!(
            account_warning(Some(SignedOut::TokenMissing), &all).as_deref(),
            Some("Signed out: its token is missing. Sign in again.")
        );
        assert_eq!(
            account_warning(None, &scopes(&["gist", "repo"])).as_deref(),
            Some("Missing scopes: read:org, workflow")
        );
        // GitHub lists no scopes for a GitHub App's or a fine-grained token: they have
        // permissions instead, so none are "missing".
        assert_eq!(
            account_warning(None, &[]).as_deref(),
            Some("A token without OAuth scopes: what it can do depends on its permissions.")
        );
    }

    #[test]
    fn a_gh_account_without_scopes_isnt_said_to_miss_them() {
        let account = |scopes: &[&str]| GhAccount {
            login: "a".into(),
            active: false,
            state: "success".into(),
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
            refreshable: false,
        };
        assert_eq!(
            gh_account_notes(&account(&["gist", "read:org", "repo"]), false),
            ["no workflow scope: can't merge PRs that change workflows"]
        );
        assert_eq!(
            gh_account_notes(&account(&["gist"]), false),
            ["missing scopes: repo, read:org, workflow"]
        );
        assert_eq!(
            gh_account_notes(&account(&[]), false),
            ["a token without OAuth scopes, such as a GitHub App's"]
        );
    }

    #[test]
    fn the_countdown_reads_as_minutes_and_seconds() {
        assert_eq!(format_remaining(Duration::from_secs(899)), "14:59");
        assert_eq!(format_remaining(Duration::from_secs(5)), "0:05");
    }
}
