//! The PR tab: one pull request's header, its files in review order, its side-by-side diff and its
//! threads (design §10 "Window"). rozed's window has one tab per pull request, and opening a pull
//! request that's already open switches to its tab.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::time::{Duration, Instant, SystemTime};

use anyhow::Context as _;
use futures::channel::oneshot;

use editor::scroll::Autoscroll;
use editor::{
    Anchor, Direction, Editor, EditorEvent, EditorSettings, SelectionEffects, SplittableEditor,
};
use github::accounts::RepoAccount;
use github::files::CutShort;
use github::poll::{PollEvent, Poller};
use github::queries::{FileViewedState, PullRequestData};
use github::viewed::ViewedRecords;
use github::{GithubClient, GithubError, HelpLink, PullRef, Target};
use gpui::{
    AnyElement, App, AsyncApp, AsyncWindowContext, Context, ElementId, Entity, EntityId,
    EventEmitter, FocusHandle, Focusable, KeyBinding, Render, SharedString, Subscription, Task,
    UniformListScrollHandle, WeakEntity, Window, actions, uniform_list,
};
use language::{Capability, Point};
use project::WorktreeId;
use repo_cache::Oid;
use review::revisions::{PushRecord, Revisions, ViewedState, build_revisions};
use settings::{DiffViewStyle, Settings as _, SettingsStore};
use ui::{ListItem, ListItemSpacing, Tooltip, prelude::*};
use util::rel_path::RelPath;
use workspace::item::{Item, ItemEvent, TabContentParams};
use workspace::notifications::NotificationId;
use workspace::{ItemNavHistory, Toast, Workspace};

use crate::account::{self, AccountDecision, Acting, Refusal};
use crate::account_picker::{self, NotChosen};
use crate::api_diff::{self, FilesReader, GithubDiff, thousands};
use crate::diff_cursor;
use crate::file_list::{FileChange, FileEntry, FileList, FileOrder};
use crate::github_api::{self, CanSignIn, PullRequest};
use crate::github_layer::{self, Saved};
use crate::poll_state::{self, PollNote, PollState};
use crate::pr_banner::{self, BannerAction, BannerKind, Banners};
use crate::pr_diff::{self, CopyProgress, Trust};
use crate::pr_link::PrLink;
use crate::shown_files::{self, Expanded};
use crate::tabs::{TabKey, find_tab};
use crate::viewed_marks::{Toggle, ViewedMarks, changed_since_viewed, write_failed};
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
        /// Side by side, moves the cursor to the diff's other side.
        SwitchSide,
        /// Shows the diff inline or side by side.
        ToggleDiffLayout,
        /// Hides changes to whitespace alone, or shows them again.
        ToggleWhitespace,
        /// Opens the current file when it's collapsed, loads it when it's too large to have
        /// loaded, and otherwise shows the whole file or only its changes.
        ExpandFile,
        /// Marks the current file viewed and moves to the next unviewed file, or clears the mark
        /// of a viewed file.
        ToggleViewed,
        /// Moves to the next file in the diff.
        NextFile,
        /// Moves to the previous file in the diff.
        PreviousFile,
        /// Moves to the next change, in whichever file it is.
        NextChange,
        /// Moves to the previous change, in whichever file it is.
        PreviousChange,
    ]
);

const KEY_CONTEXT: &str = "PrTab";
const FILE_LIST_CONTEXT: &str = "PrFileList";
/// The diff's editors, inside the tab, where `tab` would otherwise indent: full editors of many
/// files, not an editor that's typed in, such as a comment's, which edits one buffer.
const DIFF_CONTEXT: &str = "PrTab > Editor && mode == full && multibuffer";
/// The tab outside any text box in it, such as a comment box: keys that type a character work
/// here, so a text box still types them. The diff's own editors are the only full-size editors of
/// many files.
const READING_CONTEXT: &str = "PrTab && !(Editor && (mode != full || !multibuffer))";

/// The tab's keys. Panels toggle with Zed's keys for its docks.
pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("tab", SwitchSide, Some(DIFF_CONTEXT)),
        // Option and a letter types a character where there's text to type, so these work only
        // in the diff and the file list.
        KeyBinding::new("alt-v", ToggleDiffLayout, Some(DIFF_CONTEXT)),
        KeyBinding::new("alt-v", ToggleDiffLayout, Some(FILE_LIST_CONTEXT)),
        KeyBinding::new("alt-w", ToggleWhitespace, Some(DIFF_CONTEXT)),
        KeyBinding::new("alt-w", ToggleWhitespace, Some(FILE_LIST_CONTEXT)),
        KeyBinding::new("alt-a", ExpandFile, Some(DIFF_CONTEXT)),
        KeyBinding::new("alt-a", ExpandFile, Some(FILE_LIST_CONTEXT)),
        KeyBinding::new("cmd-b", ToggleFileList, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-alt-b", ToggleThreads, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-r", ToggleThreads, Some(KEY_CONTEXT)),
        // alt-o types ø.
        KeyBinding::new("alt-o", ToggleFileOrder, Some(READING_CONTEXT)),
        KeyBinding::new("e", ToggleViewed, Some(READING_CONTEXT)),
        KeyBinding::new("] f", NextFile, Some(READING_CONTEXT)),
        KeyBinding::new("[ f", PreviousFile, Some(READING_CONTEXT)),
        KeyBinding::new("] c", NextChange, Some(READING_CONTEXT)),
        KeyBinding::new("[ c", PreviousChange, Some(READING_CONTEXT)),
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
    // The window may be titled after it while it isn't the active item, as with an empty pane
    // active.
    cx.subscribe_in(&tab, window, |workspace, _, _: &PrTabEvent, window, cx| {
        window_title::update(workspace, window, cx)
    })
    .detach();
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

/// Closes `tab` and opens its pull request again in its place, at its latest push.
pub(crate) fn reopen(
    workspace: &mut Workspace,
    tab: &Entity<PrTab>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Option<Entity<PrTab>> {
    reopen_with(workspace, tab, window, cx, PrTab::load)
}

/// [`reopen`], with `start` run on the new tab instead of loading it.
fn reopen_with(
    workspace: &mut Workspace,
    tab: &Entity<PrTab>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
    start: impl FnOnce(&mut PrTab, &mut Window, &mut Context<PrTab>),
) -> Option<Entity<PrTab>> {
    let pane = workspace.pane_for(tab)?;
    let index = pane.read(cx).index_for_item(tab)?;
    let (link, trust) = {
        let tab = tab.read(cx);
        (tab.link().clone(), tab.trust)
    };
    log::info!("{link}: opening it again at its latest push");
    pane.update(cx, |pane, cx| {
        pane.remove_item(tab.entity_id(), false, false, window, cx)
    });
    let weak_workspace = workspace.weak_handle();
    let reopened = cx.new(|cx| {
        let mut reopened = PrTab::new(link, trust, weak_workspace, window, cx);
        start(&mut reopened, window, cx);
        reopened
    });
    workspace.add_item(
        pane,
        Box::new(reopened.clone()),
        Some(index),
        true,
        true,
        window,
        cx,
    );
    Some(reopened)
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
    /// GitHub's view didn't load, and the diff is the local copy's, read-only, while rozed keeps
    /// trying. Says so, and why.
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
    /// The tab's own entity's.
    id: EntityId,
    key: TabKey,
    trust: Trust,
    workspace: WeakEntity<Workspace>,
    focus_handle: FocusHandle,
    file_list_focus: FocusHandle,
    file_list_scroll: UniformListScrollHandle,
    acting: Option<Acting>,
    /// The account to open the pull request with, while it can see the repository, whatever
    /// account the repository's choice is: a recent row's, for the load that row started.
    preferred: Option<String>,
    /// Why the diff shown is a local copy that isn't of GitHub's latest head, while it is.
    showing_copy: Option<OlderCopy>,
    header: Header,
    created_at: Option<SystemTime>,
    files: FileList,
    /// GitHub's viewed marks, and the ones being set, kept for when the files arrive.
    marks: ViewedMarks,
    /// The pull request's GraphQL node ID, which writes name it by.
    node_id: Option<String>,
    /// The head on screen: GitHub's at first, then the one the local copy fetched.
    head: Option<String>,
    /// When rozed learned the head on screen. What GitHub said about its head before then is
    /// older news.
    head_at: Option<SystemTime>,
    /// GitHub's latest head for the pull request, from its details or its summary, and when GitHub
    /// gave it. `e` marks files only while it's the head on screen.
    latest_head: Option<(String, SystemTime)>,
    /// Where this Mac's records say each file was marked viewed, by path.
    viewed_at: HashMap<String, String>,
    /// This Mac's records of where files were marked viewed, once signed in.
    records: Option<ViewedRecords>,
    revisions: Option<Revisions>,
    status: Status,
    /// GitHub's links for fixing why the tab failed, such as authorizing single sign-on.
    help_links: Vec<HelpLink>,
    /// What the tab offers once it stopped loading for a reason only the user can fix.
    action: Option<TabAction>,
    /// Whether the viewed marks shown are another account's than the one the tab acts as, to
    /// let go of once GitHub answers for that one.
    viewed_are_others: bool,
    /// Whether GitHub gave this load the pull request, so anything that fails after is the local
    /// copy's.
    answered: bool,
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
    /// Inline or side by side, for this tab's diffs: the setting's at first, then the key's.
    diff_style: DiffViewStyle,
    /// The setting's style when last seen, to tell a change to it from any other settings change.
    setting_style: DiffViewStyle,
    /// Whether changes to whitespace alone are hidden.
    hide_whitespace: bool,
    /// Brings the diff's files in line with `hide_whitespace`.
    _whitespace: Option<Task<()>>,
    /// Loads large files on a key, one task a file of a diff, each until it's done.
    loading: HashMap<(EntityId, String), Task<()>>,
    /// Keeps the diff's images right as it goes side by side and back, and stops decoding a
    /// file's images when it folds.
    _diff_watch: Vec<Subscription>,
    show_file_list: bool,
    show_threads: bool,
    /// The account's poller and the pull request it watches for this tab, until the tab closes.
    watched: Option<(Entity<Poller>, PullRef)>,
    /// How the poller is doing, for the header.
    poll: PollState,
    /// The trusted checkout's worktree, which goes with the last tab that uses it.
    checkout: Option<WorktreeId>,
    opened: Instant,
    /// Where the tab put the diff's cursor while files arrive, until it's moved.
    parked_cursor: Option<Anchor>,
    /// Whether the diff's cursor was moved, by a key, a click or the file list. From then on the
    /// tab leaves it where it is.
    cursor_moved: bool,
    /// Keeps the file list's selection on the file the diff's cursor is in, and the cursor at the
    /// start until it's moved.
    _diff_cursor: Option<Subscription>,
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
    /// What the tab hears from the poller it watches with, replaced when it watches with another.
    _poller_subscriptions: Vec<Subscription>,
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
        let style = EditorSettings::get_global(cx).diff_view_style;
        // A diff goes back to the setting's style on any change to settings; the tab's style
        // follows a change to the setting itself, and otherwise puts its own back.
        let on_settings = cx.observe_global_in::<SettingsStore>(window, |this, window, cx| {
            let setting = EditorSettings::get_global(cx).diff_view_style;
            if setting != this.setting_style {
                this.setting_style = setting;
                this.diff_style = setting;
            }
            cx.defer_in(window, |this, window, cx| this.apply_diff_style(window, cx));
        });
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
            showing_copy: None,
            header: Header::default(),
            created_at: None,
            files: FileList::default(),
            marks: ViewedMarks::default(),
            node_id: None,
            head: None,
            head_at: None,
            latest_head: None,
            viewed_at: HashMap::new(),
            records: None,
            revisions: None,
            status: Status::Loading("Finding an account that can see it…".into()),
            help_links: Vec::new(),
            action: None,
            viewed_are_others: false,
            answered: false,
            saved: None,
            banners,
            diff: None,
            diff_source: None,
            nav_history: None,
            diff_style: style,
            setting_style: style,
            hide_whitespace: false,
            _whitespace: None,
            loading: HashMap::new(),
            _diff_watch: Vec::new(),
            show_file_list: true,
            show_threads: true,
            watched: None,
            poll: PollState::default(),
            checkout: None,
            opened: Instant::now(),
            parked_cursor: None,
            cursor_moved: false,
            _diff_cursor: None,
            _load: None,
            _github_diff: None,
            copy_failed: None,
            github_failed: false,
            api_head: None,
            github_head: None,
            retry_copy: None,
            _subscriptions: vec![on_focus, on_banners, on_settings],
            _poller_subscriptions: Vec::new(),
        }
    }

    /// Starts loading the pull request.
    pub fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (link, trust, opened) = (self.key.opened_as.clone(), self.trust, self.opened);
        self._load = Some(cx.spawn_in(window, async move |this, cx| {
            // How many times GitHub was checked since a load last worked, so the waits keep
            // growing across loads that fail again.
            let mut checks = 0;
            // Only the load the user started may ask which account to use; one rozed starts by
            // itself never puts the picker up.
            let mut may_ask = true;
            loop {
                let saved = saved_details(&link, cx).await;
                let loaded = load(
                    this.clone(),
                    link.clone(),
                    trust,
                    opened,
                    &saved,
                    may_ask,
                    cx,
                );
                // GitHub answered, which ends showing the copy for want of it.
                let Err(error) = loaded.await else {
                    return;
                };
                let waitable = is_waitable(&error);
                match this.read_with(cx, |this, _| (this.showing_copy.is_some(), this.answered)) {
                    Err(_) => return,
                    // GitHub gave the pull request, so its diff, or the copy shown, stays, with
                    // why the rest didn't load. Waiting won't fix that: the local copy tries
                    // again by itself for what it may.
                    Ok((_, true)) => {
                        log::error!("{link}: {error:#}");
                        this.update(cx, |this, cx| {
                            this.help_links = help_links(&error);
                            this.load_failed(&error, cx)
                        })
                        .ok();
                        return;
                    }
                    // The local copy stays, whatever kept GitHub's from loading, with why.
                    Ok((true, false)) => {
                        log::warn!("{link}: {error:#}; still showing the local copy");
                        let shown =
                            this.update(cx, |this, cx| this.showing_copy_because(&error, cx));
                        if shown.is_err() {
                            return;
                        }
                    }
                    Ok((false, false)) if waitable => {
                        if let Err(error) =
                            show_offline(this.clone(), link.clone(), opened, saved, error, cx).await
                        {
                            failed(&this, &link, error, cx);
                        }
                    }
                    Ok((false, false)) => {
                        failed(&this, &link, error, cx);
                        return;
                    }
                }
                // Only what waiting may fix is tried again by itself; the rest waits for the
                // user, whose button or link starts over.
                if !waitable {
                    return;
                }
                if !wait_for_github(&mut checks, cx).await {
                    return;
                }
                may_ask = false;
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
        if let Some(diff) = &self.diff {
            shown_files::release_images(diff, None, cx);
        }
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
        // A recent row's account is for the load it started.
        self.preferred = None;
        self.help_links.clear();
        self.action = None;
        self.answered = false;
        // The load before is dropped, and with it the copy its Retry button would try again.
        self.retry_copy = None;
        self.copy_failed = None;
        self.github_failed = false;
        self.banners.update(cx, |banners, cx| {
            banners.hide(BannerKind::LocalCopyFailed, cx)
        });
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

    /// Whether the tab's diffs show inline or side by side.
    pub fn diff_style(&self) -> DiffViewStyle {
        self.diff_style
    }

    /// Whether the tab's diffs hide changes to whitespace alone.
    pub fn hides_whitespace(&self) -> bool {
        self.hide_whitespace
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
        // Viewed marks are the account's own, so another account's saved ones go once GitHub
        // answers for this one; until then, as when this load fails and the local copy stays,
        // they stay.
        if let Some((saved_for, _)) = &self.saved {
            self.viewed_are_others = acting
                .login()
                .is_none_or(|login| !login.eq_ignore_ascii_case(saved_for));
        }
        self.acting = Some(acting);
        self.set_status(Status::Loading("Asking GitHub for it…".into()), cx);
    }

    /// Shows what rozed saved about the pull request, at once and online or not, until GitHub
    /// answers.
    pub(crate) fn show_saved(&mut self, saved: &Saved, cx: &mut Context<Self>) {
        self.take_details(&saved.details, saved.fetched_at, Some(&saved.login), cx);
        self.saved = Some((saved.login.clone(), saved.fetched_at));
        self.show_saved_banner(true, cx);
    }

    /// The tab shows the local copy's diff with what rozed saved, because loading from GitHub
    /// failed with `why`: with GitHub's links to fix it, and when waiting won't fix it, a button
    /// to try again.
    fn showing_copy_because(&mut self, why: &anyhow::Error, cx: &mut Context<Self>) {
        let offline = is_offline(why);
        self.showing_copy = Some(if offline {
            OlderCopy::Unreachable
        } else {
            OlderCopy::NotLoaded
        });
        self.show_saved_banner(false, cx);
        self.help_links = help_links(why);
        let waitable = is_waitable(why);
        self.action = (!waitable).then(|| action_for(why));
        let reason = sentence(why);
        let text = if offline {
            format!(
                "Offline: {reason}. This is the local copy's diff, read-only, until GitHub \
                 answers again; rozed keeps trying."
            )
        } else if is_retryable(why) {
            format!(
                "GitHub didn't answer: {reason}. This is the local copy's diff, read-only, until \
                 it does; rozed keeps trying."
            )
        } else if waitable {
            format!(
                "rozed couldn't load the pull request from GitHub: {reason}. This is the local \
                 copy's diff, read-only; rozed keeps trying."
            )
        } else if self.action == Some(TabAction::ChooseAccount) {
            format!("{reason}. This is the local copy's diff, read-only.")
        } else {
            format!(
                "rozed couldn't load the pull request from GitHub: {reason}. This is the local \
                 copy's diff, read-only."
            )
        };
        self.set_status(Status::Offline(text.into()), cx);
    }

    /// Loads the pull request again because the user asked to, with the tab's button: this load
    /// may ask which account to use.
    fn load_again(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.start_again(cx);
        self.load(window, cx);
    }

    /// The tab waits for the account picker to ask about `subject`, or the picker asks now.
    pub(crate) fn waiting_for_account(
        &mut self,
        subject: &str,
        asking: bool,
        cx: &mut Context<Self>,
    ) {
        let text = if asking {
            format!("Choose the account rozed uses for {subject}…")
        } else {
            format!("Waiting to ask which account rozed uses for {subject}…")
        };
        self.set_status(Status::Loading(text.into()), cx);
    }

    /// Reads where this Mac recorded `saved`'s account viewing the files, for the notes on what
    /// changed since, while the local copy shows without GitHub. Nothing's written.
    fn read_saved_records(&mut self, saved: &Saved, cx: &App) {
        let Some(layer) = github_layer::loaded(cx) else {
            return;
        };
        // The records go by the repository's name as GitHub gave it with the saved details.
        let link = saved
            .details
            .url
            .parse::<PrLink>()
            .unwrap_or_else(|_| self.link().clone());
        let Ok(number) = i64::try_from(link.number) else {
            return;
        };
        let pull = PullRef::new(&link.owner, &link.repo, number);
        match layer.read(cx).viewed_records().heads(&saved.login, &pull) {
            Ok(heads) => self.viewed_at = heads,
            Err(error) => log::error!("{pull}: reading where files were viewed: {error:#}"),
        }
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

    /// Takes GitHub's REST view of the pull request, which GitHub gave at `answered_at`.
    pub fn set_pull_request(
        &mut self,
        pull_request: &PullRequest,
        answered_at: SystemTime,
        cx: &mut Context<Self>,
    ) {
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
        // A diff on screen, such as the local copy shown offline, keeps its head until the new
        // copy replaces it; if GitHub's is newer, `e` won't mark files as of it meanwhile.
        if self.diff.is_none() {
            self.set_head(&pull_request.head.sha, answered_at);
        }
        self.note_latest_head(&pull_request.head.sha, answered_at);
        self.api_head = Some(pull_request.head.sha.clone());
        self.answered = true;
        self.showing_copy = None;
        self.status = Status::Loading("Copying it locally…".into());
        // GitHub answered for the account the tab acts as, whose viewed marks its details bring.
        if std::mem::take(&mut self.viewed_are_others) {
            self.marks.set_github(HashMap::new());
            self.files.set_viewed(&self.marks.shown());
        }
        // GitHub answered, and the account's poller brings its details up to date.
        if self.saved.take().is_some() {
            self.banners
                .update(cx, |banners, cx| banners.hide(BannerKind::Saved, cx));
        }
        self.changed(cx);
    }

    /// Takes the pull request's details, just fetched by its account's poller: its title, author,
    /// head and the account's viewed marks.
    pub fn set_details(&mut self, details: &PullRequestData, cx: &mut Context<Self>) {
        let login = self.watched_login(cx);
        self.take_details(details, SystemTime::now(), login.as_deref(), cx);
    }

    /// The account whose poller the tab watches its pull request with.
    fn watched_login(&self, cx: &App) -> Option<String> {
        let (poller, _) = self.watched.as_ref()?;
        Some(poller.read(cx).login().to_string())
    }

    /// Whether this Mac's records of where files were viewed follow `login`'s marks: those of
    /// the account the tab acts as and watches with, whose records the tab writes.
    fn records_follow(&self, login: Option<&str>, cx: &App) -> bool {
        let (Some(login), Some(watched)) = (login, self.watched_login(cx)) else {
            return false;
        };
        let acting = self.acting.as_ref().and_then(Acting::login);
        watched.eq_ignore_ascii_case(login)
            && acting.is_some_and(|acting| acting.eq_ignore_ascii_case(login))
    }

    /// Takes the pull request's details as GitHub gave them at `fetched_at` to `login`, from the
    /// poller or from what rozed saved.
    fn take_details(
        &mut self,
        details: &PullRequestData,
        fetched_at: SystemTime,
        login: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        self.note_latest_head(&details.head_ref_oid, fetched_at);
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
        self.node_id = Some(details.id.clone());
        // Details from before a write finished, such as the ones a tab switch hands back, don't
        // undo it.
        let viewed = self.marks.without_stale(viewed, fetched_at);
        // Only the watched account's marks say where it viewed files.
        if self.records_follow(login, cx) {
            self.follow_marks_set_elsewhere(&viewed, &details.head_ref_oid, cx);
        }
        let listed_changed = self.marks.set_listed(listed_paths(details));
        let marks_changed = self.marks.set_github(viewed) || listed_changed;
        // A change of title makes the workspace report the active item changed, which comes back
        // here through `set_active`, so only a real change is reported.
        if header == self.header && !marks_changed {
            return;
        }
        self.header = header;
        self.files.set_viewed(&self.marks.shown());
        self.changed(cx);
    }

    /// GitHub's `marks`, read at `head`, changed in ways rozed's own writes don't explain, so this
    /// Mac's records follow them (design §6): a file GitHub says isn't viewed has no record, and a
    /// file marked viewed again elsewhere is recorded at `head`. A file changed since viewed keeps
    /// its record, which is what it's for.
    fn follow_marks_set_elsewhere(
        &mut self,
        marks: &HashMap<String, ViewedState>,
        head: &str,
        cx: &mut Context<Self>,
    ) {
        let changes: Vec<(String, Option<String>)> = marks
            .iter()
            .filter(|(path, _)| !self.marks.is_pending(path))
            .filter_map(|(path, state)| match state {
                ViewedState::Unviewed if self.viewed_at.contains_key(path) => {
                    Some((path.clone(), None))
                }
                ViewedState::Viewed
                    if matches!(
                        self.marks.github_state(path),
                        Some(ViewedState::Unviewed | ViewedState::Dismissed)
                    ) =>
                {
                    Some((path.clone(), Some(head.to_string())))
                }
                _ => None,
            })
            .collect();
        if changes.is_empty() {
            return;
        }
        for (path, head) in &changes {
            match head {
                Some(head) => self.viewed_at.insert(path.clone(), head.clone()),
                None => self.viewed_at.remove(path),
            };
        }
        let (Some(records), Some((poller, pull))) = (self.records.clone(), self.watched.clone())
        else {
            return;
        };
        let login = poller.read(cx).login().to_string();
        let now = SystemTime::now();
        cx.spawn(async move |_, _| {
            for (path, head) in changes {
                let written = match head {
                    Some(head) => records.record(&login, &pull, &path, &head, now).await,
                    None => records.forget(&login, &pull, &path).await,
                };
                if let Err(error) = written {
                    log::error!("{pull}: recording where {path} was viewed: {error:#}");
                }
            }
        })
        .detach();
    }

    /// The local copy listed the pull request's files at `head`, which GitHub gave at
    /// `polled_at`: lists them, builds its revisions, and returns where each file goes in the
    /// diff, by its index in `entries`.
    pub(crate) fn copied(
        &mut self,
        entries: Vec<FileEntry>,
        head: &Oid,
        polled_at: SystemTime,
        cx: &mut Context<Self>,
    ) -> Vec<usize> {
        self.list(entries);
        self.set_head(head.as_str(), polled_at);
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
        // The list may reorder, as files its new entries say are generated go last; the file
        // selected stays selected and in view.
        self.scroll_to_selected();
        cx.notify();
        self.files.review_positions()
    }

    /// Where each of the local copy's `entries` goes in its diff, by its index in `entries`,
    /// without listing them yet.
    pub(crate) fn positions_of(&self, entries: &[FileEntry]) -> Vec<usize> {
        FileList::new(entries.to_vec()).review_positions()
    }

    /// Lists `entries` in place of the files listed so far, in the same order and keeping the
    /// selected file.
    fn list(&mut self, entries: Vec<FileEntry>) {
        let mut list = FileList::new(entries);
        list.set_order(self.files.order());
        list.set_viewed(&self.marks.shown());
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
            title: format!(
                "{link}: {}",
                window_title::without_bidi(&pull_request.title)
            ),
            repo_name: name.to_string(),
            head: None,
            listed_at: SystemTime::now(),
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
            source.listed_at = SystemTime::now();
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

    /// GitHub's diff, listed at `head` at `listed_at`, is ready to show in `editor`, which lists
    /// `entries`. Unless the local copy's diff got there first, lists them and shows it, and
    /// returns where each file goes in it.
    pub(crate) fn show_github_diff(
        &mut self,
        entries: Vec<FileEntry>,
        editor: Entity<SplittableEditor>,
        head: Option<String>,
        listed_at: SystemTime,
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
        // The head on screen is the one GitHub listed the files at, which GitHub says is its
        // latest.
        if let Some(head) = &head {
            self.set_head(head, listed_at);
            self.note_latest_head(head, listed_at);
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

    /// What the tab shows until the local copy's diff is ready, if it shows GitHub's or an older
    /// copy.
    fn meanwhile(&self) -> String {
        if self.diff_source == Some(DiffSource::GitHub) {
            " Until then, this is GitHub's diff: changed lines only.".to_string()
        } else if self.shows_older_copy() {
            let shown = self.head.as_deref().map(short).unwrap_or_default();
            format!(" Until then, this is the local copy of {shown}, an older push.")
        } else {
            String::new()
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
        let status = if self.shows_older_copy() {
            // The banner's button tries the copy again.
            self.keep_older_copy(why, cx);
            return;
        } else if self.diff.is_some() {
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
        if self.showing_copy == Some(OlderCopy::NewerNotCopied) {
            self.showing_copy = None;
        }
        self.banners.update(cx, |banners, cx| {
            banners.hide(BannerKind::LocalCopyFailed, cx)
        });
        self.set_status(Status::Loading("Copying it locally…".into()), cx);
    }

    /// Whether the diff on screen is a local copy of an older head than the one GitHub gave
    /// this load, as when the copy shown offline waits for a newer push to be copied.
    fn shows_older_copy(&self) -> bool {
        self.diff_source == Some(DiffSource::LocalCopy)
            && self.api_head.is_some()
            && self.head != self.api_head
    }

    /// GitHub gave a newer head than the local copy on screen, and copying it failed for `why`:
    /// the older copy stays, read-only, saying so.
    fn keep_older_copy(&mut self, why: &str, cx: &mut Context<Self>) {
        self.showing_copy = Some(OlderCopy::NewerNotCopied);
        let latest = self.api_head.as_deref().map(short).unwrap_or_default();
        let shown = self.head.as_deref().map(short).unwrap_or_default();
        let why = why.trim_end().trim_end_matches('.');
        let text = format!(
            "GitHub has {latest}, but rozed couldn't copy it: {why}. This is the local copy of \
             {shown}, read-only."
        );
        self.set_status(Status::Offline(text.into()), cx);
    }

    /// Shows that the local copy's diff is loading file `file` of its `total`, counting from 1.
    pub(crate) fn loading_diff(&mut self, file: usize, total: usize, cx: &mut Context<Self>) {
        let what = if self.diff_source == Some(DiffSource::GitHub) {
            "the local copy's diff"
        } else {
            "the diff"
        };
        self.set_status(
            Status::Loading(format!("Loading {what}: file {file} of {total}…").into()),
            cx,
        );
    }

    /// Every file is in the local copy's diff.
    pub(crate) fn diff_loaded(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_status(Status::Ready, cx);
        // A file added while the whitespace key was pressed may show the other way.
        self.sync_whitespace(window, cx);
    }

    /// Loading failed with `error`. GitHub's diff, if it shows or is on its way, stays, with a
    /// banner saying why the local copy's won't come; otherwise the tab says why it couldn't open.
    fn load_failed(&mut self, error: &anyhow::Error, cx: &mut Context<Self>) {
        let github_coming =
            self.diff.is_none() && self._github_diff.is_some() && !self.github_failed;
        if self.diff.is_none() && !github_coming {
            self.action = Some(TabAction::TryAgain);
            self.set_status(Status::Failed(format!("{error:#}").into()), cx);
            return;
        }
        if self.shows_older_copy() {
            self.action = Some(TabAction::TryAgain);
            self.keep_older_copy(&format!("{error:#}"), cx);
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

    /// Shows `editor` as the tab's diff, from `source`, with its cursor at its start until it's
    /// moved. The local copy's diff stops GitHub's from loading.
    pub(crate) fn show_diff(
        &mut self,
        editor: Entity<SplittableEditor>,
        source: DiffSource,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focused = self.focus_handle.contains_focused(window, cx);
        if let Some(old) = self.diff.as_ref().filter(|old| **old != editor) {
            shown_files::release_images(old, Some(window), cx);
        }
        // Loads into a diff no longer shown stop, and let it go.
        let shown = editor.entity_id();
        self.loading.retain(|(diff, _), _| *diff == shown);
        self._diff_watch = vec![
            cx.observe_in(&editor, window, |_, diff, window, cx| {
                shown_files::sync_side_by_side(&diff, window, cx)
            }),
            cx.subscribe_in(&editor, window, |_, diff, event: &EditorEvent, _, cx| {
                if let EditorEvent::BufferFoldToggled { ids, folded } = event {
                    shown_files::fold_toggled(diff, ids, *folded, cx);
                }
            }),
        ];
        let rhs = editor.read(cx).rhs_editor().clone();
        if let Some(history) = self.nav_history.clone() {
            rhs.update(cx, |editor, _| editor.set_nav_history(Some(history)));
        }
        self._diff_cursor = Some(cx.subscribe_in(
            &rhs,
            window,
            |this, _, event: &EditorEvent, window, cx| match event {
                EditorEvent::SelectionsChanged { .. } => this.diff_cursor_moved(cx),
                EditorEvent::ScrollPositionChanged { .. } => this.select_shown_file(cx),
                EditorEvent::BufferRangesUpdated { .. } => this.park_cursor(window, cx),
                _ => {}
            },
        ));
        self.parked_cursor = None;
        self.diff = Some(editor.clone());
        self.diff_source = Some(source);
        if source == DiffSource::LocalCopy {
            self._github_diff = None;
            self.banners.update(cx, |banners, cx| {
                banners.hide(BannerKind::GithubDiffFailed, cx);
                banners.hide(BannerKind::GithubDiffCut, cx);
            });
        }
        // A diff with its files in already, as when the local copy's replaces GitHub's, sends no
        // event as they arrive, so its cursor would stay at its end, in the last file.
        self.cursor_moved = false;
        self.park_cursor(window, cx);
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
        // Files stay open or folded as the reviewer left them, before the view scrolls to the
        // same line.
        let to_load = match self.diff.clone() {
            Some(old) => {
                // The file the reviewer is reading, as `e` and alt-a see it.
                let shown = diff_cursor::shown_file(&old, cx).map(|(_, path)| path);
                shown_files::carry_over(&old, &editor, shown.as_deref(), window, cx)
            }
            None => Vec::new(),
        };
        self.show_diff(editor.clone(), DiffSource::LocalCopy, window, cx);
        // A large file the reviewer had open in GitHub's diff loads, as alt-a would load it.
        for path in to_load {
            self.load_file(editor.clone(), path, window, cx);
        }
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

    /// Watches the pull request with its account's poller, once GitHub confirmed it exists, and
    /// reads where this Mac's `records` say its files were marked viewed. It becomes the poller's
    /// open pull request only while it's the active tab ([`sync_active`]).
    fn watch(
        &mut self,
        poller: Entity<Poller>,
        pull: PullRef,
        records: Option<ViewedRecords>,
        cx: &mut Context<Self>,
    ) {
        // A load as another account, or by the repository's name now, stops the watch before,
        // and another account's marks go with it.
        if let Some((old, old_pull)) = self.watched.take() {
            if old != poller || old_pull != pull {
                old.update(cx, |old, cx| old.unwatch(&old_pull, cx));
            }
            if !old
                .read(cx)
                .login()
                .eq_ignore_ascii_case(poller.read(cx).login())
            {
                self.marks = ViewedMarks::default();
                self.viewed_at.clear();
                self.node_id = None;
                self.files.set_viewed(&self.marks.shown());
            }
        }
        if let Some(records) = &records {
            match records.heads(poller.read(cx).login(), &pull) {
                Ok(heads) => self.viewed_at = heads,
                Err(error) => log::error!("{pull}: reading where files were viewed: {error:#}"),
            }
        }
        self.records = records;
        poller.update(cx, |poller, cx| poller.watch(pull.clone(), cx));
        self.poll.status = poller.read(cx).status().clone();
        self._poller_subscriptions = vec![
            cx.observe(&poller, |this, poller, cx| {
                let status = poller.read(cx).status().clone();
                if this.poll.status != status {
                    this.poll.status = status;
                    cx.notify();
                }
            }),
            cx.subscribe(&poller, |this, _, event, cx| this.poll_event(event, cx)),
        ];
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
                self.note_latest_head(&summary.head.sha, SystemTime::now());
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

    /// GitHub said at `at` that the pull request's head is `head`. Something it said earlier
    /// doesn't replace something it said later.
    fn note_latest_head(&mut self, head: &str, at: SystemTime) {
        if self
            .latest_head
            .as_ref()
            .is_none_or(|(_, seen_at)| at >= *seen_at)
        {
            self.latest_head = Some((head.to_string(), at));
        }
    }

    /// The diff shows the pull request at `head`, which GitHub gave at `given_at`.
    fn set_head(&mut self, head: &str, given_at: SystemTime) {
        if self.head.as_deref() != Some(head) {
            self.head = Some(head.to_string());
            self.head_at = Some(given_at);
        }
    }

    /// GitHub's latest head, when GitHub gave it no earlier than it gave the head on screen and
    /// it isn't that head.
    fn newer_head(&self) -> Option<&str> {
        let (latest, at) = self.latest_head.as_ref()?;
        let newer =
            Some(latest) != self.head.as_ref() && self.head_at.is_none_or(|head_at| *at >= head_at);
        newer.then_some(latest.as_str())
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
                let login = poller.read(cx).login().to_string();
                self.take_details(&answer.details, answer.fetched_at, Some(&login), cx);
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

    /// `tab`: side by side, moves the cursor to the diff's other side.
    fn switch_side(&mut self, _: &SwitchSide, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(diff) = &self.diff {
            shown_files::switch_side(diff, window, cx);
        }
    }

    /// Shows the tab's diffs inline or side by side.
    fn toggle_diff_layout(
        &mut self,
        _: &ToggleDiffLayout,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.diff_style = match self.diff_style {
            DiffViewStyle::Split => DiffViewStyle::Unified,
            DiffViewStyle::Unified => DiffViewStyle::Split,
        };
        self.apply_diff_style(window, cx);
        cx.notify();
    }

    fn apply_diff_style(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(diff) = &self.diff {
            shown_files::set_style(diff, self.diff_style, window, cx);
        }
    }

    /// Hides changes to whitespace alone, or shows them again.
    fn toggle_whitespace(
        &mut self,
        _: &ToggleWhitespace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.hide_whitespace = !self.hide_whitespace;
        self.sync_whitespace(window, cx);
        cx.notify();
    }

    /// Brings each of the diff's files in line with whether whitespace changes are hidden.
    pub(crate) fn sync_whitespace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(diff) = self.diff.clone() else {
            return;
        };
        let hide = self.hide_whitespace;
        // Replacing the task stops the one before, so no file is changed by two at once.
        self._whitespace = Some(cx.spawn_in(window, async move |_, cx| {
            shown_files::hide_whitespace(diff, hide, cx).await
        }));
    }

    /// Opens, loads or shows the whole of the file selected in the file list, when it has focus,
    /// or else the file the diff shows, which `e` acts on too.
    fn expand_file(&mut self, _: &ExpandFile, window: &mut Window, cx: &mut Context<Self>) {
        let Some(diff) = self.diff.clone() else {
            return;
        };
        let in_file_list = self.file_list_focus.contains_focused(window, cx);
        let selected = self
            .files
            .selected()
            .filter(|_| in_file_list)
            .map(|entry| entry.path().to_string());
        let Some(path) = selected.or_else(|| self.shown_file(cx).map(|(_, path)| path)) else {
            return;
        };
        if shown_files::expand(&diff, &path, window, cx) == Expanded::Load {
            self.load_file(diff, path, window, cx);
        }
    }

    /// Loads `path`, a file too large to have been loaded with the others, into `diff`, unless
    /// it's already loading.
    fn load_file(
        &mut self,
        diff: Entity<SplittableEditor>,
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = (diff.entity_id(), path.clone());
        if self.loading.contains_key(&key) {
            return;
        }
        let Some(project) = self
            .workspace
            .upgrade()
            .map(|workspace| workspace.read(cx).project().clone())
        else {
            return;
        };
        let hide = self.hide_whitespace;
        let task = cx.spawn_in(window, {
            let key = key.clone();
            async move |this, cx| {
                let loaded = pr_diff::load_large_file(diff, project, path.clone(), hide, cx).await;
                this.update_in(cx, |this, window, cx| {
                    this.loading.remove(&key);
                    if let Err(error) = loaded {
                        log::error!("can't load {path}: {error:#}");
                        this.banners.update(cx, |banners, cx| {
                            banners.show(
                                BannerKind::LoadFileFailed,
                                Severity::Warning,
                                format!("rozed couldn't load {path}: {error:#}"),
                                cx,
                            )
                        });
                    }
                    // Whitespace changes may have been hidden, or shown again, while it loaded.
                    this.sync_whitespace(window, cx);
                })
                .ok();
            }
        });
        self.loading.insert(key, task);
    }

    /// The file `e` and the movement keys act on: the one the diff shows
    /// ([`diff_cursor::shown_file`]), by its position in review order and its path. `None` until
    /// the diff shows a file.
    fn shown_file(&self, cx: &mut App) -> Option<(usize, String)> {
        diff_cursor::shown_file(self.diff.as_ref()?, cx)
    }

    /// The path GitHub knows the listed file at `path` by ([`ViewedMarks::github_path`]).
    fn github_path(&self, path: &str) -> Option<String> {
        let previous = self.files.entry(path)?.file.previous_path.as_deref();
        self.marks.github_path(path, previous)
    }

    /// Where `path`'s file starts in the diff, once it's there.
    fn location_in_diff(&self, path: &str, cx: &App) -> Option<Anchor> {
        let diff = self.diff.as_ref()?;
        let position = self.files.review_position(path)?;
        let key = pr_diff::path_key(position, RelPath::from_unix_str(path).ok()?.into_arc());
        let multi_buffer = diff.read(cx).rhs_editor().read(cx).buffer().clone();
        multi_buffer.read(cx).location_for_path(&key, cx)
    }

    /// What the diff says about the file it shows when GitHub says it changed since it was viewed:
    /// the file, and since when, from the revision this Mac recorded when it was marked viewed.
    pub fn changed_since_viewed(&self, cx: &mut App) -> Option<(String, String)> {
        let (_, path) = self.shown_file(cx)?;
        let github_path = self.github_path(&path)?;
        let none = Revisions::default();
        let note = changed_since_viewed(
            self.revisions.as_ref().unwrap_or(&none),
            self.marks.state(&github_path),
            self.viewed_at.get(&github_path).map(String::as_str),
        )?;
        Some((path, note))
    }

    /// The diff's cursor head, on its right side.
    fn diff_cursor(&self, cx: &App) -> Option<Anchor> {
        let diff = self.diff.as_ref()?.read(cx);
        Some(diff.rhs_editor().read(cx).selections.newest_anchor().head())
    }

    /// Files arrived in the diff. Until the cursor is moved, it stays at the diff's start, in the
    /// first file in review order whichever file arrived first. An empty editor's cursor sits at
    /// its end, which would otherwise leave it in the last file.
    fn park_cursor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.cursor_moved {
            return;
        }
        let (Some(diff), Some(head)) = (&self.diff, self.diff_cursor(cx)) else {
            return;
        };
        // Moved since it was parked, though the move's event hasn't arrived yet.
        if self.parked_cursor.is_some_and(|parked| parked != head) {
            return;
        }
        let rhs = diff.read(cx).rhs_editor().clone();
        self.parked_cursor = Some(rhs.update(cx, |editor, cx| {
            editor.change_selections(SelectionEffects::no_scroll(), window, cx, |selections| {
                selections.select_ranges([Point::zero()..Point::zero()])
            });
            editor.selections.newest_anchor().head()
        }));
    }

    /// The diff's cursor moved: the list selects the file the diff shows. Where the tab parked it
    /// isn't a move, but the first real move ends the parking, even one back to the same place.
    fn diff_cursor_moved(&mut self, cx: &mut Context<Self>) {
        if !self.cursor_moved {
            if self.diff_cursor(cx) == self.parked_cursor {
                return;
            }
            self.cursor_moved = true;
            self.parked_cursor = None;
        }
        self.select_shown_file(cx);
    }

    /// The list selects the file the diff shows, the one `e` acts on.
    fn select_shown_file(&mut self, cx: &mut Context<Self>) {
        let Some((_, path)) = self.shown_file(cx) else {
            return;
        };
        if self.files.selected().map(FileEntry::path) != Some(path.as_str()) {
            self.files.select_path(&path);
            self.scroll_to_selected();
            cx.notify();
        }
    }

    /// `]f` and `[f`: the next or previous file in review order, which is the diff's order,
    /// among the files in the diff. They stop at the ends.
    fn move_file(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some((current, _)) = self.shown_file(cx) else {
            return;
        };
        let positions: Vec<usize> = match forward {
            true => (current + 1..self.files.len()).collect(),
            false => (0..current).rev().collect(),
        };
        let target = positions
            .into_iter()
            .filter_map(|position| self.files.at_review_position(position))
            .map(|entry| entry.path().to_string())
            .find(|path| self.location_in_diff(path, cx).is_some());
        if let Some(path) = target {
            self.files.select_path(&path);
            self.selection_moved(window, cx);
        }
    }

    fn next_file(&mut self, _: &NextFile, window: &mut Window, cx: &mut Context<Self>) {
        self.move_file(true, window, cx);
    }

    fn previous_file(&mut self, _: &PreviousFile, window: &mut Window, cx: &mut Context<Self>) {
        self.move_file(false, window, cx);
    }

    /// `]c` and `[c`: the next or previous change, across files. The list follows the cursor.
    fn next_change(&mut self, _: &NextChange, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(diff) = &self.diff {
            diff_cursor::go_to_change(diff, Direction::Next, window, cx);
        }
    }

    fn previous_change(&mut self, _: &PreviousChange, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(diff) = &self.diff {
            diff_cursor::go_to_change(diff, Direction::Prev, window, cx);
        }
    }

    /// `e`: marks the current file viewed and moves to the next unviewed file, or clears a viewed
    /// file's mark. The new mark shows at once, and goes back to GitHub's if the write fails.
    fn toggle_viewed(&mut self, _: &ToggleViewed, window: &mut Window, cx: &mut Context<Self>) {
        let Some((_, path)) = self.shown_file(cx) else {
            self.toast(
                "The diff is still loading, so there's no file to mark viewed yet.".into(),
                cx,
            );
            return;
        };
        // GitHub's view didn't load, so neither would a write: the local copy shows, read-only.
        // One older than GitHub's newest head gets the newer push's refusal below.
        let why = match self.showing_copy {
            Some(OlderCopy::Unreachable) => Some(format!(
                "rozed can't reach GitHub, so it can't mark {path} viewed until it's back"
            )),
            Some(OlderCopy::NotLoaded) => Some(format!(
                "GitHub's view of this pull request didn't load, so rozed can't mark {path} \
                 viewed until it does"
            )),
            Some(OlderCopy::NewerNotCopied) | None => None,
        };
        if let Some(why) = why {
            self.toast(
                format!("{why}. Until then, this is the local copy, read-only."),
                cx,
            );
            return;
        }
        let Some((poller, pull)) = self.watched.clone() else {
            self.toast(
                "Sign in to mark files viewed: this pull request opened without an account.".into(),
                cx,
            );
            return;
        };
        // Every write would fail; marking ahead of GitHub would move past files whose marks then
        // go back.
        if self.poll.status.offline.is_some() {
            self.toast(
                format!(
                    "rozed can't reach GitHub, so it can't mark {path} viewed until it's back."
                ),
                cx,
            );
            return;
        }
        if self.poll.status.signed_out {
            let login = poller.read(cx).login().to_string();
            self.toast(
                format!(
                    "{login} is signed out, so rozed can't mark {path} viewed. Sign in again \
                     from Accounts… in the rozed menu."
                ),
                cx,
            );
            return;
        }
        let Some(node_id) = self.node_id.clone() else {
            self.toast(
                format!(
                    "Couldn't mark {path} viewed: GitHub hasn't sent this pull request's details \
                     yet. Try again in a moment."
                ),
                cx,
            );
            return;
        };
        let Some(github_path) = self.github_path(&path) else {
            self.file_toast(
                &path,
                format!(
                    "GitHub doesn't list {path} in this pull request, so rozed can't mark it \
                     viewed. GitHub and the local copy see this file's change differently."
                ),
                cx,
            );
            return;
        };
        // GitHub marks a file as of the pull request's latest head (`markFileAsViewed` takes no
        // commit), so a file is marked only while that's the head on screen. A push inside the
        // poll interval can still slip through.
        let marks = self.marks.state(&github_path) != Some(ViewedState::Viewed);
        if marks && self.newer_head().is_some() {
            self.newer_push_toast(&path, cx);
            return;
        }
        let toggle = self.marks.toggle(&github_path);
        self.files.set_viewed(&self.marks.shown());
        if toggle.mark {
            // Only to a file the diff shows; the others are still loading.
            let unviewed: Vec<String> = self
                .files
                .unviewed_after(&path)
                .map(|entry| entry.path().to_string())
                .collect();
            match unviewed
                .iter()
                .find(|next| self.location_in_diff(next, cx).is_some())
            {
                Some(next) => {
                    self.files.select_path(next);
                    self.selection_moved(window, cx);
                }
                None => {
                    if let Some(next) = unviewed.first() {
                        self.toast(
                            format!(
                                "{next}, the next unviewed file, is still loading, so the diff \
                                 stays on {path}."
                            ),
                            cx,
                        );
                    }
                }
            }
        }
        let (mark, github_path) = (toggle.mark, toggle.path.clone());
        let written = ViewedWrite {
            record: match (toggle.mark, &self.head) {
                (true, Some(head)) => RecordChange::Record(head.clone()),
                (true, None) => RecordChange::Keep,
                (false, _) => RecordChange::Forget,
            },
            toggle,
            pressed_at: SystemTime::now(),
            records: self.records.clone(),
            login: poller.read(cx).login().to_string(),
            pull: pull.clone(),
            workspace: self.workspace.clone(),
            toast: toast_id(cx.entity_id(), Some(&path)),
        };
        let write = poller.update(cx, |poller, cx| {
            poller.write(
                pull,
                move |client: GithubClient| async move {
                    if mark {
                        client.mark_file_as_viewed(&node_id, &github_path).await
                    } else {
                        client.unmark_file_as_viewed(&node_id, &github_path).await
                    }
                },
                cx,
            )
        });
        cx.spawn(async move |this, cx| {
            let result = write.await;
            this.update(cx, |this, cx| {
                this.viewed_written(&written, result.is_ok(), cx)
            })
            .ok();
            // The tab may have closed, which the write, this Mac's record and the toast outlive.
            written.finish(result, cx).await;
        })
        .detach();
        cx.notify();
    }

    /// A toggle's write finished: the file shows the new mark, or GitHub's again if the write
    /// failed, and on success the tab knows where this Mac records the file was viewed.
    fn viewed_written(&mut self, written: &ViewedWrite, succeeded: bool, cx: &mut Context<Self>) {
        self.marks
            .finish(&written.toggle, succeeded, SystemTime::now());
        self.files.set_viewed(&self.marks.shown());
        if succeeded {
            let path = written.toggle.path.clone();
            match &written.record {
                RecordChange::Record(head) => {
                    self.viewed_at.insert(path, head.clone());
                }
                RecordChange::Forget => {
                    self.viewed_at.remove(&path);
                }
                RecordChange::Keep => {}
            }
        }
        cx.notify();
    }

    /// Shows a toast about the tab, which replaces the tab's last one.
    fn toast(&self, message: String, cx: &mut Context<Self>) {
        let id = toast_id(cx.entity_id(), None);
        show_toast(&self.workspace, id, message, None, cx);
    }

    /// Shows a toast about `path`, which replaces only the last one about it.
    fn file_toast(&self, path: &str, message: String, cx: &mut Context<Self>) {
        let id = toast_id(cx.entity_id(), Some(path));
        show_toast(&self.workspace, id, message, None, cx);
    }

    /// Says `e` didn't mark `path` because GitHub has a newer push, and offers to load it.
    fn newer_push_toast(&self, path: &str, cx: &mut Context<Self>) {
        let (workspace, tab) = (self.workspace.clone(), cx.entity().downgrade());
        let load = move |window: &mut Window, cx: &mut App| {
            let (Some(workspace), Some(tab)) = (workspace.upgrade(), tab.upgrade()) else {
                return;
            };
            workspace.update(cx, |workspace, cx| reopen(workspace, &tab, window, cx));
        };
        show_toast(
            &self.workspace,
            toast_id(cx.entity_id(), None),
            format!(
                "There's a newer push to this pull request than the one on screen, so rozed \
                 didn't mark {path} viewed: GitHub would mark it as of that push. Load it to mark \
                 files viewed."
            ),
            Some(("Load the newer push", Box::new(load))),
            cx,
        );
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
        let Some(location) = self.location_in_diff(entry.path(), cx) else {
            return;
        };
        let rhs_editor = diff.read(cx).rhs_editor().clone();
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

    fn render_status(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let step = match &self.status {
            Status::Ready => None,
            Status::Loading(step) => Some((IconName::ArrowCircle, Color::Muted, step.clone())),
            Status::Offline(why) => Some((IconName::Warning, Color::Warning, why.clone())),
            Status::Failed(error) => Some((IconName::Warning, Color::Error, error.clone())),
        };
        let offline = matches!(self.status, Status::Offline(_));
        let github = self.diff_source == Some(DiffSource::GitHub);
        let whitespace = self.hide_whitespace;
        if step.is_none() && !github && !whitespace {
            return None;
        }
        Some(
            h_flex()
                .px_3()
                .py_1()
                .gap_2()
                .justify_between()
                .child(
                    h_flex()
                        .gap_2()
                        .children(step.map(|(icon, color, text)| {
                            h_flex()
                                .gap_2()
                                .child(Icon::new(icon).size(IconSize::Small).color(color))
                                .child(Label::new(text).size(LabelSize::Small).color(color))
                        }))
                        .when(offline, |this| this.children(self.render_actions(cx))),
                )
                .child(
                    h_flex()
                        .gap_3()
                        .when(whitespace, |this| {
                            this.child(
                                div()
                                    .id("whitespace-hidden")
                                    .child(
                                        Label::new("Whitespace changes hidden")
                                            .size(LabelSize::Small)
                                            .color(Color::Muted),
                                    )
                                    .tooltip(Tooltip::text(
                                        "Lines whose only change is to whitespace show as \
                                         unchanged, with the head's whitespace on both sides, \
                                         so text copied from the left side has the head's \
                                         whitespace too.",
                                    )),
                            )
                        })
                        .when(github, |this| {
                            this.child(
                                Label::new("GitHub's diff: changed lines only")
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                            )
                        }),
                )
                .into_any_element(),
        )
    }

    /// The note above the diff while the current file changed since it was viewed.
    fn render_changed_since_viewed(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (path, note) = self.changed_since_viewed(cx)?;
        let colors = cx.theme().colors();
        Some(
            h_flex()
                .id("changed-since-viewed")
                .w_full()
                .flex_shrink_0()
                .px_3()
                .py_1()
                .gap_2()
                .border_b_1()
                .border_color(colors.border_variant)
                .bg(colors.panel_background)
                .child(
                    Icon::new(IconName::FileDiff)
                        .size(IconSize::Small)
                        .color(Color::Warning),
                )
                .child(Label::new(path).size(LabelSize::Small))
                .child(
                    Label::new(note)
                        .size(LabelSize::Small)
                        .color(Color::Muted)
                        .truncate(),
                )
                .into_any_element(),
        )
    }

    /// GitHub's links for fixing why the tab stopped, and its button to try again or choose an
    /// account.
    fn render_actions(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let links = self.help_links.iter().enumerate().map(|(index, link)| {
            let url = link.url().to_string();
            Button::new(("help-link", index), link.label())
                .style(ButtonStyle::Filled)
                .on_click(move |_, _, cx| cx.open_url(&url))
                .into_any_element()
        });
        let action = self.action.map(|action| {
            Button::new("tab-action", action.label())
                .on_click(cx.listener(|this, _, window, cx| this.load_again(window, cx)))
                .into_any_element()
        });
        links.chain(action).collect()
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
                        .children(viewed_mark(row, entry.viewed)),
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

/// The toast that says why a viewed mark couldn't be set or cleared.
struct ViewedMarkToast;

/// The ID of a viewed-mark toast from the tab `tab`, about the file at `path` or about the tab.
/// Each file and each tab has its own, so one toast doesn't replace another's.
fn toast_id(tab: EntityId, path: Option<&str>) -> NotificationId {
    let id = match path {
        Some(path) => format!("{}:{path}", tab.as_u64()),
        None => tab.as_u64().to_string(),
    };
    NotificationId::composite::<ViewedMarkToast>(ElementId::Name(id.into()))
}

/// A toggle's write to GitHub, with what's needed once it finishes, whether or not its tab is
/// still open.
struct ViewedWrite {
    toggle: Toggle,
    /// What this Mac records about the file once GitHub took the write.
    record: RecordChange,
    /// When `e` was pressed, which the record keeps.
    pressed_at: SystemTime,
    records: Option<ViewedRecords>,
    login: String,
    pull: PullRef,
    workspace: WeakEntity<Workspace>,
    /// The toast that says why the write failed, the file's own.
    toast: NotificationId,
}

/// How a successful write changes this Mac's record of where a file was viewed.
enum RecordChange {
    /// Marked viewed with this head on screen.
    Record(String),
    /// Cleared.
    Forget,
    /// Marked, but the head on screen isn't known.
    Keep,
}

impl ViewedWrite {
    /// The write finished: on success this Mac records or forgets where the file was viewed; on
    /// failure a toast says why.
    async fn finish(self, result: Result<(), GithubError>, cx: &mut AsyncApp) {
        let Self {
            toggle,
            record,
            pressed_at,
            records,
            login,
            pull,
            workspace,
            toast,
        } = self;
        if let Err(error) = result {
            log::warn!("{pull}: {}: {error}", toggle.path);
            let message = write_failed(&toggle, &login, &error);
            cx.update(|cx| show_toast(&workspace, toast, message, None, cx));
            return;
        }
        let Some(records) = records else {
            return;
        };
        let path = &toggle.path;
        let written = match record {
            RecordChange::Record(head) => {
                records.record(&login, &pull, path, &head, pressed_at).await
            }
            RecordChange::Forget => records.forget(&login, &pull, path).await,
            RecordChange::Keep => Ok(()),
        };
        if let Err(error) = written {
            log::error!("{pull}: recording where {path} was viewed: {error:#}");
        }
    }
}

/// A toast's button: its label and what it does.
type ToastButton = (&'static str, Box<dyn Fn(&mut Window, &mut App)>);

/// Shows a toast in `workspace` about a viewed mark, with `button` if there's one.
fn show_toast(
    workspace: &WeakEntity<Workspace>,
    id: NotificationId,
    message: String,
    button: Option<ToastButton>,
    cx: &mut App,
) {
    #[cfg(test)]
    cx.default_global::<ToastLog>().0.push((
        id.clone(),
        message.clone(),
        button.as_ref().map(|(label, _)| label.to_string()),
    ));
    let mut toast = Toast::new(id, message);
    if let Some((label, on_click)) = button {
        toast = toast.on_click(label, on_click);
    }
    workspace
        .update(cx, |workspace, cx| workspace.show_toast(toast, cx))
        .ok();
}

/// The toasts the tabs showed, for tests: each one's ID, text and button.
#[cfg(test)]
#[derive(Default)]
struct ToastLog(Vec<(NotificationId, String, Option<String>)>);

#[cfg(test)]
impl gpui::Global for ToastLog {}

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
/// Every path GitHub lists for the pull request, when its details list them all: GitHub caps a
/// pull request's files, and leaves out what it couldn't show.
fn listed_paths(details: &PullRequestData) -> Option<HashSet<String>> {
    let files = details.files.as_ref()?;
    let nodes = files.nodes.as_ref()?;
    let listed: HashSet<String> = nodes
        .iter()
        .map(|node| Some(node.as_ref()?.path.clone()))
        .collect::<Option<_>>()?;
    (i64::try_from(nodes.len()).ok() == Some(files.total_count)).then_some(listed)
}

fn viewed_state(state: &FileViewedState) -> Option<ViewedState> {
    match state {
        FileViewedState::VIEWED => Some(ViewedState::Viewed),
        FileViewedState::UNVIEWED => Some(ViewedState::Unviewed),
        FileViewedState::DISMISSED => Some(ViewedState::Dismissed),
        FileViewedState::Other(_) => None,
    }
}

/// How the file list shows the viewed mark of the file at `row`.
fn viewed_mark(row: usize, viewed: Option<ViewedState>) -> Option<AnyElement> {
    match viewed? {
        ViewedState::Viewed => Some(
            Icon::new(IconName::Check)
                .size(IconSize::XSmall)
                .color(Color::Success)
                .into_any_element(),
        ),
        ViewedState::Dismissed => Some(
            div()
                .id(("pr-file-changed", row))
                .child(
                    Label::new("changed")
                        .size(LabelSize::XSmall)
                        .color(Color::Warning),
                )
                .tooltip(Tooltip::text(
                    "Changed since viewed: a push changed it after it was marked viewed",
                ))
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
    let waitable = is_waitable(&error);
    let text = if waitable {
        format!("{}. rozed keeps trying GitHub.", sentence(&error))
    } else {
        format!("{}.", sentence(&error))
    };
    this.update(cx, |this, cx| {
        this.help_links = help_links(&error);
        this.action = (!waitable).then(|| action_for(&error));
        this.show_saved_banner(false, cx);
        this.set_status(Status::Failed(text.into()), cx)
    })
    .ok();
}

/// How long an offline tab waits before checking GitHub the first time, doubling each time
/// after, up to [`OFFLINE_MAX_WAIT`].
const OFFLINE_FIRST_WAIT: Duration = Duration::from_secs(5);
const OFFLINE_MAX_WAIT: Duration = Duration::from_secs(5 * 60);

/// Waits until GitHub answers again, checking less often the longer it doesn't: `checks` counts
/// the checks so far, kept by the caller across loads. Each wait is up to a fifth shorter at
/// random, so tabs offline together don't all load at once. Returns false once the window's gone.
async fn wait_for_github(checks: &mut u32, cx: &mut AsyncWindowContext) -> bool {
    loop {
        *checks += 1;
        let wait = github::poll::backoff(
            *checks,
            OFFLINE_FIRST_WAIT,
            OFFLINE_MAX_WAIT,
            0.2,
            jitter_sample(),
        );
        cx.background_executor().timer(wait).await;
        let Ok(http) = cx.update(|_, cx| cx.http_client()) else {
            return false;
        };
        let executor = cx.background_executor().clone();
        if github_api::github_answers(http, executor).await {
            return true;
        }
    }
}

/// A number in `0..1` to jitter a wait with; the clock's nanoseconds will do.
fn jitter_sample() -> f64 {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    f64::from(nanos % 1_000_000) / 1_000_000.0
}

/// Loads `link`'s pull request into `this`: picks the account, reads the pull request, copies it
/// locally and shows its diff. What rozed `saved` about it shows until GitHub answers. When
/// several accounts can see it and none was chosen, it asks which to use only if it `may_ask`.
async fn load(
    this: WeakEntity<PrTab>,
    link: PrLink,
    trust: Trust,
    opened: Instant,
    saved: &Option<Saved>,
    may_ask: bool,
    cx: &mut AsyncWindowContext,
) -> anyhow::Result<()> {
    let layer = cx.update(|_, cx| github_layer::ready(cx))?.await;
    let pull = PullRef::new(&link.owner, &link.repo, i64::try_from(link.number)?);
    // What rozed saved shows at once, and offline.
    if let Some(saved) = saved {
        this.update(cx, |this, cx| this.show_saved(saved, cx))?;
    }

    // The account that can see the repository, or none for a public one. Checked again when an
    // account is added or signed in again while the tab waits for an answer.
    let acting = loop {
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
        // A recent row's account, if it can still see the repository.
        let preferred = this.read_with(cx, |this, _| this.preferred.clone())?;
        let preferred = preferred.and_then(|login| {
            found
                .as_ref()?
                .visible
                .iter()
                .find(|visible| visible.eq_ignore_ascii_case(&login))
                .cloned()
        });
        break match (preferred, account::decide(found.as_ref()), &found) {
            (Some(login), _, _) => Acting::Account(login),
            (None, AccountDecision::Open(acting), _) => acting,
            (None, AccountDecision::Ask(logins), Some(found)) if may_ask => {
                match ask_for_account(&this, &link, found, logins, cx).await? {
                    Some(login) => Acting::Account(login),
                    None => continue,
                }
            }
            (None, AccountDecision::Ask(_), Some(found)) => {
                return Err(AccountNeeded {
                    message: format!(
                        "Choose which account rozed uses for {}",
                        question_subject(found)
                    ),
                }
                .into());
            }
            (None, AccountDecision::Ask(_), None) => Acting::Anonymous,
            (None, AccountDecision::Refuse(refusal), _) => return Err(refusal.into()),
        };
    };
    log::info!(
        "{link}: opening {}",
        match &acting {
            Acting::Account(login) => format!("as {login}"),
            Acting::Anonymous => "without signing in".to_string(),
        }
    );
    this.update(cx, |this, cx| this.set_acting(acting.clone(), cx))?;

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
    // When GitHub gave the head it names, which the local copy then fetches.
    let answered_at = SystemTime::now();
    this.update(cx, |this, cx| {
        this.set_pull_request(&pull_request, answered_at, cx)
    })?;
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
    // GitHub's diff shows while the local copy loads, unless a diff, such as the copy shown
    // offline, is on screen until the new copy replaces it.
    this.update_in(cx, |this, window, cx| {
        if this.diff.is_none() {
            this.load_github_diff(reader, &pull_request, window, cx)
        }
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
        let records = layer
            .as_ref()
            .map(|layer| layer.read_with(cx, |layer, _| layer.viewed_records().clone()));
        this.update(cx, |this, cx| this.watch(poller, pull, records, cx))?;
        workspace.update(cx, |workspace, cx| sync_active(workspace, cx))?;
    }

    pr_diff::open(
        this.clone(),
        workspace,
        link,
        pull_request,
        answered_at,
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

/// Whether waiting may fix `error`, so the tab tries again by itself: GitHub couldn't be
/// reached or didn't answer in time, a server error or a rate limit, or a local copy's fetch
/// that failed the same ways. Not a refusal, a choice of account, or a pull request that isn't
/// there, which only the user can fix.
fn is_waitable(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        if let Some(error) = cause.downcast_ref::<GithubError>() {
            return error.is_waitable();
        }
        if let Some(error) = cause.downcast_ref::<repo_cache::Error>() {
            return error.is_retryable();
        }
        cause
            .downcast_ref::<Refusal>()
            .is_some_and(|refusal| refusal.waitable)
    })
}

/// GitHub's links for fixing `error`, such as authorizing single sign-on.
fn help_links(error: &anyhow::Error) -> Vec<HelpLink> {
    error
        .downcast_ref::<Refusal>()
        .map(|refusal| refusal.links.clone())
        .unwrap_or_default()
}

/// What the tab offers after `error`, which waiting won't fix.
fn action_for(error: &anyhow::Error) -> TabAction {
    if error.chain().any(|cause| cause.is::<AccountNeeded>()) {
        TabAction::ChooseAccount
    } else {
        TabAction::TryAgain
    }
}

/// `error`'s text as a sentence to go on with: without the full stop it may end with.
fn sentence(error: &anyhow::Error) -> String {
    format!("{error:#}")
        .trim_end()
        .trim_end_matches('.')
        .to_string()
}

/// Why the diff on screen is a local copy that isn't of GitHub's latest head.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OlderCopy {
    /// GitHub couldn't be reached.
    Unreachable,
    /// GitHub's view of the pull request didn't load.
    NotLoaded,
    /// GitHub gave a newer head, which couldn't be copied.
    NewerNotCopied,
}

/// What a tab that stopped loading offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabAction {
    /// Loading again, as reopening the link does.
    TryAgain,
    /// Asking which account to use, which rozed doesn't do by itself.
    ChooseAccount,
}

impl TabAction {
    fn label(self) -> &'static str {
        match self {
            Self::TryAgain => "Try again",
            Self::ChooseAccount => "Choose account…",
        }
    }
}

/// The tab needs the user to choose which account to use for `subject`: rozed's own load didn't
/// ask, or the user dismissed the picker.
#[derive(Debug)]
struct AccountNeeded {
    message: String,
}

impl std::fmt::Display for AccountNeeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AccountNeeded {}

/// Whether `error` says GitHub may answer if asked again soon: it couldn't be reached, or a
/// request got no answer in time (see [`GithubError::is_retryable`]).
fn is_retryable(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<GithubError>()
            .is_some_and(GithubError::is_retryable)
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
    let workspace = this.update(cx, |this, cx| {
        // `e` says why it won't mark the copy's files as they arrive, and their notes say what
        // changed since this Mac recorded them viewed.
        this.showing_copy = Some(if is_offline(&offline) {
            OlderCopy::Unreachable
        } else {
            OlderCopy::NotLoaded
        });
        this.read_saved_records(&saved, cx);
        this.workspace.clone()
    })?;
    let shown =
        pr_diff::open_offline(this.clone(), workspace, link.clone(), &saved, opened, cx).await;
    let shown = match shown {
        Ok(shown) => shown,
        Err(error) => {
            log::warn!("{link}: can't show the local copy offline: {error:#}");
            false
        }
    };
    this.update(cx, |this, cx| {
        if shown {
            this.showing_copy_because(&offline, cx);
        } else {
            this.showing_copy = None;
        }
    })?;
    if shown { Ok(()) } else { Err(offline) }
}

/// What a question about the account for `found`'s repository is for: its owner, or the
/// repository alone when the owner's account can't see it.
fn question_subject(found: &RepoAccount) -> String {
    match found.remembered.owner {
        Some(_) => format!("{}/{}", found.owner, found.name),
        None => found.owner.clone(),
    }
}

/// Asks which of `logins`, the accounts that can see `link`'s repository, to open it with
/// (design §3 "Which account"). The picker remembers the answer: for the repository's owner,
/// as GitHub names it now, so rozed asks once per owner; or for the repository alone, when the
/// owner's account can't see it. `None` when an account was added or signed in meanwhile, so
/// which accounts can see it should be checked again.
async fn ask_for_account(
    this: &WeakEntity<PrTab>,
    link: &PrLink,
    found: &RepoAccount,
    logins: Vec<String>,
    cx: &mut AsyncWindowContext,
) -> anyhow::Result<Option<String>> {
    log::info!(
        "{link}: several accounts can see it ({}); asking which to use",
        logins.join(", ")
    );
    let subject = question_subject(found);
    let workspace = this.update(cx, |this, cx| {
        this.waiting_for_account(&subject, false, cx);
        this.workspace.clone()
    })?;
    let window = cx.window_handle();
    let tab = this.clone();
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
        Ok(Ok(login)) => Ok(Some(login)),
        Ok(Err(NotChosen::AccountsChanged)) => {
            log::info!("{link}: the accounts changed, so checking which can see it again");
            Ok(None)
        }
        Ok(Err(NotChosen::NoneLeft { subject })) => anyhow::bail!(
            "No account that can see {subject} is signed in any more. Sign in from Accounts… in \
             the rozed menu, then try again"
        ),
        Ok(Err(NotChosen::Dismissed { subject })) => Err(AccountNeeded {
            message: format!("No account was chosen for {subject}"),
        }
        .into()),
        Err(_) => Err(AccountNeeded {
            message: format!("No account was chosen for {subject}"),
        }
        .into()),
    }
}

impl Focusable for PrTab {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PrTab {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let background = cx.theme().colors().editor_background;
        let diff: AnyElement = match (&self.diff, &self.status) {
            (_, Status::Failed(error)) => v_flex()
                .size_full()
                .p_4()
                .gap_2()
                .child(Label::new("rozed couldn't open this pull request").color(Color::Error))
                .child(Label::new(error.clone()).size(LabelSize::Small))
                .child(h_flex().gap_2().children(self.render_actions(cx)))
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
            .on_action(cx.listener(Self::switch_side))
            .on_action(cx.listener(Self::toggle_diff_layout))
            .on_action(cx.listener(Self::toggle_whitespace))
            .on_action(cx.listener(Self::expand_file))
            .on_action(cx.listener(Self::toggle_viewed))
            .on_action(cx.listener(Self::next_file))
            .on_action(cx.listener(Self::previous_file))
            .on_action(cx.listener(Self::next_change))
            .on_action(cx.listener(Self::previous_change))
            .size_full()
            .bg(background)
            .child(self.render_header(cx))
            .children(self.render_status(cx))
            .children(pr_banner::render(self.banners.read(cx).messages()))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .when(self.show_file_list, |this| {
                        this.child(self.render_file_list(cx))
                    })
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .children(self.render_changed_since_viewed(cx))
                            .child(div().flex_1().min_h_0().w_full().child(diff)),
                    )
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
    use repo_cache::{Change, ChangedFile, FileMode, FileVersion, LineCounts};

    use std::sync::Arc;

    use github::files::{FileStatus, PullFile};

    use crate::github_api::{Base, BaseRepo, Head, User};
    use crate::pr_diff::tests::fixtures::Entry;
    use crate::pr_diff::{DiffCursor, DiffPosition};
    use futures::FutureExt as _;

    use crate::test_support::{
        Answer, FakeGithub, FakePull, GitServer, anonymous, init, sign_in, sign_in_as, workspace,
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

    /// The file list's entries for `files`, none of them generated by `.gitattributes`.
    fn local_entries(files: &[ChangedFile]) -> Vec<FileEntry> {
        files
            .iter()
            .map(|file| FileEntry::from_local(file, None))
            .collect()
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

    /// Makes `tab` the active tab, as clicking it does.
    fn activate(workspace: &Entity<Workspace>, tab: &Entity<PrTab>, cx: &mut VisualTestContext) {
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.activate_item(tab, true, true, window, cx)
        });
        cx.run_until_parked();
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
        assert_eq!(message.as_ref(), "No account was chosen for acme.");
        assert_eq!(
            tab.read_with(cx, |tab, _| tab.action),
            Some(TabAction::ChooseAccount)
        );
        // It doesn't ask again by itself.
        cx.executor().advance_clock(Duration::from_secs(10 * 60));
        cx.run_until_parked();
        assert_eq!(picker(&workspace, cx), None);
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
    async fn closing_the_only_tab_waiting_on_the_picker_closes_it(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        two_accounts_see_acme(&github, &git, cx).await;
        let tab = open_loaded(&workspace, "github.com/acme/app/pull/1", cx);
        assert!(picker(&workspace, cx).is_some());

        close(&workspace, tab, cx);

        assert_eq!(picker(&workspace, cx), None);
    }

    /// With the picker listing `home` and `work`, `work` is removed, or signed out: it drops out,
    /// and the picker asks again with `home` alone, rather than open as an account that can't.
    async fn an_account_dropping_out_of_the_picker(signed_out: bool, cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        let accounts = two_accounts_see_acme(&github, &git, cx).await;
        let tab = open_loaded(&workspace, "github.com/acme/app/pull/1", cx);
        assert_eq!(
            picker(&workspace, cx),
            Some(("acme".to_string(), logins(&["home", "work"])))
        );
        if signed_out {
            accounts.update(cx, |accounts, cx| {
                accounts.mark_signed_out("work", github::accounts::SignedOut::Refused, cx)
            });
        } else {
            accounts
                .update(cx, |accounts, cx| accounts.remove("work", cx))
                .await
                .unwrap();
        }
        cx.run_until_parked();

        assert_eq!(
            picker(&workspace, cx),
            Some(("acme".to_string(), logins(&["home"])))
        );
        assert_eq!(acting(&tab, cx), None);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(acting(&tab, cx), Some(Acting::Account("home".into())));
        assert_eq!(
            accounts.read_with(cx, |accounts, _| accounts
                .remembered_account("acme")
                .unwrap()),
            Some("home".to_string())
        );
    }

    #[gpui::test]
    async fn an_account_removed_while_the_picker_lists_it_drops_out(cx: &mut TestAppContext) {
        an_account_dropping_out_of_the_picker(false, cx).await;
    }

    #[gpui::test]
    async fn an_account_signed_out_while_the_picker_lists_it_drops_out(cx: &mut TestAppContext) {
        an_account_dropping_out_of_the_picker(true, cx).await;
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
        activate(&workspace, &app, cx);
        load(&[&app, &secret], cx);

        // Both checks are done before the answer, and the picker lists the active tab's
        // accounts, acme/app's.
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
        // accounts, for itself alone, once it's the active tab, and isn't opened as work.
        assert_eq!(status(&app, cx), Status::Ready);
        assert_eq!(acting(&app, cx), Some(Acting::Account("work".into())));
        assert_eq!(picker(&workspace, cx), None);
        assert_eq!(
            status(&secret, cx),
            Status::Loading("Waiting to ask which account rozed uses for acme/secret…".into())
        );
        activate(&workspace, &secret, cx);
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
    async fn a_question_waits_until_its_tab_is_active(cx: &mut TestAppContext) {
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
        // beta is the active tab, and acme loads in the background, as a tab coming back online
        // does.
        load(&[&acme, &beta], cx);

        // The picker asks about the active tab's owner. acme's question waits, without the
        // picker over beta, and its tab doesn't fail.
        assert_eq!(
            picker(&workspace, cx)
                .map(|(subject, _)| subject)
                .as_deref(),
            Some("beta")
        );
        assert_eq!(
            status(&acme, cx),
            Status::Loading("Waiting to ask which account rozed uses for acme…".into())
        );
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(status(&beta, cx), Status::Ready);
        assert_eq!(picker(&workspace, cx), None);
        assert!(matches!(status(&acme, cx), Status::Loading(_)));

        // Once acme's tab is active, the picker asks about acme.
        activate(&workspace, &acme, cx);
        assert_eq!(
            picker(&workspace, cx)
                .map(|(subject, _)| subject)
                .as_deref(),
            Some("acme")
        );
        cx.simulate_keystrokes("j enter");
        cx.run_until_parked();
        assert_eq!(status(&acme, cx), Status::Ready);
        assert_eq!(acting(&acme, cx), Some(Acting::Account("work".into())));
        assert_eq!(acting(&beta, cx), Some(Acting::Account("home".into())));
    }

    #[gpui::test]
    async fn a_check_that_finishes_after_the_answer_uses_it(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        let accounts = two_accounts_see_acme(&github, &git, cx).await;
        let app = open_unloaded(&workspace, "github.com/acme/app/pull/1", cx);
        let api = open_unloaded(&workspace, "github.com/acme/api/pull/2", cx);
        activate(&workspace, &app, cx);
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
        details_based("Add b", "VIEWED", &pull.head, &pull.base_branch, &pull.base)
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
            |pull| details_based("Add b", "VIEWED", &newer, &pull.base_branch, &pull.base),
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
            |pull| details_based("Add b", "VIEWED", &pull.head, &pull.base_branch, &moved_on),
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
                details_based(
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
    async fn offline_the_copy_shows_though_its_account_was_signed_out(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        playground_copied_then_offline(&workspace, &github, &git, saved_as_given, cx).await;
        let accounts = cx.update(|_, cx| {
            let layer = github_layer::loaded(cx).unwrap();
            layer.read(cx).accounts().clone()
        });
        accounts.update(cx, |accounts, cx| {
            accounts.mark_signed_out("rozed-bot", github::accounts::SignedOut::Refused, cx)
        });

        // With no account signed in, it asks GitHub without one, and gets no answer.
        let tab = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);

        assert!(
            matches!(status(&tab, cx), Status::Offline(_)),
            "{:?}",
            status(&tab, cx)
        );
        tab.read_with(cx, |tab, _| assert!(tab.diff().is_some()));
    }

    #[gpui::test]
    async fn a_slow_answer_shows_the_copy_and_keeps_trying_without_saying_offline(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        playground_copied_then_offline(&workspace, &github, &git, saved_as_given, cx).await;
        // GitHub can be reached, but nothing answers in time, as on a connection that died while
        // the Mac slept.
        github.set_offline(false);
        github.set_timing_out(true);

        let tab = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);

        let Status::Offline(message) = status(&tab, cx) else {
            panic!("expected the local copy: {:?}", status(&tab, cx));
        };
        assert!(message.starts_with("GitHub didn't answer: "), "{message}");
        assert!(message.contains("operation timed out"), "{message}");
        assert!(message.ends_with("rozed keeps trying."), "{message}");
        tab.read_with(cx, |tab, _| assert!(tab.diff().is_some()));

        // Once it answers in time, the tab loads by itself.
        github.set_timing_out(false);
        git.restore("rozed-dev/playground");
        cx.executor().advance_clock(Duration::from_secs(5));
        cx.run_until_parked();
        assert_eq!(status(&tab, cx), Status::Ready);
    }

    #[gpui::test]
    async fn one_accounts_failed_connection_is_waited_for_rather_than_opening_as_another(
        cx: &mut TestAppContext,
    ) {
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
        // home's requests can't connect, and home might see the repository too.
        github.cut_off("gho_HOME");

        let tab = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);

        let Status::Failed(message) = status(&tab, cx) else {
            panic!("the tab opened: {:?}", status(&tab, cx));
        };
        assert!(
            message.starts_with("home's check got no answer"),
            "{message}"
        );
        assert!(message.ends_with("rozed keeps trying GitHub."), "{message}");
        assert_eq!(acting(&tab, cx), None);
        let requests = github.requests();
        assert!(
            !requests.iter().any(|request| request.contains("/pulls/")),
            "{requests:#?}"
        );

        // Once home gets an answer, which is that it can't see it, the tab opens as rozed-bot.
        github.reconnect("gho_HOME");
        cx.executor().advance_clock(Duration::from_secs(5));
        cx.run_until_parked();
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

    fn paths(tab: &Entity<PrTab>, cx: &mut VisualTestContext) -> Vec<String> {
        tab.read_with(cx, |tab, _| {
            tab.files()
                .rows()
                .map(|entry| entry.path().to_string())
                .collect()
        })
    }

    #[gpui::test]
    async fn offline_the_saved_base_is_used_though_the_copy_fetched_another_since(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        // The pull request's head sits on a stack's lower layer, stack-a, and merges into main.
        let (mut pull, lower) = git.stacked_pull_request(
            "rozed-dev/playground",
            2,
            &[("a.rs", "a\n")],
            &[("a.rs", "a\n"), ("lower.rs", "lower\n")],
            &[("a.rs", "a2\n"), ("lower.rs", "lower\n")],
        );
        pull.title = "Add b".into();
        github.add_pull(pull.clone());
        github.let_see("gho_BOT", "rozed-dev/playground");
        let (_, cache) = sign_in(&github, &[("rozed-bot", "gho_BOT")], cx).await;
        let link = "github.com/rozed-dev/playground/pull/2";

        // Copied against main, which shows the lower layer's file too.
        let tab = open_loaded(&workspace, link, cx);
        assert_eq!(paths(&tab, cx), ["a.rs", "lower.rs"]);
        close(&workspace, tab, cx);
        // Then against stack-a, after a retarget, which doesn't.
        github.retarget("rozed-dev/playground", 2, "stack-a", &lower);
        let tab = open_loaded(&workspace, link, cx);
        assert_eq!(paths(&tab, cx), ["a.rs"]);
        close(&workspace, tab, cx);

        // What rozed saved says main, at the tip the copy fetched first.
        let two_hours_ago = SystemTime::now() - Duration::from_secs(2 * 3_600 + 30);
        cache
            .put(
                "rozed-bot",
                &Target::Pull(PullRef::new("rozed-dev", "playground", 2)),
                &details_based("Add b", "VIEWED", &pull.head, "main", &pull.base),
                None,
                two_hours_ago,
            )
            .await
            .unwrap();
        github.set_offline(true);
        git.hide("rozed-dev/playground");

        let tab = open_loaded(&workspace, link, cx);

        assert!(matches!(status(&tab, cx), Status::Offline(_)));
        assert_eq!(paths(&tab, cx), ["a.rs", "lower.rs"]);
    }

    #[gpui::test]
    async fn when_github_answers_but_loading_fails_the_copy_stays_and_it_keeps_trying(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        playground_copied_then_offline(&workspace, &github, &git, saved_as_given, cx).await;
        let tab = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);
        assert!(matches!(status(&tab, cx), Status::Offline(_)));

        // GitHub answers again, but with an error for the repository.
        github.set_offline(false);
        github.answer(
            "gho_BOT",
            "/repos/rozed-dev/playground",
            Answer::new(502, "Bad gateway"),
        );
        cx.executor().advance_clock(Duration::from_secs(5));
        cx.run_until_parked();

        let Status::Offline(message) = status(&tab, cx) else {
            panic!("expected the local copy: {:?}", status(&tab, cx));
        };
        assert!(
            message.starts_with("rozed couldn't load the pull request from GitHub: "),
            "{message}"
        );
        assert!(message.contains("502"), "{message}");
        tab.read_with(cx, |tab, _| assert!(tab.diff().is_some()));
        assert_eq!(old_texts(&tab, cx), ["a\n", "b\n"]);

        // It keeps checking, and the waits keep growing: not again within 5 seconds.
        github.requests();
        cx.executor().advance_clock(Duration::from_secs(5));
        cx.run_until_parked();
        assert_eq!(github.requests(), Vec::<String>::new());

        // Once it can, it loads.
        github.forget_answer("gho_BOT", "/repos/rozed-dev/playground");
        git.restore("rozed-dev/playground");
        cx.executor().advance_clock(Duration::from_secs(5));
        cx.run_until_parked();
        assert_eq!(status(&tab, cx), Status::Ready);
    }

    #[gpui::test]
    async fn a_proxy_that_starts_refusing_is_reported_rather_than_waited_out(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        playground_copied_then_offline(&workspace, &github, &git, saved_as_given, cx).await;
        let tab = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);
        assert!(matches!(status(&tab, cx), Status::Offline(_)));

        // The network's back, behind a proxy that wants a password: waiting won't fix that, so
        // the next check loads, and the tab says why it still shows the copy.
        github.set_offline(false);
        github.set_proxy_refusing(true);
        cx.executor().advance_clock(Duration::from_secs(5));
        cx.run_until_parked();

        let Status::Offline(message) = status(&tab, cx) else {
            panic!("expected the local copy: {:?}", status(&tab, cx));
        };
        assert!(
            message.starts_with("rozed couldn't load the pull request from GitHub: "),
            "{message}"
        );
        assert!(
            message.contains("proxy authentication required"),
            "{message}"
        );
        tab.read_with(cx, |tab, _| assert!(tab.diff().is_some()));
    }

    #[gpui::test]
    async fn a_tab_opening_while_the_picker_asks_doesnt_take_its_keys(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        two_accounts_see_acme(&github, &git, cx).await;
        let app = open_loaded(&workspace, "github.com/acme/app/pull/1", cx);
        assert!(picker(&workspace, cx).is_some());

        // A link opens another tab, which becomes the active one.
        open_unloaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);

        let focused = workspace.read_with(cx, |workspace, cx| {
            let picker = workspace
                .active_modal::<account_picker::AccountPicker>(cx)
                .unwrap();
            picker.read(cx).focus_handle(cx)
        });
        assert!(cx.update(|window, cx| focused.contains_focused(window, cx)));
        cx.simulate_keystrokes("j enter");
        cx.run_until_parked();
        assert_eq!(acting(&app, cx), Some(Acting::Account("work".into())));
    }

    #[gpui::test]
    async fn the_picker_set_aside_for_another_dialog_asks_again_once_it_closes(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        two_accounts_see_acme(&github, &git, cx).await;
        let tab = open_loaded(&workspace, "github.com/acme/app/pull/1", cx);
        assert!(picker(&workspace, cx).is_some());

        // As Accounts… does before it opens its dialog.
        workspace.update_in(cx, |workspace, window, cx| {
            assert!(account_picker::set_aside(workspace, cx));
            crate::OpenBox::toggle(workspace, window, cx);
        });
        cx.run_until_parked();
        assert_eq!(picker(&workspace, cx), None);
        assert!(workspace.read_with(cx, |workspace, cx| {
            workspace.active_modal::<crate::OpenBox>(cx).is_some()
        }));
        assert_eq!(
            status(&tab, cx),
            Status::Loading("Waiting to ask which account rozed uses for acme…".into())
        );

        cx.simulate_keystrokes("escape");
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        assert_eq!(
            picker(&workspace, cx),
            Some(("acme".to_string(), logins(&["home", "work"])))
        );
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(acting(&tab, cx), Some(Acting::Account("home".into())));
    }

    #[gpui::test]
    async fn the_check_that_github_answers_gives_up_after_a_while(cx: &mut TestAppContext) {
        let github = FakeGithub::default();
        github.hold("/rate_limit");
        let http: std::sync::Arc<dyn http_client::HttpClient> = std::sync::Arc::new(github.clone());
        let answered = cx
            .executor()
            .spawn(github_api::github_answers(http, cx.executor()));
        cx.run_until_parked();

        cx.executor()
            .advance_clock(github_api::ANSWER_TIMEOUT + Duration::from_secs(1));
        cx.run_until_parked();

        assert!(!answered.await);
    }

    fn two_hours_ago() -> SystemTime {
        SystemTime::now() - Duration::from_secs(2 * 3_600 + 30)
    }

    fn action(tab: &Entity<PrTab>, cx: &mut VisualTestContext) -> Option<TabAction> {
        tab.read_with(cx, |tab, _| tab.action)
    }

    #[gpui::test]
    async fn a_load_rozed_starts_never_asks_which_account_and_escape_sticks(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        let pull = git.pull_request("acme/app", 1, &[("a.rs", "a\n")], &[("a.rs", "a2\n")]);
        github.add_pull(pull.clone());
        for token in ["gho_HOME", "gho_WORK"] {
            github.let_see(token, "acme/app");
        }
        // Copied once while only home was signed in, so it opened without asking.
        let (accounts, cache) = sign_in(&github, &[("home", "gho_HOME")], cx).await;
        let link = "github.com/acme/app/pull/1";
        let tab = open_loaded(&workspace, link, cx);
        assert_eq!(status(&tab, cx), Status::Ready);
        close(&workspace, tab, cx);
        let target = Target::Pull(PullRef::new("acme", "app", 1));
        let mut saved = details_based("Add a", "VIEWED", &pull.head, &pull.base_branch, &pull.base);
        saved.url = "https://github.com/acme/app/pull/1".into();
        saved.number = 1;
        cache
            .put("home", &target, &saved, None, two_hours_ago())
            .await
            .unwrap();
        sign_in_as(&accounts, "work", "gho_WORK", cx).await;
        github.set_offline(true);
        git.hide("acme/app");
        let tab = open_loaded(&workspace, link, cx);
        assert!(matches!(status(&tab, cx), Status::Offline(_)));

        // Back online, rozed loads it again by itself, but doesn't put the picker up.
        github.set_offline(false);
        git.restore("acme/app");
        cx.executor().advance_clock(Duration::from_secs(5));
        cx.run_until_parked();
        assert_eq!(picker(&workspace, cx), None);
        assert_eq!(
            status(&tab, cx),
            Status::Offline(
                "Choose which account rozed uses for acme. This is the local copy's diff, \
                 read-only."
                    .into()
            )
        );
        assert_eq!(action(&tab, cx), Some(TabAction::ChooseAccount));
        cx.executor().advance_clock(Duration::from_secs(10 * 60));
        cx.run_until_parked();
        assert_eq!(picker(&workspace, cx), None);

        // The button asks. Escape leaves the copy, and nothing asks again by itself.
        tab.update_in(cx, |tab, window, cx| tab.load_again(window, cx));
        cx.run_until_parked();
        assert_eq!(
            picker(&workspace, cx),
            Some(("acme".to_string(), logins(&["home", "work"])))
        );
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert_eq!(
            status(&tab, cx),
            Status::Offline(
                "No account was chosen for acme. This is the local copy's diff, read-only.".into()
            )
        );
        cx.executor().advance_clock(Duration::from_secs(10 * 60));
        cx.run_until_parked();
        assert_eq!(picker(&workspace, cx), None);
        tab.read_with(cx, |tab, _| assert!(tab.diff().is_some()));
    }

    #[gpui::test]
    async fn a_refusal_while_the_copy_shows_gives_githubs_link_and_waits_for_the_user(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        playground_copied_then_offline(&workspace, &github, &git, saved_as_given, cx).await;
        let tab = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);
        assert!(matches!(status(&tab, cx), Status::Offline(_)));

        // Back online, the organization wants single sign-on, which only the user can give.
        github.set_offline(false);
        git.restore("rozed-dev/playground");
        github.answer("gho_BOT", "/repos/rozed-dev/playground", needs_sso());
        cx.executor().advance_clock(Duration::from_secs(5));
        cx.run_until_parked();

        let Status::Offline(message) = status(&tab, cx) else {
            panic!("expected the local copy: {:?}", status(&tab, cx));
        };
        assert!(
            message.starts_with("rozed couldn't load the pull request from GitHub: "),
            "{message}"
        );
        assert!(
            message.ends_with("This is the local copy's diff, read-only."),
            "{message}"
        );
        assert!(!message.contains(".."), "{message}");
        assert_eq!(
            help_links(&tab, cx),
            ["https://github.com/orgs/acme/sso?authorization_request=A1"]
        );
        assert_eq!(action(&tab, cx), Some(TabAction::TryAgain));
        // Nothing's tried again by itself.
        github.requests();
        cx.executor().advance_clock(Duration::from_secs(10 * 60));
        cx.run_until_parked();
        assert_eq!(github.requests(), Vec::<String>::new());

        // Once authorized, the button loads it.
        github.forget_answer("gho_BOT", "/repos/rozed-dev/playground");
        tab.update_in(cx, |tab, window, cx| tab.load_again(window, cx));
        cx.run_until_parked();
        assert_eq!(status(&tab, cx), Status::Ready);
    }

    #[gpui::test]
    async fn a_refusal_waiting_wont_fix_fails_a_fresh_tab_with_a_button_to_try_again(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        playground_copied_then_offline(&workspace, &github, &git, saved_as_given, cx).await;
        // A proxy that wants a password, or a captive portal's certificate: not offline.
        github.set_offline(false);
        github.set_proxy_refusing(true);

        let tab = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);

        let Status::Failed(message) = status(&tab, cx) else {
            panic!("expected the tab to fail: {:?}", status(&tab, cx));
        };
        assert!(
            message.contains("proxy authentication required"),
            "{message}"
        );
        assert!(!message.contains("keeps trying"), "{message}");
        assert_eq!(action(&tab, cx), Some(TabAction::TryAgain));
        github.requests();
        cx.executor().advance_clock(Duration::from_secs(10 * 60));
        cx.run_until_parked();
        assert_eq!(github.requests(), Vec::<String>::new());

        github.set_proxy_refusing(false);
        git.restore("rozed-dev/playground");
        tab.update_in(cx, |tab, window, cx| tab.load_again(window, cx));
        cx.run_until_parked();
        assert_eq!(status(&tab, cx), Status::Ready);
    }

    /// A dialog standing in for Accounts…, which lives in the app.
    struct AccountsDialog {
        focus_handle: FocusHandle,
    }

    impl EventEmitter<gpui::DismissEvent> for AccountsDialog {}

    impl workspace::ModalView for AccountsDialog {}

    impl Focusable for AccountsDialog {
        fn focus_handle(&self, _: &App) -> FocusHandle {
            self.focus_handle.clone()
        }
    }

    impl Render for AccountsDialog {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().track_focus(&self.focus_handle).child("Accounts")
        }
    }

    #[gpui::test]
    async fn an_account_signed_in_again_in_accounts_is_offered_again(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        let accounts = two_accounts_see_acme(&github, &git, cx).await;
        let tab = open_loaded(&workspace, "github.com/acme/app/pull/1", cx);
        accounts.update(cx, |accounts, cx| {
            accounts.mark_signed_out("work", github::accounts::SignedOut::Refused, cx)
        });
        cx.run_until_parked();
        assert_eq!(
            picker(&workspace, cx),
            Some(("acme".to_string(), logins(&["home"])))
        );

        // Accounts… sets the picker aside and opens its dialog, where work signs in again.
        workspace.update_in(cx, |workspace, window, cx| {
            assert!(account_picker::set_aside(workspace, cx));
            workspace.toggle_modal(window, cx, |_, cx| AccountsDialog {
                focus_handle: cx.focus_handle(),
            });
        });
        sign_in_as(&accounts, "work", "gho_WORK", cx).await;
        assert_eq!(picker(&workspace, cx), None);
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.hide_modal(window, cx);
        });
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();

        assert_eq!(
            picker(&workspace, cx),
            Some(("acme".to_string(), logins(&["home", "work"])))
        );
        cx.simulate_keystrokes("j enter");
        cx.run_until_parked();
        assert_eq!(acting(&tab, cx), Some(Acting::Account("work".into())));
    }

    #[gpui::test]
    async fn a_tab_beside_an_empty_pane_is_asked_about(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        two_accounts_see_acme(&github, &git, cx).await;
        let app = open_unloaded(&workspace, "github.com/acme/app/pull/1", cx);
        workspace.update_in(cx, |workspace, window, cx| {
            let pane = workspace.active_pane().clone();
            workspace.split_pane(pane, workspace::SplitDirection::Right, window, cx);
        });
        cx.run_until_parked();

        load(&[&app], cx);

        assert_eq!(
            picker(&workspace, cx),
            Some(("acme".to_string(), logins(&["home", "work"])))
        );
    }

    #[gpui::test]
    async fn a_rows_account_is_only_for_the_load_it_started(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        let accounts = two_accounts_see_acme(&github, &git, cx).await;
        accounts
            .update(cx, |accounts, cx| {
                accounts.remember_account_for_owner("acme", "work", cx)
            })
            .await
            .unwrap();
        // A row opened as home while offline.
        github.set_offline(true);
        let tab = workspace.update_in(cx, |workspace, window, cx| {
            open_as(
                workspace,
                link("github.com/acme/app/pull/1"),
                Trust::Untrusted,
                "home".into(),
                window,
                cx,
            )
        });
        cx.run_until_parked();
        assert!(matches!(status(&tab, cx), Status::Failed(_)));

        // rozed's own load once GitHub answers uses acme's account.
        github.set_offline(false);
        cx.executor().advance_clock(Duration::from_secs(5));
        cx.run_until_parked();
        assert_eq!(status(&tab, cx), Status::Ready);
        assert_eq!(acting(&tab, cx), Some(Acting::Account("work".into())));
    }

    #[gpui::test]
    async fn a_failed_load_keeps_the_copys_viewed_marks(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        let pull = git.pull_request(
            "rozed-dev/playground",
            2,
            &[("a.rs", "a\n"), ("b.rs", "b\n")],
            &[("a.rs", "a2\n"), ("b.rs", "b2\n")],
        );
        github.add_pull(pull.clone());
        github.let_see("gho_BOT", "rozed-dev/playground");
        let (_, cache) = sign_in(
            &github,
            &[("other", "gho_OTHER"), ("rozed-bot", "gho_BOT")],
            cx,
        )
        .await;
        let link = "github.com/rozed-dev/playground/pull/2";
        let tab = open_loaded(&workspace, link, cx);
        assert_eq!(status(&tab, cx), Status::Ready);
        close(&workspace, tab, cx);
        // What rozed saved last is another account's, with its viewed marks.
        let target = Target::Pull(PullRef::new("rozed-dev", "playground", 2));
        cache
            .put(
                "other",
                &target,
                &saved_as_given(&pull),
                None,
                two_hours_ago(),
            )
            .await
            .unwrap();
        github.set_offline(true);
        git.hide("rozed-dev/playground");
        let tab = open_loaded(&workspace, link, cx);
        let marks = |cx: &mut VisualTestContext| {
            tab.read_with(cx, |tab, _| {
                tab.files()
                    .rows()
                    .map(|entry| entry.viewed)
                    .collect::<Vec<_>>()
            })
        };
        let viewed = vec![Some(ViewedState::Viewed), Some(ViewedState::Unviewed)];
        assert_eq!(marks(cx), viewed);

        // Back online, rozed-bot's request for the pull request fails, after the account is
        // chosen: the copy keeps the marks it shows.
        github.set_offline(false);
        github.answer(
            "gho_BOT",
            "/repos/rozed-dev/playground/pulls/2",
            Answer::new(502, "Bad gateway"),
        );
        cx.executor().advance_clock(Duration::from_secs(5));
        cx.run_until_parked();
        assert!(matches!(status(&tab, cx), Status::Offline(_)));
        assert_eq!(acting(&tab, cx), Some(Acting::Account("rozed-bot".into())));
        assert_eq!(marks(cx), viewed);
    }

    #[gpui::test]
    async fn a_rows_tab_shows_its_viewed_marks_and_offline_e_says_why_it_cant_mark(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        let pull = git.pull_request(
            "rozed-dev/playground",
            2,
            &[("a.rs", "a\n"), ("b.rs", "b\n")],
            &[("a.rs", "a2\n"), ("b.rs", "b2\n")],
        );
        github.add_pull(pull.clone());
        github.let_see("gho_BOT", "rozed-dev/playground");
        let (_, cache) = sign_in(&github, &[("rozed-bot", "gho_BOT")], cx).await;
        // What rozed saved for the account, with its viewed marks.
        let target = Target::Pull(PullRef::new("rozed-dev", "playground", 2));
        cache
            .put(
                "rozed-bot",
                &target,
                &saved_as_given(&pull),
                None,
                two_hours_ago(),
            )
            .await
            .unwrap();
        let open_row = |cx: &mut VisualTestContext| {
            let tab = workspace.update_in(cx, |workspace, window, cx| {
                open_as(
                    workspace,
                    link("github.com/rozed-dev/playground/pull/2"),
                    Trust::Untrusted,
                    "rozed-bot".into(),
                    window,
                    cx,
                )
            });
            cx.run_until_parked();
            tab
        };
        let viewed = [
            mark("a.rs", Some(ViewedState::Viewed)),
            mark("b.rs", Some(ViewedState::Unviewed)),
        ];

        // Opened from the recent list, the account's viewed marks show on its files.
        let tab = open_row(cx);
        assert_eq!(status(&tab, cx), Status::Ready);
        assert_eq!(marks(&tab, cx), viewed);
        close(&workspace, tab, cx);
        // Where this Mac recorded a.rs viewed.
        let records = cx.update(|_, cx| {
            let layer = github_layer::loaded(cx).unwrap();
            layer.read(cx).viewed_records().clone()
        });
        let pull_ref = PullRef::new("rozed-dev", "playground", 2);
        records
            .record(
                "rozed-bot",
                &pull_ref,
                "a.rs",
                &pull.head,
                SystemTime::now(),
            )
            .await
            .unwrap();

        // Offline, the local copy shows with the marks rozed saved, and `e` marks nothing and
        // says why.
        github.set_offline(true);
        let tab = open_row(cx);
        assert!(matches!(status(&tab, cx), Status::Offline(_)));
        assert_eq!(marks(&tab, cx), viewed);
        assert_eq!(
            tab.read_with(cx, |tab, _| tab.viewed_at.get("a.rs").cloned()),
            Some(pull.head.clone())
        );
        tab.update_in(cx, |tab, window, cx| tab.focus_diff(window, cx));
        cx.simulate_keystrokes("e");
        assert_eq!(
            toasts(cx),
            [
                "rozed can't reach GitHub, so it can't mark a.rs viewed until it's back. Until \
                 then, this is the local copy, read-only."
            ]
        );
        assert_eq!(marks(&tab, cx), viewed);
    }

    #[gpui::test]
    async fn back_online_the_older_copy_stays_until_the_newer_push_is_copied(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        let repo = "rozed-dev/playground";
        let pull = git.pull_request(
            repo,
            2,
            &[("a.rs", "a\n"), ("b.rs", "b\n")],
            &[("a.rs", "a2\n"), ("b.rs", "b2\n")],
        );
        github.add_pull(pull.clone());
        github.let_see("gho_BOT", repo);
        let (_, cache) = sign_in(&github, &[("rozed-bot", "gho_BOT")], cx).await;
        let link = "github.com/rozed-dev/playground/pull/2";
        let tab = open_loaded(&workspace, link, cx);
        assert_eq!(status(&tab, cx), Status::Ready);
        close(&workspace, tab, cx);
        // What rozed saved at that head, with no file viewed yet.
        let target = Target::Pull(PullRef::new("rozed-dev", "playground", 2));
        let saved = details_based(
            "Add b",
            "UNVIEWED",
            &pull.head,
            &pull.base_branch,
            &pull.base,
        );
        cache
            .put("rozed-bot", &target, &saved, None, two_hours_ago())
            .await
            .unwrap();
        // A push the local copy hasn't fetched.
        let newer = git.push(repo, 2, &[("a.rs", "a3\n"), ("b.rs", "b3\n")]);

        // Offline, the copy of the saved head shows, and the reviewer reads b.rs.
        github.set_offline(true);
        git.hide(repo);
        let tab = open_loaded(&workspace, link, cx);
        assert!(matches!(status(&tab, cx), Status::Offline(_)));
        go_to(&tab, "b.rs", cx);
        let older_copy = tab.read_with(cx, |tab, _| tab.diff().cloned()).unwrap();
        let unviewed = [
            mark("a.rs", Some(ViewedState::Unviewed)),
            mark("b.rs", Some(ViewedState::Unviewed)),
        ];

        // Back online, GitHub names the newer push, but copying it fails: the older copy stays,
        // read-only, saying so, with a button to try again.
        github.set_head(repo, 2, &newer);
        github.set_offline(false);
        cx.executor().advance_clock(Duration::from_secs(5));
        cx.run_until_parked();
        let Status::Offline(text) = status(&tab, cx) else {
            panic!("{:?}", status(&tab, cx));
        };
        assert!(
            text.starts_with(&format!(
                "GitHub has {}, but rozed couldn't copy it: ",
                &newer[..7]
            )),
            "{text}"
        );
        assert!(
            text.ends_with(&format!(
                "This is the local copy of {}, read-only.",
                &pull.head[..7]
            )),
            "{text}"
        );
        assert!(!text.contains(".."), "{text}");
        assert_eq!(action(&tab, cx), Some(TabAction::TryAgain));
        assert_eq!(
            tab.read_with(cx, |tab, _| tab.diff().cloned()),
            Some(older_copy.clone())
        );
        assert!(tab.read_with(cx, |tab, _| tab.status().tries_again()));

        // GitHub would mark b.rs as of the newer push, which isn't on screen, so `e` doesn't.
        github.hold("/graphql");
        tab.update_in(cx, |tab, window, cx| tab.focus_diff(window, cx));
        cx.simulate_keystrokes("e");
        let toasts = toasts(cx);
        assert!(
            toasts
                .last()
                .is_some_and(|toast| toast.starts_with("There's a newer push")),
            "{toasts:#?}"
        );
        assert_eq!(marks(&tab, cx), unviewed);

        // Trying again copies the newer push, which replaces the older copy where the reviewer
        // was, and `e` marks b.rs.
        git.restore(repo);
        tab.update_in(cx, |tab, window, cx| tab.load_again(window, cx));
        cx.run_until_parked();
        assert_eq!(status(&tab, cx), Status::Ready);
        assert_ne!(
            tab.read_with(cx, |tab, _| tab.diff().cloned()),
            Some(older_copy)
        );
        assert_eq!(cursor_file(&tab, cx).as_deref(), Some("b.rs"));
        tab.update_in(cx, |tab, window, cx| tab.focus_diff(window, cx));
        cx.simulate_keystrokes("e");
        assert_eq!(marks(&tab, cx)[1], mark("b.rs", Some(ViewedState::Viewed)));
        github.let_go("/graphql");
    }

    #[gpui::test]
    async fn a_copy_stopped_behind_an_older_copy_keeps_it_and_says_so(cx: &mut TestAppContext) {
        let (_, tab, _signed_in, cx) = three_files("copy_stopped_older", &[], &[], cx).await;
        let mut newer = fd_2212();
        newer.head.sha = NEWER.into();
        let (retry, _asked) = oneshot::channel();
        tab.update(cx, |tab, cx| {
            tab.set_pull_request(&newer, SystemTime::now(), cx);
            tab.copy_stopped("the disk is full.", 5, retry, cx);
        });
        assert_eq!(
            status(&tab, cx),
            Status::Offline(
                "GitHub has 3333333, but rozed couldn't copy it: the disk is full. This is the \
                 local copy of 2222222, read-only."
                    .into()
            )
        );

        // The banner's button tries the copy again.
        tab.update(cx, |tab, cx| tab.retry_copy(cx));
        assert!(matches!(status(&tab, cx), Status::Loading(_)));
        assert_eq!(tab.read_with(cx, |tab, _| tab.showing_copy), None);
    }

    #[gpui::test]
    async fn after_github_listed_a_newer_head_its_older_copy_doesnt_mark_as_of_it(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx).await;
        let tab = open_unloaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);
        let signed_in = sign_in_bot(&tab, "e_after_the_switch", &[], cx).await;
        // A minute ago GitHub said the head was HEAD. It then listed the files at NEWER, and its
        // details say NEWER.
        let answered_at = SystemTime::now() - Duration::from_secs(60);
        tab.update(cx, |tab, cx| {
            tab.set_pull_request(&fd_2212(), answered_at, cx)
        });
        show_github_files(&tab, vec![github_file_at("a.rs", 3, NEWER)], cx);
        tab.update(cx, |tab, cx| {
            tab.set_details(&details_at(NEWER, &[("a.rs", "UNVIEWED")]), cx)
        });

        // The local copy of HEAD, which GitHub gave first, replaces GitHub's diff.
        let files = [("a.rs", lines(5, &[]), lines(5, &[3]))];
        let head: Oid = HEAD.parse().unwrap();
        let editor = test_diff_editor(&workspace, cx);
        let positions = tab.update(cx, |tab, cx| {
            tab.copied(
                local_entries(&[changed("a.rs", Change::Modified)]),
                &head,
                answered_at,
                cx,
            )
        });
        add_test_files(&editor, &files, &positions, cx).await;
        tab.update_in(cx, |tab, window, cx| {
            tab.switch_diff(editor.clone(), &head, window, cx)
        });
        cx.run_until_parked();
        assert_eq!(
            tab.read_with(cx, |tab, _| tab.head.clone()),
            Some(HEAD.to_string())
        );

        // GitHub would mark a.rs as of NEWER, which isn't on screen.
        tab.update_in(cx, |tab, window, cx| tab.focus_diff(window, cx));
        cx.simulate_keystrokes("e");
        assert_eq!(signed_in.github.mutations(), []);
        assert!(toasts(cx)[0].starts_with("There's a newer push"));
    }

    #[gpui::test]
    async fn a_tab_that_changes_account_stops_hearing_the_old_accounts_poller(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx).await;
        let tab = open_unloaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);
        let bot = sign_in_bot(&tab, "changes_account", &[], cx).await;
        let head: Oid = HEAD.parse().unwrap();
        tab.update(cx, |tab, cx| {
            tab.copied(
                local_entries(&[changed("a.rs", Change::Modified)]),
                &head,
                SystemTime::now(),
                cx,
            );
            tab.set_details(&details_with(&[("a.rs", "UNVIEWED")]), cx);
            tab.set_active(true, cx);
        });
        // Another account's saved marks aren't written into the bot's records.
        tab.update(cx, |tab, cx| {
            let saved = Saved {
                login: "other".into(),
                details: details_with(&[("a.rs", "VIEWED")]),
                fetched_at: SystemTime::now(),
            };
            tab.show_saved(&saved, cx);
        });
        cx.run_until_parked();
        assert_eq!(bot.recorded(), HashMap::new());
        tab.update(cx, |tab, cx| {
            tab.set_details(&details_with(&[("a.rs", "VIEWED")]), cx)
        });
        let (bots, pull) = (bot._poller.clone(), bot.pull.clone());
        let open = |cx: &mut VisualTestContext| bots.read_with(cx, |bots, _| bots.open().cloned());
        assert_eq!(open(cx), Some(pull.clone()));
        assert_eq!(marks(&tab, cx), [mark("a.rs", Some(ViewedState::Viewed))]);

        // The tab loads again as another account, which watches with its own poller.
        let other = cx.new(|cx| {
            Poller::new(
                bot.accounts.clone(),
                "other",
                bot.cache.clone(),
                github::poll::PollConfig::default(),
                cx,
            )
        });
        tab.update(cx, |tab, cx| {
            tab.set_acting(Acting::Account("other".into()), cx);
            tab.watch(other.clone(), pull.clone(), Some(bot.records.clone()), cx);
        });
        cx.run_until_parked();

        // The bot's poller stops watching it, and the bot's marks go.
        assert_eq!(open(cx), None);
        assert_eq!(marks(&tab, cx), [mark("a.rs", None)]);
        // What the bot's poller says doesn't reach the tab.
        bots.update(cx, |_, cx| {
            cx.emit(PollEvent::Details {
                pull: pull.clone(),
                details: std::sync::Arc::new(details_with(&[("a.rs", "VIEWED")])),
            })
        });
        cx.run_until_parked();
        assert_eq!(marks(&tab, cx), [mark("a.rs", None)]);
    }

    #[test]
    fn only_what_waiting_may_fix_is_tried_again_by_itself() {
        let network = |error: anyhow::Error| anyhow::Error::new(GithubError::Network(error));
        // A local copy's fetch that failed on the network.
        let fetch = anyhow::Error::new(repo_cache::Error::Fetch {
            what: "pull request #1".into(),
            failure: repo_cache::FetchFailure::Network,
            message: "Could not resolve host: github.com".into(),
        })
        .context("copying acme/app#1 locally");
        assert!(is_waitable(&fetch));
        // A proxy that couldn't open the tunnel to GitHub.
        let tunnel =
            network(anyhow::anyhow!("unsuccessful tunnel").context("client error (Connect)"));
        assert!(is_waitable(&tunnel));
        // A refusal for single sign-on, and a choice of account, are the user's to fix.
        let sso = Refusal {
            message: "sso".into(),
            links: Vec::new(),
            waitable: false,
        };
        assert!(!is_waitable(&sso.into()));
        let needed = anyhow::Error::new(AccountNeeded {
            message: "No account was chosen for acme".into(),
        });
        assert!(!is_waitable(&needed));
        assert_eq!(action_for(&needed), TabAction::ChooseAccount);
        let tls = network(anyhow::anyhow!("invalid peer certificate: UnknownIssuer"));
        assert!(!is_waitable(&tls));
        assert_eq!(action_for(&tls), TabAction::TryAgain);
        // A message that ends a sentence isn't given a second full stop.
        assert_eq!(
            sentence(&anyhow::anyhow!(
                "GitHub answered 502 for a/b#1: Bad gateway."
            )),
            "GitHub answered 502 for a/b#1: Bad gateway"
        );
    }

    /// The details an account's poller hands the tab, as GitHub's GraphQL API answers.
    fn details(title: &str, viewed: &str) -> PullRequestData {
        details_headed(title, viewed, "d712aa7652cc723fee5fd9b3eba867b0480d40da")
    }

    /// [`details`], with `head` as the pull request's head.
    fn details_headed(title: &str, viewed: &str, head: &str) -> PullRequestData {
        details_based(
            title,
            viewed,
            head,
            "stack-a",
            "b046244d9569964088e952243e5e557312322488",
        )
    }

    /// [`details`], with `head` as the pull request's head and `base` as its base branch's tip.
    fn details_based(
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

    /// [`details`] with these files and marks, at the head on screen in these tests.
    fn details_with(files: &[(&str, &str)]) -> PullRequestData {
        details_at(HEAD, files)
    }

    /// [`details`] with these files and marks, with GitHub's head at `head`.
    fn details_at(head: &str, files: &[(&str, &str)]) -> PullRequestData {
        let mut details = serde_json::to_value(details("Add b", "UNVIEWED")).unwrap();
        details["headRefOid"] = head.into();
        details["files"]["totalCount"] = files.len().into();
        details["files"]["nodes"] = files
            .iter()
            .map(|(path, state)| serde_json::json!({ "path": path, "viewerViewedState": state }))
            .collect();
        serde_json::from_value(details).unwrap()
    }

    /// How GitHub answers a viewed-mark mutation.
    #[derive(Clone, Copy, Debug)]
    enum WriteAnswer {
        Done,
        Forbidden,
        RateLimited,
        Offline,
    }

    /// GitHub as these tests need it: the viewed-mark mutations answer as each test says, and every
    /// other request fails as if GitHub were down, so the poller's polls change nothing and it
    /// doesn't think it's offline.
    #[derive(Default)]
    struct WriteGithub {
        answers: std::sync::Mutex<std::collections::VecDeque<WriteAnswer>>,
        /// Each mutation's operation and variables.
        mutations: std::sync::Mutex<Vec<(String, serde_json::Value)>>,
    }

    impl WriteGithub {
        fn answer(&self, answer: WriteAnswer) {
            self.answers.lock().unwrap().push_back(answer);
        }

        fn mutations(&self) -> Vec<(String, serde_json::Value)> {
            self.mutations.lock().unwrap().clone()
        }
    }

    impl http_client::HttpClient for WriteGithub {
        fn user_agent(&self) -> Option<&http_client::http::HeaderValue> {
            None
        }

        fn proxy(&self) -> Option<&http_client::Url> {
            None
        }

        fn send(
            &self,
            request: http_client::Request<http_client::AsyncBody>,
        ) -> futures::future::BoxFuture<
            'static,
            anyhow::Result<http_client::Response<http_client::AsyncBody>>,
        > {
            use futures::AsyncReadExt as _;
            let mut body = Vec::new();
            futures::executor::block_on(request.into_body().read_to_end(&mut body)).unwrap();
            let body: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
            let operation = body["operationName"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            if !operation.ends_with("FileAsViewed") {
                let unavailable = http_client::Response::builder()
                    .status(503)
                    .body(http_client::AsyncBody::from(b"{}".to_vec()))
                    .unwrap();
                return Box::pin(async { Ok(unavailable) });
            }
            self.mutations
                .lock()
                .unwrap()
                .push((operation.clone(), body["variables"].clone()));
            let answer = self.answers.lock().unwrap().pop_front().expect("an answer");
            let field = format!("{}{}", operation[..1].to_lowercase(), &operation[1..]);
            let field = field.as_str();
            let (remaining, json) = match answer {
                WriteAnswer::Offline => {
                    return Box::pin(async { Err(anyhow::anyhow!("the network is down")) });
                }
                WriteAnswer::Done => (
                    "4999",
                    serde_json::json!({ "data": { field: { "pullRequest": { "id": "PR_x" } } } }),
                ),
                WriteAnswer::Forbidden => (
                    "4999",
                    serde_json::json!({
                        "data": { field: null },
                        "errors": [{
                            "type": "FORBIDDEN",
                            "path": [field],
                            "message": "Resource not accessible by integration",
                        }],
                    }),
                ),
                WriteAnswer::RateLimited => (
                    "0",
                    serde_json::json!({
                        "errors": [{
                            "type": "RATE_LIMITED",
                            "message": "API rate limit exceeded for user ID 1.",
                        }],
                    }),
                ),
            };
            let response = http_client::Response::builder()
                .status(200)
                .header("content-type", "application/json")
                .header("x-ratelimit-remaining", remaining)
                .body(http_client::AsyncBody::from(
                    serde_json::to_vec(&json).unwrap(),
                ))
                .unwrap();
            Box::pin(async { Ok(response) })
        }
    }

    const BOT: &str = "rozed-bot";

    /// The tab signed in as the test bot, with its poller and the database where it records
    /// viewed files.
    struct SignedIn {
        github: std::sync::Arc<WriteGithub>,
        records: ViewedRecords,
        pull: PullRef,
        accounts: Entity<github::accounts::Accounts>,
        cache: github::cache::PrCache,
        /// The account's poller, which rozed's GitHub layer keeps after its tabs close.
        _poller: Entity<Poller>,
    }

    /// Signs `tab` in as the bot, with `recorded` already in its records of viewed files.
    async fn sign_in_bot(
        tab: &Entity<PrTab>,
        name: &str,
        recorded: &[(&str, &str)],
        cx: &mut VisualTestContext,
    ) -> SignedIn {
        let github = std::sync::Arc::new(WriteGithub::default());
        let db = github::db::RozedDb::open_test(name).await;
        let store = std::sync::Arc::new(
            credentials_provider::test_support::FakeCredentialsProvider::default(),
        );
        let accounts = cx
            .update(|_, cx| github::accounts::Accounts::load(db.clone(), store, github.clone(), cx))
            .unwrap();
        let user = github::AuthenticatedUser {
            user: github::User {
                login: BOT.into(),
                id: 353226052,
                name: None,
                avatar_url: None,
            },
            scopes: vec!["repo".into()],
        };
        accounts
            .update(cx, |accounts, cx| {
                accounts.add(
                    user,
                    github::Token::new("gho_FAKE"),
                    github::db::AccountSource::DeviceFlow,
                    cx,
                )
            })
            .await
            .unwrap();
        let records = ViewedRecords::new(db.clone());
        let pull = PullRef::new("rozed-dev", "playground", 2);
        for (path, head) in recorded {
            records
                .record(BOT, &pull, path, head, SystemTime::now())
                .await
                .unwrap();
        }
        let cache = github::cache::PrCache::new(db);
        let poller = cx.new(|cx| {
            Poller::new(
                accounts.clone(),
                BOT,
                cache.clone(),
                github::poll::PollConfig::default(),
                cx,
            )
        });
        tab.update(cx, |tab, cx| {
            tab.set_acting(Acting::Account(BOT.into()), cx);
            tab.watch(poller.clone(), pull.clone(), Some(records.clone()), cx);
        });
        cx.run_until_parked();
        SignedIn {
            _poller: poller,
            github,
            records,
            pull,
            accounts,
            cache,
        }
    }

    impl SignedIn {
        fn recorded(&self) -> HashMap<String, String> {
            self.records.heads(BOT, &self.pull).unwrap()
        }
    }

    fn marks(
        tab: &Entity<PrTab>,
        cx: &mut VisualTestContext,
    ) -> Vec<(String, Option<ViewedState>)> {
        tab.read_with(cx, |tab, _| {
            tab.files()
                .rows()
                .map(|entry| (entry.path().to_string(), entry.viewed))
                .collect()
        })
    }

    fn selected(tab: &Entity<PrTab>, cx: &mut VisualTestContext) -> Option<String> {
        tab.read_with(cx, |tab, _| {
            tab.files().selected().map(|entry| entry.path().to_string())
        })
    }

    fn mark(path: &str, state: Option<ViewedState>) -> (String, Option<ViewedState>) {
        (path.to_string(), state)
    }

    const HEAD: &str = "2222222222222222222222222222222222222222";

    /// A tab on the playground's PR #2 with three files in its diff, a.rs, b.rs and c.rs, each
    /// with one change, signed in as the bot, with GitHub's marks.
    async fn three_files<'a>(
        name: &str,
        recorded: &[(&str, &str)],
        github_marks: &[(&str, &str)],
        cx: &'a mut TestAppContext,
    ) -> (
        Entity<Workspace>,
        Entity<PrTab>,
        SignedIn,
        &'a mut VisualTestContext,
    ) {
        let files = [
            ("a.rs", lines(5, &[]), lines(5, &[3])),
            ("b.rs", lines(5, &[]), lines(5, &[3])),
            ("c.rs", lines(5, &[]), lines(5, &[3])),
        ];
        let (workspace, tab, signed_in, _, cx) =
            signed_in_diff(name, recorded, &files, github_marks, cx).await;
        (workspace, tab, signed_in, cx)
    }

    /// A tab on the playground's PR #2 showing a diff of `files`, signed in as the bot, with
    /// GitHub's marks.
    async fn signed_in_diff<'a>(
        name: &str,
        recorded: &[(&str, &str)],
        files: &[(&str, String, String)],
        github_marks: &[(&str, &str)],
        cx: &'a mut TestAppContext,
    ) -> (
        Entity<Workspace>,
        Entity<PrTab>,
        SignedIn,
        Entity<SplittableEditor>,
        &'a mut VisualTestContext,
    ) {
        let (workspace, cx) = workspace(cx).await;
        let tab = open_unloaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);
        let signed_in = sign_in_bot(&tab, name, recorded, cx).await;
        let editor = show_test_diff(&tab, &workspace, files, cx).await;
        tab.update(cx, |tab, cx| {
            tab.set_details(&details_with(github_marks), cx)
        });
        cx.run_until_parked();
        (workspace, tab, signed_in, editor, cx)
    }

    /// Whether the workspace shows the last viewed-mark toast a tab showed.
    fn has_viewed_toast(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> bool {
        let Some((id, _, _)) = toast_log(cx).pop() else {
            return false;
        };
        workspace.read_with(cx, |workspace, _| workspace.has_notification(&id))
    }

    /// Selects `path` in the list, which moves the diff there.
    fn go_to(tab: &Entity<PrTab>, path: &str, cx: &mut VisualTestContext) {
        tab.update_in(cx, |tab, window, cx| {
            tab.files.select_path(path);
            tab.selection_moved(window, cx);
        });
        cx.run_until_parked();
    }

    /// The toasts the tabs showed, oldest first: each one's ID, text and button.
    fn toast_log(cx: &mut VisualTestContext) -> Vec<(NotificationId, String, Option<String>)> {
        cx.update(|_, cx| {
            cx.try_global::<ToastLog>()
                .map(|log| log.0.clone())
                .unwrap_or_default()
        })
    }

    /// The text of the toasts the tabs showed, oldest first.
    fn toasts(cx: &mut VisualTestContext) -> Vec<String> {
        toast_log(cx).into_iter().map(|(_, text, _)| text).collect()
    }

    /// The file the diff's cursor is in.
    fn cursor_file(tab: &Entity<PrTab>, cx: &mut VisualTestContext) -> Option<String> {
        let diff = tab.read_with(cx, |tab, _| tab.diff().cloned())?;
        cx.update(|_, cx| crate::diff_cursor::file_at_cursor(&diff, cx))
            .map(|(_, path)| path)
    }

    #[gpui::test]
    async fn e_before_the_diff_shows_a_file_marks_nothing_and_says_why(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let tab = open_unloaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);
        let github = sign_in_bot(&tab, "e_before_the_diff", &[], cx).await;
        let head: Oid = HEAD.parse().unwrap();
        tab.update(cx, |tab, cx| {
            tab.copied(
                local_entries(&[changed("a.rs", Change::Modified)]),
                &head,
                SystemTime::now(),
                cx,
            );
            tab.set_details(&details_with(&[("a.rs", "UNVIEWED")]), cx);
        });

        // The list has the file, but the diff doesn't exist yet.
        cx.simulate_keystrokes("e");
        assert_eq!(
            toasts(cx),
            ["The diff is still loading, so there's no file to mark viewed yet."]
        );
        assert_eq!(marks(&tab, cx), [mark("a.rs", Some(ViewedState::Unviewed))]);
        assert_eq!(github.github.mutations(), []);

        // Nor once it exists, until a file is in it.
        let editor = test_diff_editor(&workspace, cx);
        tab.update_in(cx, |tab, window, cx| {
            tab.show_diff(editor.clone(), DiffSource::LocalCopy, window, cx)
        });
        cx.simulate_keystrokes("e");
        assert_eq!(toasts(cx).len(), 2);
        assert_eq!(github.github.mutations(), []);
    }

    #[gpui::test]
    async fn e_acts_on_the_file_at_the_top_of_the_view_when_the_cursor_is_out_of_it(
        cx: &mut TestAppContext,
    ) {
        let files = [
            ("a.rs", String::new(), lines(200, &[])),
            ("b.rs", String::new(), lines(200, &[])),
        ];
        let (_, tab, github, editor, cx) = signed_in_diff(
            "e_at_the_top",
            &[],
            &files,
            &[("a.rs", "UNVIEWED"), ("b.rs", "UNVIEWED")],
            cx,
        )
        .await;
        // Scrolling doesn't move the cursor, which stays at a.rs's start.
        let rhs = editor.read_with(cx, |editor, _| editor.rhs_editor().clone());
        rhs.update_in(cx, |editor, window, cx| {
            editor.set_scroll_position(gpui::point(0., 260.), window, cx);
        });
        cx.run_until_parked();
        assert_eq!(cursor(&tab, &editor, cx).0.as_deref(), Some("a.rs"));
        // The list follows the view.
        assert_eq!(selected(&tab, cx).as_deref(), Some("b.rs"));

        github.github.answer(WriteAnswer::Done);
        cx.simulate_keystrokes("e");
        assert_eq!(github.github.mutations()[0].1["path"], "b.rs");
    }

    #[gpui::test]
    async fn e_and_the_file_keys_move_only_to_files_in_the_diff(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let tab = open_unloaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);
        let github = sign_in_bot(&tab, "e_while_loading", &[], cx).await;
        let files = [
            ("a.rs", lines(5, &[]), lines(5, &[3])),
            ("b.rs", lines(5, &[]), lines(5, &[3])),
            ("c.rs", lines(5, &[]), lines(5, &[3])),
        ];
        let positions = list_test_files(&tab, &files, cx);
        tab.update(cx, |tab, cx| {
            tab.set_details(
                &details_with(&[
                    ("a.rs", "UNVIEWED"),
                    ("b.rs", "UNVIEWED"),
                    ("c.rs", "UNVIEWED"),
                ]),
                cx,
            )
        });
        let editor = test_diff_editor(&workspace, cx);
        tab.update_in(cx, |tab, window, cx| {
            tab.show_diff(editor.clone(), DiffSource::LocalCopy, window, cx)
        });
        // Only a.rs has arrived.
        add_test_files(&editor, &files[..1], &positions[..1], cx).await;
        cx.run_until_parked();

        // There's no file in the diff to move to.
        cx.simulate_keystrokes("] f");
        assert_eq!(cursor(&tab, &editor, cx).0.as_deref(), Some("a.rs"));

        // e marks a.rs, and says why it stays: b.rs, the next unviewed file, isn't in yet.
        github.github.answer(WriteAnswer::Done);
        cx.simulate_keystrokes("e");
        assert_eq!(github.github.mutations()[0].1["path"], "a.rs");
        assert_eq!(cursor(&tab, &editor, cx).0.as_deref(), Some("a.rs"));
        assert_ne!(selected(&tab, cx).as_deref(), Some("b.rs"));
        assert_eq!(
            toasts(cx),
            ["b.rs, the next unviewed file, is still loading, so the diff stays on a.rs."]
        );

        // c.rs arrives before b.rs, and the keys skip b.rs while it isn't in.
        add_test_files(&editor, &files[2..], &positions[2..], cx).await;
        cx.run_until_parked();
        cx.simulate_keystrokes("] f");
        assert_eq!(cursor(&tab, &editor, cx).0.as_deref(), Some("c.rs"));
    }

    #[gpui::test]
    async fn e_marks_the_file_viewed_and_moves_to_the_next_unviewed_one(cx: &mut TestAppContext) {
        let (workspace, tab, github, cx) = three_files(
            "e_marks",
            &[],
            &[
                ("a.rs", "UNVIEWED"),
                ("b.rs", "VIEWED"),
                ("c.rs", "DISMISSED"),
            ],
            cx,
        )
        .await;
        github.github.answer(WriteAnswer::Done);

        // GitHub's head is the head on screen, and the diff's cursor starts in the first file.
        cx.simulate_keystrokes("e");

        use ViewedState::*;
        assert_eq!(
            marks(&tab, cx),
            [
                mark("a.rs", Some(Viewed)),
                mark("b.rs", Some(Viewed)),
                mark("c.rs", Some(Dismissed))
            ]
        );
        // b.rs is viewed, and a file changed since viewed counts as unviewed.
        assert_eq!(selected(&tab, cx).as_deref(), Some("c.rs"));
        assert_eq!(
            github.github.mutations(),
            [(
                "MarkFileAsViewed".to_string(),
                serde_json::json!({ "pullRequestId": "PR_kwDOVk6v9M8AAAABNQwFBw", "path": "a.rs" })
            )]
        );
        // This Mac records the head on screen when the file was marked.
        assert_eq!(
            github.recorded(),
            HashMap::from([("a.rs".to_string(), HEAD.to_string())])
        );
        assert!(!has_viewed_toast(&workspace, cx));
    }

    const NEWER: &str = "3333333333333333333333333333333333333333";

    /// GitHub's REST view of PR #2 with its head at `head`, as the poller's summary carries it.
    fn summary_at(head: &str) -> github::PullSummary {
        serde_json::from_value(serde_json::json!({
            "number": 2,
            "title": "Add b",
            "state": "open",
            "updated_at": "2026-11-06T09:00:00Z",
            "head": { "ref": "stack-b", "sha": head },
            "base": { "ref": "stack-a", "sha": "b046244d9569964088e952243e5e557312322488" },
        }))
        .unwrap()
    }

    #[gpui::test]
    async fn after_a_newer_push_e_wont_mark_a_file_but_still_clears_one(cx: &mut TestAppContext) {
        let marks_now = [
            ("a.rs", "UNVIEWED"),
            ("b.rs", "VIEWED"),
            ("c.rs", "UNVIEWED"),
        ];
        let (_, tab, github, cx) = three_files("e_after_a_push", &[], &marks_now, cx).await;
        // GitHub's details say the pull request moved on from the head on screen.
        tab.update(cx, |tab, cx| {
            tab.set_details(&details_at(NEWER, &marks_now), cx)
        });
        let before = marks(&tab, cx);

        // markFileAsViewed takes no commit, so GitHub would mark a.rs as of the newer push.
        cx.simulate_keystrokes("e");
        assert_eq!(marks(&tab, cx), before);
        assert_eq!(github.github.mutations(), []);
        assert_eq!(cursor_file(&tab, cx).as_deref(), Some("a.rs"));
        let toast = toast_log(cx).pop().unwrap();
        assert_eq!(
            toast.1,
            "There's a newer push to this pull request than the one on screen, so rozed didn't \
             mark a.rs viewed: GitHub would mark it as of that push. Load it to mark files viewed."
        );
        assert_eq!(toast.2.as_deref(), Some("Load the newer push"));

        // Clearing a mark is always allowed.
        go_to(&tab, "b.rs", cx);
        github.github.answer(WriteAnswer::Done);
        cx.simulate_keystrokes("e");
        assert_eq!(github.github.mutations()[0].0, "UnmarkFileAsViewed");
    }

    #[gpui::test]
    async fn a_newer_push_in_githubs_summary_also_stops_e_marking(cx: &mut TestAppContext) {
        let (_, tab, github, cx) =
            three_files("e_after_a_summary", &[], &[("a.rs", "UNVIEWED")], cx).await;
        let pull = github.pull.clone();
        tab.update(cx, |tab, cx| {
            tab.poll_event(
                &PollEvent::Summary {
                    pull,
                    summary: std::sync::Arc::new(summary_at(NEWER)),
                },
                cx,
            )
        });

        cx.simulate_keystrokes("e");
        assert_eq!(github.github.mutations(), []);
        assert!(toasts(cx)[0].starts_with("There's a newer push"));

        // Older details, from before the summary, don't bring the old head back.
        let older = SystemTime::now() - Duration::from_secs(60);
        tab.update(cx, |tab, cx| {
            tab.take_details(&details_with(&[("a.rs", "UNVIEWED")]), older, Some(BOT), cx)
        });
        cx.simulate_keystrokes("e");
        assert_eq!(github.github.mutations(), []);
    }

    #[gpui::test]
    async fn an_older_head_from_before_the_diff_loaded_doesnt_stop_e(cx: &mut TestAppContext) {
        let (_, tab, github, cx) =
            three_files("e_after_saved", &[], &[("a.rs", "UNVIEWED")], cx).await;
        // What rozed saved, from before this diff's head was pushed.
        let saved_at = SystemTime::now() - Duration::from_secs(3600);
        tab.update(cx, |tab, cx| {
            tab.latest_head = None;
            tab.take_details(
                &details_at(
                    "1111111111111111111111111111111111111111",
                    &[("a.rs", "UNVIEWED")],
                ),
                saved_at,
                Some(BOT),
                cx,
            )
        });

        github.github.answer(WriteAnswer::Done);
        cx.simulate_keystrokes("e");
        assert_eq!(github.github.mutations()[0].0, "MarkFileAsViewed");
    }

    #[gpui::test]
    async fn loading_the_newer_push_reopens_the_tab_in_its_place(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let first = open_unloaded(&workspace, "https://github.com/sharkdp/fd/pull/1", cx);
        let tab = open_unloaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);
        open_unloaded(&workspace, "https://github.com/sharkdp/fd/pull/3", cx);

        let reopened = workspace
            .update_in(cx, |workspace, window, cx| {
                reopen_with(workspace, &tab, window, cx, |_, _, _| {})
            })
            .unwrap();
        cx.run_until_parked();

        let tabs = tabs(&workspace, cx);
        assert_eq!(tabs.len(), 3);
        assert_eq!(tabs[0], first);
        assert_eq!(tabs[1], reopened);
        assert_ne!(reopened, tab);
        assert_eq!(
            reopened.read_with(cx, |tab, _| tab.link().to_string()),
            "sharkdp/fd#2212"
        );
        assert_eq!(active_tab(&workspace, cx), Some(reopened));
    }

    #[gpui::test]
    async fn e_wont_mark_a_file_github_doesnt_list(cx: &mut TestAppContext) {
        // GitHub lists every file of the pull request, and not c.rs.
        let (_, tab, github, cx) = three_files(
            "e_unlisted",
            &[],
            &[("a.rs", "UNVIEWED"), ("b.rs", "UNVIEWED")],
            cx,
        )
        .await;
        go_to(&tab, "c.rs", cx);

        cx.simulate_keystrokes("e");

        assert_eq!(github.github.mutations(), []);
        assert_eq!(marks(&tab, cx)[2], mark("c.rs", None));
        assert_eq!(
            toasts(cx),
            [
                "GitHub doesn't list c.rs in this pull request, so rozed can't mark it viewed. \
              GitHub and the local copy see this file's change differently."
            ]
        );
    }

    #[gpui::test]
    async fn e_marks_a_renamed_file_at_the_path_github_lists(cx: &mut TestAppContext) {
        let files = [
            ("a.rs", lines(5, &[]), lines(5, &[3])),
            ("new.rs", lines(5, &[]), lines(5, &[3])),
        ];
        // The local copy sees a rename; GitHub lists the file by its old path.
        let (_, tab, github, _, cx) = signed_in_diff(
            "e_renamed",
            &[],
            &files,
            &[("a.rs", "UNVIEWED"), ("old.rs", "UNVIEWED")],
            cx,
        )
        .await;
        let mut renamed = changed("new.rs", Change::Renamed { similarity: 90 });
        renamed.old.as_mut().unwrap().path = "old.rs".into();
        let head: Oid = HEAD.parse().unwrap();
        tab.update(cx, |tab, cx| {
            tab.copied(
                local_entries(&[changed("a.rs", Change::Modified), renamed]),
                &head,
                SystemTime::now(),
                cx,
            );
        });
        assert_eq!(
            marks(&tab, cx)[1],
            mark("new.rs", Some(ViewedState::Unviewed))
        );
        go_to(&tab, "new.rs", cx);

        github.github.answer(WriteAnswer::Done);
        cx.simulate_keystrokes("e");
        cx.run_until_parked();

        assert_eq!(github.github.mutations()[0].1["path"], "old.rs");
        assert_eq!(
            marks(&tab, cx)[1],
            mark("new.rs", Some(ViewedState::Viewed))
        );
        assert_eq!(
            github.recorded(),
            HashMap::from([("old.rs".to_string(), HEAD.to_string())])
        );
    }

    #[gpui::test]
    async fn writes_waiting_when_the_tab_closes_still_record_and_say_why_they_failed(
        cx: &mut TestAppContext,
    ) {
        let (workspace, tab, github, cx) = three_files(
            "e_then_close",
            &[],
            &[
                ("a.rs", "UNVIEWED"),
                ("b.rs", "UNVIEWED"),
                ("c.rs", "UNVIEWED"),
            ],
            cx,
        )
        .await;
        github.github.answer(WriteAnswer::Done);
        github.github.answer(WriteAnswer::Done);
        github.github.answer(WriteAnswer::Forbidden);

        // Writes are a second apart, so the second and third wait while the tab closes.
        cx.simulate_keystrokes("e");
        cx.simulate_keystrokes("e");
        cx.simulate_keystrokes("e");
        close(&workspace, tab, cx);
        assert!(tabs(&workspace, cx).is_empty());
        cx.executor().advance_clock(Duration::from_secs(3));
        cx.run_until_parked();

        assert_eq!(github.github.mutations().len(), 3);
        assert_eq!(
            github.recorded(),
            HashMap::from([
                ("a.rs".to_string(), HEAD.to_string()),
                ("b.rs".to_string(), HEAD.to_string()),
            ])
        );
        assert_eq!(
            toasts(cx),
            [
                "Couldn't mark c.rs viewed: rozed-bot isn't allowed to (Resource not accessible by \
              integration). It shows GitHub's mark again."
            ]
        );
    }

    #[gpui::test]
    async fn marks_cleared_or_set_elsewhere_update_this_macs_records(cx: &mut TestAppContext) {
        const R1: &str = "1111111111111111111111111111111111111111";
        let (_, tab, github, cx) = three_files(
            "records_follow_github",
            &[("a.rs", R1), ("b.rs", R1)],
            &[("a.rs", "VIEWED"), ("b.rs", "VIEWED"), ("c.rs", "UNVIEWED")],
            cx,
        )
        .await;
        let viewed_at =
            |cx: &mut VisualTestContext| tab.read_with(cx, |tab, _| tab.viewed_at.clone());
        let both = HashMap::from([
            ("a.rs".to_string(), R1.to_string()),
            ("b.rs".to_string(), R1.to_string()),
        ]);
        // Marks set before the tab opened keep their records.
        assert_eq!(github.recorded(), both);

        // a.rs's mark cleared on the web, and b.rs changed by a push.
        tab.update(cx, |tab, cx| {
            tab.set_details(
                &details_with(&[
                    ("a.rs", "UNVIEWED"),
                    ("b.rs", "DISMISSED"),
                    ("c.rs", "UNVIEWED"),
                ]),
                cx,
            )
        });
        cx.run_until_parked();
        let b_only = HashMap::from([("b.rs".to_string(), R1.to_string())]);
        assert_eq!(github.recorded(), b_only);
        assert_eq!(viewed_at(cx), b_only);

        // Both marked again on another Mac, at the head GitHub has then.
        tab.update(cx, |tab, cx| {
            tab.set_details(
                &details_with(&[("a.rs", "VIEWED"), ("b.rs", "VIEWED"), ("c.rs", "UNVIEWED")]),
                cx,
            )
        });
        cx.run_until_parked();
        let at_head = HashMap::from([
            ("a.rs".to_string(), HEAD.to_string()),
            ("b.rs".to_string(), HEAD.to_string()),
        ]);
        assert_eq!(github.recorded(), at_head);
        assert_eq!(viewed_at(cx), at_head);
    }

    #[gpui::test]
    async fn each_failed_write_has_its_own_toast_and_the_selection_stays(cx: &mut TestAppContext) {
        let (workspace, tab, github, cx) = three_files(
            "e_fails_twice",
            &[],
            &[
                ("a.rs", "UNVIEWED"),
                ("b.rs", "UNVIEWED"),
                ("c.rs", "UNVIEWED"),
            ],
            cx,
        )
        .await;
        github.github.answer(WriteAnswer::Forbidden);
        github.github.answer(WriteAnswer::Forbidden);

        cx.simulate_keystrokes("e");
        cx.simulate_keystrokes("e");
        assert_eq!(selected(&tab, cx).as_deref(), Some("c.rs"));
        cx.executor().advance_clock(Duration::from_secs(2));
        cx.run_until_parked();

        // Both say what failed, one toast a file.
        let log = toast_log(cx);
        assert_eq!(log.len(), 2);
        assert!(
            log[0].1.starts_with("Couldn't mark a.rs viewed"),
            "{}",
            log[0].1
        );
        assert!(
            log[1].1.starts_with("Couldn't mark b.rs viewed"),
            "{}",
            log[1].1
        );
        assert_ne!(log[0].0, log[1].0);
        for (id, _, _) in &log {
            assert!(workspace.read_with(cx, |workspace, _| workspace.has_notification(id)));
        }
        // Only the marks went back: the failures don't move the selection.
        assert_eq!(selected(&tab, cx).as_deref(), Some("c.rs"));
        assert_eq!(
            marks(&tab, cx),
            [
                mark("a.rs", Some(ViewedState::Unviewed)),
                mark("b.rs", Some(ViewedState::Unviewed)),
                mark("c.rs", Some(ViewedState::Unviewed))
            ]
        );
    }

    #[gpui::test]
    async fn while_github_cant_be_reached_e_marks_nothing_and_stays(cx: &mut TestAppContext) {
        let (_, tab, github, cx) = three_files("e_offline", &[], &[("a.rs", "UNVIEWED")], cx).await;
        tab.update(cx, |tab, _| {
            tab.poll.status.offline = Some(github::poll::Offline {
                since: SystemTime::now(),
                retry_at: SystemTime::now() + Duration::from_secs(30),
                error: "the network is down".into(),
            })
        });

        cx.simulate_keystrokes("e");

        assert_eq!(github.github.mutations(), []);
        assert_eq!(
            marks(&tab, cx)[0],
            mark("a.rs", Some(ViewedState::Unviewed))
        );
        assert_eq!(cursor_file(&tab, cx).as_deref(), Some("a.rs"));
        assert_eq!(
            toasts(cx),
            ["rozed can't reach GitHub, so it can't mark a.rs viewed until it's back."]
        );
    }

    #[gpui::test]
    async fn marks_github_read_before_a_write_finished_dont_bring_the_old_mark_back(
        cx: &mut TestAppContext,
    ) {
        let (_, tab, github, cx) = three_files(
            "e_then_switch_tabs",
            &[],
            &[("a.rs", "UNVIEWED"), ("b.rs", "UNVIEWED")],
            cx,
        )
        .await;
        let before_the_write = SystemTime::now() - Duration::from_millis(1);
        github.github.answer(WriteAnswer::Done);
        cx.simulate_keystrokes("e");
        cx.run_until_parked();
        assert_eq!(marks(&tab, cx)[0], mark("a.rs", Some(ViewedState::Viewed)));

        // Switching back to the tab hands over the details the poller held from before.
        let stale = details_with(&[("a.rs", "UNVIEWED"), ("b.rs", "VIEWED")]);
        tab.update(cx, |tab, cx| {
            tab.take_details(&stale, before_the_write, Some(BOT), cx)
        });
        assert_eq!(
            marks(&tab, cx)[..2],
            [
                mark("a.rs", Some(ViewedState::Viewed)),
                mark("b.rs", Some(ViewedState::Viewed))
            ]
        );
        // And its record stays.
        assert_eq!(
            github.recorded(),
            HashMap::from([("a.rs".to_string(), HEAD.to_string())])
        );

        // Details fetched after the write stand.
        tab.update(cx, |tab, cx| tab.set_details(&stale, cx));
        assert_eq!(
            marks(&tab, cx)[0],
            mark("a.rs", Some(ViewedState::Unviewed))
        );
    }

    #[gpui::test]
    async fn e_on_a_viewed_file_clears_its_mark(cx: &mut TestAppContext) {
        let (_, tab, github, cx) = three_files(
            "e_clears",
            &[("b.rs", "1111111111111111111111111111111111111111")],
            &[
                ("a.rs", "UNVIEWED"),
                ("b.rs", "VIEWED"),
                ("c.rs", "UNVIEWED"),
            ],
            cx,
        )
        .await;
        go_to(&tab, "b.rs", cx);
        github.github.answer(WriteAnswer::Done);

        cx.simulate_keystrokes("e");

        use ViewedState::*;
        assert_eq!(
            marks(&tab, cx),
            [
                mark("a.rs", Some(Unviewed)),
                mark("b.rs", Some(Unviewed)),
                mark("c.rs", Some(Unviewed))
            ]
        );
        // Clearing a mark stays on the file, and forgets where it was viewed.
        assert_eq!(selected(&tab, cx).as_deref(), Some("b.rs"));
        assert_eq!(github.github.mutations()[0].0, "UnmarkFileAsViewed");
        assert_eq!(github.recorded(), HashMap::new());

        // Pressed again, after the second's spacing between writes, it's marked again.
        github.github.answer(WriteAnswer::Done);
        cx.simulate_keystrokes("e");
        cx.executor().advance_clock(Duration::from_secs(2));
        cx.run_until_parked();
        assert_eq!(marks(&tab, cx)[1], mark("b.rs", Some(Viewed)));
        assert_eq!(github.github.mutations()[1].0, "MarkFileAsViewed");
        assert_eq!(
            github.recorded(),
            HashMap::from([("b.rs".to_string(), HEAD.to_string())])
        );
    }

    #[gpui::test]
    async fn a_mark_that_github_refuses_reverts_and_a_toast_says_why(cx: &mut TestAppContext) {
        let (workspace, tab, github, cx) = three_files(
            "e_reverts",
            &[],
            &[
                ("a.rs", "UNVIEWED"),
                ("b.rs", "VIEWED"),
                ("c.rs", "UNVIEWED"),
            ],
            cx,
        )
        .await;
        use ViewedState::*;
        let before = marks(&tab, cx);

        for answer in [
            WriteAnswer::Forbidden,
            WriteAnswer::Offline,
            WriteAnswer::RateLimited,
        ] {
            workspace.update(cx, |workspace, cx| workspace.clear_all_notifications(cx));
            go_to(&tab, "a.rs", cx);
            github.github.answer(answer);

            cx.simulate_keystrokes("e");
            cx.executor().advance_clock(Duration::from_secs(2));
            cx.run_until_parked();

            assert_eq!(marks(&tab, cx), before, "{answer:?}");
            assert!(has_viewed_toast(&workspace, cx), "{answer:?}");
            assert_eq!(github.recorded(), HashMap::new(), "{answer:?}");
            // The selection still moved on; only the mark went back.
            assert_eq!(selected(&tab, cx).as_deref(), Some("c.rs"), "{answer:?}");
        }
        assert_eq!(github.github.mutations().len(), 3);
        assert_eq!(before[0], mark("a.rs", Some(Unviewed)));
    }

    #[gpui::test]
    async fn the_new_mark_shows_while_the_write_waits(cx: &mut TestAppContext) {
        let (_, tab, github, cx) = three_files(
            "e_waits",
            &[],
            &[
                ("a.rs", "UNVIEWED"),
                ("b.rs", "UNVIEWED"),
                ("c.rs", "UNVIEWED"),
            ],
            cx,
        )
        .await;
        github.github.answer(WriteAnswer::Done);
        github.github.answer(WriteAnswer::Forbidden);

        // The second write waits a second after the first, so its mark shows before GitHub answers.
        cx.simulate_keystrokes("e");
        cx.simulate_keystrokes("e");

        use ViewedState::*;
        assert_eq!(
            marks(&tab, cx),
            [
                mark("a.rs", Some(Viewed)),
                mark("b.rs", Some(Viewed)),
                mark("c.rs", Some(Unviewed))
            ]
        );
        assert_eq!(github.github.mutations().len(), 1);

        cx.executor().advance_clock(Duration::from_secs(2));
        cx.run_until_parked();
        // GitHub took a.rs's mark and refused b.rs's, which goes back and says why.
        assert_eq!(
            marks(&tab, cx),
            [
                mark("a.rs", Some(Viewed)),
                mark("b.rs", Some(Unviewed)),
                mark("c.rs", Some(Unviewed))
            ]
        );
        assert_eq!(github.github.mutations().len(), 2);
        assert_eq!(
            toasts(cx),
            [
                "Couldn't mark b.rs viewed: rozed-bot isn't allowed to (Resource not accessible by \
              integration). It shows GitHub's mark again."
            ]
        );
        assert_eq!(
            github.recorded(),
            HashMap::from([("a.rs".to_string(), HEAD.to_string())])
        );
    }

    #[gpui::test]
    async fn e_says_why_it_cant_mark_without_an_account_or_githubs_details(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx).await;
        let files = [("a.rs", lines(5, &[]), lines(5, &[3]))];
        let anonymous = open_unloaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);
        anonymous.update(cx, |tab, cx| tab.set_acting(Acting::Anonymous, cx));
        show_test_diff(&anonymous, &workspace, &files, cx).await;

        cx.simulate_keystrokes("e");
        assert_eq!(
            toasts(cx),
            ["Sign in to mark files viewed: this pull request opened without an account."]
        );
        assert!(has_viewed_toast(&workspace, cx));
        assert_eq!(marks(&anonymous, cx), [mark("a.rs", None)]);

        // Signed in, but GitHub hasn't sent the pull request's details, which name it for writes.
        let tab = open_unloaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);
        let github = sign_in_bot(&tab, "e_without_details", &[], cx).await;
        show_test_diff(&tab, &workspace, &files, cx).await;

        cx.simulate_keystrokes("e");
        assert_eq!(
            toasts(cx)[1..],
            [
                "Couldn't mark a.rs viewed: GitHub hasn't sent this pull request's details yet. Try \
              again in a moment."
            ]
        );
        assert!(has_viewed_toast(&workspace, cx));
        assert_eq!(marks(&tab, cx), [mark("a.rs", None)]);
        assert_eq!(github.github.mutations(), []);
        // Each tab's toast is its own, so the first still shows.
        let first = toast_log(cx)[0].0.clone();
        assert!(workspace.read_with(cx, |workspace, _| workspace.has_notification(&first)));
    }

    #[gpui::test]
    async fn a_file_that_changed_since_it_was_viewed_says_so_when_opened(cx: &mut TestAppContext) {
        const VIEWED_AT: &str = "1111111111111111111111111111111111111111";
        let (_, tab, _, cx) = three_files(
            "changed_since_viewed",
            &[("b.rs", VIEWED_AT)],
            &[
                ("a.rs", "UNVIEWED"),
                ("b.rs", "DISMISSED"),
                ("c.rs", "VIEWED"),
            ],
            cx,
        )
        .await;
        let note = |cx: &mut VisualTestContext| {
            tab.update(cx, |tab, cx| tab.changed_since_viewed(cx))
                .map(|(_, note)| note)
        };
        assert_eq!(note(cx), None, "a.rs is current, and unviewed");

        // The list shows b.rs changed since viewed; opening it says since when.
        assert_eq!(
            marks(&tab, cx)[1],
            mark("b.rs", Some(ViewedState::Dismissed))
        );
        cx.simulate_keystrokes("] f");
        assert_eq!(
            note(cx).as_deref(),
            Some(
                "Changed since it was viewed. rozed can't show only what changed since then yet, \
                 so this is the file's whole diff."
            ),
            "the head this Mac recorded isn't a revision the tab knows"
        );

        // With the revision this Mac recorded among the pull request's, it's named.
        tab.update(cx, |tab, _| {
            let at = |secs: u64| SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000 + secs);
            tab.revisions = Some(build_revisions(
                &[
                    PushRecord::Polled {
                        head: VIEWED_AT.into(),
                        at: at(0),
                    },
                    PushRecord::Polled {
                        head: HEAD.into(),
                        at: at(60),
                    },
                ],
                &[],
                HEAD,
                SystemTime::UNIX_EPOCH,
            ));
        });
        assert_eq!(
            note(cx).as_deref(),
            Some(
                "Changed since you viewed it in r1. rozed can't show only what changed since \
                 then yet, so this is the file's whole diff."
            )
        );

        // A viewed file says nothing.
        cx.simulate_keystrokes("] f");
        assert_eq!(note(cx), None);
    }

    #[gpui::test]
    async fn the_file_keys_move_between_files_in_review_order(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let tab = open_unloaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);
        show_test_diff(
            &tab,
            &workspace,
            &[
                ("Cargo.lock", lines(5, &[]), lines(5, &[2])),
                ("src/main.rs", lines(5, &[]), lines(5, &[3])),
                ("tests/main.rs", lines(5, &[]), lines(5, &[4])),
            ],
            cx,
        )
        .await;
        // The list shows path order; the keys still follow the diff's order, review order.
        cx.simulate_keystrokes("alt-o");

        // Until something is selected, the first file in review order is the current one.
        cx.simulate_keystrokes("] f");
        assert_eq!(selected(&tab, cx).as_deref(), Some("tests/main.rs"));
        cx.simulate_keystrokes("] f");
        assert_eq!(selected(&tab, cx).as_deref(), Some("Cargo.lock"));
        cx.simulate_keystrokes("] f");
        assert_eq!(selected(&tab, cx).as_deref(), Some("Cargo.lock"));
        cx.simulate_keystrokes("[ f");
        assert_eq!(selected(&tab, cx).as_deref(), Some("tests/main.rs"));
        cx.simulate_keystrokes("[ f [ f");
        assert_eq!(selected(&tab, cx).as_deref(), Some("src/main.rs"));
    }

    /// `count` numbered lines, with the lines at `changed` (from 1) edited.
    fn lines(count: usize, changed: &[usize]) -> String {
        (1..=count)
            .map(|line| match changed.contains(&line) {
                true => format!("line {line} changed\n"),
                false => format!("line {line}\n"),
            })
            .collect()
    }

    /// Shows a diff of `files`, (path, old text, new text), in `tab`, each file at its place in
    /// review order. As with the local copy's diff, the tab shows the editor first and the files
    /// arrive one by one.
    async fn show_test_diff(
        tab: &Entity<PrTab>,
        workspace: &Entity<Workspace>,
        files: &[(&str, String, String)],
        cx: &mut VisualTestContext,
    ) -> Entity<SplittableEditor> {
        let positions = list_test_files(tab, files, cx);
        let editor = test_diff_editor(workspace, cx);
        tab.update_in(cx, |tab, window, cx| {
            tab.show_diff(editor.clone(), DiffSource::LocalCopy, window, cx)
        });
        add_test_files(&editor, files, &positions, cx).await;
        cx.run_until_parked();
        editor
    }

    /// Lists `files` in `tab` as the local copy does, and returns where each goes in the diff.
    fn list_test_files(
        tab: &Entity<PrTab>,
        files: &[(&str, String, String)],
        cx: &mut VisualTestContext,
    ) -> Vec<usize> {
        let changed: Vec<_> = files
            .iter()
            .map(|(path, _, _)| changed(path, Change::Modified))
            .collect();
        let head: Oid = HEAD.parse().unwrap();
        tab.update(cx, |tab, cx| {
            tab.copied(local_entries(&changed), &head, SystemTime::now(), cx)
        })
    }

    /// An empty side-by-side diff, not shown anywhere yet.
    fn test_diff_editor(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> Entity<SplittableEditor> {
        let project = workspace.read_with(cx, |workspace, _| workspace.project().clone());
        let multi_buffer = cx.new(|cx| {
            let mut multi_buffer = multi_buffer::MultiBuffer::new(Capability::ReadOnly);
            multi_buffer.set_all_diff_hunks_expanded(cx);
            multi_buffer
        });
        workspace.update_in(cx, |_, window, cx| {
            let workspace = cx.entity();
            cx.new(|cx| {
                SplittableEditor::new(
                    settings::DiffViewStyle::Split,
                    multi_buffer,
                    project,
                    workspace,
                    window,
                    cx,
                )
            })
        })
    }

    /// Adds `files`, (path, old text, new text), to `editor`, each at its position in `positions`,
    /// one by one.
    async fn add_test_files(
        editor: &Entity<SplittableEditor>,
        files: &[(&str, String, String)],
        positions: &[usize],
        cx: &mut VisualTestContext,
    ) {
        for ((path, old, new), position) in files.iter().zip(positions) {
            let buffer = cx.new(|cx| language::Buffer::local(new.as_str(), cx));
            let snapshot = buffer.read_with(cx, |buffer, _| buffer.snapshot());
            let diff = cx.new(|cx| buffer_diff::BufferDiff::new(&snapshot.text, None, None, cx));
            diff.update(cx, |diff, cx| {
                diff.set_base_text(Some(old.as_str().into()), snapshot.text.clone(), cx)
            })
            .await;
            let ranges: Vec<_> = diff.read_with(cx, |diff, cx| {
                use language::OffsetRangeExt as _;
                diff.snapshot(cx)
                    .hunks(&snapshot)
                    .map(|hunk| hunk.buffer_range.to_point(&snapshot))
                    .collect()
            });
            let key =
                pr_diff::path_key(*position, RelPath::from_unix_str(path).unwrap().into_arc());
            editor.update_in(cx, |editor, window, cx| {
                editor.update_excerpts_for_path(key, buffer, ranges, 3, diff, cx);
                editor.split(window, cx);
            });
        }
    }

    /// The file the diff's cursor is in, the cursor's row, and the file the list selects.
    fn cursor(
        tab: &Entity<PrTab>,
        editor: &Entity<SplittableEditor>,
        cx: &mut VisualTestContext,
    ) -> (Option<String>, u32, Option<String>) {
        let file = cx.update(|_, cx| crate::diff_cursor::file_at_cursor(editor, cx));
        let rhs = editor.read_with(cx, |editor, _| editor.rhs_editor().clone());
        let row = rhs.update(cx, |editor, cx| {
            editor
                .selections
                .newest::<language::Point>(&editor.display_snapshot(cx))
                .head()
                .row
        });
        (file.map(|(_, path)| path), row, selected(tab, cx))
    }

    #[gpui::test]
    async fn the_change_keys_move_between_changes_across_files(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let tab = open_unloaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);
        let editor = show_test_diff(
            &tab,
            &workspace,
            &[
                ("src/a.rs", lines(30, &[]), lines(30, &[2, 25])),
                ("src/b.rs", lines(5, &[]), lines(5, &[3])),
                ("src/c.rs", lines(5, &[]), lines(5, &[4])),
            ],
            cx,
        )
        .await;
        let file = |cx: &mut VisualTestContext| cursor(&tab, &editor, cx).0;
        assert_eq!(file(cx).as_deref(), Some("src/a.rs"));

        // Both of a.rs's changes, then on into b.rs, which the list follows.
        cx.simulate_keystrokes("] c");
        let (in_file, first, _) = cursor(&tab, &editor, cx);
        assert_eq!(in_file.as_deref(), Some("src/a.rs"));
        cx.simulate_keystrokes("] c");
        let (in_file, second, _) = cursor(&tab, &editor, cx);
        assert_eq!(in_file.as_deref(), Some("src/a.rs"));
        assert!(second > first, "{first} then {second}");
        cx.simulate_keystrokes("] c");
        let (in_file, _, listed) = cursor(&tab, &editor, cx);
        assert_eq!(in_file.as_deref(), Some("src/b.rs"));
        assert_eq!(listed.as_deref(), Some("src/b.rs"));

        // The last change stays put.
        cx.simulate_keystrokes("] c");
        let last = cursor(&tab, &editor, cx);
        assert_eq!(last.0.as_deref(), Some("src/c.rs"));
        cx.simulate_keystrokes("] c");
        assert_eq!(cursor(&tab, &editor, cx), last);

        // And back, across files.
        cx.simulate_keystrokes("[ c [ c");
        let (in_file, row, listed) = cursor(&tab, &editor, cx);
        assert_eq!((in_file.as_deref(), row), (Some("src/a.rs"), second));
        assert_eq!(listed.as_deref(), Some("src/a.rs"));

        // The first change stays put too.
        cx.simulate_keystrokes("[ c");
        assert_eq!(cursor(&tab, &editor, cx).1, first);
        cx.simulate_keystrokes("[ c");
        assert_eq!(
            cursor(&tab, &editor, cx),
            (Some("src/a.rs".into()), first, Some("src/a.rs".into()))
        );
    }

    #[gpui::test]
    fn the_tabs_typing_keys_type_in_a_text_box_inside_it(cx: &mut TestAppContext) {
        init(cx);
        cx.update(|cx| {
            let keymap = cx.key_bindings();
            let keymap = keymap.borrow();
            let stack = |contexts: &[&str]| -> Vec<gpui::KeyContext> {
                contexts
                    .iter()
                    .map(|context| gpui::KeyContext::parse(context).unwrap())
                    .collect()
            };
            let tab = ["Workspace", "Pane", "PrTab"];
            let diff = stack(&[&tab[..], &["Editor mode=full multibuffer"]].concat());
            let file_list = stack(&[&tab[..], &["PrFileList"]].concat());
            // A comment box in the diff, as milestone 8 adds, or any other text box in the tab.
            let text_box = stack(
                &[
                    &tab[..],
                    &["Editor mode=full multibuffer", "Editor mode=auto_height"],
                ]
                .concat(),
            );
            let line = stack(&[&tab[..], &["Editor mode=single_line"]].concat());
            let rozed = |input: &'static str, contexts: &[gpui::KeyContext]| {
                let keystrokes: Vec<gpui::Keystroke> = input
                    .split(' ')
                    .map(|key| gpui::Keystroke::parse(key).unwrap())
                    .collect();
                let (bindings, pending) = keymap.bindings_for_input(&keystrokes, contexts);
                let actions: Vec<&str> = bindings
                    .iter()
                    .map(|binding| binding.action().name())
                    .filter(|name| name.starts_with("review_ui::"))
                    .collect();
                (actions, pending)
            };

            for (keys, action) in [
                ("e", "review_ui::ToggleViewed"),
                ("] f", "review_ui::NextFile"),
                ("[ f", "review_ui::PreviousFile"),
                ("] c", "review_ui::NextChange"),
                ("[ c", "review_ui::PreviousChange"),
                ("alt-o", "review_ui::ToggleFileOrder"),
            ] {
                assert_eq!(rozed(keys, &diff).0, [action], "{keys} in the diff");
                assert_eq!(
                    rozed(keys, &file_list).0,
                    [action],
                    "{keys} in the file list"
                );
                for contexts in [&text_box, &line] {
                    assert_eq!(rozed(keys, contexts).0, [""; 0], "{keys} in a text box");
                }
            }
            // A bracket types at once, not waiting for a second key.
            for prefix in ["]", "["] {
                assert!(rozed(prefix, &diff).1, "{prefix} waits in the diff");
                assert!(!rozed(prefix, &text_box).1, "{prefix} types in a text box");
            }
        });
    }

    #[gpui::test]
    async fn the_change_keys_skip_folded_files(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let tab = open_unloaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);
        let editor = show_test_diff(
            &tab,
            &workspace,
            &[
                ("src/a.rs", lines(5, &[]), lines(5, &[3])),
                ("src/b.rs", lines(5, &[]), lines(5, &[2, 4])),
                ("src/c.rs", lines(5, &[]), lines(5, &[3])),
            ],
            cx,
        )
        .await;
        // b.rs is collapsed, as a generated file starts, or as its header's chevron folds it.
        let rhs = editor.read_with(cx, |editor, _| editor.rhs_editor().clone());
        rhs.update(cx, |editor, cx| {
            let multi_buffer = editor.buffer().read(cx);
            let snapshot = multi_buffer.snapshot(cx);
            let b = multi_buffer
                .all_buffers_iter()
                .map(|buffer| buffer.read(cx).remote_id())
                .find(|id| {
                    snapshot
                        .path_for_buffer(*id)
                        .is_some_and(|key| key.path.as_unix_str() == "src/b.rs")
                })
                .unwrap();
            editor.fold_buffers([b], cx);
        });
        cx.run_until_parked();

        cx.simulate_keystrokes("] c");
        assert_eq!(cursor(&tab, &editor, cx).0.as_deref(), Some("src/a.rs"));
        cx.simulate_keystrokes("] c");
        let (in_file, _, listed) = cursor(&tab, &editor, cx);
        assert_eq!(in_file.as_deref(), Some("src/c.rs"));
        assert_eq!(listed.as_deref(), Some("src/c.rs"));
        cx.simulate_keystrokes("[ c");
        assert_eq!(cursor(&tab, &editor, cx).0.as_deref(), Some("src/a.rs"));
        // b.rs stays folded.
        assert!(rhs.read_with(cx, |editor, cx| editor.has_any_buffer_folded(cx)));
    }

    #[gpui::test]
    async fn a_change_key_back_to_where_the_cursor_started_moves_the_list_with_it(
        cx: &mut TestAppContext,
    ) {
        // An added first file: its one change starts at its first row, where the cursor starts.
        let files = [
            ("a.rs", String::new(), lines(3, &[])),
            ("b.rs", lines(5, &[]), lines(5, &[3])),
        ];
        let (_, tab, github, editor, cx) = signed_in_diff(
            "back_to_the_start",
            &[],
            &files,
            &[("a.rs", "UNVIEWED"), ("b.rs", "UNVIEWED")],
            cx,
        )
        .await;
        assert_eq!(cursor(&tab, &editor, cx), (Some("a.rs".into()), 0, None));

        cx.simulate_keystrokes("] c");
        assert_eq!(cursor(&tab, &editor, cx).2.as_deref(), Some("b.rs"));
        cx.simulate_keystrokes("[ c");
        assert_eq!(
            cursor(&tab, &editor, cx),
            (Some("a.rs".into()), 0, Some("a.rs".into()))
        );

        // `e` marks the file read, not the one the list showed before.
        github.github.answer(WriteAnswer::Done);
        cx.simulate_keystrokes("e");
        assert_eq!(github.github.mutations()[0].1["path"], "a.rs");
    }

    #[gpui::test]
    async fn the_diffs_cursor_starts_in_the_first_file_in_review_order(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let tab = open_unloaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);
        // Files arrive in git's order; the diff shows them in review order.
        let editor = show_test_diff(
            &tab,
            &workspace,
            &[
                ("Cargo.lock", lines(5, &[]), lines(5, &[2])),
                ("src/main.rs", lines(5, &[]), lines(5, &[3])),
                ("tests/main.rs", lines(5, &[]), lines(5, &[4])),
            ],
            cx,
        )
        .await;

        let (in_file, row, listed) = cursor(&tab, &editor, cx);
        assert_eq!((in_file.as_deref(), row), (Some("src/main.rs"), 0));
        // Where the tab put the cursor isn't a move, so the list still selects nothing.
        assert_eq!(listed, None);
        cx.simulate_keystrokes("] f");
        assert_eq!(
            cursor(&tab, &editor, cx).0.as_deref(),
            Some("tests/main.rs")
        );
    }

    /// Builds a diff of `files` out of sight, then shows it in `tab`, as the local copy's diff
    /// replaces GitHub's.
    async fn show_built_diff(
        tab: &Entity<PrTab>,
        workspace: &Entity<Workspace>,
        files: &[(&str, String, String)],
        cx: &mut VisualTestContext,
    ) -> Entity<SplittableEditor> {
        let positions = list_test_files(tab, files, cx);
        let editor = test_diff_editor(workspace, cx);
        add_test_files(&editor, files, &positions, cx).await;
        cx.run_until_parked();
        tab.update_in(cx, |tab, window, cx| {
            tab.show_diff(editor.clone(), DiffSource::LocalCopy, window, cx)
        });
        cx.run_until_parked();
        editor
    }

    #[gpui::test]
    async fn a_diff_shown_with_its_files_in_starts_in_its_first_file(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let tab = open_unloaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);
        let editor = show_built_diff(
            &tab,
            &workspace,
            &[
                ("Cargo.lock", lines(5, &[]), lines(5, &[2])),
                ("src/main.rs", lines(5, &[]), lines(5, &[3])),
                ("tests/main.rs", lines(5, &[]), lines(5, &[4])),
            ],
            cx,
        )
        .await;

        // Not at the end, in the last file, where an editor's cursor starts.
        assert_eq!(
            cursor(&tab, &editor, cx),
            (Some("src/main.rs".into()), 0, None)
        );
    }

    #[gpui::test]
    async fn the_file_keys_move_the_diffs_cursor_to_each_file(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let tab = open_unloaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);
        let editor = show_test_diff(
            &tab,
            &workspace,
            &[
                ("src/a.rs", lines(30, &[]), lines(30, &[2, 25])),
                ("src/b.rs", lines(5, &[]), lines(5, &[3])),
                ("src/c.rs", lines(5, &[]), lines(5, &[4])),
            ],
            cx,
        )
        .await;

        // From the cursor's file, even before the list selected anything.
        cx.simulate_keystrokes("] f");
        let (in_file, _, listed) = cursor(&tab, &editor, cx);
        assert_eq!(in_file.as_deref(), Some("src/b.rs"));
        assert_eq!(listed.as_deref(), Some("src/b.rs"));

        // From a change deep in a file, the previous file is the one before it.
        cx.simulate_keystrokes("] c [ f");
        assert_eq!(cursor(&tab, &editor, cx).0.as_deref(), Some("src/a.rs"));
        cx.simulate_keystrokes("] f ] f");
        assert_eq!(cursor(&tab, &editor, cx).0.as_deref(), Some("src/c.rs"));
    }

    #[gpui::test]
    async fn the_file_list_keeps_its_selection_when_the_local_copy_reorders_it(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx).await;
        let tab = open_unloaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);
        let head: Oid = "2".repeat(40).parse().unwrap();
        let files = [
            changed("a.pb.go", Change::Modified),
            changed("b.rs", Change::Modified),
        ];
        let entries = |generated: Option<bool>| -> Vec<FileEntry> {
            files
                .iter()
                .map(|file| {
                    let is_pb = file.display_path().ends_with(".pb.go");
                    FileEntry::from_local(file, generated.filter(|_| is_pb))
                })
                .collect()
        };
        tab.update(cx, |tab, cx| {
            tab.copied(entries(None), &head, SystemTime::now(), cx);
            tab.files.select_path("a.pb.go");
        });
        assert_eq!(
            tab.read_with(cx, |tab, _| tab.files().selected_row()),
            Some(0)
        );

        // .gitattributes marks a.pb.go generated, so it lists last, still selected.
        tab.update(cx, |tab, cx| {
            tab.copied(entries(Some(true)), &head, SystemTime::now(), cx)
        });
        tab.read_with(cx, |tab, _| {
            assert_eq!(tab.files().selected().map(FileEntry::path), Some("a.pb.go"));
            assert_eq!(tab.files().selected_row(), Some(1));
        });
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
            tab.set_pull_request(&pull_request, SystemTime::now(), cx);
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
            tab.copied(
                files
                    .iter()
                    .map(|file| FileEntry::from_local(file, None))
                    .collect(),
                &head,
                SystemTime::now(),
                cx,
            )
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
    fn rozeds_keys_win_over_zeds_and_leave_text_fields_alone(cx: &mut TestAppContext) {
        init(cx);
        cx.update(|cx| {
            let keymap = cx.key_bindings();
            let keymap = keymap.borrow();
            let first = |key: &'static str, contexts: &[&str]| -> Option<&'static str> {
                let stack: Vec<gpui::KeyContext> = contexts
                    .iter()
                    .map(|context| gpui::KeyContext::parse(context).unwrap())
                    .collect();
                let keystroke = gpui::Keystroke::parse(key).unwrap();
                let (bindings, _) = keymap.bindings_for_input(&[keystroke], &stack);
                bindings.first().map(|binding| binding.action().name())
            };
            let diff = ["Workspace", "Pane", "PrTab", "Editor mode=full multibuffer"];
            let file_list = ["Workspace", "Pane", "PrTab", "PrFileList"];
            let comment = ["Workspace", "Pane", "PrTab", "Editor mode=auto_height"];
            // A comment box can be a full editor too, of one buffer.
            let full_comment = ["Workspace", "Pane", "PrTab", "Editor mode=full"];
            let elsewhere = ["Workspace", "Pane", "Editor mode=full multibuffer"];

            // Zed's keys are bound, and rozed's, bound after, win where both apply.
            assert_eq!(first("tab", &elsewhere), Some("editor::Tab"));
            assert_eq!(first("tab", &diff), Some("review_ui::SwitchSide"));
            assert_eq!(first("tab", &comment), Some("editor::Tab"));
            assert_eq!(first("tab", &full_comment), Some("editor::Tab"));
            for (key, action) in [
                ("alt-v", "review_ui::ToggleDiffLayout"),
                ("alt-w", "review_ui::ToggleWhitespace"),
                ("alt-a", "review_ui::ExpandFile"),
            ] {
                assert_eq!(first(key, &diff), Some(action), "{key}");
                assert_eq!(first(key, &file_list), Some(action), "{key}");
                // Where text is typed, option and a letter types a character.
                assert_eq!(first(key, &comment), None, "{key}");
                assert_eq!(first(key, &full_comment), None, "{key}");
            }
        });
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
                listed_at: SystemTime::now(),
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
                    SystemTime::now(),
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
    async fn a_cursor_never_moved_starts_the_local_copys_diff_in_its_first_file(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx).await;
        let tab = open_unloaded(&workspace, PULL, cx);
        show_github_files(
            &tab,
            vec![github_file("a.txt", 50), github_file("b.txt", 60)],
            cx,
        );

        let _local_copy = show_local_copy(&tab, cx);

        let local = tab.read_with(cx, |tab, _| tab.diff().unwrap().clone());
        // At the diff's first row, not at its end in b.txt, where the local copy's editor's
        // cursor starts.
        let (in_file, row, _) = cursor(&tab, &local, cx);
        assert_eq!((in_file.as_deref(), row), (Some("a.txt"), 0));
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
    async fn opening_a_stopped_copy_again_starts_over_without_its_retry_button(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx).await;
        let tab = open_unloaded(&workspace, PULL, cx);
        let (retry, retried) = oneshot::channel();
        tab.update(cx, |tab, cx| tab.copy_stopped("git said no", 5, retry, cx));
        assert!(matches!(status(&tab, cx), Status::Failed(_)));
        assert_eq!(banners(&tab, cx).len(), 1);

        open_unloaded(&workspace, PULL, cx);

        assert!(matches!(status(&tab, cx), Status::Loading(_)));
        assert!(banners(&tab, cx).is_empty());
        // The old copy's button is gone with it.
        assert!(retried.now_or_never().unwrap().is_err());
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
                listed_at: SystemTime::now(),
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
