//! The PR tab: one pull request's header, its files in review order, its side-by-side diff and its
//! threads (design §10 "Window"). rozed's window has one tab per pull request, and opening a pull
//! request that's already open switches to its tab.

use std::collections::HashMap;
use std::ops::Range;
use std::time::{Duration, Instant, SystemTime};

use anyhow::Context as _;

use editor::scroll::Autoscroll;
use editor::{Editor, SelectionEffects, SplittableEditor};
use github::accounts::RepoAccount;
use github::poll::{PollEvent, Poller};
use github::queries::{FileViewedState, PullRequestData};
use github::{GithubError, HelpLink, PullRef, Target};
use gpui::{
    AnyElement, App, AsyncWindowContext, Context, Entity, EntityId, EventEmitter, FocusHandle,
    Focusable, KeyBinding, Render, SharedString, Subscription, Task, UniformListScrollHandle,
    WeakEntity, Window, actions, uniform_list,
};
use language::Capability;
use project::WorktreeId;
use repo_cache::{ChangedFile, Oid};
use review::revisions::{PushRecord, Revisions, ViewedState, build_revisions};
use ui::{ListItem, ListItemSpacing, Tooltip, prelude::*};
use util::rel_path::RelPath;
use workspace::item::{Item, ItemEvent, TabContentParams};
use workspace::{ItemNavHistory, Workspace};

use crate::account::{self, AccountDecision, Acting, Refusal};
use crate::account_picker::{self, NotChosen};
use crate::file_list::{FileChange, FileEntry, FileList, FileOrder};
use crate::github_api::{self, CanSignIn, PullRequest};
use crate::github_layer::{self, Saved};
use crate::poll_state::{self, PollNote, PollState};
use crate::pr_banner::{self, BannerKind, Banners};
use crate::pr_diff::{self, Trust};
use crate::pr_link::PrLink;
use crate::tabs::{TabKey, find_tab};
use crate::window_title;

actions!(
    review_ui,
    [
        /// Shows or hides the file list.
        ToggleFileList,
        /// Shows or hides the threads panel.
        ToggleThreads,
        /// Switches the file list between review order and path order.
        ToggleFileOrder,
    ]
);

const KEY_CONTEXT: &str = "PrTab";
const FILE_LIST_CONTEXT: &str = "PrFileList";

/// The tab's keys. Panels toggle with Zed's keys for its docks.
pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-b", ToggleFileList, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-alt-b", ToggleThreads, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-r", ToggleThreads, Some(KEY_CONTEXT)),
        KeyBinding::new("alt-o", ToggleFileOrder, Some(KEY_CONTEXT)),
        KeyBinding::new("o", ToggleFileOrder, Some(FILE_LIST_CONTEXT)),
        // Back and forward through where the diff's cursor has been (design §10), beside Zed's
        // ctrl-- and ctrl-_.
        KeyBinding::new("ctrl-o", workspace::pane::GoBack, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-i", workspace::pane::GoForward, Some(KEY_CONTEXT)),
        // The workspace binds escape to stop following a collaborator.
        KeyBinding::new("escape", menu::Cancel, Some(FILE_LIST_CONTEXT)),
        KeyBinding::new("enter", menu::Confirm, Some(FILE_LIST_CONTEXT)),
        KeyBinding::new("j", menu::SelectNext, Some(FILE_LIST_CONTEXT)),
        KeyBinding::new("k", menu::SelectPrevious, Some(FILE_LIST_CONTEXT)),
    ]);
}

/// Opens `link`'s pull request in a tab of `workspace`, or switches to its tab if it's open. A new
/// tab starts loading: it picks the account, reads the pull request from GitHub, copies it
/// locally and shows its diff. A tab that failed to load tries again.
pub fn open(
    workspace: &mut Workspace,
    link: PrLink,
    trust: Trust,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Entity<PrTab> {
    open_with(workspace, link, trust, window, cx, PrTab::load)
}

/// [`open`] with `login`'s account, if it's still signed in, rather than the account chosen for
/// the repository: for a pull request listed as opened with it.
pub fn open_as(
    workspace: &mut Workspace,
    link: PrLink,
    trust: Trust,
    login: String,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Entity<PrTab> {
    open_with(
        workspace,
        link,
        trust,
        window,
        cx,
        move |tab, window, cx| {
            tab.preferred = Some(login);
            tab.load(window, cx);
        },
    )
}

/// [`open`], with `start` run on a new tab, or on a tab that failed, instead of loading it.
pub fn open_with(
    workspace: &mut Workspace,
    link: PrLink,
    trust: Trust,
    window: &mut Window,
    cx: &mut Context<Workspace>,
    start: impl FnOnce(&mut PrTab, &mut Window, &mut Context<PrTab>),
) -> Entity<PrTab> {
    let tabs: Vec<Entity<PrTab>> = workspace.items_of_type::<PrTab>(cx).collect();
    let keys: Vec<TabKey> = tabs.iter().map(|tab| tab.read(cx).key.clone()).collect();
    if let Some(index) = find_tab(&keys, &link) {
        let tab = tabs[index].clone();
        workspace.activate_item(&tab, true, true, window, cx);
        if tab.read(cx).status.tries_again() {
            log::info!("{link} didn't load from GitHub before; trying again");
            tab.update(cx, |tab, cx| {
                tab.start_again(cx);
                start(tab, window, cx);
            });
        } else {
            log::info!("{link} is already open; switching to its tab");
        }
        return tab;
    }
    log::info!("opening {link}");
    let weak_workspace = workspace.weak_handle();
    let tab = cx.new(|cx| {
        let mut tab = PrTab::new(link, trust, weak_workspace, window, cx);
        start(&mut tab, window, cx);
        tab
    });
    workspace.add_item_to_active_pane(Box::new(tab.clone()), None, true, window, cx);
    tab
}

/// Tells each tab in `workspace` whether it's the active one, so the active tab's pull request is
/// the one its account's poller polls most often.
pub(crate) fn sync_active(workspace: &Workspace, cx: &mut App) {
    let active = workspace.active_item_as::<PrTab>(cx);
    let tabs: Vec<Entity<PrTab>> = workspace.items_of_type::<PrTab>(cx).collect();
    for tab in tabs.iter().filter(|tab| Some(*tab) != active.as_ref()) {
        tab.update(cx, |tab, cx| tab.set_active(false, cx));
    }
    if let Some(active) = active {
        active.update(cx, |tab, cx| tab.set_active(true, cx));
    }
}

/// What the header shows about the pull request.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Header {
    pub title: Option<String>,
    pub author: Option<String>,
    /// `owner/name` as GitHub names the repository now.
    pub repo: Option<String>,
}

/// How far the tab has got.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Loading(SharedString),
    /// Every file is in the diff.
    Ready,
    /// GitHub couldn't be reached, and the diff is the local copy's, read-only. Says so, and why.
    Offline(SharedString),
    Failed(SharedString),
}

impl Status {
    /// Whether opening the tab's link again loads it again: it failed, or showed only what rozed
    /// had while offline.
    fn tries_again(&self) -> bool {
        matches!(self, Self::Failed(_) | Self::Offline(_))
    }
}

/// Sent when what the tab's title shows changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrTabEvent {
    Changed,
}

/// One pull request in a tab.
pub struct PrTab {
    /// The tab's own entity's.
    id: EntityId,
    key: TabKey,
    trust: Trust,
    workspace: WeakEntity<Workspace>,
    focus_handle: FocusHandle,
    file_list_focus: FocusHandle,
    file_list_scroll: UniformListScrollHandle,
    acting: Option<Acting>,
    /// The account to open the pull request with while it's signed in, whatever account the
    /// repository's choice is.
    preferred: Option<String>,
    header: Header,
    created_at: Option<SystemTime>,
    files: FileList,
    /// GitHub's viewed marks, by path, kept for when the files arrive.
    viewed: HashMap<String, ViewedState>,
    revisions: Option<Revisions>,
    status: Status,
    /// GitHub's links for fixing why the tab failed, such as authorizing single sign-on.
    help_links: Vec<HelpLink>,
    /// The account and the time of the details rozed saved, while the tab shows them instead of
    /// GitHub's answer.
    saved: Option<(String, SystemTime)>,
    /// What the tab should tell about what its diff shows, under its header.
    banners: Entity<Banners>,
    diff: Option<Entity<SplittableEditor>>,
    /// The pane's history, for the diff to record where its cursor goes. The pane hands it over
    /// when the tab joins it, before the diff exists.
    nav_history: Option<ItemNavHistory>,
    show_file_list: bool,
    show_threads: bool,
    /// The account's poller and the pull request it watches for this tab, until the tab closes.
    watched: Option<(Entity<Poller>, PullRef)>,
    /// How the poller is doing, for the header.
    poll: PollState,
    /// The trusted checkout's worktree, which goes with the last tab that uses it.
    checkout: Option<WorktreeId>,
    opened: Instant,
    _load: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<PrTabEvent> for PrTab {}

impl PrTab {
    /// A tab for `link`'s pull request, showing nothing yet. [`Self::load`] fills it.
    pub fn new(
        link: PrLink,
        trust: Trust,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        // Focusing the tab focuses its diff, where the keys for reading it work.
        let on_focus = cx.on_focus(&focus_handle, window, |this, window, cx| {
            if let Some(diff) = &this.diff {
                window.focus(&diff.focus_handle(cx), cx);
            }
        });
        // Moving the tab to another pane removes it from one pane and adds it to another, so
        // only closing it, which releases it, lets go of what it holds.
        cx.on_release(Self::released).detach();
        let banners = cx.new(|_| Banners::default());
        let on_banners = cx.observe(&banners, |_, _, cx| cx.notify());
        Self {
            id: cx.entity_id(),
            key: TabKey::new(link),
            trust,
            workspace,
            focus_handle,
            file_list_focus: cx.focus_handle(),
            file_list_scroll: UniformListScrollHandle::new(),
            acting: None,
            preferred: None,
            header: Header::default(),
            created_at: None,
            files: FileList::default(),
            viewed: HashMap::new(),
            revisions: None,
            status: Status::Loading("Finding an account that can see it…".into()),
            help_links: Vec::new(),
            saved: None,
            banners,
            diff: None,
            nav_history: None,
            show_file_list: true,
            show_threads: true,
            watched: None,
            poll: PollState::default(),
            checkout: None,
            opened: Instant::now(),
            _load: None,
            _subscriptions: vec![on_focus, on_banners],
        }
    }

    /// Starts loading the pull request.
    pub fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (link, trust, opened) = (self.key.opened_as.clone(), self.trust, self.opened);
        self._load = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                let saved = saved_details(&link, cx).await;
                let error = match load(this.clone(), link.clone(), trust, opened, &saved, cx).await
                {
                    Ok(()) => return,
                    Err(error) => error,
                };
                if !is_offline(&error) {
                    failed(&this, &link, error, cx);
                    return;
                }
                if let Err(error) =
                    show_offline(this.clone(), link.clone(), opened, saved, error, cx).await
                {
                    failed(&this, &link, error, cx);
                }
                // An offline tab keeps checking, and loads again once GitHub answers.
                wait_for_github(cx).await;
                log::info!("{link}: GitHub answers again, so loading it again");
                if this.update(cx, |this, cx| this.start_again(cx)).is_err() {
                    return;
                }
            }
        }));
    }

    /// The tab shows its pull request with language servers from the trusted checkout in
    /// `worktree`.
    pub(crate) fn uses_checkout(&mut self, worktree: WorktreeId) {
        self.checkout = Some(worktree);
    }

    /// The tab closed: its poller stops watching its pull request, and a trusted checkout no other
    /// tab uses leaves the project, which stops its language servers (design §11).
    fn released(&mut self, cx: &mut App) {
        // A question about its account is moot.
        account_picker::tab_closed(self.id, cx);
        if let Some((poller, pull)) = self.watched.take() {
            poller.update(cx, |poller, cx| poller.unwatch(&pull, cx));
        }
        let (Some(checkout), Some(workspace)) = (self.checkout.take(), self.workspace.upgrade())
        else {
            return;
        };
        let still_used = workspace
            .read(cx)
            .items_of_type::<PrTab>(cx)
            .any(|tab| tab.read(cx).checkout == Some(checkout));
        if still_used {
            return;
        }
        let project = workspace.read(cx).project().clone();
        project.update(cx, |project, cx| {
            if let Some(worktree) = project.worktree_for_id(checkout, cx) {
                log::info!(
                    "closing the checkout {} and its language servers",
                    worktree.read(cx).abs_path().display()
                );
            }
            project.remove_worktree(checkout, cx);
        });
    }

    /// Forgets why the tab failed, before it loads again.
    fn start_again(&mut self, cx: &mut Context<Self>) {
        self.acting = None;
        self.help_links.clear();
        self.set_status(
            Status::Loading("Finding an account that can see it…".into()),
            cx,
        );
    }

    pub fn key(&self) -> &TabKey {
        &self.key
    }

    pub fn link(&self) -> &PrLink {
        &self.key.opened_as
    }

    pub fn acting(&self) -> Option<&Acting> {
        self.acting.as_ref()
    }

    pub fn header(&self) -> &Header {
        &self.header
    }

    pub fn files(&self) -> &FileList {
        &self.files
    }

    pub fn revisions(&self) -> Option<&Revisions> {
        self.revisions.as_ref()
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    /// GitHub's links for fixing why the tab failed.
    pub fn help_links(&self) -> &[HelpLink] {
        &self.help_links
    }

    pub fn banners(&self) -> &Entity<Banners> {
        &self.banners
    }

    pub fn diff(&self) -> Option<&Entity<SplittableEditor>> {
        self.diff.as_ref()
    }

    pub fn is_file_list_shown(&self) -> bool {
        self.show_file_list
    }

    pub fn is_threads_panel_shown(&self) -> bool {
        self.show_threads
    }

    pub fn file_list_focus_handle(&self) -> &FocusHandle {
        &self.file_list_focus
    }

    fn set_status(&mut self, status: Status, cx: &mut Context<Self>) {
        self.status = status;
        cx.notify();
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        cx.emit(PrTabEvent::Changed);
        cx.notify();
    }

    pub fn set_acting(&mut self, acting: Acting, cx: &mut Context<Self>) {
        // Viewed marks are the account's own, so another account's saved ones don't show.
        if let Some((saved_for, _)) = &self.saved
            && acting
                .login()
                .is_none_or(|login| !login.eq_ignore_ascii_case(saved_for))
        {
            self.viewed.clear();
            self.files.set_viewed(&self.viewed);
        }
        self.acting = Some(acting);
        self.set_status(Status::Loading("Asking GitHub for it…".into()), cx);
    }

    /// Shows what rozed saved about the pull request, at once and online or not, until GitHub
    /// answers.
    pub(crate) fn show_saved(&mut self, saved: &Saved, cx: &mut Context<Self>) {
        self.set_details(&saved.details, cx);
        self.saved = Some((saved.login.clone(), saved.fetched_at));
        self.show_saved_banner(true, cx);
    }

    /// GitHub couldn't be reached, so the tab shows the local copy's diff with what rozed saved.
    fn showing_offline(&mut self, why: &anyhow::Error, cx: &mut Context<Self>) {
        self.show_saved_banner(false, cx);
        let text = format!(
            "Offline: {why:#}. This is the local copy's diff, read-only, until GitHub answers \
             again; rozed keeps trying."
        );
        self.set_status(Status::Offline(text.into()), cx);
    }

    /// Says how old the saved details the tab shows are, if it shows them.
    fn show_saved_banner(&self, asking: bool, cx: &mut Context<Self>) {
        let Some((login, fetched_at)) = &self.saved else {
            return;
        };
        let mut text = format!(
            "Showing what rozed saved for {login} {}",
            poll_state::ago(*fetched_at, SystemTime::now())
        );
        text.push_str(if asking {
            ", until GitHub answers."
        } else {
            "."
        });
        self.banners.update(cx, |banners, cx| {
            banners.show(BannerKind::Saved, Severity::Info, text, cx)
        });
    }

    /// Takes GitHub's REST view of the pull request.
    pub fn set_pull_request(&mut self, pull_request: &PullRequest, cx: &mut Context<Self>) {
        self.header.title = Some(window_title::without_bidi(&pull_request.title));
        if let Some(user) = &pull_request.user {
            self.header.author = Some(user.login.clone());
        }
        let repo = &pull_request.base.repo;
        self.header.repo = Some(repo.full_name.clone());
        if let Ok((owner, name)) = repo.owner_and_name() {
            self.key.canonical = Some((owner.to_string(), name.to_string()));
        }
        self.created_at = Some(pull_request.created_at.into());
        self.status = Status::Loading("Copying it locally…".into());
        // GitHub answered, and the account's poller brings its details up to date.
        if self.saved.take().is_some() {
            self.banners
                .update(cx, |banners, cx| banners.hide(BannerKind::Saved, cx));
        }
        self.changed(cx);
    }

    /// Takes the pull request's details from its account's poller: its title, author and the
    /// account's viewed marks.
    pub fn set_details(&mut self, details: &PullRequestData, cx: &mut Context<Self>) {
        let mut header = self.header.clone();
        header.title = Some(window_title::without_bidi(&details.title));
        if let Some(author) = &details.author {
            header.author = Some(author.login.clone());
        }
        let viewed: HashMap<String, ViewedState> = details
            .files
            .iter()
            .flat_map(|files| files.nodes.iter().flatten())
            .flatten()
            .filter_map(|file| Some((file.path.clone(), viewed_state(&file.viewer_viewed_state)?)))
            .collect();
        // A change of title makes the workspace report the active item changed, which comes back
        // here through `set_active`, so only a real change is reported.
        if header == self.header && viewed == self.viewed {
            return;
        }
        self.header = header;
        self.viewed = viewed;
        self.files.set_viewed(&self.viewed);
        self.changed(cx);
    }

    /// The local copy listed the pull request's files at `head`, which GitHub gave at
    /// `polled_at`: lists them, builds its revisions, and returns where each file goes in the
    /// diff, by its index in `files`.
    pub(crate) fn copied(
        &mut self,
        files: &[ChangedFile],
        head: &Oid,
        polled_at: SystemTime,
        cx: &mut Context<Self>,
    ) -> Vec<usize> {
        let mut list = FileList::new(files.iter().map(FileEntry::from_local).collect());
        list.set_order(self.files.order());
        list.set_viewed(&self.viewed);
        self.files = list;
        // Until rozed reads the branch's push history (milestone 6), the head the local copy
        // fetched is the one revision it knows.
        let records = [PushRecord::Polled {
            head: head.as_str().to_string(),
            at: polled_at,
        }];
        self.revisions = Some(build_revisions(
            &records,
            &[],
            head.as_str(),
            self.created_at.unwrap_or(polled_at),
        ));
        self.status = Status::Loading("Loading the diff…".into());
        cx.notify();
        self.files.review_positions()
    }

    /// Shows `editor` as the tab's diff.
    pub(crate) fn show_diff(
        &mut self,
        editor: Entity<SplittableEditor>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focused = self.focus_handle.contains_focused(window, cx);
        if let Some(history) = self.nav_history.clone() {
            let rhs_editor = editor.read(cx).rhs_editor().clone();
            rhs_editor.update(cx, |editor, _| editor.set_nav_history(Some(history)));
        }
        self.diff = Some(editor.clone());
        if focused && !self.file_list_focus.contains_focused(window, cx) {
            window.focus(&editor.focus_handle(cx), cx);
        }
        cx.notify();
    }

    /// Watches the pull request with its account's poller, once GitHub confirmed it exists. It
    /// becomes the poller's open pull request only while it's the active tab ([`sync_active`]).
    fn watch(&mut self, poller: Entity<Poller>, pull: PullRef, cx: &mut Context<Self>) {
        poller.update(cx, |poller, cx| poller.watch(pull.clone(), cx));
        self.poll.status = poller.read(cx).status().clone();
        self._subscriptions
            .push(cx.observe(&poller, |this, poller, cx| {
                let status = poller.read(cx).status().clone();
                if this.poll.status != status {
                    this.poll.status = status;
                    cx.notify();
                }
            }));
        self._subscriptions
            .push(cx.subscribe(&poller, |this, _, event, cx| this.poll_event(event, cx)));
        self.watched = Some((poller, pull));
    }

    fn poll_event(&mut self, event: &PollEvent, cx: &mut Context<Self>) {
        let Some((_, watched)) = &self.watched else {
            return;
        };
        match event {
            PollEvent::Details { pull, details } if pull == watched => {
                self.poll.details_at = Some(SystemTime::now());
                self.poll_succeeded(cx);
                self.set_details(details, cx);
            }
            PollEvent::Summary { pull, summary } if pull == watched => {
                self.poll_succeeded(cx);
                let title = window_title::without_bidi(&summary.title);
                if self.header.title.as_ref() != Some(&title) {
                    self.header.title = Some(title);
                    self.changed(cx);
                }
            }
            PollEvent::Check { pull, .. } if pull == watched => self.poll_succeeded(cx),
            PollEvent::Failed {
                target: Target::Pull(pull),
                error,
            } if pull == watched => {
                log::warn!("polling {pull}: {error}");
                self.poll.failure = Some(error.clone());
                cx.notify();
            }
            _ => {}
        }
    }

    fn poll_succeeded(&mut self, cx: &mut Context<Self>) {
        if self.poll.failure.take().is_some() {
            cx.notify();
        }
    }

    /// The header's title: the pull request's, once known.
    pub fn title(&self) -> String {
        match (&self.header.title, &self.status) {
            (Some(title), _) => title.clone(),
            (None, Status::Failed(_)) => "Not opened".to_string(),
            (None, _) => "Loading…".to_string(),
        }
    }

    /// The window's title while this is the active tab.
    pub fn window_title(&self) -> String {
        window_title::for_pull_request(
            &self.repo_name(),
            self.link().number,
            self.header.title.as_deref(),
        )
    }

    /// Who the header says the tab acts as.
    pub fn acting_text(&self) -> String {
        match &self.acting {
            Some(Acting::Account(login)) => format!("as {login}"),
            Some(Acting::Anonymous) => "not signed in".to_string(),
            None => String::new(),
        }
    }

    /// What the header says about how polling the pull request goes, if anything's wrong.
    pub fn poll_note(&self) -> Option<PollNote> {
        let login = self.acting.as_ref()?.login()?;
        self.watched.as_ref()?;
        poll_state::note(login, &self.poll, SystemTime::now())
    }

    /// Whether this is the window's active tab. The account's poller polls the active tab's pull
    /// request most often.
    fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        let Some((poller, pull)) = self.watched.clone() else {
            return;
        };
        let is_open = poller.read(cx).open() == Some(&pull);
        if active && !is_open {
            let answer = poller.update(cx, |poller, cx| poller.set_open(Some(pull), cx));
            if let Some(answer) = answer {
                self.poll.details_at = Some(answer.fetched_at);
                self.set_details(&answer.details, cx);
            }
        } else if !active && is_open {
            poller.update(cx, |poller, cx| poller.set_open(None, cx));
        }
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

    fn toggle_threads(&mut self, _: &ToggleThreads, _: &mut Window, cx: &mut Context<Self>) {
        self.show_threads = !self.show_threads;
        cx.notify();
    }

    fn toggle_file_order(&mut self, _: &ToggleFileOrder, _: &mut Window, cx: &mut Context<Self>) {
        self.files.toggle_order();
        self.scroll_to_selected();
        cx.notify();
    }

    fn focus_diff(&self, window: &mut Window, cx: &mut App) {
        match &self.diff {
            Some(diff) => window.focus(&diff.focus_handle(cx), cx),
            None => window.focus(&self.focus_handle, cx),
        }
    }

    fn scroll_to_selected(&self) {
        if let Some(row) = self.files.selected_row() {
            self.file_list_scroll
                .scroll_to_item(row, gpui::ScrollStrategy::Center);
        }
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

    fn selection_moved(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.scroll_to_selected();
        self.reveal_selected(window, cx);
        cx.notify();
    }

    fn confirm(&mut self, _: &menu::Confirm, window: &mut Window, cx: &mut Context<Self>) {
        self.reveal_selected(window, cx);
        self.focus_diff(window, cx);
    }

    fn cancel(&mut self, _: &menu::Cancel, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_diff(window, cx);
    }

    /// Scrolls the diff to the selected file.
    fn reveal_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(diff), Some(entry)) = (&self.diff, self.files.selected()) else {
            return;
        };
        let Some(position) = self.files.review_position(entry.path()) else {
            return;
        };
        let Ok(path) = RelPath::from_unix_str(entry.path()) else {
            return;
        };
        let key = pr_diff::path_key(position, path.into_arc());
        let rhs_editor = diff.read(cx).rhs_editor().clone();
        let multi_buffer = rhs_editor.read(cx).buffer().clone();
        let Some(location) = multi_buffer.read(cx).location_for_path(&key, cx) else {
            return;
        };
        rhs_editor.update(cx, |editor, cx| {
            editor.change_selections(
                // A jump to another file, which back goes back from.
                SelectionEffects::scroll(Autoscroll::top_relative(1.)).nav_history(true),
                window,
                cx,
                |selections| selections.select_ranges([location..location]),
            )
        });
    }

    /// `owner/repo`, as GitHub names it now once it's said.
    fn repo_name(&self) -> String {
        self.header.repo.clone().unwrap_or_else(|| {
            let link = self.link();
            format!("{}/{}", link.owner, link.repo)
        })
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let number = self.link().number;
        let title = self.title();
        let acting = self.acting_text();
        let revision = self
            .revisions
            .as_ref()
            .and_then(|revisions| {
                let latest = revisions.latest()?;
                Some(format!(
                    "{} of {}",
                    revisions.label(&latest.head)?,
                    revisions.len()
                ))
            })
            .unwrap_or_else(|| "—".to_string());
        h_flex()
            .id("pr-header")
            .w_full()
            .flex_shrink_0()
            .px_3()
            .py_2()
            .gap_4()
            .justify_between()
            .border_b_1()
            .border_color(colors.border)
            .bg(colors.panel_background)
            .child(
                v_flex()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap_2()
                            .child(Label::new(title).size(LabelSize::Large).truncate())
                            .child(
                                Label::new(format!("#{number}"))
                                    .size(LabelSize::Large)
                                    .color(Color::Muted),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_1p5()
                            .child(
                                Label::new(self.repo_name())
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                            )
                            .children(self.header.author.as_ref().map(|author| {
                                Label::new(format!("by {author}"))
                                    .size(LabelSize::Small)
                                    .color(Color::Muted)
                            }))
                            .when(!acting.is_empty(), |this| {
                                this.child(
                                    Label::new("·").size(LabelSize::Small).color(Color::Muted),
                                )
                                .child(Label::new(acting).size(LabelSize::Small))
                            }),
                    )
                    .children(self.poll_note().map(render_poll_note)),
            )
            .child(
                h_flex()
                    .flex_shrink_0()
                    .gap_2()
                    // Placeholders until the merge panel and the revision picker arrive.
                    .child(chip("merge-status", "Merge status", "not checked yet", cx))
                    .child(chip("revision", "Revision", revision, cx)),
            )
    }

    fn render_status(&self) -> Option<AnyElement> {
        let (icon, color, text) = match &self.status {
            Status::Ready => return None,
            Status::Loading(step) => (IconName::ArrowCircle, Color::Muted, step.clone()),
            Status::Offline(why) => (IconName::Warning, Color::Warning, why.clone()),
            Status::Failed(error) => (IconName::Warning, Color::Error, error.clone()),
        };
        Some(
            h_flex()
                .px_3()
                .py_1()
                .gap_2()
                .child(Icon::new(icon).size(IconSize::Small).color(color))
                .child(Label::new(text).size(LabelSize::Small).color(color))
                .into_any_element(),
        )
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
            .w(px(300.))
            .h_full()
            .flex_shrink_0()
            .border_r_1()
            .border_color(colors.border)
            .bg(colors.panel_background)
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
                        .children(viewed_mark(entry.viewed)),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.files.select_row(row);
                    this.reveal_selected(window, cx);
                    cx.notify();
                }))
                .into_any_element(),
        )
    }

    fn render_threads(&self, cx: &mut Context<Self>) -> impl IntoElement {
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

/// GitHub's viewed mark as rozed's review model has it.
fn viewed_state(state: &FileViewedState) -> Option<ViewedState> {
    match state {
        FileViewedState::VIEWED => Some(ViewedState::Viewed),
        FileViewedState::UNVIEWED => Some(ViewedState::Unviewed),
        FileViewedState::DISMISSED => Some(ViewedState::Dismissed),
        FileViewedState::Other(_) => None,
    }
}

/// How the file list shows a file's viewed mark.
fn viewed_mark(viewed: Option<ViewedState>) -> Option<AnyElement> {
    match viewed? {
        ViewedState::Viewed => Some(
            Icon::new(IconName::Check)
                .size(IconSize::XSmall)
                .color(Color::Success)
                .into_any_element(),
        ),
        ViewedState::Dismissed => Some(
            Label::new("changed")
                .size(LabelSize::XSmall)
                .color(Color::Warning)
                .into_any_element(),
        ),
        ViewedState::Unviewed => None,
    }
}

/// The header's line about how polling goes, with GitHub's link to fix it if there's one.
fn render_poll_note(note: PollNote) -> impl IntoElement {
    let (icon, color) = match note.severity {
        Severity::Error => (IconName::XCircle, Color::Error),
        Severity::Warning => (IconName::Warning, Color::Warning),
        _ => (IconName::Info, Color::Muted),
    };
    h_flex()
        .id("poll-note")
        .min_w_0()
        .gap_1p5()
        .child(Icon::new(icon).size(IconSize::Small).color(color))
        .child(
            Label::new(note.text)
                .size(LabelSize::Small)
                .color(color)
                .truncate(),
        )
        .children(note.link.map(|link| {
            let url = link.url().to_string();
            Button::new("poll-note-link", link.label())
                .label_size(LabelSize::Small)
                .on_click(move |_, _, cx| cx.open_url(&url))
        }))
}

/// A placeholder in the header: a name and what it says.
fn chip(
    id: &'static str,
    name: &'static str,
    value: impl Into<SharedString>,
    cx: &App,
) -> impl IntoElement {
    h_flex()
        .id(id)
        .gap_1()
        .px_2()
        .py_0p5()
        .rounded_sm()
        .border_1()
        .border_color(cx.theme().colors().border_variant)
        .child(Label::new(name).size(LabelSize::XSmall).color(Color::Muted))
        .child(Label::new(value.into()).size(LabelSize::Small))
        .tooltip(Tooltip::text(format!("{name} isn't built yet")))
}

/// The tab's title: the repository, the number and as much of the title as fits.
pub fn tab_title(repo: &str, number: u64, title: Option<&str>) -> String {
    const MAX_TITLE: usize = 32;
    let mut text = format!("{repo}#{number}");
    if let Some(title) = title {
        text.push(' ');
        if title.chars().count() > MAX_TITLE {
            text.extend(title.chars().take(MAX_TITLE - 1));
            text.push('…');
        } else {
            text.push_str(title);
        }
    }
    text
}

/// When another tab already shows `tab`'s pull request by another of its repository's names,
/// as after a rename, closes `tab` and brings that one to the front, trying it again if it
/// failed. Returns whether it did.
fn merge_into_twin(
    workspace: &mut Workspace,
    tab: &Entity<PrTab>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> bool {
    let key = tab.read(cx).key.clone();
    let Some(twin) = workspace
        .items_of_type::<PrTab>(cx)
        .find(|other| other != tab && other.read(cx).key.is_same_pull_request(&key))
    else {
        return false;
    };
    log::info!(
        "{} is {} by another name; keeping that tab",
        key.opened_as,
        twin.read(cx).key.opened_as
    );
    twin.update(cx, |twin, cx| {
        twin.key.merge(key);
        if twin.status.tries_again() {
            twin.start_again(cx);
            twin.load(window, cx);
        }
    });
    if let Some(pane) = workspace.pane_for(tab) {
        pane.update(cx, |pane, cx| {
            pane.remove_item(tab.entity_id(), false, false, window, cx)
        });
    }
    workspace.activate_item(&twin, true, true, window, cx);
    true
}

/// What rozed saved about `link`'s pull request, with any account.
async fn saved_details(link: &PrLink, cx: &mut AsyncWindowContext) -> Option<Saved> {
    let layer = cx.update(|_, cx| github_layer::ready(cx)).ok()?.await?;
    let pull = PullRef::new(&link.owner, &link.repo, i64::try_from(link.number).ok()?);
    cx.update(|_, cx| layer.read(cx).saved_details(&pull, cx))
        .ok()?
}

/// The tab failed to load: says why, with GitHub's links to fix it.
fn failed(
    this: &WeakEntity<PrTab>,
    link: &PrLink,
    error: anyhow::Error,
    cx: &mut AsyncWindowContext,
) {
    log::error!("{link}: {error:#}");
    let links = error
        .downcast_ref::<Refusal>()
        .map(|refusal| refusal.links.clone())
        .unwrap_or_default();
    let mut text = format!("{error:#}");
    if is_offline(&error) {
        text.push_str(". rozed keeps trying GitHub.");
    }
    this.update(cx, |this, cx| {
        this.help_links = links;
        this.show_saved_banner(false, cx);
        this.set_status(Status::Failed(text.into()), cx)
    })
    .ok();
}

/// How long an offline tab waits before checking GitHub the first time, doubling each time
/// after, up to [`OFFLINE_MAX_WAIT`].
const OFFLINE_FIRST_WAIT: Duration = Duration::from_secs(5);
const OFFLINE_MAX_WAIT: Duration = Duration::from_secs(5 * 60);

/// Waits until GitHub answers again, checking less often the longer it doesn't.
async fn wait_for_github(cx: &mut AsyncWindowContext) {
    let mut attempt = 0;
    loop {
        attempt += 1;
        let wait = github::poll::backoff(attempt, OFFLINE_FIRST_WAIT, OFFLINE_MAX_WAIT, 0., 0.);
        cx.background_executor().timer(wait).await;
        let Ok(http) = cx.update(|_, cx| cx.http_client()) else {
            return;
        };
        if github_api::github_answers(http).await {
            return;
        }
    }
}

/// Loads `link`'s pull request into `this`: picks the account, reads the pull request, copies it
/// locally and shows its diff. What rozed `saved` about it shows until GitHub answers.
async fn load(
    this: WeakEntity<PrTab>,
    link: PrLink,
    trust: Trust,
    opened: Instant,
    saved: &Option<Saved>,
    cx: &mut AsyncWindowContext,
) -> anyhow::Result<()> {
    let layer = cx.update(|_, cx| github_layer::ready(cx))?.await;
    let pull = PullRef::new(&link.owner, &link.repo, i64::try_from(link.number)?);
    // What rozed saved shows at once, and offline.
    if let Some(saved) = saved {
        this.update(cx, |this, cx| this.show_saved(saved, cx))?;
    }

    // The account it was asked to open with, while it's signed in.
    let preferred = this.read_with(cx, |this, _| this.preferred.clone())?;
    let preferred = match (&layer, preferred) {
        (Some(layer), Some(login)) => cx.update(|_, cx| {
            let accounts = layer.read(cx).accounts().read(cx);
            (accounts.account(&login).is_some() && !accounts.is_signed_out(&login)).then_some(login)
        })?,
        _ => None,
    };

    // Else the account that can see the repository, or none for a public one.
    let found = match (&layer, &preferred) {
        (_, Some(_)) | (None, _) => None,
        (Some(layer), None) => Some(
            layer
                .update(cx, |layer, cx| {
                    layer.account_for_repo(&link.owner, &link.repo, cx)
                })
                .await?,
        ),
    };
    let acting = match (preferred, account::decide(found.as_ref()), &found) {
        (Some(login), _, _) => Acting::Account(login),
        (None, AccountDecision::Open(acting), _) => acting,
        (None, AccountDecision::Ask(logins), Some(found)) => {
            Acting::Account(ask_for_account(&this, &link, found, logins, cx).await?)
        }
        (None, AccountDecision::Ask(_), None) => Acting::Anonymous,
        (None, AccountDecision::Refuse(refusal), _) => return Err(refusal.into()),
    };
    log::info!(
        "{link}: opening {}",
        match &acting {
            Acting::Account(login) => format!("as {login}"),
            Acting::Anonymous => "without signing in".to_string(),
        }
    );
    this.update(cx, |this, cx| this.set_acting(acting.clone(), cx))?;

    // Signed in, the account's client reads the pull request in the account's queue, and its
    // token copies it.
    let (pull_request, token, poller) = match (&acting, &layer) {
        (Acting::Account(login), Some(layer)) => {
            let poller = layer.update(cx, |layer, cx| layer.poller(login, cx));
            let accounts = layer.read_with(cx, |layer, _| layer.accounts().clone());
            let client = accounts
                .update(cx, |accounts, cx| accounts.client(login, cx))
                .await?;
            let queue = poller.read_with(cx, |poller, _| poller.queue().clone());
            let pull_request = queue
                .read(|| github_api::fetch_as(&client, login, &link))
                .await?;
            let token = repo_cache::Token::new(client.token().expose());
            (pull_request, Some(token), Some(poller))
        }
        _ => {
            let http = cx.update(|_, cx| cx.http_client())?;
            let can_sign_in = match layer {
                Some(_) => CanSignIn::Yes,
                None => CanSignIn::No,
            };
            (
                github_api::fetch_pull_request(http, &link, can_sign_in).await?,
                None,
                None,
            )
        }
    };
    this.update(cx, |this, cx| this.set_pull_request(&pull_request, cx))?;
    // Opened by a name the repository no longer has, it's listed once, by its name now.
    if let Some(layer) = &layer {
        let other_names = this.read_with(cx, |this, _| this.key.other_names())?;
        cx.update(|_, cx| {
            for (owner, name) in other_names {
                let old = PullRef::new(owner, name, pull.number);
                layer.read(cx).forget_recent(old, cx);
            }
        })?;
    }

    // A tab opened by another of the repository's names may show it already.
    let workspace = this.read_with(cx, |this, _| this.workspace.clone())?;
    let tab = this.upgrade().context("the tab closed")?;
    if workspace.update_in(cx, |workspace, window, cx| {
        merge_into_twin(workspace, &tab, window, cx)
    })? {
        return Ok(());
    }

    // GitHub confirmed the pull request exists, so the account's poller watches it, by the
    // repository's name now, and keeps the active tab's up to date most often.
    let (owner, name) = pull_request.base.repo.owner_and_name()?;
    let pull = PullRef::new(owner, name, pull.number);
    if let Some(poller) = poller {
        this.update(cx, |this, cx| this.watch(poller, pull, cx))?;
        workspace.update(cx, |workspace, cx| sync_active(workspace, cx))?;
    }

    pr_diff::open(
        this.clone(),
        workspace,
        link,
        pull_request,
        token,
        trust,
        opened,
        cx,
    )
    .await?;
    this.update(cx, |this, cx| this.set_status(Status::Ready, cx))?;
    Ok(())
}

/// Whether `error` says GitHub couldn't be reached at all: no connection could be made. A slow
/// answer that timed out isn't offline.
fn is_offline(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<GithubError>()
            .is_some_and(GithubError::is_unreachable)
    })
}

/// When GitHub couldn't be reached (`offline`), shows the pull request's diff from the local
/// copy, if it has the head rozed `saved`, with the saved details and their age (design §11
/// "Offline"). Without a copy, it's `offline` again, and the tab shows the saved details alone.
async fn show_offline(
    this: WeakEntity<PrTab>,
    link: PrLink,
    opened: Instant,
    saved: Option<Saved>,
    offline: anyhow::Error,
    cx: &mut AsyncWindowContext,
) -> anyhow::Result<()> {
    let Some(saved) = saved else {
        return Err(offline);
    };
    let workspace = this.read_with(cx, |this, _| this.workspace.clone())?;
    let shown =
        pr_diff::open_offline(this.clone(), workspace, link.clone(), &saved, opened, cx).await;
    match shown {
        Ok(true) => {
            this.update(cx, |this, cx| this.showing_offline(&offline, cx))?;
            Ok(())
        }
        Ok(false) => Err(offline),
        Err(error) => {
            log::warn!("{link}: can't show the local copy offline: {error:#}");
            Err(offline)
        }
    }
}

/// Asks which of `logins`, the accounts that can see `link`'s repository, to open it with
/// (design §3 "Which account"). The picker remembers the answer: for the repository's owner,
/// as GitHub names it now, so rozed asks once per owner; or for the repository alone, when the
/// owner's account can't see it.
async fn ask_for_account(
    this: &WeakEntity<PrTab>,
    link: &PrLink,
    found: &RepoAccount,
    logins: Vec<String>,
    cx: &mut AsyncWindowContext,
) -> anyhow::Result<String> {
    log::info!(
        "{link}: several accounts can see it ({}); asking which to use",
        logins.join(", ")
    );
    let subject = match found.remembered.owner {
        Some(_) => format!("{}/{}", found.owner, found.name),
        None => found.owner.clone(),
    };
    let workspace = this.update(cx, |this, cx| {
        this.set_status(
            Status::Loading(format!("Choose the account rozed uses for {subject}…").into()),
            cx,
        );
        this.workspace.clone()
    })?;
    let window = cx.window_handle();
    let tab = this.entity_id();
    let answer = cx.update(|_, cx| {
        account_picker::ask(
            tab,
            window,
            workspace,
            &found.owner,
            &found.name,
            logins,
            cx,
        )
    })?;
    match answer.await {
        Ok(Ok(login)) => Ok(login),
        Ok(Err(NotChosen { subject })) => anyhow::bail!(
            "No account was chosen for {subject}. Open the pull request again to choose one."
        ),
        Err(_) => anyhow::bail!(
            "No account was chosen for {subject}. Open the pull request again to choose one."
        ),
    }
}

impl Focusable for PrTab {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PrTab {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let diff: AnyElement =
            match (&self.diff, &self.status) {
                (_, Status::Failed(error)) => v_flex()
                    .size_full()
                    .p_4()
                    .gap_2()
                    .child(Label::new("rozed couldn't open this pull request").color(Color::Error))
                    .child(Label::new(error.clone()).size(LabelSize::Small))
                    .when(!self.help_links.is_empty(), |this| {
                        this.child(h_flex().gap_2().children(
                            self.help_links.iter().enumerate().map(|(index, link)| {
                                let url = link.url().to_string();
                                Button::new(("help-link", index), link.label())
                                    .style(ButtonStyle::Filled)
                                    .on_click(move |_, _, cx| cx.open_url(&url))
                            }),
                        ))
                    })
                    .into_any_element(),
                (Some(diff), _) => diff.clone().into_any_element(),
                (None, _) => div().size_full().into_any_element(),
            };
        v_flex()
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::toggle_file_list))
            .on_action(cx.listener(Self::toggle_threads))
            .on_action(cx.listener(Self::toggle_file_order))
            .size_full()
            .bg(colors.editor_background)
            .child(self.render_header(cx))
            .children(self.render_status())
            .children(pr_banner::render(self.banners.read(cx).messages()))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .when(self.show_file_list, |this| {
                        this.child(self.render_file_list(cx))
                    })
                    .child(div().flex_1().min_w_0().h_full().child(diff))
                    .when(self.show_threads, |this| {
                        this.child(self.render_threads(cx))
                    }),
            )
    }
}

impl Item for PrTab {
    type Event = PrTabEvent;

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        let repo = self.repo_name();
        let name = repo.rsplit('/').next().unwrap_or(&repo);
        tab_title(name, self.link().number, self.header.title.as_deref()).into()
    }

    fn tab_content(&self, params: TabContentParams, _window: &Window, cx: &App) -> AnyElement {
        Label::new(self.tab_content_text(0, cx))
            .single_line()
            .color(params.text_color())
            .into_any_element()
    }

    fn tab_tooltip_text(&self, _: &App) -> Option<SharedString> {
        let pull = format!("{}#{}", self.repo_name(), self.link().number);
        Some(match &self.header.title {
            Some(title) => format!("{pull}: {title}").into(),
            None => pull.into(),
        })
    }

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::PullRequest).color(Color::Muted))
    }

    fn to_item_events(event: &PrTabEvent, f: &mut dyn FnMut(ItemEvent)) {
        match event {
            PrTabEvent::Changed => f(ItemEvent::UpdateTab),
        }
    }

    fn capability(&self, _: &App) -> Capability {
        Capability::ReadOnly
    }

    fn as_searchable(
        &self,
        _: &Entity<Self>,
        _: &App,
    ) -> Option<Box<dyn workspace::searchable::SearchableItemHandle>> {
        Some(Box::new(self.diff.clone()?))
    }

    fn act_as_type<'a>(
        &'a self,
        type_id: std::any::TypeId,
        self_handle: &'a Entity<Self>,
        cx: &'a App,
    ) -> Option<gpui::AnyEntity> {
        if type_id == std::any::TypeId::of::<Self>() {
            Some(self_handle.clone().into())
        } else if type_id == std::any::TypeId::of::<SplittableEditor>() {
            Some(self.diff.clone()?.into())
        } else if type_id == std::any::TypeId::of::<Editor>() {
            Some(self.diff.as_ref()?.read(cx).rhs_editor().clone().into())
        } else {
            None
        }
    }

    fn deactivated(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(diff) = &self.diff {
            diff.update(cx, |diff, cx| diff.deactivated(window, cx));
        }
    }

    fn navigate(
        &mut self,
        data: std::sync::Arc<dyn std::any::Any + Send>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match &self.diff {
            Some(diff) => diff.update(cx, |diff, cx| diff.navigate(data, window, cx)),
            None => false,
        }
    }

    fn set_nav_history(&mut self, history: ItemNavHistory, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(diff) = &self.diff {
            let editor = diff.read(cx).rhs_editor().clone();
            editor.update(cx, |editor, _| {
                editor.set_nav_history(Some(history.clone()))
            });
        }
        self.nav_history = Some(history);
    }

    /// Keeps files and folders dropped on the tab from the pane, which would open them. The
    /// window already ends such drags (`read_only`); this is the second line.
    fn handle_drop(
        &self,
        _: &workspace::Pane,
        dropped: &dyn std::any::Any,
        _: &mut Window,
        _: &mut App,
    ) -> bool {
        dropped.is::<gpui::ExternalPaths>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::Path;
    use std::time::Duration;

    use github::cache::PrCache;
    use gpui::{TestAppContext, VisualTestContext};
    use repo_cache::{Change, FileMode, FileVersion, LineCounts};

    use crate::github_api::{Base, BaseRepo, Head, User};
    use crate::test_support::{
        Answer, FakeGithub, FakePull, GitServer, anonymous, sign_in, workspace,
    };

    #[test]
    fn tab_titles_fit_the_repository_number_and_title() {
        assert_eq!(tab_title("fd", 2212, None), "fd#2212");
        assert_eq!(
            tab_title("playground", 2, Some("Add a greeting")),
            "playground#2 Add a greeting"
        );
        assert_eq!(
            tab_title("fd", 2212, Some("Honor explicit use-jemalloc requests now")),
            "fd#2212 Honor explicit use-jemalloc req…"
        );
    }

    fn link(link: &str) -> PrLink {
        link.parse().unwrap()
    }

    /// Opens `link` without loading it from GitHub.
    fn open_unloaded(
        workspace: &Entity<Workspace>,
        link: &str,
        cx: &mut VisualTestContext,
    ) -> Entity<PrTab> {
        let link = self::link(link);
        let tab = workspace.update_in(cx, |workspace, window, cx| {
            open_with(workspace, link, Trust::Untrusted, window, cx, |_, _, _| {})
        });
        cx.run_until_parked();
        tab
    }

    fn tabs(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Vec<Entity<PrTab>> {
        workspace.read_with(cx, |workspace, cx| {
            workspace.items_of_type::<PrTab>(cx).collect()
        })
    }

    fn active_tab(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> Option<Entity<PrTab>> {
        workspace.read_with(cx, |workspace, cx| workspace.active_item_as::<PrTab>(cx))
    }

    fn fd_2212() -> PullRequest {
        PullRequest {
            title: "Honor explicit use-jemalloc requests".into(),
            user: Some(User {
                login: "voidsipp".into(),
            }),
            created_at: "2026-03-02T10:20:30Z".parse().unwrap(),
            head: Head {
                sha: "2".repeat(40),
            },
            base: Base {
                branch: "master".into(),
                sha: "1".repeat(40),
                repo: BaseRepo {
                    full_name: "sharkdp/fd".into(),
                    clone_url: "https://github.com/sharkdp/fd.git".into(),
                },
            },
        }
    }

    fn changed(path: &str, change: Change) -> ChangedFile {
        let version = || {
            Some(FileVersion {
                path: path.into(),
                oid: "3".repeat(40).parse().unwrap(),
                mode: FileMode::Regular,
                size: Some(10),
            })
        };
        ChangedFile {
            change,
            old: (change != Change::Added).then(version).flatten(),
            new: (change != Change::Deleted).then(version).flatten(),
            binary: false,
            lines: Some(LineCounts {
                added: 2,
                deleted: 1,
            }),
        }
    }

    /// Opens `link` and loads it, as rozed does.
    fn open_loaded(
        workspace: &Entity<Workspace>,
        link: &str,
        cx: &mut VisualTestContext,
    ) -> Entity<PrTab> {
        let link = self::link(link);
        let tab = workspace.update_in(cx, |workspace, window, cx| {
            open(workspace, link, Trust::Untrusted, window, cx)
        });
        cx.run_until_parked();
        tab
    }

    fn status(tab: &Entity<PrTab>, cx: &mut VisualTestContext) -> Status {
        tab.read_with(cx, |tab, _| tab.status().clone())
    }

    /// A public pull request on the fake GitHub: a source file, its test and a lockfile.
    fn public_pull(github: &FakeGithub, git: &GitServer, repo: &str, number: u64) -> FakePull {
        let pull = git.pull_request(
            repo,
            number,
            &[("src/main.rs", "fn main() {}\n"), ("Cargo.lock", "a\n")],
            &[
                ("src/main.rs", "fn main() {\n    run();\n}\n"),
                ("tests/main.rs", "#[test]\nfn runs() {}\n"),
                ("Cargo.lock", "b\n"),
            ],
        );
        github.add_pull(pull.clone());
        github.make_public(repo);
        pull
    }

    #[gpui::test]
    async fn a_public_pull_request_loads_without_signing_in(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        let pull = public_pull(&github, &git, "sharkdp/fd", 2212);
        anonymous(&github, cx);

        let tab = open_loaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);

        assert_eq!(status(&tab, cx), Status::Ready);
        tab.read_with(cx, |tab, cx| {
            assert_eq!(tab.acting(), Some(&Acting::Anonymous));
            assert_eq!(tab.header().title.as_deref(), Some("Pull request 2212"));
            assert_eq!(tab.header().repo.as_deref(), Some("sharkdp/fd"));
            let paths: Vec<_> = tab.files().rows().map(FileEntry::path).collect();
            assert_eq!(paths, ["src/main.rs", "tests/main.rs", "Cargo.lock"]);
            let revisions = tab.revisions().unwrap();
            assert_eq!(revisions.label(&pull.head).as_deref(), Some("r1"));
            assert!(tab.diff().is_some());
            assert_eq!(tab.tab_content_text(0, cx), "fd#2212 Pull request 2212");
        });
        assert_eq!(
            github.requests(),
            ["GET /repos/sharkdp/fd/pulls/2212 as nobody"]
        );
    }

    #[gpui::test]
    async fn a_private_pull_request_loads_with_the_account_that_can_see_it(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        let pull = git.pull_request(
            "rozed-dev/playground",
            2,
            &[("a.rs", "a\n")],
            &[("a.rs", "b\n")],
        );
        github.add_pull(pull);
        github.let_see("gho_BOT", "rozed-dev/playground");
        sign_in(
            &github,
            &[("other", "gho_OTHER"), ("rozed-bot", "gho_BOT")],
            cx,
        )
        .await;

        let tab = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);

        assert_eq!(status(&tab, cx), Status::Ready);
        tab.read_with(cx, |tab, _| {
            assert_eq!(tab.acting(), Some(&Acting::Account("rozed-bot".into())));
            assert_eq!(tab.acting_text(), "as rozed-bot");
            let paths: Vec<_> = tab.files().rows().map(FileEntry::path).collect();
            assert_eq!(paths, ["a.rs"]);
        });
        let requests = github.requests();
        assert!(
            requests.contains(&"GET /repos/rozed-dev/playground/pulls/2 as gho_BOT".to_string()),
            "{requests:#?}"
        );
        assert!(
            !requests
                .iter()
                .any(|request| request.ends_with("as nobody")),
            "{requests:#?}"
        );
    }

    /// GitHub's answer when an organization's SAML single sign-on hasn't authorized a token.
    fn needs_sso() -> Answer {
        Answer::new(
            403,
            r#"{"message":"Resource protected by organization SAML enforcement. You must grant your OAuth token access to this organization."}"#,
        )
        .header(
            "x-github-sso",
            "required; url=https://github.com/orgs/acme/sso?authorization_request=A1",
        )
    }

    fn help_links(tab: &Entity<PrTab>, cx: &mut VisualTestContext) -> Vec<String> {
        tab.read_with(cx, |tab, _| {
            tab.help_links()
                .iter()
                .map(|link| link.url().to_string())
                .collect()
        })
    }

    /// What the account picker asks about, an owner or one repository, and the accounts it names,
    /// if it's open.
    fn picker(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> Option<(String, Vec<String>)> {
        workspace.read_with(cx, |workspace, cx| {
            let picker = workspace.active_modal::<account_picker::AccountPicker>(cx)?;
            let question = picker.read(cx).question();
            Some((question.subject(), question.logins.clone()))
        })
    }

    fn logins(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    /// Starts loading `tabs`, one after another.
    fn load(tabs: &[&Entity<PrTab>], cx: &mut VisualTestContext) {
        for tab in tabs {
            tab.update_in(cx, |tab, window, cx| tab.load(window, cx));
            cx.run_until_parked();
        }
    }

    fn acting(tab: &Entity<PrTab>, cx: &mut VisualTestContext) -> Option<Acting> {
        tab.read_with(cx, |tab, _| tab.acting().cloned())
    }

    /// Pull requests on the fake GitHub, each `(repo, number, tokens that can see it)`, and the
    /// accounts `(login, token)` signed in.
    async fn pulls_seen_by(
        github: &FakeGithub,
        git: &GitServer,
        pulls: &[(&str, u64, &[&str])],
        accounts: &[(&str, &str)],
        cx: &mut TestAppContext,
    ) -> Entity<github::accounts::Accounts> {
        for (repo, number, tokens) in pulls {
            let pull = git.pull_request(repo, *number, &[], &[("a.rs", "a\n")]);
            github.add_pull(pull);
            for token in *tokens {
                github.let_see(token, repo);
            }
        }
        sign_in(github, accounts, cx).await.0
    }

    /// `acme/app#1` and `acme/api#2` on the fake GitHub, which `home` and `work` can both see.
    async fn two_accounts_see_acme(
        github: &FakeGithub,
        git: &GitServer,
        cx: &mut TestAppContext,
    ) -> Entity<github::accounts::Accounts> {
        for (repo, number) in [("acme/app", 1), ("acme/api", 2)] {
            let pull = git.pull_request(repo, number, &[], &[("a.rs", "a\n")]);
            github.add_pull(pull);
            github.let_see("gho_HOME", repo);
            github.let_see("gho_WORK", repo);
        }
        let (accounts, _) =
            sign_in(github, &[("home", "gho_HOME"), ("work", "gho_WORK")], cx).await;
        accounts
    }

    #[gpui::test]
    async fn with_several_accounts_it_asks_once_per_owner_and_remembers(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        let accounts = two_accounts_see_acme(&github, &git, cx).await;

        // The picker names each account, and the tab waits for the answer.
        let tab = open_loaded(&workspace, "github.com/acme/app/pull/1", cx);
        assert_eq!(
            picker(&workspace, cx),
            Some((
                "acme".to_string(),
                vec!["home".to_string(), "work".to_string()]
            ))
        );
        assert!(matches!(status(&tab, cx), Status::Loading(_)));
        assert_eq!(tab.read_with(cx, |tab, _| tab.acting().cloned()), None);
        github.requests();

        cx.simulate_keystrokes("j enter");
        cx.run_until_parked();

        assert_eq!(picker(&workspace, cx), None);
        assert_eq!(status(&tab, cx), Status::Ready);
        assert_eq!(tab.read_with(cx, |tab, _| tab.acting_text()), "as work");
        let requests = github.requests();
        assert!(
            requests.contains(&"GET /repos/acme/app/pulls/1 as gho_WORK".to_string()),
            "{requests:#?}"
        );
        assert_eq!(
            accounts.read_with(cx, |accounts, _| accounts
                .remembered_account("acme")
                .unwrap()),
            Some("work".to_string())
        );

        // Another of the owner's repositories opens with it, without asking.
        let api = open_loaded(&workspace, "github.com/acme/api/pull/2", cx);
        assert_eq!(picker(&workspace, cx), None);
        assert_eq!(status(&api, cx), Status::Ready);
        assert_eq!(api.read_with(cx, |tab, _| tab.acting_text()), "as work");
    }

    #[gpui::test]
    async fn closing_the_account_picker_opens_nothing_until_the_link_is_opened_again(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        let accounts = two_accounts_see_acme(&github, &git, cx).await;

        let tab = open_loaded(&workspace, "github.com/acme/app/pull/1", cx);
        github.requests();
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

        assert_eq!(picker(&workspace, cx), None);
        let Status::Failed(message) = status(&tab, cx) else {
            panic!("the tab opened: {:?}", status(&tab, cx));
        };
        assert_eq!(
            message.as_ref(),
            "No account was chosen for acme. Open the pull request again to choose one."
        );
        assert_eq!(github.requests(), Vec::<String>::new());
        assert_eq!(
            accounts.read_with(cx, |accounts, _| accounts
                .remembered_account("acme")
                .unwrap()),
            None
        );

        // Opening it again asks again.
        let reopened = open_loaded(&workspace, "github.com/acme/app/pull/1", cx);
        assert_eq!(reopened, tab);
        assert_eq!(
            picker(&workspace, cx).map(|(owner, _)| owner),
            Some("acme".to_string())
        );
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(status(&tab, cx), Status::Ready);
        assert_eq!(tab.read_with(cx, |tab, _| tab.acting_text()), "as home");
    }

    #[gpui::test]
    async fn two_tabs_waiting_on_the_same_owner_share_one_answer(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        two_accounts_see_acme(&github, &git, cx).await;

        let app = open_unloaded(&workspace, "github.com/acme/app/pull/1", cx);
        let api = open_unloaded(&workspace, "github.com/acme/api/pull/2", cx);
        for tab in [&app, &api] {
            tab.update_in(cx, |tab, window, cx| tab.load(window, cx));
        }
        cx.run_until_parked();
        assert!(picker(&workspace, cx).is_some());

        cx.simulate_keystrokes("down enter");
        cx.run_until_parked();

        assert_eq!(picker(&workspace, cx), None);
        for tab in [&app, &api] {
            assert_eq!(status(tab, cx), Status::Ready);
            assert_eq!(tab.read_with(cx, |tab, _| tab.acting_text()), "as work");
        }
    }

    #[gpui::test]
    async fn an_answer_goes_only_to_the_tabs_whose_accounts_include_it(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        // home and work can see acme/app; home and personal can see acme/secret.
        let accounts = pulls_seen_by(
            &github,
            &git,
            &[
                ("acme/app", 1, &["gho_HOME", "gho_WORK"]),
                ("acme/secret", 2, &["gho_HOME", "gho_PERSONAL"]),
            ],
            &[
                ("home", "gho_HOME"),
                ("personal", "gho_PERSONAL"),
                ("work", "gho_WORK"),
            ],
            cx,
        )
        .await;
        let app = open_unloaded(&workspace, "github.com/acme/app/pull/1", cx);
        let secret = open_unloaded(&workspace, "github.com/acme/secret/pull/2", cx);
        load(&[&app, &secret], cx);

        // Both checks are done before the answer, and the picker lists acme/app's accounts.
        let requests = github.requests();
        assert!(
            requests.contains(&"GET /repos/acme/secret as gho_PERSONAL".to_string()),
            "{requests:#?}"
        );
        assert_eq!(
            picker(&workspace, cx),
            Some(("acme".to_string(), logins(&["home", "work"])))
        );
        for tab in [&app, &secret] {
            assert!(matches!(status(tab, cx), Status::Loading(_)));
            assert_eq!(acting(tab, cx), None);
        }

        cx.simulate_keystrokes("j enter");
        cx.run_until_parked();

        // acme/app opens as work. work can't see acme/secret, so it's asked about with its own
        // accounts, for itself alone, and isn't opened as work.
        assert_eq!(status(&app, cx), Status::Ready);
        assert_eq!(acting(&app, cx), Some(Acting::Account("work".into())));
        assert_eq!(
            picker(&workspace, cx),
            Some(("acme/secret".to_string(), logins(&["home", "personal"])))
        );
        assert!(matches!(status(&secret, cx), Status::Loading(_)));
        assert_eq!(acting(&secret, cx), None);
        let requests = github.requests();
        assert!(
            !requests
                .iter()
                .any(|request| request.contains("/repos/acme/secret/pulls/")),
            "{requests:#?}"
        );

        cx.simulate_keystrokes("j enter");
        cx.run_until_parked();

        assert_eq!(status(&secret, cx), Status::Ready);
        assert_eq!(
            acting(&secret, cx),
            Some(Acting::Account("personal".into()))
        );
        // acme keeps work, and acme/secret has its own.
        accounts.read_with(cx, |accounts, _| {
            assert_eq!(
                accounts.remembered("acme", "secret").unwrap(),
                github::accounts::Remembered {
                    repo: Some("personal".into()),
                    owner: Some("work".into()),
                }
            );
        });
    }

    #[gpui::test]
    async fn when_the_owners_account_cant_see_a_repository_it_asks_for_that_repository_alone(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        let accounts = pulls_seen_by(
            &github,
            &git,
            &[
                ("acme/app", 1, &["gho_HOME", "gho_WORK"]),
                ("acme/secret", 2, &["gho_HOME", "gho_PERSONAL"]),
            ],
            &[
                ("home", "gho_HOME"),
                ("personal", "gho_PERSONAL"),
                ("work", "gho_WORK"),
            ],
            cx,
        )
        .await;
        accounts
            .update(cx, |accounts, cx| {
                accounts.remember_account_for_owner("acme", "work", cx)
            })
            .await
            .unwrap();

        let secret = open_loaded(&workspace, "github.com/acme/secret/pull/2", cx);
        assert_eq!(
            picker(&workspace, cx),
            Some(("acme/secret".to_string(), logins(&["home", "personal"])))
        );
        let owners_account = workspace.read_with(cx, |workspace, cx| {
            let picker = workspace
                .active_modal::<account_picker::AccountPicker>(cx)
                .unwrap();
            picker.read(cx).question().owners_account.clone()
        });
        assert_eq!(owners_account.as_deref(), Some("work"));
        assert_eq!(
            status(&secret, cx),
            Status::Loading("Choose the account rozed uses for acme/secret…".into())
        );

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(acting(&secret, cx), Some(Acting::Account("home".into())));
        assert_eq!(
            accounts.read_with(cx, |accounts, _| accounts
                .remembered_account("acme")
                .unwrap()),
            Some("work".to_string())
        );

        // The owner's other repositories still open with work, and acme/secret with home, without
        // asking.
        let app = open_loaded(&workspace, "github.com/acme/app/pull/1", cx);
        assert_eq!(picker(&workspace, cx), None);
        assert_eq!(acting(&app, cx), Some(Acting::Account("work".into())));
        close(&workspace, secret, cx);
        let secret = open_loaded(&workspace, "github.com/acme/secret/pull/2", cx);
        assert_eq!(picker(&workspace, cx), None);
        assert_eq!(status(&secret, cx), Status::Ready);
        assert_eq!(acting(&secret, cx), Some(Acting::Account("home".into())));
    }

    #[gpui::test]
    async fn a_transferred_repository_uses_and_keeps_its_current_owners_account(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        // alice/tool was transferred to BigOrg.
        let mut pull = git.pull_request("BigOrg/tool", 1, &[], &[("a.rs", "a\n")]);
        pull.old_names = vec!["alice/tool".into()];
        github.add_pull(pull);
        for token in ["gho_HOME", "gho_WORK"] {
            github.let_see(token, "BigOrg/tool");
        }
        let (accounts, _) =
            sign_in(&github, &[("home", "gho_HOME"), ("work", "gho_WORK")], cx).await;
        accounts
            .update(cx, |accounts, cx| {
                accounts.remember_account_for_owner("alice", "home", cx)
            })
            .await
            .unwrap();

        // An old link asks about the owner now, not alice, whose account is home.
        let tab = open_loaded(&workspace, "github.com/alice/tool/pull/1", cx);
        assert_eq!(
            picker(&workspace, cx),
            Some(("BigOrg".to_string(), logins(&["home", "work"])))
        );
        cx.simulate_keystrokes("j enter");
        cx.run_until_parked();
        assert_eq!(acting(&tab, cx), Some(Acting::Account("work".into())));
        accounts.read_with(cx, |accounts, _| {
            assert_eq!(
                accounts.remembered_account("bigorg").unwrap().as_deref(),
                Some("work")
            );
            assert_eq!(
                accounts.remembered_account("alice").unwrap().as_deref(),
                Some("home")
            );
        });

        // Opened by its old name again, it uses BigOrg's choice.
        close(&workspace, tab, cx);
        let tab = open_loaded(&workspace, "github.com/alice/tool/pull/1", cx);
        assert_eq!(picker(&workspace, cx), None);
        assert_eq!(acting(&tab, cx), Some(Acting::Account("work".into())));
    }

    #[gpui::test]
    async fn another_dialog_or_a_click_elsewhere_leaves_the_picker_and_its_tab_waiting(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        two_accounts_see_acme(&github, &git, cx).await;
        let tab = open_loaded(&workspace, "github.com/acme/app/pull/1", cx);
        let asking = Some(("acme".to_string(), logins(&["home", "work"])));
        assert_eq!(picker(&workspace, cx), asking);

        // cmd-o's box doesn't replace it.
        cx.simulate_keystrokes("cmd-o");
        cx.run_until_parked();
        assert_eq!(picker(&workspace, cx), asking);
        // Nor does a click outside it.
        cx.simulate_click(gpui::point(px(10.), px(600.)), gpui::Modifiers::none());
        cx.run_until_parked();
        assert_eq!(picker(&workspace, cx), asking);
        assert!(matches!(status(&tab, cx), Status::Loading(_)));

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(status(&tab, cx), Status::Ready);
        assert_eq!(acting(&tab, cx), Some(Acting::Account("home".into())));
    }

    #[gpui::test]
    async fn a_question_waits_while_another_dialog_is_open(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        two_accounts_see_acme(&github, &git, cx).await;
        let tab = open_unloaded(&workspace, "github.com/acme/app/pull/1", cx);
        let open_box = |cx: &mut VisualTestContext| {
            workspace.read_with(cx, |workspace, cx| {
                workspace
                    .active_modal::<crate::OpenBox>(cx)
                    .map(|open_box| open_box.read(cx).input().read(cx).text(cx))
            })
        };
        cx.simulate_keystrokes("cmd-o");
        cx.simulate_input("github.com/acme");

        // The tab's question waits for the box, which keeps what's typed.
        load(&[&tab], cx);
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        assert_eq!(open_box(cx).as_deref(), Some("github.com/acme"));
        assert_eq!(picker(&workspace, cx), None);
        assert!(matches!(status(&tab, cx), Status::Loading(_)));

        cx.simulate_keystrokes("escape");
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        assert_eq!(
            picker(&workspace, cx),
            Some(("acme".to_string(), logins(&["home", "work"])))
        );
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(status(&tab, cx), Status::Ready);
    }

    #[gpui::test]
    async fn questions_about_two_owners_take_turns(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        let both: &[&str] = &["gho_HOME", "gho_WORK"];
        pulls_seen_by(
            &github,
            &git,
            &[("acme/app", 1, both), ("beta/x", 2, both)],
            &[("home", "gho_HOME"), ("work", "gho_WORK")],
            cx,
        )
        .await;
        let acme = open_unloaded(&workspace, "github.com/acme/app/pull/1", cx);
        let beta = open_unloaded(&workspace, "github.com/beta/x/pull/2", cx);
        load(&[&acme, &beta], cx);

        // beta's question waits its turn; its tab doesn't fail.
        assert_eq!(
            picker(&workspace, cx)
                .map(|(subject, _)| subject)
                .as_deref(),
            Some("acme")
        );
        assert!(matches!(status(&beta, cx), Status::Loading(_)));

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(status(&acme, cx), Status::Ready);
        assert_eq!(
            picker(&workspace, cx)
                .map(|(subject, _)| subject)
                .as_deref(),
            Some("beta")
        );
        cx.simulate_keystrokes("j enter");
        cx.run_until_parked();
        assert_eq!(status(&beta, cx), Status::Ready);
        assert_eq!(acting(&beta, cx), Some(Acting::Account("work".into())));
        assert_eq!(acting(&acme, cx), Some(Acting::Account("home".into())));
    }

    #[gpui::test]
    async fn a_check_that_finishes_after_the_answer_uses_it(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        let accounts = two_accounts_see_acme(&github, &git, cx).await;
        let app = open_unloaded(&workspace, "github.com/acme/app/pull/1", cx);
        let api = open_unloaded(&workspace, "github.com/acme/api/pull/2", cx);
        // acme/api's check waits in the accounts' queues until after the answer.
        github.hold("/repos/acme/api");
        load(&[&app, &api], cx);
        assert_eq!(
            picker(&workspace, cx),
            Some(("acme".to_string(), logins(&["home", "work"])))
        );

        cx.simulate_keystrokes("j enter");
        cx.run_until_parked();
        assert_eq!(picker(&workspace, cx), None);
        assert_eq!(
            accounts.read_with(cx, |accounts, _| accounts
                .remembered_account("acme")
                .unwrap()),
            Some("work".to_string())
        );

        github.let_go("/repos/acme/api");
        cx.run_until_parked();
        assert_eq!(picker(&workspace, cx), None);
        for tab in [&app, &api] {
            assert_eq!(status(tab, cx), Status::Ready);
            assert_eq!(acting(tab, cx), Some(Acting::Account("work".into())));
        }
    }

    #[gpui::test]
    async fn a_poller_started_while_rozed_is_in_the_background_starts_slow(
        cx: &mut TestAppContext,
    ) {
        let (_workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        sign_in(&github, &[("home", "gho_HOME")], cx).await;
        let layer = cx.update(|_, cx| github_layer::ready(cx));
        let layer = layer.await.unwrap();

        layer.update(cx, |layer, cx| layer.set_foreground(false, cx));
        let poller = layer.update(cx, |layer, cx| layer.poller("home", cx));

        assert!(!poller.read_with(cx, |poller, _| poller.is_foreground()));
    }

    #[gpui::test]
    async fn when_no_account_can_check_it_the_tab_says_why_with_githubs_link(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        // A public repository, which would open without signing in.
        public_pull(&github, &git, "acme/app", 7);
        github.answer("gho_WORK", "/repos/acme/app", needs_sso());
        sign_in(&github, &[("work", "gho_WORK")], cx).await;

        let tab = open_loaded(&workspace, "github.com/acme/app/pull/7", cx);

        let Status::Failed(message) = status(&tab, cx) else {
            panic!("the tab opened: {:?}", status(&tab, cx));
        };
        assert!(
            message.contains("work: Resource protected by organization SAML enforcement."),
            "{message}"
        );
        assert_eq!(
            help_links(&tab, cx),
            ["https://github.com/orgs/acme/sso?authorization_request=A1"]
        );
        let requests = github.requests();
        assert!(
            !requests
                .iter()
                .any(|request| request.ends_with("as nobody")),
            "{requests:#?}"
        );
        tab.read_with(cx, |tab, _| {
            assert_eq!(tab.acting(), None);
            assert_eq!(tab.title(), "Not opened");
        });
    }

    #[gpui::test]
    async fn a_remembered_account_that_cant_check_is_never_swapped_for_another(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        let pull = git.pull_request("acme/api", 12, &[("a.rs", "a\n")], &[("a.rs", "b\n")]);
        github.add_pull(pull);
        github.let_see("gho_WORK", "acme/api");
        github.let_see("gho_HOME", "acme/api");
        let (accounts, _) =
            sign_in(&github, &[("home", "gho_HOME"), ("work", "gho_WORK")], cx).await;
        accounts
            .update(cx, |accounts, cx| {
                accounts.remember_account_for_owner("acme", "work", cx)
            })
            .await
            .unwrap();
        github.answer("gho_WORK", "/repos/acme/api", needs_sso());

        let tab = open_loaded(&workspace, "github.com/acme/api/pull/12", cx);

        let Status::Failed(message) = status(&tab, cx) else {
            panic!("the tab opened: {:?}", status(&tab, cx));
        };
        assert!(
            message.starts_with("work, the account rozed uses for acme, couldn't check acme/api"),
            "{message}"
        );
        assert_eq!(
            help_links(&tab, cx),
            ["https://github.com/orgs/acme/sso?authorization_request=A1"]
        );
        let requests = github.requests();
        assert!(
            !requests.iter().any(|request| request.contains("/pulls/")),
            "{requests:#?}"
        );

        // Signed out, it isn't swapped either.
        accounts.update(cx, |accounts, cx| {
            accounts.mark_signed_out("work", github::accounts::SignedOut::Refused, cx)
        });
        let reopened = open_loaded(&workspace, "github.com/acme/api/pull/12", cx);
        assert_eq!(reopened, tab);
        let Status::Failed(message) = status(&tab, cx) else {
            panic!("the tab opened: {:?}", status(&tab, cx));
        };
        assert!(message.contains("work is signed out"), "{message}");
        assert_eq!(help_links(&tab, cx), Vec::<String>::new());
    }

    /// `rozed-dev/playground#2` on the fake GitHub, which `rozed-bot` can see, with what rozed
    /// saved about it for `rozed-bot` two hours ago.
    async fn playground_seen_two_hours_ago(
        github: &FakeGithub,
        git: &GitServer,
        cx: &mut TestAppContext,
    ) -> PrCache {
        let mut pull = git.pull_request(
            "rozed-dev/playground",
            2,
            &[("a.rs", "a\n"), ("b.rs", "b\n")],
            &[("a.rs", "a2\n"), ("b.rs", "b2\n")],
        );
        pull.title = "Add b".into();
        github.add_pull(pull);
        github.let_see("gho_BOT", "rozed-dev/playground");
        let (_, cache) = sign_in(github, &[("rozed-bot", "gho_BOT")], cx).await;
        let two_hours_ago = SystemTime::now() - Duration::from_secs(2 * 3_600 + 30);
        cache
            .put(
                "rozed-bot",
                &Target::Pull(PullRef::new("rozed-dev", "playground", 2)),
                &details("Add b", "VIEWED"),
                None,
                two_hours_ago,
            )
            .await
            .unwrap();
        cache
    }

    fn banners(tab: &Entity<PrTab>, cx: &mut VisualTestContext) -> Vec<String> {
        tab.read_with(cx, |tab, cx| {
            tab.banners()
                .read(cx)
                .messages()
                .iter()
                .map(|message| message.text.to_string())
                .collect()
        })
    }

    #[gpui::test]
    async fn offline_what_rozed_saved_shows_with_its_age(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        let cache = playground_seen_two_hours_ago(&github, &git, cx).await;
        github.set_offline(true);

        let tab = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);

        let Status::Failed(message) = status(&tab, cx) else {
            panic!("expected the tab to fail offline: {:?}", status(&tab, cx));
        };
        assert!(message.contains("couldn't reach GitHub"), "{message}");
        assert_eq!(
            tab.read_with(cx, |tab, cx| tab.tab_content_text(0, cx)),
            "playground#2 Add b"
        );
        assert_eq!(
            banners(&tab, cx),
            ["Showing what rozed saved for rozed-bot 2 hours ago."]
        );
        // There's no local copy, so no diff.
        tab.read_with(cx, |tab, _| {
            assert!(tab.diff().is_none());
            assert_eq!(tab.files().len(), 0);
        });
        // Offline, GitHub can't confirm the pull request, so nothing records it as opened.
        assert_eq!(cache.recent(10).unwrap(), Vec::new());

        // It keeps checking, and once GitHub answers again it loads, and the banner goes.
        github.set_offline(false);
        cx.executor().advance_clock(Duration::from_secs(5));
        cx.run_until_parked();
        assert_eq!(status(&tab, cx), Status::Ready);
        assert_eq!(banners(&tab, cx), Vec::<String>::new());
        let recent: Vec<_> = cache
            .recent(10)
            .unwrap()
            .into_iter()
            .map(|recent| (recent.pull.to_string(), recent.account))
            .collect();
        assert_eq!(
            recent,
            [(
                "rozed-dev/playground#2".to_string(),
                "rozed-bot".to_string()
            )]
        );
    }

    /// `rozed-dev/playground#2`, opened once online, which copied it, and closed; with what
    /// rozed saved about it for `rozed-bot` two hours ago, `saved` from the pull request on the
    /// fake GitHub. Then offline, GitHub's git server included.
    async fn playground_copied_then_offline(
        workspace: &Entity<Workspace>,
        github: &FakeGithub,
        git: &GitServer,
        saved: impl FnOnce(&FakePull) -> PullRequestData,
        cx: &mut VisualTestContext,
    ) -> FakePull {
        let mut pull = git.pull_request(
            "rozed-dev/playground",
            2,
            &[("a.rs", "a\n"), ("b.rs", "b\n")],
            &[("a.rs", "a2\n"), ("b.rs", "b2\n")],
        );
        pull.title = "Add b".into();
        github.add_pull(pull.clone());
        github.let_see("gho_BOT", "rozed-dev/playground");
        let (_, cache) = sign_in(github, &[("rozed-bot", "gho_BOT")], cx).await;
        let tab = open_loaded(workspace, "github.com/rozed-dev/playground/pull/2", cx);
        assert_eq!(status(&tab, cx), Status::Ready);
        close(workspace, tab, cx);

        let two_hours_ago = SystemTime::now() - Duration::from_secs(2 * 3_600 + 30);
        cache
            .put(
                "rozed-bot",
                &Target::Pull(PullRef::new("rozed-dev", "playground", 2)),
                &saved(&pull),
                None,
                two_hours_ago,
            )
            .await
            .unwrap();
        github.set_offline(true);
        git.hide("rozed-dev/playground");
        pull
    }

    /// What rozed saved about `pull`: its head and base as GitHub gave them.
    fn saved_as_given(pull: &FakePull) -> PullRequestData {
        details_with("Add b", "VIEWED", &pull.head, &pull.base_branch, &pull.base)
    }

    /// The text of each file's old side in `tab`'s diff, sorted.
    fn old_texts(tab: &Entity<PrTab>, cx: &mut VisualTestContext) -> Vec<String> {
        tab.read_with(cx, |tab, cx| {
            let editor = tab.diff().unwrap().read(cx).rhs_editor().clone();
            let multi_buffer = editor.read(cx).buffer().read(cx);
            let mut texts: Vec<String> = multi_buffer
                .all_buffers()
                .iter()
                .filter_map(|buffer| {
                    let diff = multi_buffer.diff_for(buffer.read(cx).remote_id())?;
                    diff.read(cx).base_text_string(cx)
                })
                .collect();
            texts.sort();
            texts
        })
    }

    #[gpui::test]
    async fn offline_a_pull_request_the_local_copy_has_shows_its_diff_from_the_copy(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        let pull =
            playground_copied_then_offline(&workspace, &github, &git, saved_as_given, cx).await;

        let tab = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);

        let Status::Offline(message) = status(&tab, cx) else {
            panic!("expected the local copy offline: {:?}", status(&tab, cx));
        };
        assert!(message.starts_with("Offline: "), "{message}");
        assert!(message.contains("couldn't reach GitHub"), "{message}");
        assert!(
            message.ends_with("until GitHub answers again; rozed keeps trying."),
            "{message}"
        );
        assert_eq!(
            banners(&tab, cx),
            ["Showing what rozed saved for rozed-bot 2 hours ago."]
        );
        tab.read_with(cx, |tab, cx| {
            assert!(tab.diff().is_some());
            let files: Vec<_> = tab
                .files()
                .rows()
                .map(|entry| (entry.path().to_string(), entry.viewed))
                .collect();
            assert_eq!(
                files,
                [
                    ("a.rs".to_string(), Some(ViewedState::Viewed)),
                    ("b.rs".to_string(), Some(ViewedState::Unviewed))
                ]
            );
            assert_eq!(
                tab.revisions().unwrap().label(&pull.head).as_deref(),
                Some("r1")
            );
            assert_eq!(tab.tab_content_text(0, cx), "playground#2 Add b");
        });
        assert_eq!(
            cx.window_title().as_deref(),
            Some("rozed-dev/playground#2 — Add b")
        );
        let diff_text = tab.read_with(cx, |tab, cx| {
            let editor = tab.diff().unwrap().read(cx).rhs_editor().clone();
            editor.read(cx).buffer().read(cx).snapshot(cx).text()
        });
        assert!(diff_text.contains("a2"), "{diff_text}");
        assert!(diff_text.contains("b2"), "{diff_text}");
        // Against the saved base, which the copy has.
        assert_eq!(old_texts(&tab, cx), ["a\n", "b\n"]);

        // While GitHub can't be reached, it keeps checking, less often each time, and stays as
        // it is.
        github.requests();
        cx.executor().advance_clock(Duration::from_secs(5 + 10));
        cx.run_until_parked();
        assert_eq!(
            github.requests(),
            ["GET /rate_limit as nobody", "GET /rate_limit as nobody"]
        );
        assert!(matches!(status(&tab, cx), Status::Offline(_)));

        // Back online, it loads from GitHub by itself: the local copy fetches again, and the
        // offline status and the saved details' banner go.
        github.set_offline(false);
        git.restore("rozed-dev/playground");
        cx.executor().advance_clock(Duration::from_secs(20));
        cx.run_until_parked();
        let requests = github.requests();
        assert!(
            requests.contains(&"GET /repos/rozed-dev/playground/pulls/2 as gho_BOT".to_string()),
            "{requests:#?}"
        );
        assert_eq!(status(&tab, cx), Status::Ready);
        assert_eq!(banners(&tab, cx), Vec::<String>::new());
        tab.read_with(cx, |tab, _| {
            assert_eq!(tab.acting(), Some(&Acting::Account("rozed-bot".into())));
            assert!(tab.diff().is_some());
        });
        assert_eq!(old_texts(&tab, cx), ["a\n", "b\n"]);
    }

    #[gpui::test]
    async fn offline_a_head_the_local_copy_never_fetched_shows_only_the_saved_details(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        // GitHub had a newer head when rozed last saved the details.
        let newer = "e".repeat(40);
        playground_copied_then_offline(
            &workspace,
            &github,
            &git,
            |pull| details_with("Add b", "VIEWED", &newer, &pull.base_branch, &pull.base),
            cx,
        )
        .await;

        let tab = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);

        let Status::Failed(message) = status(&tab, cx) else {
            panic!("expected only the saved details: {:?}", status(&tab, cx));
        };
        assert!(message.contains("couldn't reach GitHub"), "{message}");
        assert_eq!(
            banners(&tab, cx),
            ["Showing what rozed saved for rozed-bot 2 hours ago."]
        );
        tab.read_with(cx, |tab, cx| {
            assert!(tab.diff().is_none());
            assert_eq!(tab.tab_content_text(0, cx), "playground#2 Add b");
        });
    }

    #[gpui::test]
    async fn offline_a_newer_tip_of_the_same_base_branch_uses_the_one_fetched(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        // main moved on after the copy fetched it.
        let moved_on = "e".repeat(40);
        playground_copied_then_offline(
            &workspace,
            &github,
            &git,
            |pull| details_with("Add b", "VIEWED", &pull.head, &pull.base_branch, &moved_on),
            cx,
        )
        .await;

        let tab = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);

        assert!(matches!(status(&tab, cx), Status::Offline(_)));
        assert_eq!(old_texts(&tab, cx), ["a\n", "b\n"]);
    }

    #[gpui::test]
    async fn offline_a_base_branch_the_copy_never_fetched_shows_only_the_saved_details(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        // GitHub retargeted the pull request onto another branch after the copy fetched it, so
        // the base the copy has would show other changes.
        playground_copied_then_offline(
            &workspace,
            &github,
            &git,
            |pull| {
                details_with(
                    "Add b",
                    "VIEWED",
                    &pull.head,
                    "stack-a",
                    "b046244d9569964088e952243e5e557312322488",
                )
            },
            cx,
        )
        .await;

        let tab = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);

        let Status::Failed(message) = status(&tab, cx) else {
            panic!("expected only the saved details: {:?}", status(&tab, cx));
        };
        assert!(message.contains("couldn't reach GitHub"), "{message}");
        tab.read_with(cx, |tab, cx| {
            assert!(tab.diff().is_none());
            assert_eq!(tab.tab_content_text(0, cx), "playground#2 Add b");
        });
    }

    #[gpui::test]
    async fn a_slow_answer_isnt_offline(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        playground_copied_then_offline(&workspace, &github, &git, saved_as_given, cx).await;
        // GitHub can be reached, but answers too slowly.
        github.set_offline(false);
        github.set_timing_out(true);

        let tab = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);

        let Status::Failed(message) = status(&tab, cx) else {
            panic!("expected the tab to fail: {:?}", status(&tab, cx));
        };
        assert!(message.contains("operation timed out"), "{message}");
        assert!(!message.contains("keeps trying"), "{message}");
        tab.read_with(cx, |tab, _| assert!(tab.diff().is_none()));
    }

    #[gpui::test]
    async fn one_accounts_failed_connection_isnt_offline(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        let pull = git.pull_request("rozed-dev/playground", 2, &[], &[("a.rs", "a\n")]);
        github.add_pull(pull);
        github.let_see("gho_BOT", "rozed-dev/playground");
        sign_in(
            &github,
            &[("home", "gho_HOME"), ("rozed-bot", "gho_BOT")],
            cx,
        )
        .await;
        // home's requests can't connect, as through a VPN that's down.
        github.cut_off("gho_HOME");

        let tab = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);

        assert_eq!(status(&tab, cx), Status::Ready);
        assert_eq!(acting(&tab, cx), Some(Acting::Account("rozed-bot".into())));
    }

    #[gpui::test]
    async fn offline_the_newest_saved_details_show_whichever_account_saved_them(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        let pull = git.pull_request("rozed-dev/playground", 2, &[], &[("a.rs", "a\n")]);
        github.add_pull(pull);
        let (accounts, cache) = sign_in(
            &github,
            &[("home", "gho_HOME"), ("rozed-bot", "gho_BOT")],
            cx,
        )
        .await;
        accounts
            .update(cx, |accounts, cx| {
                accounts.remember_account_for_owner("rozed-dev", "rozed-bot", cx)
            })
            .await
            .unwrap();
        // rozed-bot, the owner's account, saved them weeks ago, and home an hour ago.
        let target = Target::Pull(PullRef::new("rozed-dev", "playground", 2));
        for (login, title, ago) in [
            ("rozed-bot", "Old title", 21 * 86_400),
            ("home", "New title", 3_600 + 30),
        ] {
            let at = SystemTime::now() - Duration::from_secs(ago);
            cache
                .put(login, &target, &details(title, "VIEWED"), None, at)
                .await
                .unwrap();
        }
        github.set_offline(true);

        let tab = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);

        assert_eq!(
            tab.read_with(cx, |tab, _| tab.header().title.clone())
                .as_deref(),
            Some("New title")
        );
        assert_eq!(
            banners(&tab, cx),
            ["Showing what rozed saved for home an hour ago."]
        );
    }

    #[test]
    fn only_an_unreachable_github_counts_as_offline() {
        let network =
            |kind| anyhow::Error::new(GithubError::Network(std::io::Error::from(kind).into()));
        let unreachable = || network(std::io::ErrorKind::ConnectionRefused);
        assert!(is_offline(&unreachable()));
        assert!(is_offline(
            &unreachable().context("asking GitHub for a/b#1")
        ));
        // A slow answer isn't offline.
        assert!(!is_offline(&network(std::io::ErrorKind::TimedOut)));
        assert!(!is_offline(&anyhow::anyhow!(
            "GitHub has no pull request a/b#1, or bot can't see it"
        )));
        assert!(!is_offline(&anyhow::Error::new(
            GithubError::Unauthorized {
                message: "Bad credentials".into()
            }
        )));
    }

    #[gpui::test]
    async fn a_pull_request_github_doesnt_have_is_neither_polled_nor_recent(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        let cache = playground_seen_two_hours_ago(&github, &git, cx).await;

        // A typo: there's no #999.
        let tab = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/999", cx);

        let Status::Failed(message) = status(&tab, cx) else {
            panic!("the tab opened: {:?}", status(&tab, cx));
        };
        assert!(message.contains("GitHub has no pull request"), "{message}");
        github.requests();
        cx.executor().advance_clock(Duration::from_secs(10 * 60));
        cx.run_until_parked();
        assert_eq!(github.requests(), Vec::<String>::new());
        assert_eq!(cache.recent(10).unwrap(), Vec::new());
    }

    #[gpui::test]
    async fn a_tab_loading_in_another_pane_doesnt_take_the_open_pull_request(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        playground_seen_two_hours_ago(&github, &git, cx).await;
        let other = git.pull_request("rozed-dev/playground", 3, &[], &[("c.rs", "c\n")]);
        github.add_pull(other);

        // #2 is active in the left pane, and #3 waits in a split to its right.
        let left = workspace.read_with(cx, |workspace, _| workspace.active_pane().clone());
        let two = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);
        let three = open_unloaded(&workspace, "github.com/rozed-dev/playground/pull/3", cx);
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.split_and_move(left.clone(), workspace::SplitDirection::Right, window, cx);
        });
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.activate_item(&two, true, true, window, cx);
        });
        cx.run_until_parked();
        let poller = cx.update(|_, cx| {
            let layer = github_layer::loaded(cx).unwrap();
            layer.update(cx, |layer, cx| layer.poller("rozed-bot", cx))
        });
        let open = |cx: &mut VisualTestContext| {
            poller.read_with(cx, |poller, _| poller.open().map(|pull| pull.number))
        };
        assert_eq!(open(cx), Some(2));

        // #3 loads in the background.
        assert_eq!(status(&two, cx), Status::Ready);
        three.update_in(cx, |tab, window, cx| tab.load(window, cx));
        cx.run_until_parked();

        assert_eq!(status(&three, cx), Status::Ready);
        assert_eq!(open(cx), Some(2));
        // Switching to it makes it the open one.
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.activate_item(&three, true, true, window, cx);
        });
        cx.run_until_parked();
        assert_eq!(open(cx), Some(3));
    }

    #[gpui::test]
    async fn a_new_tab_shows_its_link_until_github_answers(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let tab = open_unloaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);

        assert_eq!(tabs(&workspace, cx), vec![tab.clone()]);
        assert_eq!(active_tab(&workspace, cx), Some(tab.clone()));
        tab.read_with(cx, |tab, cx| {
            assert_eq!(tab.tab_content_text(0, cx), "fd#2212");
            assert!(matches!(tab.status(), Status::Loading(_)));
            assert!(tab.is_file_list_shown());
            assert!(tab.is_threads_panel_shown());
        });
    }

    #[gpui::test]
    async fn the_window_is_titled_after_the_active_pull_request(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        assert_eq!(cx.window_title().as_deref(), Some("rozed"));
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        public_pull(&github, &git, "sharkdp/fd", 2212);
        public_pull(&github, &git, "sharkdp/bat", 3000);
        anonymous(&github, cx);

        // Until GitHub answers, the title is the link's.
        let fd = open_unloaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);
        assert_eq!(cx.window_title().as_deref(), Some("sharkdp/fd#2212"));
        fd.update_in(cx, |tab, window, cx| tab.load(window, cx));
        cx.run_until_parked();
        assert_eq!(
            cx.window_title().as_deref(),
            Some("sharkdp/fd#2212 — Pull request 2212")
        );

        // It follows the active tab.
        let bat = open_loaded(&workspace, "github.com/sharkdp/bat/pull/3000", cx);
        assert_eq!(
            cx.window_title().as_deref(),
            Some("sharkdp/bat#3000 — Pull request 3000")
        );
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.activate_item(&fd, true, true, window, cx);
        });
        cx.run_until_parked();
        assert_eq!(
            cx.window_title().as_deref(),
            Some("sharkdp/fd#2212 — Pull request 2212")
        );

        // And the active pull request's title.
        fd.update(cx, |tab, cx| {
            tab.set_details(
                &details("Honor explicit use-jemalloc requests", "VIEWED"),
                cx,
            )
        });
        cx.run_until_parked();
        assert_eq!(
            cx.window_title().as_deref(),
            Some("sharkdp/fd#2212 — Honor explicit use-jemalloc requests")
        );

        // With none open, it's rozed's.
        close(&workspace, fd, cx);
        close(&workspace, bat, cx);
        assert_eq!(cx.window_title().as_deref(), Some("rozed"));
    }

    #[gpui::test]
    async fn with_an_empty_pane_active_the_window_keeps_the_last_pull_requests_title(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        public_pull(&github, &git, "sharkdp/fd", 2212);
        anonymous(&github, cx);
        let fd = open_loaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);
        let titled = Some("sharkdp/fd#2212 — Pull request 2212");
        assert_eq!(cx.window_title().as_deref(), titled);

        // A split leaves an empty pane active.
        workspace.update_in(cx, |workspace, window, cx| {
            let pane = workspace.active_pane().clone();
            workspace.split_pane(pane, workspace::SplitDirection::Right, window, cx);
        });
        cx.run_until_parked();
        assert!(workspace.read_with(cx, |workspace, cx| workspace.active_item(cx).is_none()));
        assert_eq!(cx.window_title().as_deref(), titled);

        // It follows that pull request's title, without the characters that turn text around.
        fd.update(cx, |tab, cx| {
            tab.set_details(&details("Fix \u{202E}gpj.exe", "VIEWED"), cx)
        });
        cx.run_until_parked();
        assert_eq!(
            cx.window_title().as_deref(),
            Some("sharkdp/fd#2212 — Fix gpj.exe")
        );
        assert_eq!(
            fd.read_with(cx, |tab, cx| tab.tab_content_text(0, cx)),
            "fd#2212 Fix gpj.exe"
        );
    }

    #[gpui::test]
    async fn with_rozeds_title_format_a_new_folder_doesnt_retitle_the_window(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx).await;
        // As rozed's forced settings have it.
        cx.update(|_, cx| {
            cx.update_global::<settings::SettingsStore, _>(|store, cx| {
                let format =
                    serde_json::json!({ "window_title_format": window_title::NO_PULL_REQUEST });
                store.set_server_settings(&format.to_string(), cx).unwrap();
            })
        });
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        public_pull(&github, &git, "sharkdp/fd", 2212);
        anonymous(&github, cx);
        open_loaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);
        let titled = Some("sharkdp/fd#2212 — Pull request 2212");
        assert_eq!(cx.window_title().as_deref(), titled);

        // A trusted checkout adds a folder to the project.
        let project = workspace.read_with(cx, |workspace, _| workspace.project().clone());
        project
            .update(cx, |project, cx| {
                project.find_or_create_worktree("/elsewhere", true, cx)
            })
            .await
            .unwrap();
        cx.run_until_parked();

        assert_eq!(worktrees(&workspace, cx), 1);
        assert_eq!(cx.window_title().as_deref(), titled);
    }

    /// The details an account's poller hands the tab, as GitHub's GraphQL API answers.
    fn details(title: &str, viewed: &str) -> PullRequestData {
        details_at(title, viewed, "d712aa7652cc723fee5fd9b3eba867b0480d40da")
    }

    /// [`details`], with `head` as the pull request's head.
    fn details_at(title: &str, viewed: &str, head: &str) -> PullRequestData {
        details_with(
            title,
            viewed,
            head,
            "stack-a",
            "b046244d9569964088e952243e5e557312322488",
        )
    }

    /// [`details`], with `head` as the pull request's head and `base` as its base branch's tip.
    fn details_with(
        title: &str,
        viewed: &str,
        head: &str,
        base_branch: &str,
        base: &str,
    ) -> PullRequestData {
        serde_json::from_value(serde_json::json!({
            "id": "PR_kwDOVk6v9M8AAAABNQwFBw",
            "number": 2,
            "title": title,
            "url": "https://github.com/rozed-dev/playground/pull/2",
            "state": "OPEN",
            "isDraft": false,
            "updatedAt": "2026-11-06T08:15:07Z",
            "author": {
                "__typename": "User",
                "login": "rozed-bot",
                "avatarUrl": "https://avatars.githubusercontent.com/u/353226052?v=4"
            },
            "baseRefName": base_branch,
            "baseRefOid": base,
            "headRefName": "stack-b",
            "headRefOid": head,
            "isCrossRepository": false,
            "headRepository": { "nameWithOwner": "rozed-dev/playground", "isFork": false },
            "files": {
                "totalCount": 2,
                "pageInfo": { "hasNextPage": false, "endCursor": "Mg" },
                "nodes": [
                    { "path": "a.rs", "viewerViewedState": viewed },
                    { "path": "b.rs", "viewerViewedState": "UNVIEWED" }
                ]
            }
        }))
        .unwrap()
    }

    #[gpui::test]
    async fn the_pollers_details_fill_the_header_and_viewed_marks_once(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        playground_seen_two_hours_ago(&github, &git, cx).await;
        let other = git.pull_request("rozed-dev/playground", 3, &[], &[("c.rs", "c\n")]);
        github.add_pull(other);
        // A changed title makes the workspace report the active item changed, which comes back
        // to the tabs; the same details again must not, or the two would loop.
        let active_item_changes = std::rc::Rc::new(std::cell::Cell::new(0));
        let _subscription = cx.update(|_, cx| {
            let changes = active_item_changes.clone();
            cx.subscribe(&workspace, move |_, event, _| {
                if let workspace::Event::ActiveItemChanged = event {
                    changes.set(changes.get() + 1);
                }
            })
        });
        let marks = |tab: &Entity<PrTab>, cx: &mut VisualTestContext| {
            tab.read_with(cx, |tab, _| {
                tab.files()
                    .rows()
                    .map(|entry| (entry.path().to_string(), entry.viewed))
                    .collect::<Vec<_>>()
            })
        };

        let two = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);
        let poller = cx.update(|_, cx| {
            let layer = github_layer::loaded(cx).unwrap();
            layer.update(cx, |layer, cx| layer.poller("rozed-bot", cx))
        });
        let open = |cx: &mut VisualTestContext| {
            poller.read_with(cx, |poller, _| poller.open().map(|pull| pull.number))
        };

        // The poller's details: the title and the account's viewed marks.
        assert_eq!(status(&two, cx), Status::Ready);
        assert_eq!(open(cx), Some(2));
        assert_eq!(
            two.read_with(cx, |tab, cx| tab.tab_content_text(0, cx)),
            "playground#2 Add b"
        );
        assert_eq!(
            marks(&two, cx),
            [
                ("a.rs".to_string(), Some(ViewedState::Viewed)),
                ("b.rs".to_string(), Some(ViewedState::Unviewed))
            ]
        );

        // The open pull request follows the active tab.
        let three = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/3", cx);
        assert_eq!(status(&three, cx), Status::Ready);
        assert_eq!(open(cx), Some(3));
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.activate_item(&two, true, true, window, cx);
        });
        cx.run_until_parked();
        assert_eq!(open(cx), Some(2));

        // Once settled, the tabs hearing again which is active changes nothing.
        let settled = active_item_changes.get();
        workspace.update(cx, |workspace, cx| sync_active(workspace, cx));
        cx.run_until_parked();
        assert_eq!(active_item_changes.get(), settled);
        assert_eq!(open(cx), Some(2));

        // A file marked viewed elsewhere changes the marks, and only them.
        two.update(cx, |tab, cx| {
            tab.set_details(&details("Add b", "DISMISSED"), cx)
        });
        cx.run_until_parked();
        assert_eq!(
            marks(&two, cx)[0],
            ("a.rs".to_string(), Some(ViewedState::Dismissed))
        );
    }

    #[gpui::test]
    async fn reopening_a_pull_request_switches_to_its_tab(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let fd = open_unloaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);
        let playground = open_unloaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);
        assert_eq!(tabs(&workspace, cx), vec![fd.clone(), playground.clone()]);
        assert_eq!(active_tab(&workspace, cx), Some(playground.clone()));

        let reopened = open_unloaded(
            &workspace,
            "https://github.com/SharkDP/fd/pull/2212/files",
            cx,
        );

        assert_eq!(reopened, fd);
        assert_eq!(tabs(&workspace, cx), vec![fd.clone(), playground.clone()]);
        assert_eq!(active_tab(&workspace, cx), Some(fd.clone()));

        // Once GitHub gives the repository's current name, a link with that name finds the tab.
        playground.update(cx, |tab, cx| {
            let mut pull_request = fd_2212();
            pull_request.base.repo.full_name = "rozed-dev/renamed".into();
            tab.set_pull_request(&pull_request, cx);
        });
        let reopened = open_unloaded(&workspace, "github.com/rozed-dev/renamed/pull/2", cx);
        assert_eq!(reopened, playground);
        assert_eq!(tabs(&workspace, cx).len(), 2);
        assert_eq!(active_tab(&workspace, cx), Some(playground));
    }

    /// `rozed-dev/renamed#2`, once called `rozed-dev/playground`, which `rozed-bot` can see.
    async fn renamed_repository(github: &FakeGithub, git: &GitServer, cx: &mut TestAppContext) {
        let mut pull = git.pull_request("rozed-dev/renamed", 2, &[], &[("a.rs", "a\n")]);
        pull.old_names = vec!["rozed-dev/playground".into()];
        github.add_pull(pull);
        github.let_see("gho_BOT", "rozed-dev/renamed");
        sign_in(github, &[("rozed-bot", "gho_BOT")], cx).await;
    }

    #[gpui::test]
    async fn a_renamed_repositorys_old_name_finds_the_tab_opened_by_its_new_one(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        renamed_repository(&github, &git, cx).await;

        let tab = open_loaded(&workspace, "github.com/rozed-dev/renamed/pull/2", cx);
        assert_eq!(status(&tab, cx), Status::Ready);
        // The old name opens a tab until GitHub names the repository, and then it's the same
        // pull request, so that tab goes and the first comes to the front.
        open_loaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);
        assert_eq!(tabs(&workspace, cx), vec![tab.clone()]);
        assert_eq!(active_tab(&workspace, cx), Some(tab.clone()));

        // From then on, the old name finds the tab at once.
        github.requests();
        let reopened = open_loaded(&workspace, "github.com/Rozed-Dev/Playground/pull/2", cx);
        assert_eq!(reopened, tab);
        assert_eq!(github.requests(), Vec::<String>::new());

        // The poller watches the pull request once, by its name now.
        let poller = cx.update(|_, cx| {
            let layer = github_layer::loaded(cx).unwrap();
            layer.update(cx, |layer, cx| layer.poller("rozed-bot", cx))
        });
        let open = poller.read_with(cx, |poller, _| poller.open().cloned());
        let open = open.unwrap();
        assert_eq!(
            (
                open.repo.owner.as_str(),
                open.repo.name.as_str(),
                open.number
            ),
            ("rozed-dev", "renamed", 2)
        );
    }

    #[gpui::test]
    async fn two_names_opened_before_either_loads_end_in_one_tab(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        renamed_repository(&github, &git, cx).await;

        let old = open_unloaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);
        let new = open_unloaded(&workspace, "github.com/rozed-dev/renamed/pull/2", cx);
        for tab in [&old, &new] {
            tab.update_in(cx, |tab, window, cx| tab.load(window, cx));
        }
        cx.run_until_parked();

        let left = tabs(&workspace, cx);
        assert_eq!(left.len(), 1);
        assert_eq!(status(&left[0], cx), Status::Ready);
        for link in [
            "github.com/rozed-dev/playground/pull/2",
            "github.com/rozed-dev/renamed/pull/2",
        ] {
            assert_eq!(open_unloaded(&workspace, link, cx), left[0]);
        }
    }

    /// Closes `tab`, as its close button does, and lets go of it.
    fn close(workspace: &Entity<Workspace>, tab: Entity<PrTab>, cx: &mut VisualTestContext) {
        let pane = workspace
            .read_with(cx, |workspace, _| workspace.pane_for(&tab))
            .unwrap();
        let id = tab.entity_id();
        drop(tab);
        pane.update_in(cx, |pane, window, cx| {
            pane.close_item_by_id(id, workspace::SaveIntent::Skip, window, cx)
        })
        .detach();
        cx.run_until_parked();
    }

    /// Whether the account's poller polls pull request `number` of `rozed-dev/playground`, as
    /// it does every pull request it watches, within three minutes in the foreground.
    fn polled(number: u64, github: &FakeGithub, cx: &mut VisualTestContext) -> bool {
        github.requests();
        cx.executor().advance_clock(Duration::from_secs(3 * 60));
        cx.run_until_parked();
        let path = format!("/repos/rozed-dev/playground/pulls/{number} ");
        github
            .requests()
            .iter()
            .any(|request| request.contains(&path))
    }

    #[gpui::test]
    async fn a_tab_stays_watched_when_it_moves_and_isnt_once_it_closes(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        playground_seen_two_hours_ago(&github, &git, cx).await;
        let other = git.pull_request("rozed-dev/playground", 3, &[], &[("c.rs", "c\n")]);
        github.add_pull(other);
        let left = workspace.read_with(cx, |workspace, _| workspace.active_pane().clone());
        let three = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/3", cx);
        let two = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);
        let poller = cx.update(|_, cx| {
            let layer = github_layer::loaded(cx).unwrap();
            layer.update(cx, |layer, cx| layer.poller("rozed-bot", cx))
        });
        assert!(poller.read_with(cx, |poller, _| poller.is_foreground()));
        assert!(polled(2, &github, cx));

        // #2 moves into a split, and #3, left behind, is the active tab.
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.split_and_move(left.clone(), workspace::SplitDirection::Right, window, cx);
            workspace.activate_item(&three, true, true, window, cx);
        });
        cx.run_until_parked();
        assert_ne!(
            workspace.read_with(cx, |workspace, _| workspace.pane_for(&two)),
            Some(left)
        );
        assert!(polled(2, &github, cx));
        assert!(polled(3, &github, cx));

        close(&workspace, two, cx);
        assert!(!polled(2, &github, cx));
        assert!(polled(3, &github, cx));
    }

    #[gpui::test]
    async fn a_trusted_checkout_goes_with_the_last_tab_that_uses_it(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let project = workspace.read_with(cx, |workspace, _| workspace.project().clone());
        let languages = project.read_with(cx, |project, _| project.languages().clone());
        languages.add(language::rust_lang());
        let mut servers = languages.register_fake_lsp(
            "Rust",
            language::FakeLspAdapter {
                name: "rust-analyzer",
                ..Default::default()
            },
        );
        // A checkout, which rozed trusts and opens as a folder of the project, with
        // rust-analyzer running.
        cx.update(|_, cx| crate::trust::trust_checkout(&project, Path::new("/elsewhere"), cx))
            .unwrap();
        let (worktree, _) = project
            .update(cx, |project, cx| {
                project.find_or_create_worktree("/elsewhere", true, cx)
            })
            .await
            .unwrap();
        let checkout = worktree.read_with(cx, |worktree, _| worktree.id());
        drop(worktree);
        cx.run_until_parked();
        let (_buffer, _lsp) = project
            .update(cx, |project, cx| {
                project.open_local_buffer_with_lsp("/elsewhere/src/main.rs", cx)
            })
            .await
            .unwrap();
        cx.run_until_parked();
        servers.try_recv().expect("rust-analyzer started");
        let running = |cx: &mut VisualTestContext| {
            project.read_with(cx, |project, cx| {
                project
                    .lsp_store()
                    .read(cx)
                    .language_server_statuses()
                    .count()
            })
        };
        assert_eq!(running(cx), 1);

        // Two tabs use it, as when a second link to the same head opened before the first
        // loaded.
        let first = open_unloaded(&workspace, "github.com/acme/app/pull/1", cx);
        let second = open_unloaded(&workspace, "github.com/acme/app/pull/2", cx);
        for tab in [&first, &second] {
            tab.update(cx, |tab, _| tab.uses_checkout(checkout));
        }

        close(&workspace, first, cx);
        assert_eq!(worktrees(&workspace, cx), 1);
        assert_eq!(running(cx), 1);

        close(&workspace, second, cx);
        assert_eq!(worktrees(&workspace, cx), 0);
        assert_eq!(running(cx), 0);
    }

    /// The row of the diff's cursor.
    fn cursor_row(tab: &Entity<PrTab>, cx: &mut VisualTestContext) -> u32 {
        let editor = tab.read_with(cx, |tab, cx| {
            tab.diff().unwrap().read(cx).rhs_editor().clone()
        });
        editor.update(cx, |editor, cx| {
            let snapshot = editor.display_snapshot(cx);
            editor
                .selections
                .newest::<language::Point>(&snapshot)
                .head()
                .row
        })
    }

    #[gpui::test]
    async fn back_and_forward_retrace_jumps_between_files(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        let lines = |prefix: &str| {
            (0..30)
                .map(|n| format!("{prefix} {n}\n"))
                .collect::<String>()
        };
        let (a, b) = (lines("a"), lines("b"));
        let pull = git.pull_request(
            "sharkdp/fd",
            1,
            &[("a.rs", "a\n"), ("b.rs", "b\n")],
            &[("a.rs", &a), ("b.rs", &b)],
        );
        github.add_pull(pull);
        github.make_public("sharkdp/fd");
        anonymous(&github, cx);
        let tab = open_loaded(&workspace, "github.com/sharkdp/fd/pull/1", cx);
        assert_eq!(status(&tab, cx), Status::Ready);

        // In the file list, j goes to the first file and then the next, and enter goes back to
        // the diff.
        cx.simulate_keystrokes("cmd-b cmd-b j");
        let at_a = cursor_row(&tab, cx);
        cx.simulate_keystrokes("j enter");
        let at_b = cursor_row(&tab, cx);
        assert!(at_b > at_a, "{at_a} → {at_b}");

        cx.simulate_keystrokes("ctrl-o");
        assert_eq!(cursor_row(&tab, cx), at_a);
        cx.simulate_keystrokes("ctrl-i");
        assert_eq!(cursor_row(&tab, cx), at_b);
    }

    #[gpui::test]
    async fn the_header_says_how_polling_goes(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        playground_seen_two_hours_ago(&github, &git, cx).await;
        let note = |tab: &Entity<PrTab>, cx: &mut VisualTestContext| {
            tab.read_with(cx, |tab, _| tab.poll_note().map(|note| note.text))
        };
        // The fake GitHub has no GraphQL API, so the poller's first poll for details fails.
        let tab = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);
        assert_eq!(status(&tab, cx), Status::Ready);
        assert_eq!(
            note(&tab, cx).as_deref(),
            Some("rozed couldn't update this pull request: Not Found")
        );

        github.set_offline(true);
        cx.executor().advance_clock(Duration::from_secs(30));
        cx.run_until_parked();
        let offline = note(&tab, cx).unwrap();
        assert!(
            offline.starts_with("Offline since just now. These details are from 2 hours ago."),
            "{offline}"
        );

        github.set_offline(false);
        let accounts = cx.update(|_, cx| {
            let layer = github_layer::loaded(cx).unwrap();
            layer.read(cx).accounts().clone()
        });
        accounts.update(cx, |accounts, cx| {
            accounts.mark_signed_out("rozed-bot", github::accounts::SignedOut::Refused, cx)
        });
        cx.executor().advance_clock(Duration::from_secs(5 * 60));
        cx.run_until_parked();
        assert_eq!(
            note(&tab, cx).as_deref(),
            Some(
                "Signed out: GitHub refused rozed-bot's token. Sign in again from Accounts… in \
                 the rozed menu."
            )
        );
    }

    #[gpui::test]
    async fn panels_open_and_close_with_their_keys(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let tab = open_unloaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);
        let files = [
            changed("b.rs", Change::Modified),
            changed("a.json", Change::Modified),
        ];
        let head: Oid = "2".repeat(40).parse().unwrap();
        tab.update(cx, |tab, cx| {
            tab.copied(&files, &head, SystemTime::now(), cx)
        });
        let shown = |cx: &mut VisualTestContext| {
            tab.read_with(cx, |tab, _| {
                (tab.is_file_list_shown(), tab.is_threads_panel_shown())
            })
        };
        assert_eq!(shown(cx), (true, true));

        cx.simulate_keystrokes("cmd-b");
        assert_eq!(shown(cx), (false, true));
        cx.simulate_keystrokes("cmd-alt-b");
        assert_eq!(shown(cx), (false, false));
        cx.simulate_keystrokes("cmd-r");
        assert_eq!(shown(cx), (false, true));

        // Showing the file list focuses it, where j and k move and o switches the order.
        cx.simulate_keystrokes("cmd-b");
        assert_eq!(shown(cx), (true, true));
        let in_file_list = |cx: &mut VisualTestContext| {
            let focus = tab.read_with(cx, |tab, _| tab.file_list_focus_handle().clone());
            cx.update(|window, cx| focus.contains_focused(window, cx))
        };
        assert!(in_file_list(cx));
        let selected = |cx: &mut VisualTestContext| {
            tab.read_with(cx, |tab, _| {
                (
                    tab.files().order(),
                    tab.files().selected().map(|entry| entry.path().to_string()),
                )
            })
        };
        cx.simulate_keystrokes("j");
        assert_eq!(selected(cx), (FileOrder::Review, Some("b.rs".into())));
        cx.simulate_keystrokes("j");
        assert_eq!(selected(cx), (FileOrder::Review, Some("a.json".into())));
        cx.simulate_keystrokes("o");
        assert_eq!(selected(cx), (FileOrder::Path, Some("a.json".into())));
        cx.simulate_keystrokes("j k");
        assert_eq!(selected(cx), (FileOrder::Path, Some("a.json".into())));
        cx.simulate_keystrokes("alt-o");
        assert_eq!(selected(cx).0, FileOrder::Review);

        // Escape goes back to the diff, leaving the list shown, and cmd-b then hides it.
        cx.simulate_keystrokes("escape");
        assert!(!in_file_list(cx));
        cx.simulate_keystrokes("cmd-b");
        assert_eq!(shown(cx), (false, true));
    }

    #[gpui::test]
    async fn cmd_o_opens_a_link_in_a_tab_or_switches_to_it(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let fd = open_unloaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);
        let playground = open_unloaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);
        let has_box = |cx: &mut VisualTestContext| {
            workspace.read_with(cx, |workspace, cx| {
                workspace.active_modal::<crate::OpenBox>(cx).is_some()
            })
        };

        cx.simulate_keystrokes("cmd-o");
        assert!(has_box(cx));
        cx.simulate_input("not a link");
        cx.simulate_keystrokes("enter");
        let error = workspace.read_with(cx, |workspace, cx| {
            let open_box = workspace.active_modal::<crate::OpenBox>(cx).unwrap();
            open_box.read(cx).error().cloned()
        });
        assert_eq!(
            error.as_deref(),
            Some("Can't open that: it isn't a link to github.com.")
        );
        cx.simulate_keystrokes("escape");
        assert!(!has_box(cx));

        cx.simulate_keystrokes("cmd-o");
        cx.simulate_input("https://github.com/sharkdp/fd/pull/2212");
        cx.simulate_keystrokes("enter");

        assert!(!has_box(cx));
        assert_eq!(tabs(&workspace, cx), vec![fd.clone(), playground.clone()]);
        assert_eq!(active_tab(&workspace, cx), Some(fd.clone()));

        // A link that isn't open opens and loads in a new tab.
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        public_pull(&github, &git, "sharkdp/bat", 3000);
        anonymous(&github, cx);
        cx.simulate_keystrokes("cmd-o");
        cx.simulate_input("https://github.com/sharkdp/bat/pull/3000/files");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        let all = tabs(&workspace, cx);
        assert_eq!(all.len(), 3);
        // Zed adds a tab after the active one, fd's.
        let bat = all[1].clone();
        assert_eq!(bat.read_with(cx, |tab, _| tab.link().repo.clone()), "bat");
        assert_eq!(active_tab(&workspace, cx), Some(bat.clone()));
        assert_eq!(status(&bat, cx), Status::Ready);
        assert_eq!(
            bat.read_with(cx, |tab, cx| tab.tab_content_text(0, cx)),
            "bat#3000 Pull request 3000"
        );
    }

    #[gpui::test]
    async fn a_tab_that_failed_tries_again_when_its_link_is_opened_again(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        public_pull(&github, &git, "sharkdp/fd", 2212);
        anonymous(&github, cx);
        github.answer(
            "",
            "/repos/sharkdp/fd/pulls/2212",
            Answer::new(502, "Bad gateway"),
        );
        let tab = open_loaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);
        assert!(matches!(status(&tab, cx), Status::Failed(_)));

        // GitHub's back, and the user opens the link again, by another spelling.
        github.forget_answer("", "/repos/sharkdp/fd/pulls/2212");
        let reopened = open_loaded(&workspace, "github.com/SharkDP/fd/pull/2212/files", cx);

        assert_eq!(reopened, tab);
        assert_eq!(tabs(&workspace, cx), vec![tab.clone()]);
        assert_eq!(status(&tab, cx), Status::Ready);
        tab.read_with(cx, |tab, _| assert_eq!(tab.files().len(), 3));

        // A tab that opened only switches.
        github.requests();
        open_loaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);
        assert_eq!(github.requests(), Vec::<String>::new());
    }

    /// Drags `path` from Finder and drops it at the middle of the window.
    fn drop_file(path: &str, cx: &mut VisualTestContext) {
        let position = gpui::point(px(500.), px(400.));
        cx.simulate_event(gpui::FileDropEvent::Entered {
            position,
            paths: gpui::ExternalPaths([std::path::PathBuf::from(path)].into_iter().collect()),
        });
        cx.simulate_event(gpui::FileDropEvent::Pending { position });
        cx.simulate_event(gpui::FileDropEvent::Submit { position });
        cx.run_until_parked();
    }

    fn worktrees(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> usize {
        workspace.read_with(cx, |workspace, cx| {
            workspace.project().read(cx).worktrees(cx).count()
        })
    }

    #[gpui::test]
    async fn a_file_dropped_on_an_empty_pane_opens_nothing(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;

        drop_file("/elsewhere/src/main.rs", cx);
        drop_file("/elsewhere", cx);

        workspace.read_with(cx, |workspace, cx| {
            assert_eq!(workspace.items(cx).count(), 0)
        });
        assert_eq!(worktrees(&workspace, cx), 0);
    }

    #[gpui::test]
    async fn a_file_dropped_on_a_pull_requests_tab_opens_nothing(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let tab = open_unloaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);

        drop_file("/elsewhere/src/main.rs", cx);

        assert_eq!(tabs(&workspace, cx), vec![tab.clone()]);
        workspace.read_with(cx, |workspace, cx| {
            assert_eq!(workspace.items(cx).count(), 1)
        });
        assert_eq!(worktrees(&workspace, cx), 0);

        // Even if a drop reaches the tab, the tab keeps it from the pane.
        let dropped = gpui::ExternalPaths(
            [std::path::PathBuf::from("/elsewhere/src/main.rs")]
                .into_iter()
                .collect(),
        );
        let pane = workspace.read_with(cx, |workspace, _| workspace.active_pane().clone());
        let swallowed = cx.update(|window, cx| {
            pane.update(cx, |pane, cx| {
                tab.update(cx, |tab, cx| tab.handle_drop(pane, &dropped, window, cx))
            })
        });
        assert!(swallowed);
    }

    #[gpui::test]
    async fn no_file_action_runs_or_shows_in_the_command_palette(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        // Where the command palette sends an action: what had focus, here an empty pane, which a
        // double click in the tab bar also sends NewFile to.
        workspace.update_in(cx, |workspace, window, cx| {
            window.focus(&workspace.active_pane().focus_handle(cx), cx)
        });
        cx.run_until_parked();

        let names = crate::read_only::FileActions::names();
        for name in [
            "workspace::NewFile",
            "workspace::NewFileSplit",
            "workspace::NewFileSplitVertical",
            "workspace::NewFileSplitHorizontal",
            "workspace::NewWindow",
            "workspace::SaveAs",
            "workspace::Open",
        ] {
            assert!(names.contains(&name), "{name}");
        }
        for name in names {
            // Two take an argument: which recent project, and which way to split.
            let data = match name {
                "welcome::OpenRecentProject" => Some(serde_json::json!(0)),
                "workspace::NewFileSplit" => Some(serde_json::json!("right")),
                _ => None,
            };
            let action = cx.update(|_, cx| cx.build_action(name, data)).unwrap();
            let hidden = cx.update(|_, cx| {
                command_palette_hooks::CommandPaletteFilter::try_global(cx)
                    .is_some_and(|filter| filter.is_hidden(&*action))
            });
            assert!(hidden, "the command palette lists {name}");
            cx.update(|window, cx| window.dispatch_action(action.boxed_clone(), cx));
            cx.run_until_parked();
            assert_eq!(
                workspace.read_with(cx, |workspace, cx| workspace.items(cx).count()),
                0,
                "{name} opened something"
            );
            assert_eq!(cx.update(|_, cx| cx.windows().len()), 1, "{name}");
            assert_eq!(worktrees(&workspace, cx), 0, "{name}");
        }
    }

    #[gpui::test]
    async fn the_window_opens_editors_read_only(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        // A trusted checkout is a folder of the project, and a file header's Open File opens one
        // of its files in an ordinary editor.
        let project = workspace.read_with(cx, |workspace, _| workspace.project().clone());
        let (worktree, _) = project
            .update(cx, |project, cx| {
                project.find_or_create_worktree("/elsewhere", true, cx)
            })
            .await
            .unwrap();
        cx.run_until_parked();
        let worktree_id = worktree.read_with(cx, |worktree, _| worktree.id());
        let path = RelPath::from_unix_str("src/main.rs").unwrap().into_arc();
        let item = workspace
            .update_in(cx, |workspace, window, cx| {
                workspace.open_path((worktree_id, path), None, true, window, cx)
            })
            .await
            .unwrap();

        let editor = item.downcast::<Editor>().unwrap();
        assert_eq!(
            workspace.read_with(cx, |workspace, cx| workspace.items(cx).count()),
            1
        );
        assert!(editor.read_with(cx, |editor, cx| editor.read_only(cx)));
    }
}
