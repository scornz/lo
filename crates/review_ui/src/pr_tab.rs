//! The PR tab: one pull request's header, its files in review order, its side-by-side diff and its
//! threads (design §10 "Window"). rozed's window has one tab per pull request, and opening a pull
//! request that's already open switches to its tab.

use std::collections::HashMap;
use std::ops::Range;
use std::time::{Duration, Instant, SystemTime};

use anyhow::Context as _;
use futures::channel::oneshot;

use editor::scroll::Autoscroll;
use editor::{Editor, SelectionEffects, SplittableEditor};
use github::files::CutShort;
use github::poll::{PollEvent, Poller};
use github::queries::{FileViewedState, PullRequestData};
use github::{GithubError, HelpLink, PullRef, Target};
use gpui::{
    AnyElement, App, AsyncWindowContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    KeyBinding, Render, SharedString, Subscription, Task, UniformListScrollHandle, WeakEntity,
    Window, actions, uniform_list,
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
use crate::api_diff::{self, FilesReader, GithubDiff, thousands};
use crate::file_list::{FileChange, FileEntry, FileList, FileOrder};
use crate::github_api::{self, CanSignIn, PullRequest};
use crate::github_layer::{self, Saved};
use crate::poll_state::{self, PollNote, PollState};
use crate::pr_banner::{self, BannerAction, BannerKind, Banners};
use crate::pr_diff::{self, CopyProgress, Trust};
use crate::pr_link::PrLink;
use crate::tabs::{TabKey, find_tab};

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
        if matches!(tab.read(cx).status, Status::Failed(_)) {
            log::info!("{link} failed to open before; trying again");
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
    Failed(SharedString),
}

/// Where the diff the tab shows comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffSource {
    /// GitHub's patches: changed lines only, until the local copy is ready (design §11).
    GitHub,
    /// The local copy: whole files.
    LocalCopy,
}

/// Sent when what the tab's title shows changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrTabEvent {
    Changed,
}

/// One pull request in a tab.
pub struct PrTab {
    key: TabKey,
    trust: Trust,
    workspace: WeakEntity<Workspace>,
    focus_handle: FocusHandle,
    file_list_focus: FocusHandle,
    file_list_scroll: UniformListScrollHandle,
    acting: Option<Acting>,
    /// How many accounts could see the repository, when none was chosen for its owner and the
    /// tab acts as the first.
    guessed_among: Option<usize>,
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
    diff_source: Option<DiffSource>,
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
    /// Reads and shows GitHub's diff, until the local copy's replaces it.
    _github_diff: Option<Task<()>>,
    /// Why the local copy failed for good, when it did before GitHub's diff showed.
    copy_failed: Option<SharedString>,
    /// Whether GitHub's diff failed for good, or wasn't read, so it won't show.
    github_failed: bool,
    /// The head GitHub's REST API gave for the pull request, which the local copy fetches.
    api_head: Option<String>,
    /// The head GitHub's diff was listed at, once it shows and if GitHub's links say.
    github_head: Option<String>,
    /// Tries copying again, when rozed stopped trying after failures it doesn't know.
    retry_copy: Option<oneshot::Sender<()>>,
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
            key: TabKey::new(link),
            trust,
            workspace,
            focus_handle,
            file_list_focus: cx.focus_handle(),
            file_list_scroll: UniformListScrollHandle::new(),
            acting: None,
            guessed_among: None,
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
            diff_source: None,
            nav_history: None,
            show_file_list: true,
            show_threads: true,
            watched: None,
            poll: PollState::default(),
            checkout: None,
            opened: Instant::now(),
            _load: None,
            _github_diff: None,
            copy_failed: None,
            github_failed: false,
            api_head: None,
            github_head: None,
            retry_copy: None,
            _subscriptions: vec![on_focus, on_banners],
        }
    }

    /// Starts loading the pull request.
    pub fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (link, trust, opened) = (self.key.opened_as.clone(), self.trust, self.opened);
        self._load = Some(cx.spawn_in(window, async move |this, cx| {
            if let Err(error) = load(this.clone(), link.clone(), trust, opened, cx).await {
                log::error!("{link}: {error:#}");
                let links = error
                    .downcast_ref::<Refusal>()
                    .map(|refusal| refusal.links.clone())
                    .unwrap_or_default();
                this.update(cx, |this, cx| {
                    this.help_links = links;
                    this.show_saved_banner(false, cx);
                    this.load_failed(&error, cx)
                })
                .ok();
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
        self.guessed_among = None;
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

    pub(crate) fn workspace(&self) -> &WeakEntity<Workspace> {
        &self.workspace
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

    /// Where the diff shown comes from, once one shows.
    pub fn diff_source(&self) -> Option<DiffSource> {
        self.diff_source
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
        self.header.title = Some(pull_request.title.clone());
        if let Some(user) = &pull_request.user {
            self.header.author = Some(user.login.clone());
        }
        let repo = &pull_request.base.repo;
        self.header.repo = Some(repo.full_name.clone());
        if let Ok((owner, name)) = repo.owner_and_name() {
            self.key.canonical = Some((owner.to_string(), name.to_string()));
        }
        self.created_at = Some(pull_request.created_at.into());
        self.api_head = Some(pull_request.head.sha.clone());
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
        header.title = Some(details.title.clone());
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

    /// The local copy listed the pull request's files: lists them, builds its revisions, and
    /// returns where each file goes in the diff, by its index in `files`.
    pub(crate) fn copied(
        &mut self,
        files: &[ChangedFile],
        head: &Oid,
        cx: &mut Context<Self>,
    ) -> Vec<usize> {
        self.list(files.iter().map(FileEntry::from_local).collect());
        // Until rozed reads the branch's push history (milestone 6), the head the local copy
        // fetched is the one revision it knows.
        let now = SystemTime::now();
        let records = [PushRecord::Polled {
            head: head.as_str().to_string(),
            at: now,
        }];
        self.revisions = Some(build_revisions(
            &records,
            &[],
            head.as_str(),
            self.created_at.unwrap_or(now),
        ));
        self.status = Status::Loading("Loading the diff…".into());
        cx.notify();
        self.files.review_positions()
    }

    /// Where each of the local copy's `files` goes in its diff, by its index in `files`, without
    /// listing them yet.
    pub(crate) fn positions_of(&self, files: &[ChangedFile]) -> Vec<usize> {
        FileList::new(files.iter().map(FileEntry::from_local).collect()).review_positions()
    }

    /// Lists `entries` in place of the files listed so far, in the same order and keeping the
    /// selected file.
    fn list(&mut self, entries: Vec<FileEntry>) {
        let mut list = FileList::new(entries);
        list.set_order(self.files.order());
        list.set_viewed(&self.viewed);
        if let Some(selected) = self.files.selected() {
            list.select_path(selected.path());
        }
        self.files = list;
    }

    /// Reads GitHub's diff of the pull request with `reader` and shows it until the local copy's
    /// diff is ready.
    pub(crate) fn load_github_diff(
        &mut self,
        reader: FilesReader,
        pull_request: &PullRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let link = self.link().clone();
        let (owner, name) = match pull_request.base.repo.owner_and_name() {
            Ok(owner_and_name) => owner_and_name,
            Err(error) => {
                log::warn!("{link}: not reading GitHub's diff: {error:#}");
                return;
            }
        };
        let Ok(number) = i64::try_from(link.number) else {
            log::warn!("{link}: not reading GitHub's diff: GitHub's API can't name its number");
            return;
        };
        let pull = PullRef::new(owner, name, number);
        let mut source = GithubDiff {
            link: link.clone(),
            title: format!("{link}: {}", pull_request.title),
            repo_name: name.to_string(),
            head: None,
        };
        let opened = self.opened;
        let changed_files = pull_request.changed_files;
        self._github_diff = Some(cx.spawn_in(window, async move |this, cx| {
            let mut wait = GITHUB_FIRST_RETRY;
            let read = loop {
                let started = Instant::now();
                match reader.read(&pull).await {
                    Ok(read) => {
                        log::info!(
                            "{link}: GitHub listed {} files in {:?}",
                            read.files.len(),
                            started.elapsed()
                        );
                        break read;
                    }
                    Err(error) => {
                        let again = github_retry_wait(&error, wait, SystemTime::now());
                        log::warn!(
                            "{link}: can't read GitHub's diff: {error}; {}",
                            match again {
                                Some(again) => format!("trying again in {again:?}"),
                                None => "not trying again".to_string(),
                            }
                        );
                        let Some(again) = again else {
                            this.update(cx, |this, cx| this.github_diff_failed(&error, cx))
                                .ok();
                            return;
                        };
                        this.update(cx, |this, cx| this.github_diff_retried(&error, cx))
                            .ok();
                        // Dropped, and so stopped, once the local copy's diff shows.
                        cx.background_executor().timer(again).await;
                        wait = pr_diff::next_wait(wait);
                    }
                }
            };
            if read.files.is_empty() && read.cut_short.is_some() {
                this.update(cx, |this, cx| this.github_diff_not_read(cx))
                    .ok();
                return;
            }
            let cut = cut_text(read.files.len(), changed_files, read.cut_short);
            let shown = this.update(cx, |this, cx| {
                this.banners.update(cx, |banners, cx| {
                    banners.hide(BannerKind::GithubDiffFailed, cx);
                    if let Some(cut) = cut {
                        log::info!("{link}: {cut}");
                        banners.show(BannerKind::GithubDiffCut, Severity::Info, cut, cx)
                    }
                })
            });
            if shown.is_err() {
                return;
            }
            source.head = read.head;
            let tab = this.clone();
            if let Err(error) = api_diff::show(this, source, read.files, opened, cx).await
                && tab.upgrade().is_some()
            {
                log::warn!("{link}: can't show GitHub's diff: {error:#}");
            }
        }));
    }

    /// Shows GitHub's diff of `files` until the local copy's diff is ready, as
    /// [`Self::load_github_diff`] does once it has read them.
    #[cfg(test)]
    pub(crate) fn show_github_files(
        &mut self,
        source: GithubDiff,
        files: Vec<github::files::PullFile>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let opened = self.opened;
        self._github_diff = Some(cx.spawn_in(window, async move |this, cx| {
            let link = source.link.clone();
            if let Err(error) = api_diff::show(this, source, files, opened, cx).await {
                log::warn!("{link}: can't show GitHub's diff: {error:#}");
            }
        }));
    }

    /// GitHub's diff, listed at `head`, is ready to show in `editor`, which lists `entries`.
    /// Unless the local copy's diff got there first, lists them and shows it, and returns where
    /// each file goes in it.
    pub(crate) fn show_github_diff(
        &mut self,
        entries: Vec<FileEntry>,
        editor: Entity<SplittableEditor>,
        head: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Vec<usize>> {
        if self.diff.is_some() {
            return None;
        }
        // The local copy fetches the head GitHub gave before it listed the files.
        if let (Some(head), Some(api_head)) = (&head, &self.api_head)
            && head != api_head
        {
            let text = format!(
                "This pull request moved on while rozed read it. GitHub's diff shows {}, and the \
                 local copy's diff shows {} once it's ready.",
                short(head),
                short(api_head)
            );
            self.banners.update(cx, |banners, cx| {
                banners.show(BannerKind::MovedOn, Severity::Warning, text, cx)
            });
        }
        self.github_head = head;
        self.list(entries);
        self.show_diff(editor, DiffSource::GitHub, window, cx);
        // The local copy failed for good, so this is the diff the tab shows.
        if self.copy_failed.is_some() {
            self.set_status(Status::Ready, cx);
        }
        Some(self.files.review_positions())
    }

    /// GitHub's diff couldn't be read for a reason that may pass, and is read again soon.
    fn github_diff_retried(&mut self, error: &GithubError, cx: &mut Context<Self>) {
        if self.diff.is_some() {
            return;
        }
        self.banners.update(cx, |banners, cx| {
            banners.show(
                BannerKind::GithubDiffFailed,
                Severity::Info,
                format!(
                    "rozed couldn't read GitHub's diff, and tries again until the local copy's \
                     diff is ready: {error}"
                ),
                cx,
            )
        });
    }

    /// GitHub's diff can't be read, and isn't tried again.
    fn github_diff_failed(&mut self, error: &GithubError, cx: &mut Context<Self>) {
        self.github_failed = true;
        if self.diff.is_some() {
            return;
        }
        // Neither diff can show.
        if let Some(copy_failed) = self.copy_failed.clone() {
            self.banners.update(cx, |banners, cx| {
                banners.hide(BannerKind::LocalCopyFailed, cx)
            });
            self.set_status(Status::Failed(copy_failed), cx);
            return;
        }
        self.banners.update(cx, |banners, cx| {
            banners.show(
                BannerKind::GithubDiffFailed,
                Severity::Info,
                format!(
                    "rozed couldn't read GitHub's diff, so the diff shows once the local copy is \
                     ready: {error}"
                ),
                cx,
            )
        });
    }

    /// GitHub's diff wasn't read, to save the requests GitHub allows without signing in.
    fn github_diff_not_read(&mut self, cx: &mut Context<Self>) {
        self.github_failed = true;
        if self.diff.is_some() {
            return;
        }
        if let Some(copy_failed) = self.copy_failed.clone() {
            self.set_status(Status::Failed(copy_failed), cx);
        }
        self.banners.update(cx, |banners, cx| {
            banners.show(
                BannerKind::GithubDiffFailed,
                Severity::Info,
                "rozed didn't read GitHub's diff, to keep the few requests GitHub allows without \
                 signing in for opening pull requests. The diff shows once the local copy is \
                 ready.",
                cx,
            )
        });
    }

    /// Shows how far copying the pull request locally has got.
    pub(crate) fn copy_progress(&mut self, progress: &CopyProgress, cx: &mut Context<Self>) {
        self.set_status(Status::Loading(progress.text().into()), cx);
    }

    /// Says copying the pull request locally failed with `why` and will be tried again in `left`,
    /// or now when `left` is `None`. With no `problem`, the copy worked and the banner goes.
    pub(crate) fn copy_retried(
        &mut self,
        problem: Option<(&str, Option<Duration>)>,
        cx: &mut Context<Self>,
    ) {
        let Some((why, left)) = problem else {
            self.banners.update(cx, |banners, cx| {
                banners.hide(BannerKind::LocalCopyFailed, cx)
            });
            return;
        };
        let when = match left {
            // Whole seconds, rounded up, so it never says 0.
            Some(left) => format!("Trying again in {} s.", left.as_millis().div_ceil(1000)),
            None => "Trying again now.".to_string(),
        };
        let meanwhile = self.meanwhile();
        let text =
            format!("rozed couldn't copy this pull request locally: {why}. {when}{meanwhile}");
        self.banners.update(cx, |banners, cx| {
            banners.show(BannerKind::LocalCopyFailed, Severity::Warning, text, cx)
        });
        let step = match left {
            Some(_) => "Waiting to copy it locally again…",
            None => "Copying it locally…",
        };
        self.set_status(Status::Loading(step.into()), cx);
    }

    /// What the tab shows until the local copy's diff is ready, if it shows GitHub's.
    fn meanwhile(&self) -> &'static str {
        if self.diff_source == Some(DiffSource::GitHub) {
            " Until then, this is GitHub's diff: changed lines only."
        } else {
            ""
        }
    }

    /// Says rozed stopped trying to copy the pull request locally after `tries` failures for
    /// `why`, a reason it doesn't know, with a button that tries again through `retry`.
    pub(crate) fn copy_stopped(
        &mut self,
        why: &str,
        tries: usize,
        retry: oneshot::Sender<()>,
        cx: &mut Context<Self>,
    ) {
        self.retry_copy = Some(retry);
        let text = format!(
            "rozed stopped trying to copy this pull request locally after {tries} tries: {why}.{}",
            self.meanwhile()
        );
        let tab = cx.weak_entity();
        let action = BannerAction::new("Retry", move |_, cx| {
            tab.update(cx, |tab, cx| tab.retry_copy(cx)).ok();
        });
        self.banners.update(cx, |banners, cx| {
            banners.show_with(
                BannerKind::LocalCopyFailed,
                Severity::Error,
                text,
                Some(action),
                cx,
            )
        });
        let status = if self.diff.is_some() {
            Status::Ready
        } else if self._github_diff.is_some() && !self.github_failed {
            // GitHub's diff shows when it arrives, and it's all there is until a retry.
            self.copy_failed = Some(why.to_string().into());
            Status::Loading("Reading GitHub's diff…".into())
        } else {
            Status::Failed(why.to_string().into())
        };
        self.set_status(status, cx);
    }

    /// Tries copying the pull request locally again, after rozed stopped trying.
    pub(crate) fn retry_copy(&mut self, cx: &mut Context<Self>) {
        let Some(retry) = self.retry_copy.take() else {
            return;
        };
        retry.send(()).ok();
        self.copy_failed = None;
        self.banners.update(cx, |banners, cx| {
            banners.hide(BannerKind::LocalCopyFailed, cx)
        });
        self.set_status(Status::Loading("Copying it locally…".into()), cx);
    }

    /// Shows that the local copy's diff has `done` of its `total` files.
    pub(crate) fn loading_diff(&mut self, done: usize, total: usize, cx: &mut Context<Self>) {
        let what = if self.diff_source == Some(DiffSource::GitHub) {
            "the local copy's diff"
        } else {
            "the diff"
        };
        self.set_status(
            Status::Loading(format!("Loading {what}: {done} of {total} files…").into()),
            cx,
        );
    }

    /// Every file is in the local copy's diff.
    pub(crate) fn diff_loaded(&mut self, cx: &mut Context<Self>) {
        self.set_status(Status::Ready, cx);
    }

    /// Loading failed with `error`. GitHub's diff, if it shows or is on its way, stays, with a
    /// banner saying why the local copy's won't come; otherwise the tab says why it couldn't open.
    fn load_failed(&mut self, error: &anyhow::Error, cx: &mut Context<Self>) {
        let github_coming =
            self.diff.is_none() && self._github_diff.is_some() && !self.github_failed;
        if self.diff.is_none() && !github_coming {
            self.set_status(Status::Failed(format!("{error:#}").into()), cx);
            return;
        }
        if github_coming {
            self.copy_failed = Some(format!("{error:#}").into());
            self.banners.update(cx, |banners, cx| {
                banners.show(
                    BannerKind::LocalCopyFailed,
                    Severity::Error,
                    format!(
                        "rozed couldn't show the local copy's diff, so this is GitHub's diff: \
                         changed lines only. {error:#}"
                    ),
                    cx,
                )
            });
            self.set_status(Status::Loading("Reading GitHub's diff…".into()), cx);
            return;
        }
        let text = match self.diff_source {
            Some(DiffSource::GitHub) => format!(
                "rozed couldn't show the local copy's diff, so this is GitHub's diff: changed \
                 lines only. {error:#}"
            ),
            _ => format!("rozed couldn't finish loading this pull request: {error:#}"),
        };
        self.banners.update(cx, |banners, cx| {
            banners.show(BannerKind::LocalCopyFailed, Severity::Error, text, cx)
        });
        self.set_status(Status::Ready, cx);
    }

    /// Shows `editor` as the tab's diff, from `source`. The local copy's diff stops GitHub's from
    /// loading.
    pub(crate) fn show_diff(
        &mut self,
        editor: Entity<SplittableEditor>,
        source: DiffSource,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focused = self.focus_handle.contains_focused(window, cx);
        if let Some(history) = self.nav_history.clone() {
            let rhs_editor = editor.read(cx).rhs_editor().clone();
            rhs_editor.update(cx, |editor, _| editor.set_nav_history(Some(history)));
        }
        self.diff = Some(editor.clone());
        self.diff_source = Some(source);
        if source == DiffSource::LocalCopy {
            self._github_diff = None;
            self.banners.update(cx, |banners, cx| {
                banners.hide(BannerKind::GithubDiffFailed, cx);
                banners.hide(BannerKind::GithubDiffCut, cx);
            });
        }
        if focused && !self.file_list_focus.contains_focused(window, cx) {
            window.focus(&editor.focus_handle(cx), cx);
        }
        cx.notify();
    }

    /// Replaces GitHub's diff with the local copy's, `editor` of `head`, at the same file and
    /// line, and says so if GitHub's diff showed another head.
    pub(crate) fn switch_diff(
        &mut self,
        editor: Entity<SplittableEditor>,
        head: &Oid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let moved_on = self
            .github_head
            .as_deref()
            .filter(|github_head| *github_head != head.as_str())
            .map(|github_head| {
                format!(
                    "This pull request moved on while it loaded. This diff shows {}; GitHub's \
                     diff showed {}.",
                    short(head.as_str()),
                    short(github_head)
                )
            });
        self.banners.update(cx, |banners, cx| match moved_on {
            Some(text) => banners.show(BannerKind::MovedOn, Severity::Warning, text, cx),
            None => banners.hide(BannerKind::MovedOn, cx),
        });
        let (position, cursor) = match &self.diff {
            Some(diff) => (
                pr_diff::scroll_position(diff, cx),
                pr_diff::cursor_position(diff, cx),
            ),
            None => (None, None),
        };
        self.show_diff(editor.clone(), DiffSource::LocalCopy, window, cx);
        if let Some(position) = &position
            && !pr_diff::scroll_to(&editor, position, window, cx)
        {
            log::warn!(
                "{}: the local copy's diff has no {}, so it starts at the top",
                self.link(),
                position.path
            );
        }
        // The cursor goes where it was, or, when it was out of view, to the top of the view, so
        // the first key that moves it doesn't scroll to where it was.
        let cursor = cursor.or_else(|| {
            position.map(|position| pr_diff::DiffCursor {
                path: position.path,
                point: language::Point::new(position.row, 0),
            })
        });
        if let Some(cursor) = cursor {
            pr_diff::put_cursor(&editor, &cursor, window, cx);
        }
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
                if self.header.title.as_ref() != Some(&summary.title) {
                    self.header.title = Some(summary.title.clone());
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

    /// Who the header says the tab acts as.
    pub fn acting_text(&self) -> String {
        match (&self.acting, &self.guessed_among) {
            (Some(Acting::Account(login)), Some(count)) => {
                format!("as {login}, the first of the {count} accounts that can see it")
            }
            (Some(Acting::Account(login)), None) => format!("as {login}"),
            (Some(Acting::Anonymous), _) => "not signed in".to_string(),
            (None, _) => String::new(),
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
        let step = match &self.status {
            Status::Ready => None,
            Status::Loading(step) => Some((IconName::ArrowCircle, Color::Muted, step.clone())),
            Status::Failed(error) => Some((IconName::Warning, Color::Error, error.clone())),
        };
        let github = self.diff_source == Some(DiffSource::GitHub);
        if step.is_none() && !github {
            return None;
        }
        Some(
            h_flex()
                .px_3()
                .py_1()
                .gap_2()
                .justify_between()
                .child(h_flex().gap_2().children(step.map(|(icon, color, text)| {
                    h_flex()
                        .gap_2()
                        .child(Icon::new(icon).size(IconSize::Small).color(color))
                        .child(Label::new(text).size(LabelSize::Small).color(color))
                })))
                .when(github, |this| {
                    this.child(
                        Label::new("GitHub's diff: changed lines only")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                })
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

/// How long to wait before reading GitHub's diff again the first time. Each failure doubles it, up
/// to 5 minutes, as for the local copy.
const GITHUB_FIRST_RETRY: Duration = Duration::from_secs(5);

/// How long to wait before reading GitHub's diff again after `error`, having waited `wait` last
/// time, or `None` when reading it again won't help: the network, a server error, GitHub's pages
/// not fitting together and a rate limit pass; GitHub refusing the pull request doesn't.
fn github_retry_wait(error: &GithubError, wait: Duration, now: SystemTime) -> Option<Duration> {
    match error {
        GithubError::Network(_) | GithubError::Inconsistent(_) => Some(wait),
        GithubError::Api { status, .. } if *status >= 500 => Some(wait),
        GithubError::RateLimited {
            retry_after, reset, ..
        } => {
            let until_reset = reset.map(|reset| {
                (SystemTime::UNIX_EPOCH + Duration::from_secs(reset))
                    .duration_since(now)
                    .unwrap_or_default()
            });
            Some(retry_after.or(until_reset).unwrap_or(wait).max(wait))
        }
        _ => None,
    }
}

/// What the banner says when GitHub's diff lists only `listed` of the pull request's `changed`
/// files, or `None` when it lists them all.
fn cut_text(listed: usize, changed: Option<u32>, cut_short: Option<CutShort>) -> Option<String> {
    let changed = changed.map(|changed| changed as usize);
    if cut_short.is_none() && changed.is_none_or(|changed| changed <= listed) {
        return None;
    }
    let of = match changed {
        Some(changed) => format!(
            "{} of this pull request's {} files",
            thousands(listed as u64),
            thousands(changed as u64)
        ),
        None => format!("{} of this pull request's files", thousands(listed as u64)),
    };
    let text = match cut_short {
        Some(CutShort::Pages) => format!(
            "Without signing in, GitHub's diff shows the first {of}, to save GitHub's 60 requests \
             an hour."
        ),
        Some(CutShort::RateLimit) => format!(
            "Few of the requests GitHub allows without signing in are left, so GitHub's diff shows \
             the first {of}."
        ),
        None => format!("GitHub lists only {of}, so GitHub's diff shows those."),
    };
    Some(format!(
        "{text} The rest show once the local copy is ready."
    ))
}

/// A commit ID's first seven characters, as GitHub shows it.
fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
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
        if matches!(twin.status, Status::Failed(_)) {
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

/// Loads `link`'s pull request into `this`: picks the account, reads the pull request, copies it
/// locally and shows its diff.
async fn load(
    this: WeakEntity<PrTab>,
    link: PrLink,
    trust: Trust,
    opened: Instant,
    cx: &mut AsyncWindowContext,
) -> anyhow::Result<()> {
    let layer = cx.update(|_, cx| github_layer::ready(cx))?.await;
    let pull = PullRef::new(&link.owner, &link.repo, i64::try_from(link.number)?);
    // What rozed saved shows at once, and offline.
    if let Some(layer) = &layer {
        let saved = cx.update(|_, cx| layer.read(cx).saved_details(&pull, cx))?;
        if let Some(saved) = saved {
            this.update(cx, |this, cx| this.show_saved(&saved, cx))?;
        }
    }

    // The account that can see the repository, or none for a public one.
    let found = match &layer {
        Some(layer) => Some(
            layer
                .update(cx, |layer, cx| {
                    layer.account_for_repo(&link.owner, &link.repo, cx)
                })
                .await?,
        ),
        None => None,
    };
    let (acting, guessed_among) = match account::decide(&link.owner, &link.repo, found.as_ref()) {
        AccountDecision::Open(acting) => (acting, None),
        AccountDecision::Ask(logins) => {
            // Until rozed asks which account to use, the first one signed in, and the header
            // says so.
            log::info!(
                "{link}: several accounts can see it ({}); using {}",
                logins.join(", "),
                logins[0]
            );
            (Acting::Account(logins[0].clone()), Some(logins.len()))
        }
        AccountDecision::Refuse(refusal) => return Err(refusal.into()),
    };
    log::info!(
        "{link}: opening {}",
        match &acting {
            Acting::Account(login) => format!("as {login}"),
            Acting::Anonymous => "without signing in".to_string(),
        }
    );
    this.update(cx, |this, cx| {
        this.guessed_among = guessed_among;
        this.set_acting(acting.clone(), cx)
    })?;

    // Signed in, the account's client reads the pull request and GitHub's diff in the account's
    // queue, and its token copies it.
    let (pull_request, token, reader, poller) = match (&acting, &layer) {
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
            let token = client
                .token()
                .map(|token| repo_cache::Token::new(token.expose()));
            let reader = FilesReader::Account {
                client: client.clone(),
                queue,
            };
            (pull_request, token, reader, Some(poller))
        }
        _ => {
            // The app's one client without an account keeps what GitHub says about the 60
            // requests an hour it allows without one, for every tab.
            let client = cx.update(|_, cx| github_api::anonymous_client(cx))?;
            let can_sign_in = match layer {
                Some(_) => CanSignIn::Yes,
                None => CanSignIn::No,
            };
            (
                github_api::fetch_pull_request(&client, &link, can_sign_in).await?,
                None,
                FilesReader::Anonymous(client),
                None,
            )
        }
    };
    this.update(cx, |this, cx| this.set_pull_request(&pull_request, cx))?;
    // GitHub's diff shows while the local copy loads.
    this.update_in(cx, |this, window, cx| {
        this.load_github_diff(reader, &pull_request, window, cx)
    })?;

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
        let github = self.diff_source == Some(DiffSource::GitHub);
        v_flex()
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            // GitHub's diff has only the changed lines, so its excerpts can't grow.
            .when(github, |this| {
                this.capture_action(|_: &editor::actions::ExpandExcerpts, _, cx| {
                    cx.stop_propagation()
                })
                .capture_action(|_: &editor::actions::ExpandExcerptsUp, _, cx| {
                    cx.stop_propagation()
                })
                .capture_action(
                    |_: &editor::actions::ExpandExcerptsDown, _, cx| cx.stop_propagation(),
                )
            })
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
    use project::{FakeFs, Project};
    use repo_cache::{Change, FileMode, FileVersion, LineCounts};
    use workspace::MultiWorkspace;

    use std::sync::Arc;

    use github::files::{FileStatus, PullFile};

    use crate::github_api::{Base, BaseRepo, Head, User};
    use crate::pr_diff::tests::fixtures::Entry;
    use crate::pr_diff::{DiffCursor, DiffPosition};
    use futures::FutureExt as _;

    use crate::test_support::{Answer, FakeGithub, FakePull, GitServer, anonymous, sign_in};

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

    /// Sets the app up as rozed does: the command palette, the editor, the tab's keys over Zed's
    /// keymap, and rozed's changes to every workspace.
    fn init(cx: &mut TestAppContext) {
        crate::trust::tests::init_test(cx);
        cx.update(|cx| {
            command_palette_hooks::init(cx);
            editor::init(cx);
            crate::init_workspaces(cx);
            crate::keymap::load(cx);
        });
    }

    /// A window like rozed's: a workspace in a `MultiWorkspace`, which renders the workspace's
    /// actions.
    async fn workspace(cx: &mut TestAppContext) -> (Entity<Workspace>, &mut VisualTestContext) {
        init(cx);
        let fs = FakeFs::new(cx.executor());
        // A repository elsewhere on disk, which nothing in rozed should open.
        fs.insert_tree(
            "/elsewhere",
            serde_json::json!({
                "Cargo.toml": "[package]\nname = \"elsewhere\"\n",
                "src": { "main.rs": "fn main() {}\n" },
            }),
        )
        .await;
        let project = Project::test(fs, [], cx).await;
        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project, window, cx));
        let workspace =
            multi_workspace.read_with(cx, |multi_workspace, _| multi_workspace.workspace().clone());
        (workspace, cx)
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
            changed_files: None,
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

    /// A public pull request of `count` files on the fake GitHub, which its git server doesn't
    /// have, so its local copy fails and GitHub's diff stays.
    fn uncopyable_pull(github: &FakeGithub, repo: &str, number: u64, count: usize) {
        let head = "d".repeat(40);
        let files = (0..count)
            .map(|index| {
                let path = format!("src/f{index:04}.rs");
                serde_json::json!({
                    "filename": path,
                    "status": "modified",
                    "additions": 1,
                    "deletions": 1,
                    "changes": 2,
                    "patch": "@@ -1 +1 @@\n-a\n+b",
                    "blob_url": format!("https://github.com/{repo}/blob/{head}/{path}"),
                    "contents_url": format!(
                        "https://api.github.com/repos/{repo}/contents/{path}?ref={head}"
                    ),
                })
            })
            .collect();
        github.add_pull(FakePull {
            repo: repo.into(),
            old_names: Vec::new(),
            number,
            title: format!("Pull request {number}"),
            head,
            base_branch: "main".into(),
            base: "b".repeat(40),
            files,
            changed_files: None,
        });
        github.make_public(repo);
    }

    #[gpui::test]
    async fn githubs_diff_is_read_again_after_a_failure_that_may_pass(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let _git = GitServer::start(cx);
        uncopyable_pull(&github, "acme/app", 1, 2);
        anonymous(&github, cx);
        let files = "/repos/acme/app/pulls/1/files?per_page=100";
        github.answer("", files, Answer::new(502, r#"{"message":"Server Error"}"#));

        let tab = open_loaded(&workspace, "github.com/acme/app/pull/1", cx);

        // Neither diff shows yet, and GitHub's is tried again.
        assert_eq!(tab.read_with(cx, |tab, _| tab.diff_source()), None);
        assert!(matches!(status(&tab, cx), Status::Loading(_)));
        let shown = banners(&tab, cx);
        assert!(
            shown.iter().any(|banner| banner.starts_with(
                "rozed couldn't read GitHub's diff, and tries again until the local copy's diff \
                 is ready: GitHub answered 502"
            )),
            "{shown:#?}"
        );
        github.forget_answer("", files);
        cx.executor().advance_clock(Duration::from_secs(5));
        cx.run_until_parked();

        tab.read_with(cx, |tab, _| {
            assert_eq!(tab.diff_source(), Some(DiffSource::GitHub));
            assert_eq!(tab.status(), &Status::Ready);
        });
        let shown = banners(&tab, cx);
        assert!(
            !shown
                .iter()
                .any(|banner| banner.contains("GitHub's diff, and tries again")),
            "{shown:#?}"
        );
    }

    #[test]
    fn githubs_diff_is_read_again_only_after_a_failure_that_may_pass() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let wait = Duration::from_secs(10);
        let api = |status| GithubError::Api {
            status,
            message: String::new(),
            documentation_url: None,
            errors: Vec::new(),
        };
        let network = GithubError::Network(anyhow::anyhow!("offline"));
        assert_eq!(github_retry_wait(&network, wait, now), Some(wait));
        assert_eq!(github_retry_wait(&api(502), wait, now), Some(wait));
        assert_eq!(github_retry_wait(&api(422), wait, now), None);
        let not_found = GithubError::NotFound {
            message: "Not Found".into(),
            documentation_url: None,
        };
        assert_eq!(github_retry_wait(&not_found, wait, now), None);
        // A used-up rate limit waits for its reset, or for as long as GitHub asked.
        let limited = |reset, retry_after| GithubError::RateLimited {
            message: String::new(),
            secondary: false,
            reset,
            retry_after,
            resource: Some("core".into()),
        };
        assert_eq!(
            github_retry_wait(&limited(Some(1_600), None), wait, now),
            Some(Duration::from_secs(600))
        );
        assert_eq!(
            github_retry_wait(&limited(None, Some(Duration::from_secs(60))), wait, now),
            Some(Duration::from_secs(60))
        );
        assert_eq!(
            github_retry_wait(&limited(None, None), wait, now),
            Some(wait)
        );
    }

    #[gpui::test]
    async fn when_neither_diff_can_show_the_tab_says_why_the_copy_failed(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let _git = GitServer::start(cx);
        uncopyable_pull(&github, "acme/app", 1, 2);
        anonymous(&github, cx);
        let files = "/repos/acme/app/pulls/1/files?per_page=100";
        github.answer("", files, Answer::new(404, r#"{"message":"Not Found"}"#));

        let tab = open_loaded(&workspace, "github.com/acme/app/pull/1", cx);

        let Status::Failed(message) = status(&tab, cx) else {
            panic!("the tab opened: {:?}", status(&tab, cx));
        };
        assert!(
            message.contains("the repository doesn't exist"),
            "{message}"
        );
    }

    #[gpui::test]
    async fn without_signing_in_githubs_diff_reads_at_most_three_pages(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let _git = GitServer::start(cx);
        uncopyable_pull(&github, "acme/big", 1, 450);
        anonymous(&github, cx);

        let tab = open_loaded(&workspace, "github.com/acme/big/pull/1", cx);

        let requests = github.requests();
        let pages: Vec<_> = requests
            .iter()
            .filter(|request| request.contains("/files"))
            .collect();
        assert_eq!(pages.len(), 3, "{requests:#?}");
        assert_eq!(tab.read_with(cx, |tab, _| tab.files().len()), 300);
        let banners = banners(&tab, cx);
        assert!(
            banners.contains(
                &"Without signing in, GitHub's diff shows the first 300 of this pull request's \
                  450 files, to save GitHub's 60 requests an hour. The rest show once the local \
                  copy is ready."
                    .to_string()
            ),
            "{banners:#?}"
        );
    }

    #[gpui::test]
    async fn without_signing_in_githubs_diff_isnt_read_while_few_requests_are_left(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let _git = GitServer::start(cx);
        uncopyable_pull(&github, "acme/one", 1, 2);
        uncopyable_pull(&github, "acme/two", 2, 2);
        anonymous(&github, cx);
        github.limit_anonymous(13);

        // The first reads its one page, and the app's one client without an account keeps what
        // GitHub said about the limit.
        let one = open_loaded(&workspace, "github.com/acme/one/pull/1", cx);
        assert_eq!(
            one.read_with(cx, |tab, _| tab.diff_source()),
            Some(DiffSource::GitHub)
        );
        let left = cx.update(|_, cx| {
            let client = github_api::anonymous_client(cx);
            client.rate_limits().core().map(|core| core.remaining)
        });
        assert_eq!(left, Some(11));

        // With 10 left after the second's own request, its file list isn't read.
        github.requests();
        let two = open_loaded(&workspace, "github.com/acme/two/pull/2", cx);
        assert_eq!(github.requests(), ["GET /repos/acme/two/pulls/2 as nobody"]);
        assert_eq!(two.read_with(cx, |tab, _| tab.diff_source()), None);
        let banners = banners(&two, cx);
        assert!(
            banners.iter().any(|banner| banner.starts_with(
                "rozed didn't read GitHub's diff, to keep the few requests GitHub allows"
            )),
            "{banners:#?}"
        );
    }

    #[gpui::test]
    async fn githubs_diff_says_when_github_lists_only_some_files(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let _git = GitServer::start(cx);
        uncopyable_pull(&github, "acme/huge", 1, 3);
        github.let_see("gho_BOT", "acme/huge");
        sign_in(&github, &[("rozed-bot", "gho_BOT")], cx).await;
        // GitHub lists no more than 3,000 of a pull request's files; this one has 3,100.
        github.set_changed_files("acme/huge", 1, 3_100);

        let tab = open_loaded(&workspace, "github.com/acme/huge/pull/1", cx);

        assert_eq!(
            tab.read_with(cx, |tab, _| tab.diff_source()),
            Some(DiffSource::GitHub)
        );
        let banners = banners(&tab, cx);
        assert!(
            banners.contains(
                &"GitHub lists only 3 of this pull request's 3,100 files, so GitHub's diff shows \
                  those. The rest show once the local copy is ready."
                    .to_string()
            ),
            "{banners:#?}"
        );
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
        // GitHub's diff, if it showed, and the local copy's are of the same head.
        assert_eq!(banners(&tab, cx), Vec::<String>::new());
        assert_eq!(
            github.requests(),
            [
                "GET /repos/sharkdp/fd/pulls/2212 as nobody",
                "GET /repos/sharkdp/fd/pulls/2212/files?per_page=100 as nobody"
            ]
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

    #[gpui::test]
    async fn with_several_accounts_and_none_chosen_the_header_says_it_took_the_first(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        let pull = git.pull_request("acme/app", 1, &[], &[("a.rs", "a\n")]);
        github.add_pull(pull);
        github.let_see("gho_HOME", "acme/app");
        github.let_see("gho_WORK", "acme/app");
        sign_in(&github, &[("home", "gho_HOME"), ("work", "gho_WORK")], cx).await;

        let tab = open_loaded(&workspace, "github.com/acme/app/pull/1", cx);

        assert_eq!(status(&tab, cx), Status::Ready);
        assert_eq!(
            tab.read_with(cx, |tab, _| tab.acting_text()),
            "as home, the first of the 2 accounts that can see it"
        );
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
        // Offline, GitHub can't confirm the pull request, so nothing records it as opened.
        assert_eq!(cache.recent(10).unwrap(), Vec::new());

        // Back online, opening it again loads it, and the banner goes.
        github.set_offline(false);
        open_loaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);
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

    /// The details an account's poller hands the tab, as GitHub's GraphQL API answers.
    fn details(title: &str, viewed: &str) -> PullRequestData {
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
            "baseRefName": "stack-a",
            "baseRefOid": "b046244d9569964088e952243e5e557312322488",
            "headRefName": "stack-b",
            "headRefOid": "d712aa7652cc723fee5fd9b3eba867b0480d40da",
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
        tab.update(cx, |tab, cx| tab.copied(&files, &head, cx));
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

    /// `count` lines, "`name` 1" to "`name` `count`", with line `changed` as "`name` `changed`
    /// changed".
    fn numbered(name: &str, count: u32, changed: Option<u32>) -> String {
        (1..=count)
            .map(|line| match changed {
                Some(changed) if changed == line => format!("{name} {line} changed\n"),
                _ => format!("{name} {line}\n"),
            })
            .collect()
    }

    /// GitHub's entry for `name`, whose line `line` changed as [`numbered`] changes it, with three
    /// lines around it, as GitHub's patches have.
    fn github_file(name: &str, line: u32) -> PullFile {
        let start = line - 3;
        let (filename, name) = (name, name.trim_end_matches(".txt"));
        let mut patch = format!("@@ -{start},7 +{start},7 @@");
        for unchanged in start..line {
            patch.push_str(&format!("\n {name} {unchanged}"));
        }
        patch.push_str(&format!("\n-{name} {line}\n+{name} {line} changed"));
        for unchanged in line + 1..=line + 3 {
            patch.push_str(&format!("\n {name} {unchanged}"));
        }
        PullFile {
            filename: filename.to_string(),
            status: FileStatus::Modified,
            additions: 1,
            deletions: 1,
            changes: 2,
            patch: Some(patch),
            previous_filename: None,
            blob_url: None,
            contents_url: None,
        }
    }

    /// [`github_file`], listed at `head`.
    fn github_file_at(name: &str, line: u32, head: &str) -> PullFile {
        PullFile {
            blob_url: Some(format!("https://github.com/owner/repo/blob/{head}/{name}")),
            ..github_file(name, line)
        }
    }

    const PULL: &str = "https://github.com/owner/repo/pull/1";

    fn show_github_files(tab: &Entity<PrTab>, files: Vec<PullFile>, cx: &mut VisualTestContext) {
        tab.update_in(cx, |tab, window, cx| {
            let source = GithubDiff {
                link: link(PULL),
                title: format!("{PULL}: Change a and b"),
                repo_name: "repo".into(),
                head: github::files::files_head(&files).unwrap(),
            };
            tab.show_github_files(source, files, window, cx)
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn githubs_diff_read_after_a_push_says_which_head_it_shows(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let _git = GitServer::start(cx);
        // The files are listed at dddddd…, but GitHub said the head was cccccc… a moment before.
        uncopyable_pull(&github, "acme/app", 1, 2);
        github.set_head("acme/app", 1, &"c".repeat(40));
        anonymous(&github, cx);

        let tab = open_loaded(&workspace, "github.com/acme/app/pull/1", cx);

        assert_eq!(
            tab.read_with(cx, |tab, _| tab.diff_source()),
            Some(DiffSource::GitHub)
        );
        let banners = banners(&tab, cx);
        assert!(
            banners.contains(
                &"This pull request moved on while rozed read it. GitHub's diff shows ddddddd, \
                  and the local copy's diff shows ccccccc once it's ready."
                    .to_string()
            ),
            "{banners:#?}"
        );
    }

    #[gpui::test]
    async fn the_switch_says_when_the_local_copy_shows_another_head(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let tab = open_unloaded(&workspace, PULL, cx);
        let github_head = "f".repeat(40);
        show_github_files(&tab, vec![github_file_at("a.txt", 50, &github_head)], cx);
        assert_eq!(banners(&tab, cx), Vec::<String>::new());

        let _local_copy = show_local_copy(&tab, cx);

        let local_head = tab.read_with(cx, |tab, _| {
            assert_eq!(tab.diff_source(), Some(DiffSource::LocalCopy));
            tab.revisions().unwrap().latest().unwrap().head.clone()
        });
        assert_eq!(
            banners(&tab, cx),
            [format!(
                "This pull request moved on while it loaded. This diff shows {}; GitHub's diff \
                 showed fffffff.",
                &local_head[..7]
            )]
        );
    }

    /// Shows the local copy's diff of a pull request that changes line 50 of a.txt and line 60 of
    /// b.txt, each 100 lines long. The local copy lasts as long as what's returned.
    fn show_local_copy(
        tab: &Entity<PrTab>,
        cx: &mut VisualTestContext,
    ) -> (pr_diff::tests::fixtures::Fixture, tempfile::TempDir) {
        let (a, b) = (numbered("a", 100, None), numbered("b", 100, None));
        let (new_a, new_b) = (numbered("a", 100, Some(50)), numbered("b", 100, Some(60)));
        let base = [
            Entry::File("a.txt", a.as_bytes()),
            Entry::File("b.txt", b.as_bytes()),
        ];
        let head = [
            Entry::File("a.txt", new_a.as_bytes()),
            Entry::File("b.txt", new_b.as_bytes()),
        ];
        let pr_diff::tests::Copied {
            _fixture: fixture,
            _cache: cache,
            local,
            ..
        } = pr_diff::tests::copy(&base, &head, false);
        let local = Arc::new(local);
        tab.update_in(cx, |tab, window, cx| {
            let (workspace, link) = (tab.workspace().clone(), tab.link().clone());
            cx.spawn_in(window, async move |tab, cx| {
                let title = format!("{link}: Change a and b");
                pr_diff::show_local_copy(
                    tab,
                    workspace,
                    link,
                    title,
                    local,
                    Trust::Untrusted,
                    Instant::now(),
                    cx,
                )
                .await
                .unwrap()
            })
            .detach();
        });
        cx.run_until_parked();
        (fixture, cache)
    }

    fn shown_text(diff: &Entity<SplittableEditor>, cx: &mut VisualTestContext) -> String {
        diff.read_with(cx, |diff, cx| diff.rhs_editor().read(cx).text(cx))
    }

    fn scrolled_to(
        diff: &Entity<SplittableEditor>,
        cx: &mut VisualTestContext,
    ) -> Option<DiffPosition> {
        cx.update(|_, cx| pr_diff::scroll_position(diff, cx))
    }

    #[gpui::test]
    async fn githubs_diff_shows_first_then_the_local_copys_at_the_same_line(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx).await;
        let tab = open_unloaded(&workspace, PULL, cx);

        show_github_files(
            &tab,
            vec![github_file("a.txt", 50), github_file("b.txt", 60)],
            cx,
        );

        let github = tab.read_with(cx, |tab, _| {
            assert_eq!(tab.diff_source(), Some(DiffSource::GitHub));
            tab.diff().unwrap().clone()
        });
        let text = shown_text(&github, cx);
        // Only the hunks show: line 47 to line 53 of a.txt, and 57 to 63 of b.txt.
        assert!(
            text.contains("a 50 changed") && text.contains("b 60 changed"),
            "{text}"
        );
        assert!(text.contains("a 47\n") && text.contains("b 63"), "{text}");
        assert!(!text.contains("a 46\n") && !text.contains("b 64"), "{text}");
        // The rows between hunks never show.
        assert!(!text.contains('\u{E000}'), "{text:?}");
        let paths: Vec<_> = tab.read_with(cx, |tab, _| {
            tab.files()
                .rows()
                .map(|entry| entry.path().to_string())
                .collect()
        });
        assert_eq!(paths, ["a.txt", "b.txt"]);

        // Reading b.txt's change, line 60, at the top of the view.
        let position = DiffPosition {
            path: "b.txt".into(),
            row: 59,
            below_top: 0.,
        };
        cx.update(|window, cx| assert!(pr_diff::scroll_to(&github, &position, window, cx)));
        assert_eq!(scrolled_to(&github, cx), Some(position.clone()));

        let _local_copy = show_local_copy(&tab, cx);

        let local = tab.read_with(cx, |tab, _| {
            assert_eq!(tab.diff_source(), Some(DiffSource::LocalCopy));
            assert_eq!(tab.status(), &Status::Ready);
            tab.diff().unwrap().clone()
        });
        assert_ne!(local, github);
        let scrolled = scrolled_to(&local, cx).unwrap();
        assert_eq!((scrolled.path.as_str(), scrolled.row), ("b.txt", 59));
        assert_eq!(scrolled.below_top, 0.);
    }

    /// Shows every diff made from now on in one column, deleted lines above added ones.
    fn unified(cx: &mut VisualTestContext) {
        cx.update(|_, cx| {
            cx.update_global::<settings::SettingsStore, _>(|store, cx| {
                store.update_user_settings(cx, |settings| {
                    settings.editor.diff_view_style = Some(settings::DiffViewStyle::Unified)
                })
            })
        });
    }

    /// Scrolls `diff` so the display row `above` rows above `position`'s line is at the top.
    fn scroll_above(
        diff: &Entity<SplittableEditor>,
        position: &DiffPosition,
        above: f64,
        cx: &mut VisualTestContext,
    ) {
        cx.update(|window, cx| assert!(pr_diff::scroll_to(diff, position, window, cx)));
        let editor = diff.read_with(cx, |diff, _| diff.rhs_editor().clone());
        editor.update_in(cx, |editor, window, cx| {
            let top = editor.scroll_position(cx).y;
            editor.set_scroll_position(gpui::point(0., top - above), window, cx);
        });
    }

    #[gpui::test]
    async fn the_switch_keeps_a_deleted_line_at_the_top_in_one_column(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        unified(cx);
        let tab = open_unloaded(&workspace, PULL, cx);
        show_github_files(
            &tab,
            vec![github_file("a.txt", 50), github_file("b.txt", 60)],
            cx,
        );
        let github = tab.read_with(cx, |tab, _| tab.diff().unwrap().clone());
        assert!(!github.read_with(cx, |diff, _| diff.is_split()));

        // b.txt's deleted line 60, which only the base has, is at the top, above the line that
        // replaced it.
        let changed = DiffPosition {
            path: "b.txt".into(),
            row: 59,
            below_top: 0.,
        };
        scroll_above(&github, &changed, 1., cx);
        let at_top = DiffPosition {
            below_top: 1.,
            ..changed
        };
        assert_eq!(scrolled_to(&github, cx), Some(at_top.clone()));

        let _local_copy = show_local_copy(&tab, cx);

        let local = tab.read_with(cx, |tab, _| tab.diff().unwrap().clone());
        assert!(!local.read_with(cx, |diff, _| diff.is_split()));
        assert_eq!(scrolled_to(&local, cx), Some(at_top));
    }

    #[gpui::test]
    async fn the_switch_keeps_the_cursor_where_it_was(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let tab = open_unloaded(&workspace, PULL, cx);
        show_github_files(
            &tab,
            vec![github_file("a.txt", 50), github_file("b.txt", 60)],
            cx,
        );
        let github = tab.read_with(cx, |tab, _| tab.diff().unwrap().clone());
        // The cursor is on b.txt's line 61, two below the top of the view.
        let top = DiffPosition {
            path: "b.txt".into(),
            row: 58,
            below_top: 0.,
        };
        cx.update(|window, cx| assert!(pr_diff::scroll_to(&github, &top, window, cx)));
        let cursor = DiffCursor {
            path: "b.txt".into(),
            point: language::Point::new(60, 3),
        };
        cx.update(|window, cx| assert!(pr_diff::put_cursor(&github, &cursor, window, cx)));
        assert_eq!(
            cx.update(|_, cx| pr_diff::cursor_position(&github, cx)),
            Some(cursor.clone())
        );

        let _local_copy = show_local_copy(&tab, cx);

        let local = tab.read_with(cx, |tab, _| tab.diff().unwrap().clone());
        assert_eq!(
            cx.update(|_, cx| pr_diff::cursor_position(&local, cx)),
            Some(cursor)
        );
        // Moving the cursor moves it from there, and the view stays put.
        cx.simulate_keystrokes("down");
        let moved = cx
            .update(|_, cx| pr_diff::cursor_position(&local, cx))
            .unwrap();
        assert_eq!((moved.path.as_str(), moved.point.row), ("b.txt", 61));
        let scrolled = scrolled_to(&local, cx).unwrap();
        assert_eq!((scrolled.path.as_str(), scrolled.row), ("b.txt", 58));
    }

    /// A copy whose fetch failed with `failure`, as git reports it.
    fn fetch_failed(failure: repo_cache::FetchFailure) -> anyhow::Error {
        anyhow::Error::new(repo_cache::Error::Fetch {
            what: "pull request #1".into(),
            failure,
            message: "fatal: unable to access 'https://github.com/owner/repo.git/'".into(),
        })
    }

    /// Copies `tab`'s pull request locally as the tab does, trying again after failures, with
    /// each try answered by the next of `answers`: a failure, or `None` for a copy that works.
    /// Returns the task, which ends when the copy works or fails for good, and the tries so far.
    fn copy_with(
        tab: &Entity<PrTab>,
        answers: Vec<Option<repo_cache::FetchFailure>>,
        cx: &mut VisualTestContext,
    ) -> (
        Task<anyhow::Result<()>>,
        std::rc::Rc<std::cell::Cell<usize>>,
        pr_diff::tests::Copied,
    ) {
        let copied = pr_diff::tests::copy(&[Entry::File("a.txt", b"a\n")], &[], false);
        let local = std::cell::Cell::new(Some(copied.local.clone()));
        let tries = std::rc::Rc::new(std::cell::Cell::new(0));
        let mut answers = answers.into_iter();
        let copy_once = {
            let tries = tries.clone();
            move |_progress, _cx: &mut AsyncWindowContext| {
                tries.set(tries.get() + 1);
                Task::ready(match answers.next().flatten() {
                    Some(failure) => Err(fetch_failed(failure)),
                    None => Ok(local.take().expect("a copy that works ends the tries")),
                })
            }
        };
        // Nothing has focus, so no cursor blinks while the clock runs on.
        cx.update(|window, cx| window.blur(cx));
        let task = tab.update_in(cx, |tab, window, cx| {
            let link = tab.link().clone();
            cx.spawn_in(window, async move |tab, cx| {
                pr_diff::copy_with_retries(&tab, &link, copy_once, cx)
                    .await
                    .map(|_| ())
            })
        });
        cx.run_until_parked();
        (task, tries, copied)
    }

    fn wait(seconds: u64, cx: &mut VisualTestContext) {
        cx.executor().advance_clock(Duration::from_secs(seconds));
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn a_copy_that_fails_on_the_network_is_tried_again_ever_later(cx: &mut TestAppContext) {
        use repo_cache::FetchFailure::{Network, RateLimited};
        let (workspace, cx) = workspace(cx).await;
        let tab = open_unloaded(&workspace, PULL, cx);
        show_github_files(&tab, vec![github_file("a.txt", 50)], cx);
        // As many failures as stop a copy that fails for an unknown reason, then a copy that
        // works.
        let mut answers = vec![Some(Network), Some(RateLimited)];
        answers.extend([Some(Network); 3]);
        answers.push(None);

        let (task, tries, _copied) = copy_with(&tab, answers, cx);

        let why = "can't fetch pull request #1: the network failed (fatal: unable to access \
                   'https://github.com/owner/repo.git/')";
        assert_eq!(
            banners(&tab, cx),
            [format!(
                "rozed couldn't copy this pull request locally: {why}. Trying again in 5 s. \
                 Until then, this is GitHub's diff: changed lines only."
            )]
        );
        // It counts down a second at a time.
        wait(1, cx);
        assert!(banners(&tab, cx)[0].contains("Trying again in 4 s."));
        wait(4, cx);
        assert_eq!(tries.get(), 2);
        // Each wait doubles ([`pr_diff::next_wait`] caps it at 5 minutes).
        for (try_, seconds) in [10, 20, 40, 80].into_iter().enumerate() {
            let banner = &banners(&tab, cx)[0];
            assert!(
                banner.contains(&format!("Trying again in {seconds} s.")),
                "{banner}"
            );
            wait(seconds, cx);
            assert_eq!(tries.get(), try_ + 3);
        }

        // The sixth try works, and the banner goes.
        assert!(banners(&tab, cx).is_empty());
        assert!(task.now_or_never().unwrap().is_ok());
    }

    #[gpui::test]
    async fn a_copy_that_keeps_failing_for_an_unknown_reason_stops_with_a_retry_button(
        cx: &mut TestAppContext,
    ) {
        use repo_cache::FetchFailure::Other;
        let (workspace, cx) = workspace(cx).await;
        let tab = open_unloaded(&workspace, PULL, cx);
        show_github_files(&tab, vec![github_file("a.txt", 50)], cx);

        let (task, tries, _copied) = copy_with(&tab, vec![Some(Other); 5], cx);
        for seconds in [5, 10, 20, 40] {
            wait(seconds, cx);
        }

        assert_eq!(tries.get(), 5);
        let shown = tab.read_with(cx, |tab, cx| tab.banners().read(cx).messages().to_vec());
        assert_eq!(
            shown[0].text.as_ref(),
            "rozed stopped trying to copy this pull request locally after 5 tries: can't fetch \
             pull request #1: fatal: unable to access 'https://github.com/owner/repo.git/'. \
             Until then, this is GitHub's diff: changed lines only."
        );
        assert_eq!(
            tab.read_with(cx, |tab, _| tab.status().clone()),
            Status::Ready
        );
        // It waits for the button, however long.
        wait(60, cx);
        assert_eq!(tries.get(), 5);

        let retry = shown[0].action.clone().unwrap();
        assert_eq!(retry.label.as_ref(), "Retry");
        cx.update(|window, cx| (retry.on_click)(window, cx));
        cx.run_until_parked();

        assert_eq!(tries.get(), 6);
        assert!(banners(&tab, cx).is_empty());
        assert!(task.now_or_never().unwrap().is_ok());
    }

    #[gpui::test]
    async fn a_copy_that_fails_for_good_isnt_tried_again(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let tab = open_unloaded(&workspace, PULL, cx);

        let (task, tries, _copied) =
            copy_with(&tab, vec![Some(repo_cache::FetchFailure::Auth)], cx);

        assert_eq!(tries.get(), 1);
        let error = task.now_or_never().unwrap().unwrap_err();
        assert!(
            format!("{error:#}").contains("sign in with one"),
            "{error:#}"
        );
        assert!(banners(&tab, cx).is_empty());
    }

    /// Whether each side of `diff` hides the buttons that expand excerpts: the right side, and
    /// the left one if the diff is split.
    fn expand_buttons_hidden(
        diff: &Entity<SplittableEditor>,
        cx: &mut VisualTestContext,
    ) -> (bool, Option<bool>) {
        diff.read_with(cx, |diff, cx| {
            let hidden =
                |editor: &Entity<Editor>| editor.read(cx).expand_excerpt_buttons_disabled();
            (hidden(diff.rhs_editor()), diff.lhs_editor().map(hidden))
        })
    }

    #[gpui::test]
    async fn githubs_diff_cant_be_expanded_even_once_it_splits_again(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let tab = open_unloaded(&workspace, PULL, cx);
        show_github_files(&tab, vec![github_file("a.txt", 50)], cx);
        let diff = tab.read_with(cx, |tab, _| tab.diff().unwrap().clone());
        let before = shown_text(&diff, cx);
        assert_eq!(expand_buttons_hidden(&diff, cx), (true, Some(true)));

        // The left side is made anew when the diff splits again, as when it widens past
        // `minimum_split_diff_width` or the diff view style changes.
        for _ in 0..2 {
            diff.update_in(cx, |diff, window, cx| {
                diff.toggle_split(&editor::ToggleSplitDiff, window, cx)
            });
            cx.run_until_parked();
        }
        assert!(diff.read_with(cx, |diff, _| diff.is_split()));
        assert_eq!(expand_buttons_hidden(&diff, cx), (true, Some(true)));

        // Nor do the keys expand it, on either side.
        for side in [true, false] {
            let editor = diff.read_with(cx, |diff, _| {
                if side {
                    diff.rhs_editor().clone()
                } else {
                    diff.lhs_editor().unwrap().clone()
                }
            });
            cx.update(|window, cx| window.focus(&editor.focus_handle(cx), cx));
            cx.simulate_keystrokes("shift-enter");
            cx.dispatch_action(editor::actions::ExpandExcerptsUp::default());
            cx.dispatch_action(editor::actions::ExpandExcerptsDown::default());
            assert_eq!(shown_text(&diff, cx), before);
        }
    }

    #[gpui::test]
    async fn a_file_whose_diff_doesnt_match_githubs_shows_githubs_patch(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let tab = open_unloaded(&workspace, PULL, cx);
        // 94c4db2's poll cassette, whose swapped entries Zed's diff pairs across GitHub's hunks.
        let patch =
            include_str!("../tests/fixtures/github_patches/94c4db2-open-pull-requests.json.patch");
        let swapped = PullFile {
            filename: "cassette.json".into(),
            additions: 50,
            deletions: 50,
            changes: 100,
            patch: Some(patch.into()),
            ..github_file("cassette.json", 10)
        };

        show_github_files(&tab, vec![github_file("a.txt", 50), swapped], cx);

        let diff = tab.read_with(cx, |tab, _| tab.diff().unwrap().clone());
        let text = shown_text(&diff, cx);
        assert!(text.contains("a 50 changed"), "{text}");
        assert!(text.contains(patch), "{text}");
        let warnings: Vec<_> = diff.read_with(cx, |diff, cx| {
            let editor = diff.rhs_editor().read(cx);
            let headers = editor.addon::<pr_diff::FileHeaders>().unwrap();
            headers
                .warnings
                .values()
                .map(|warning| warning.label.to_string())
                .collect()
        });
        assert_eq!(warnings, [api_diff::GITHUBS_PATCH]);
    }

    #[gpui::test]
    async fn a_copy_that_fails_for_good_leaves_githubs_diff_with_a_banner(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let tab = open_unloaded(&workspace, PULL, cx);
        show_github_files(&tab, vec![github_file("a.txt", 50)], cx);

        let error = anyhow::anyhow!("the repository doesn't exist, or the account can't see it");
        tab.update(cx, |tab, cx| tab.load_failed(&error, cx));

        tab.read_with(cx, |tab, _| {
            assert_eq!(tab.status(), &Status::Ready);
            assert_eq!(tab.diff_source(), Some(DiffSource::GitHub));
        });
        assert_eq!(
            banners(&tab, cx),
            [
                "rozed couldn't show the local copy's diff, so this is GitHub's diff: changed lines \
              only. the repository doesn't exist, or the account can't see it"
            ]
        );

        // Without any diff, the tab says why it couldn't open.
        let other = open_unloaded(&workspace, "https://github.com/owner/repo/pull/2", cx);
        other.update(cx, |tab, cx| tab.load_failed(&error, cx));
        other.read_with(cx, |tab, _| {
            assert!(matches!(tab.status(), Status::Failed(_)))
        });
    }

    #[gpui::test]
    async fn a_copy_that_fails_before_githubs_diff_arrives_leaves_it_with_a_banner(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx).await;
        let tab = open_unloaded(&workspace, PULL, cx);
        let error = anyhow::anyhow!("the repository doesn't exist, or the account can't see it");

        // GitHub's diff is on its way when the copy fails for good.
        tab.update_in(cx, |tab, window, cx| {
            let source = GithubDiff {
                link: link(PULL),
                title: format!("{PULL}: Change a"),
                repo_name: "repo".into(),
                head: None,
            };
            tab.show_github_files(source, vec![github_file("a.txt", 50)], window, cx);
            tab.load_failed(&error, cx);
        });
        assert!(matches!(status(&tab, cx), Status::Loading(_)));
        assert_eq!(banners(&tab, cx).len(), 1);
        cx.run_until_parked();

        tab.read_with(cx, |tab, _| {
            assert_eq!(tab.status(), &Status::Ready);
            assert_eq!(tab.diff_source(), Some(DiffSource::GitHub));
        });
        assert_eq!(
            banners(&tab, cx),
            [
                "rozed couldn't show the local copy's diff, so this is GitHub's diff: changed lines \
              only. the repository doesn't exist, or the account can't see it"
            ]
        );
    }

    #[gpui::test]
    async fn a_public_pull_request_git_cant_copy_shows_githubs_diff(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let _git = GitServer::start(cx);
        uncopyable_pull(&github, "acme/app", 1, 2);
        anonymous(&github, cx);

        let tab = open_loaded(&workspace, "github.com/acme/app/pull/1", cx);

        tab.read_with(cx, |tab, _| {
            assert_eq!(tab.status(), &Status::Ready);
            assert_eq!(tab.diff_source(), Some(DiffSource::GitHub));
            assert_eq!(tab.files().len(), 2);
        });
        let banners = banners(&tab, cx);
        assert!(
            banners[0].starts_with("rozed couldn't show the local copy's diff"),
            "{banners:#?}"
        );
    }

    #[gpui::test]
    async fn githubs_diff_never_replaces_the_local_copys(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let tab = open_unloaded(&workspace, PULL, cx);

        let _local_copy = show_local_copy(&tab, cx);
        let local = tab.read_with(cx, |tab, _| tab.diff().unwrap().clone());
        // Whole files: the local copy has the lines GitHub's patches leave out.
        let text = shown_text(&local, cx);
        assert!(text.contains("a 48\n"), "{text:?}");
        show_github_files(
            &tab,
            vec![github_file("a.txt", 50), github_file("b.txt", 60)],
            cx,
        );

        tab.read_with(cx, |tab, _| {
            assert_eq!(tab.diff_source(), Some(DiffSource::LocalCopy));
            assert_eq!(tab.diff(), Some(&local));
        });
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
