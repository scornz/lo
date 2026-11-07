//! The PR tab: one pull request's header, its files in review order, its side-by-side diff and its
//! threads (design §10 "Window"). Lo's window has one tab per pull request, and opening a pull
//! request that's already open switches to its tab.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::Context as _;
use futures::channel::oneshot;

use editor::scroll::Autoscroll;
use editor::{Anchor, Editor, EditorEvent, EditorSettings, SelectionEffects, SplittableEditor};
use github::accounts::RepoAccount;
use github::files::CutShort;
use github::poll::{PollEvent, Poller};
use github::queries::{FileViewedState, PullRequestData};
use github::viewed::ViewedRecords;
use github::{GithubError, HelpLink, PullRef, Target};
use gpui::{
    AnyElement, AnyView, App, AsyncWindowContext, Context, Entity, EntityId, EventEmitter,
    FocusHandle, Focusable, Render, SharedString, Subscription, UniformListScrollHandle,
    WeakEntity, Window,
};
use language::{Capability, Point};
use project::WorktreeId;
use repo_cache::Oid;
use review::revisions::{PushRecord, Revisions, ViewedState, build_revisions};
use settings::{DiffViewStyle, Settings as _, SettingsStore};
use ui::prelude::*;
use util::rel_path::RelPath;
use workspace::item::{Item, ItemEvent, TabContentParams};
use workspace::{ItemNavHistory, Workspace};

use crate::account::{self, AccountDecision, Acting, Refusal};
use crate::account_picker::{self, NotChosen};
use crate::api_diff::{self, FilesReader, GithubDiff, thousands};
use crate::diff_cursor;
use crate::diff_decorations::{self, Redecorate};
use crate::file_list::{FileEntry, FileList};
use crate::github_api::{self, CanSignIn, PullRequest};
use crate::github_layer::{self, Saved};
use crate::poll_state;
use crate::pr_banner::{self, BannerAction, BannerKind, Banners};
use crate::pr_diff::{self, CopyProgress, LocalPullRequest, Trust};
use crate::pr_link::PrLink;
use crate::shown_comparison::ShownComparison;
use crate::shown_files;
use crate::tabs::{TabKey, find_tab};
use crate::viewed_marks::ViewedMarks;
use crate::window_title;

mod header;
pub(crate) mod keys;
mod left;
mod middle;
mod right;
mod shown_pull;
mod viewed;

pub use header::HeaderSlot;
pub(crate) use keys::bind_keys;
pub use left::{ToggleFileList, ToggleFileOrder};
pub use middle::{
    ExpandFile, Middle, NextChange, NextFile, PreviousChange, PreviousFile, SwitchSide,
    ToggleDiffLayout, ToggleWhitespace,
};
pub use right::ToggleThreads;
pub use shown_pull::{PullHandle, PullId, ShownPull};
pub use viewed::ToggleViewed;

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
            tab.pull.preferred = Some(login);
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
    let keys: Vec<TabKey> = tabs
        .iter()
        .map(|tab| tab.read(cx).pull.key.clone())
        .collect();
    if let Some(index) = find_tab(&keys, &link) {
        let tab = tabs[index].clone();
        workspace.activate_item(&tab, true, true, window, cx);
        if tab.read(cx).pull.status.tries_again() {
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
    cx.subscribe_in(
        &tab,
        window,
        |workspace, _, event: &PrTabEvent, window, cx| {
            if matches!(event, PrTabEvent::Changed | PrTabEvent::PullChanged) {
                window_title::update(workspace, window, cx)
            }
        },
    )
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
    /// GitHub's view didn't load, and the diff is the local copy's, read-only, while Lo keeps
    /// trying. Says so, and why.
    Offline(SharedString),
    Failed(SharedString),
}

impl Status {
    /// Whether opening the tab's link again loads it again: it failed, or showed only what Lo
    /// had while offline.
    fn tries_again(&self) -> bool {
        matches!(self, Self::Failed(_) | Self::Offline(_))
    }
}

/// A feature's view in one of the tab's parts, with the focus handle that takes keys to it.
pub struct PartView {
    view: AnyView,
    focus: FocusHandle,
}

impl PartView {
    /// `view`, which the part shows, and focuses to use it.
    pub fn new<V: Render + Focusable>(view: &Entity<V>, cx: &App) -> Self {
        Self {
            view: view.clone().into(),
            focus: view.read(cx).focus_handle(cx),
        }
    }

    /// Whether this is `entity`'s view.
    pub fn shows(&self, entity: EntityId) -> bool {
        self.view.entity_id() == entity
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

/// What changed in a tab, for the window and for features that show something about its pull
/// request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrTabEvent {
    /// What the tab's title shows.
    Changed,
    /// The tab shows another pull request ([`PrTab::show_pull`]), as a stack's tab does for each
    /// layer. Features that show anything about the pull request start over for the new one.
    PullChanged,
    /// GitHub's latest head for the pull request changed: GitHub said it for the first time, or
    /// the pull request got a push.
    HeadChanged,
    /// The diff shows another comparison: GitHub's diff, or the local copy's
    /// ([`PrTab::comparison`]).
    ComparisonChanged,
}

/// One pull request in a tab.
pub struct PrTab {
    /// The tab's own entity's.
    id: EntityId,
    trust: Trust,
    workspace: WeakEntity<Workspace>,
    focus_handle: FocusHandle,
    file_list_focus: FocusHandle,
    file_list_scroll: UniformListScrollHandle,
    /// The pull request the tab shows, and everything it knows about it, its load and its diff
    /// included.
    pull: ShownPull,
    /// Pull requests the tab keeps without showing them, as a stack's tab keeps its other
    /// layers: their loads and diffs carry on, for when they show again.
    kept: Vec<ShownPull>,
    /// Whether `pull` is a kept pull request, there only while work started for it runs
    /// ([`Self::with_pull`]). It sends no events and takes no focus.
    pull_hidden: bool,
    /// The pane's history, for the diff to record where its cursor goes. The pane hands it over
    /// when the tab joins it, before the diff exists.
    nav_history: Option<ItemNavHistory>,
    /// Inline or side by side, for this tab's diffs: the setting's at first, then the key's.
    diff_style: DiffViewStyle,
    /// The setting's style when last seen, to tell a change to it from any other settings change.
    setting_style: DiffViewStyle,
    /// Whether changes to whitespace alone are hidden.
    hide_whitespace: bool,
    /// What features show in the header's slots, by [`HeaderSlot`].
    header_slots: [Option<AnyView>; HeaderSlot::ALL.len()],
    /// The pull request's stack tree, above the file list.
    stack_tree: Option<PartView>,
    /// Whether the stack tree shows, when there is one.
    show_stack_tree: bool,
    /// What the middle of the tab shows.
    middle: Middle,
    /// The threads panel, on the right.
    threads_panel: Option<PartView>,
    show_file_list: bool,
    show_threads: bool,
    opened: Instant,
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
        // Focusing the tab focuses its diff, where the keys for reading it work, or the view in
        // the diff's place.
        let on_focus = cx.on_focus(&focus_handle, window, |this, window, cx| {
            this.focus_middle(window, cx);
        });
        // Moving the tab to another pane removes it from one pane and adds it to another, so
        // only closing it, which releases it, lets go of what it holds.
        cx.on_release(Self::released).detach();
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
            trust,
            workspace,
            focus_handle,
            file_list_focus: cx.focus_handle(),
            file_list_scroll: UniformListScrollHandle::new(),
            pull: ShownPull::new(link, cx),
            kept: Vec::new(),
            pull_hidden: false,
            nav_history: None,
            diff_style: style,
            setting_style: style,
            hide_whitespace: false,
            header_slots: Default::default(),
            stack_tree: None,
            show_stack_tree: true,
            middle: Middle::default(),
            threads_panel: None,
            show_file_list: true,
            show_threads: true,
            opened: Instant::now(),
            _subscriptions: vec![on_focus, on_settings],
        }
    }

    /// Starts loading the pull request the tab shows.
    pub fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (link, trust, opened) = (self.pull.key.opened_as.clone(), self.trust, self.opened);
        let pull = self.handle(cx);
        self.pull._load = Some(cx.spawn_in(window, async move |_, cx| {
            // How many times GitHub was checked since a load last worked, so the waits keep
            // growing across loads that fail again.
            let mut checks = 0;
            // Only the load the user started may ask which account to use; one Lo starts by
            // itself never puts the picker up.
            let mut may_ask = true;
            loop {
                let saved = saved_details(&link, cx).await;
                let loaded = load(
                    pull.clone(),
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
                match pull.read_with(cx, |pull, _| (pull.showing_copy.is_some(), pull.answered)) {
                    Err(_) => return,
                    // GitHub gave the pull request, so its diff, or the copy shown, stays, with
                    // why the rest didn't load. Waiting won't fix that: the local copy tries
                    // again by itself for what it may.
                    Ok((_, true)) => {
                        log::error!("{link}: {error:#}");
                        pull.update(cx, |this, cx| {
                            this.pull.help_links = help_links(&error);
                            this.load_failed(&error, cx)
                        })
                        .ok();
                        return;
                    }
                    // The local copy stays, whatever kept GitHub's from loading, with why.
                    Ok((true, false)) => {
                        log::warn!("{link}: {error:#}; still showing the local copy");
                        let shown =
                            pull.update(cx, |this, cx| this.showing_copy_because(&error, cx));
                        if shown.is_err() {
                            return;
                        }
                    }
                    Ok((false, false)) if waitable => {
                        if let Err(error) =
                            show_offline(pull.clone(), link.clone(), opened, saved, error, cx).await
                        {
                            failed(&pull, &link, error, cx);
                        }
                    }
                    Ok((false, false)) => {
                        failed(&pull, &link, error, cx);
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
                if pull.update(cx, |this, cx| this.start_again(cx)).is_err() {
                    return;
                }
            }
        }));
    }

    /// The pull request shows with language servers from the trusted checkout in `worktree`.
    pub(crate) fn uses_checkout(&mut self, worktree: WorktreeId) {
        self.pull.checkout = Some(worktree);
    }

    /// The pull request is copied locally, as `local`.
    pub(crate) fn set_local_copy(&mut self, local: Arc<LocalPullRequest>) {
        self.pull.local = Some(local);
    }

    /// The tab closed: it lets go of every pull request it holds, shown or kept, so their
    /// pollers stop watching them, and a trusted checkout no other pull request uses leaves the
    /// project, which stops its language servers (design §11).
    fn released(&mut self, cx: &mut App) {
        // A question about its account is moot.
        account_picker::tab_closed(self.id, cx);
        let workspace = self.workspace.upgrade();
        let project = workspace
            .as_ref()
            .map(|workspace| workspace.read(cx).project().clone());
        let mut pulls = std::mem::take(&mut self.kept);
        // The tab is going, so its own pull requests count only while not yet released.
        let mut checkouts: Vec<WorktreeId> = std::iter::once(&self.pull)
            .chain(&pulls)
            .filter_map(|pull| pull.checkout)
            .collect();
        let others = other_tabs_checkouts(workspace.as_ref(), self.id, cx);
        let release = |pull: &mut ShownPull, checkouts: &mut Vec<WorktreeId>, cx: &mut App| {
            if let Some(checkout) = pull.checkout
                && let Some(position) = checkouts.iter().position(|other| *other == checkout)
            {
                checkouts.remove(position);
            }
            let used = |checkout| others.contains(&checkout) || checkouts.contains(&checkout);
            pull.release(project.as_ref(), used, cx);
        };
        release(&mut self.pull, &mut checkouts, cx);
        for pull in &mut pulls {
            release(pull, &mut checkouts, cx);
        }
    }

    /// Lets go of `pull`, a pull request the tab no longer shows or keeps, as
    /// [`ShownPull`]'s docs say it must be: its poller stops watching it, and its trusted
    /// checkout goes unless another pull request uses it.
    pub fn release_pull(&mut self, mut pull: ShownPull, cx: &mut Context<Self>) {
        let workspace = self.workspace.upgrade();
        let project = workspace
            .as_ref()
            .map(|workspace| workspace.read(cx).project().clone());
        let mut used: Vec<WorktreeId> = other_tabs_checkouts(workspace.as_ref(), self.id, cx);
        used.extend(self.pulls().filter_map(|pull| pull.checkout));
        pull.release(project.as_ref(), |checkout| used.contains(&checkout), cx);
    }

    /// Forgets why the pull request failed to load, before it loads again.
    fn start_again(&mut self, cx: &mut Context<Self>) {
        self.pull.acting = None;
        // A recent row's account is for the load it started.
        self.pull.preferred = None;
        self.pull.help_links.clear();
        self.pull.action = None;
        self.pull.answered = false;
        // The load before is dropped, and with it the copy its Retry button would try again.
        self.pull.retry_copy = None;
        self.pull.copy_failed = None;
        self.pull.github_failed = false;
        self.pull.banners.update(cx, |banners, cx| {
            banners.hide(BannerKind::LocalCopyFailed, cx)
        });
        self.set_status(
            Status::Loading("Finding an account that can see it…".into()),
            cx,
        );
    }

    /// The pull request the tab shows, and everything it knows about it.
    pub fn pull(&self) -> &ShownPull {
        &self.pull
    }

    /// Every pull request the tab holds: the one it shows, then those it keeps.
    pub fn pulls(&self) -> impl Iterator<Item = &ShownPull> {
        std::iter::once(&self.pull).chain(&self.kept)
    }

    /// The pull request `id`, shown or kept.
    pub fn pull_by_id(&self, id: PullId) -> Option<&ShownPull> {
        self.pulls().find(|pull| pull.id == id)
    }

    /// What work started for the pull request the tab shows holds, to write to it whatever the
    /// tab shows by then.
    pub fn handle(&self, cx: &Context<Self>) -> PullHandle {
        PullHandle::new(cx.weak_entity(), self.pull.id)
    }

    /// Runs `f` with pull request `id` as the tab's `pull`, the one the tab's methods act on, and
    /// returns what it returns, or `None` once the tab no longer holds it. A pull request the tab
    /// keeps out of sight is put in `pull`'s place while `f` runs, and sends no events and takes
    /// no focus meanwhile: what its load, its GitHub's diff or a viewed mark's write does lands on
    /// it, never on the pull request shown. `f` mustn't show or keep pull requests.
    pub fn with_pull<R>(
        &mut self,
        id: PullId,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Self, &mut Context<Self>) -> R,
    ) -> Option<R> {
        if self.pull.id == id && !self.pull_hidden {
            return Some(f(self, cx));
        }
        let index = self.kept.iter().position(|pull| pull.id == id)?;
        let was_hidden = std::mem::replace(&mut self.pull_hidden, true);
        std::mem::swap(&mut self.pull, &mut self.kept[index]);
        let result = f(self, cx);
        // `f` may have kept or released pull requests: the one in `pull` goes back to its place.
        std::mem::swap(&mut self.pull, &mut self.kept[index]);
        self.pull_hidden = was_hidden;
        Some(result)
    }

    /// Whether the tab shows the pull request its methods act on now, rather than one it keeps.
    fn shows_pull(&self) -> bool {
        !self.pull_hidden
    }

    /// Sends `event`, about the pull request shown; a kept one's changes wait until it shows.
    fn emit(&self, event: PrTabEvent, cx: &mut Context<Self>) {
        if self.shows_pull() {
            cx.emit(event);
        }
    }

    /// Shows `pull` in place of the pull request the tab shows, and returns that one, as a stack's
    /// tab does when it moves to another layer, with its diff, its banners and its load. Features
    /// hear [`PrTabEvent::PullChanged`] first, and start over. The pull request the poller polls
    /// most often follows, when it was this tab's.
    ///
    /// The returned pull request goes on loading only once the tab keeps it
    /// ([`Self::keep_pull`]); one the tab won't keep must be released ([`Self::release_pull`]).
    #[must_use = "a pull request the tab no longer shows must be kept or released"]
    pub fn show_pull(
        &mut self,
        pull: ShownPull,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ShownPull {
        debug_assert!(
            !self.pull_hidden,
            "work for a kept pull request swapped pull requests"
        );
        let open = self.is_poller_open(cx);
        if open {
            self.set_active(false, cx);
        }
        let shown = std::mem::replace(&mut self.pull, pull);
        self.pull_shown(open, window, cx);
        shown
    }

    /// Keeps `pull`, one the tab no longer shows, out of sight: its load and diff carry on, for
    /// when [`Self::show_kept_pull`] shows it again, and it's released with the tab.
    pub fn keep_pull(&mut self, pull: ShownPull) {
        debug_assert!(
            !self.pull_hidden,
            "work for a kept pull request kept another"
        );
        self.kept.push(pull);
    }

    /// Shows the pull request `id` the tab keeps, keeping the one it showed. Returns whether the
    /// tab keeps `id`.
    pub fn show_kept_pull(
        &mut self,
        id: PullId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        debug_assert!(
            !self.pull_hidden,
            "work for a kept pull request swapped pull requests"
        );
        let Some(index) = self.kept.iter().position(|pull| pull.id == id) else {
            return false;
        };
        let open = self.is_poller_open(cx);
        if open {
            self.set_active(false, cx);
        }
        std::mem::swap(&mut self.pull, &mut self.kept[index]);
        self.pull_shown(open, window, cx);
        true
    }

    /// The pull requests the tab keeps out of sight.
    pub fn kept_pulls(&self) -> &[ShownPull] {
        &self.kept
    }

    /// Whether the poller polls the pull request shown most often, as it does the active tab's.
    fn is_poller_open(&self, cx: &App) -> bool {
        (self.pull.watched.as_ref())
            .is_some_and(|(poller, watched)| poller.read(cx).open() == Some(watched))
    }

    /// Another pull request shows now: features hear so first, the poller polls it most often if
    /// it did the one before (`open`), and its diff takes the tab's layout and focus.
    fn pull_shown(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(PrTabEvent::PullChanged);
        if open {
            self.set_active(true, cx);
        }
        let focused = self.focus_handle.contains_focused(window, cx);
        self.apply_diff_style(window, cx);
        self.sync_whitespace(window, cx);
        self.scroll_to_selected();
        if focused {
            self.focus_diff(window, cx);
        }
        cx.emit(PrTabEvent::ComparisonChanged);
        self.changed(cx);
    }

    pub fn key(&self) -> &TabKey {
        &self.pull.key
    }

    pub fn link(&self) -> &PrLink {
        &self.pull.key.opened_as
    }

    pub fn acting(&self) -> Option<&Acting> {
        self.pull.acting.as_ref()
    }

    pub fn header(&self) -> &Header {
        &self.pull.header
    }

    pub fn files(&self) -> &FileList {
        &self.pull.files
    }

    pub fn revisions(&self) -> Option<&Revisions> {
        self.pull.revisions.as_ref()
    }

    pub fn status(&self) -> &Status {
        &self.pull.status
    }

    pub(crate) fn workspace(&self) -> &WeakEntity<Workspace> {
        &self.workspace
    }

    /// GitHub's links for fixing why the tab failed.
    pub fn help_links(&self) -> &[HelpLink] {
        &self.pull.help_links
    }

    pub fn banners(&self) -> &Entity<Banners> {
        &self.pull.banners
    }

    pub fn diff(&self) -> Option<&Entity<SplittableEditor>> {
        self.pull.diff.as_ref()
    }

    /// Where the diff shown comes from, once one shows.
    pub fn diff_source(&self) -> Option<DiffSource> {
        self.pull.diff_source
    }

    /// What the diff shown compares, when it's the local copy's.
    pub fn comparison(&self, cx: &App) -> Option<Arc<ShownComparison>> {
        shown_files::read(self.pull.diff.as_ref()?, cx, |files| {
            files.comparison.clone()
        })
        .flatten()
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
        self.pull.status = status;
        cx.notify();
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        self.emit(PrTabEvent::Changed, cx);
        cx.notify();
    }

    pub fn set_acting(&mut self, acting: Acting, cx: &mut Context<Self>) {
        // Viewed marks are the account's own, so another account's saved ones go once GitHub
        // answers for this one; until then, as when this load fails and the local copy stays,
        // they stay.
        if let Some((saved_for, _)) = &self.pull.saved {
            self.pull.viewed_are_others = acting
                .login()
                .is_none_or(|login| !login.eq_ignore_ascii_case(saved_for));
        }
        self.pull.acting = Some(acting);
        self.set_status(Status::Loading("Asking GitHub for it…".into()), cx);
    }

    /// Shows what Lo saved about the pull request, at once and online or not, until GitHub
    /// answers.
    pub(crate) fn show_saved(&mut self, saved: &Saved, cx: &mut Context<Self>) {
        self.take_details(&saved.details, saved.fetched_at, Some(&saved.login), cx);
        self.pull.saved = Some((saved.login.clone(), saved.fetched_at));
        self.show_saved_banner(true, cx);
    }

    /// The tab shows the local copy's diff with what Lo saved, because loading from GitHub
    /// failed with `why`: with GitHub's links to fix it, and when waiting won't fix it, a button
    /// to try again.
    fn showing_copy_because(&mut self, why: &anyhow::Error, cx: &mut Context<Self>) {
        let offline = is_offline(why);
        self.pull.showing_copy = Some(if offline {
            OlderCopy::Unreachable
        } else {
            OlderCopy::NotLoaded
        });
        self.show_saved_banner(false, cx);
        self.pull.help_links = help_links(why);
        let waitable = is_waitable(why);
        self.pull.action = (!waitable).then(|| action_for(why));
        let reason = sentence(why);
        let text = if offline {
            format!(
                "Offline: {reason}. This is the local copy's diff, read-only, until GitHub \
                 answers again; Lo keeps trying."
            )
        } else if is_retryable(why) {
            format!(
                "GitHub didn't answer: {reason}. This is the local copy's diff, read-only, until \
                 it does; Lo keeps trying."
            )
        } else if waitable {
            format!(
                "Lo couldn't load the pull request from GitHub: {reason}. This is the local \
                 copy's diff, read-only; Lo keeps trying."
            )
        } else if self.pull.action == Some(TabAction::ChooseAccount) {
            format!("{reason}. This is the local copy's diff, read-only.")
        } else {
            format!(
                "Lo couldn't load the pull request from GitHub: {reason}. This is the local \
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
            format!("Choose the account Lo uses for {subject}…")
        } else {
            format!("Waiting to ask which account Lo uses for {subject}…")
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
            Ok(heads) => self.pull.viewed_at = heads,
            Err(error) => log::error!("{pull}: reading where files were viewed: {error:#}"),
        }
    }

    /// Says how old the saved details the tab shows are, if it shows them.
    fn show_saved_banner(&self, asking: bool, cx: &mut Context<Self>) {
        let Some((login, fetched_at)) = &self.pull.saved else {
            return;
        };
        let mut text = format!(
            "Showing what Lo saved for {login} {}",
            poll_state::ago(*fetched_at, SystemTime::now())
        );
        text.push_str(if asking {
            ", until GitHub answers."
        } else {
            "."
        });
        self.pull.banners.update(cx, |banners, cx| {
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
        self.pull.header.title = Some(window_title::without_bidi(&pull_request.title));
        if let Some(user) = &pull_request.user {
            self.pull.header.author = Some(user.login.clone());
        }
        let repo = &pull_request.base.repo;
        self.pull.header.repo = Some(repo.full_name.clone());
        if let Ok((owner, name)) = repo.owner_and_name() {
            self.pull.key.canonical = Some((owner.to_string(), name.to_string()));
        }
        self.pull.created_at = Some(pull_request.created_at.into());
        // A diff on screen, such as the local copy shown offline, keeps its head until the new
        // copy replaces it; if GitHub's is newer, `e` won't mark files as of it meanwhile.
        if self.pull.diff.is_none() {
            self.set_head(&pull_request.head.sha, answered_at);
        }
        self.note_latest_head(&pull_request.head.sha, answered_at, cx);
        self.pull.api_head = Some(pull_request.head.sha.clone());
        self.pull.answered = true;
        self.pull.showing_copy = None;
        self.pull.status = Status::Loading("Copying it locally…".into());
        // GitHub answered for the account the tab acts as, whose viewed marks its details bring.
        if std::mem::take(&mut self.pull.viewed_are_others) {
            self.pull.marks.set_github(HashMap::new());
            self.pull.files.set_viewed(&self.pull.marks.shown());
        }
        // GitHub answered, and the account's poller brings its details up to date.
        if self.pull.saved.take().is_some() {
            self.pull
                .banners
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
        let (poller, _) = self.pull.watched.as_ref()?;
        Some(poller.read(cx).login().to_string())
    }

    /// Whether this Mac's records of where files were viewed follow `login`'s marks: those of
    /// the account the tab acts as and watches with, whose records the tab writes.
    fn records_follow(&self, login: Option<&str>, cx: &App) -> bool {
        let (Some(login), Some(watched)) = (login, self.watched_login(cx)) else {
            return false;
        };
        let acting = self.pull.acting.as_ref().and_then(Acting::login);
        watched.eq_ignore_ascii_case(login)
            && acting.is_some_and(|acting| acting.eq_ignore_ascii_case(login))
    }

    /// Takes the pull request's details as GitHub gave them at `fetched_at` to `login`, from the
    /// poller or from what Lo saved.
    fn take_details(
        &mut self,
        details: &PullRequestData,
        fetched_at: SystemTime,
        login: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        self.note_latest_head(&details.head_ref_oid, fetched_at, cx);
        let mut header = self.pull.header.clone();
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
        self.pull.node_id = Some(details.id.clone());
        // Details from before a write finished, such as the ones a tab switch hands back, don't
        // undo it.
        let viewed = self.pull.marks.without_stale(viewed, fetched_at);
        // Only the watched account's marks say where it viewed files.
        if self.records_follow(login, cx) {
            self.follow_marks_set_elsewhere(&viewed, &details.head_ref_oid, cx);
        }
        let listed_changed = self.pull.marks.set_listed(listed_paths(details));
        let marks_changed = self.pull.marks.set_github(viewed) || listed_changed;
        // A change of title makes the workspace report the active item changed, which comes back
        // here through `set_active`, so only a real change is reported.
        if header == self.pull.header && !marks_changed {
            return;
        }
        self.pull.header = header;
        self.pull.files.set_viewed(&self.pull.marks.shown());
        self.changed(cx);
    }

    /// GitHub's `marks`, read at `head`, changed in ways Lo's own writes don't explain, so this
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
            .filter(|(path, _)| !self.pull.marks.is_pending(path))
            .filter_map(|(path, state)| match state {
                ViewedState::Unviewed if self.pull.viewed_at.contains_key(path) => {
                    Some((path.clone(), None))
                }
                ViewedState::Viewed
                    if matches!(
                        self.pull.marks.github_state(path),
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
                Some(head) => self.pull.viewed_at.insert(path.clone(), head.clone()),
                None => self.pull.viewed_at.remove(path),
            };
        }
        let (Some(records), Some((poller, pull))) =
            (self.pull.records.clone(), self.pull.watched.clone())
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
        // Until Lo reads the branch's push history (milestone 6), the head the local copy
        // fetched is the one revision it knows.
        let records = [PushRecord::Polled {
            head: head.as_str().to_string(),
            at: polled_at,
        }];
        self.pull.revisions = Some(build_revisions(
            &records,
            &[],
            head.as_str(),
            self.pull.created_at.unwrap_or(polled_at),
        ));
        self.pull.status = Status::Loading("Loading the diff…".into());
        // The list may reorder, as files its new entries say are generated go last; the file
        // selected stays selected and in view.
        self.scroll_to_selected();
        cx.notify();
        self.pull.files.review_positions()
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
        list.set_order(self.pull.files.order());
        list.set_viewed(&self.pull.marks.shown());
        if let Some(selected) = self.pull.files.selected() {
            list.select_path(selected.path());
        }
        self.pull.files = list;
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
        let this = self.handle(cx);
        self.pull._github_diff = Some(cx.spawn_in(window, async move |_, cx| {
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
                this.pull.banners.update(cx, |banners, cx| {
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
            let tab = this.tab().clone();
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
        let this = self.handle(cx);
        self.pull._github_diff = Some(cx.spawn_in(window, async move |_, cx| {
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
        if self.pull.diff.is_some() {
            return None;
        }
        // The local copy fetches the head GitHub gave before it listed the files.
        if let (Some(head), Some(api_head)) = (&head, &self.pull.api_head)
            && head != api_head
        {
            let text = format!(
                "This pull request moved on while Lo read it. GitHub's diff shows {}, and the \
                 local copy's diff shows {} once it's ready.",
                short(head),
                short(api_head)
            );
            self.pull.banners.update(cx, |banners, cx| {
                banners.show(BannerKind::MovedOn, Severity::Warning, text, cx)
            });
        }
        // The head on screen is the one GitHub listed the files at, which GitHub says is its
        // latest.
        if let Some(head) = &head {
            self.set_head(head, listed_at);
            self.note_latest_head(head, listed_at, cx);
        }
        self.pull.github_head = head;
        self.list(entries);
        self.show_diff(editor, DiffSource::GitHub, window, cx);
        // The local copy failed for good, so this is the diff the tab shows.
        if self.pull.copy_failed.is_some() {
            self.set_status(Status::Ready, cx);
        }
        Some(self.pull.files.review_positions())
    }

    /// GitHub's diff couldn't be read for a reason that may pass, and is read again soon.
    fn github_diff_retried(&mut self, error: &GithubError, cx: &mut Context<Self>) {
        if self.pull.diff.is_some() {
            return;
        }
        self.pull.banners.update(cx, |banners, cx| {
            banners.show(
                BannerKind::GithubDiffFailed,
                Severity::Info,
                format!(
                    "Lo couldn't read GitHub's diff, and tries again until the local copy's \
                     diff is ready: {error}"
                ),
                cx,
            )
        });
    }

    /// GitHub's diff can't be read, and isn't tried again.
    fn github_diff_failed(&mut self, error: &GithubError, cx: &mut Context<Self>) {
        self.pull.github_failed = true;
        if self.pull.diff.is_some() {
            return;
        }
        // Neither diff can show.
        if let Some(copy_failed) = self.pull.copy_failed.clone() {
            self.pull.banners.update(cx, |banners, cx| {
                banners.hide(BannerKind::LocalCopyFailed, cx)
            });
            self.set_status(Status::Failed(copy_failed), cx);
            return;
        }
        self.pull.banners.update(cx, |banners, cx| {
            banners.show(
                BannerKind::GithubDiffFailed,
                Severity::Info,
                format!(
                    "Lo couldn't read GitHub's diff, so the diff shows once the local copy is \
                     ready: {error}"
                ),
                cx,
            )
        });
    }

    /// GitHub's diff wasn't read, to save the requests GitHub allows without signing in.
    fn github_diff_not_read(&mut self, cx: &mut Context<Self>) {
        self.pull.github_failed = true;
        if self.pull.diff.is_some() {
            return;
        }
        if let Some(copy_failed) = self.pull.copy_failed.clone() {
            self.set_status(Status::Failed(copy_failed), cx);
        }
        self.pull.banners.update(cx, |banners, cx| {
            banners.show(
                BannerKind::GithubDiffFailed,
                Severity::Info,
                "Lo didn't read GitHub's diff, to keep the few requests GitHub allows without \
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
            self.pull.banners.update(cx, |banners, cx| {
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
        let text = format!("Lo couldn't copy this pull request locally: {why}. {when}{meanwhile}");
        self.pull.banners.update(cx, |banners, cx| {
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
        if self.pull.diff_source == Some(DiffSource::GitHub) {
            " Until then, this is GitHub's diff: changed lines only.".to_string()
        } else if self.shows_older_copy() {
            let shown = self.pull.head.as_deref().map(short).unwrap_or_default();
            format!(" Until then, this is the local copy of {shown}, an older push.")
        } else {
            String::new()
        }
    }

    /// Says Lo stopped trying to copy the pull request locally after `tries` failures for
    /// `why`, a reason it doesn't know, with a button that tries again through `retry`.
    pub(crate) fn copy_stopped(
        &mut self,
        why: &str,
        tries: usize,
        retry: oneshot::Sender<()>,
        cx: &mut Context<Self>,
    ) {
        self.pull.retry_copy = Some(retry);
        let text = format!(
            "Lo stopped trying to copy this pull request locally after {tries} tries: {why}.{}",
            self.meanwhile()
        );
        let pull = self.handle(cx);
        let action = BannerAction::new("Retry", move |_, cx| {
            pull.update(cx, |tab, cx| tab.retry_copy(cx)).ok();
        });
        self.pull.banners.update(cx, |banners, cx| {
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
        } else if self.pull.diff.is_some() {
            Status::Ready
        } else if self.pull._github_diff.is_some() && !self.pull.github_failed {
            // GitHub's diff shows when it arrives, and it's all there is until a retry.
            self.pull.copy_failed = Some(why.to_string().into());
            Status::Loading("Reading GitHub's diff…".into())
        } else {
            Status::Failed(why.to_string().into())
        };
        self.set_status(status, cx);
    }

    /// Tries copying the pull request locally again, after Lo stopped trying.
    pub(crate) fn retry_copy(&mut self, cx: &mut Context<Self>) {
        let Some(retry) = self.pull.retry_copy.take() else {
            return;
        };
        retry.send(()).ok();
        self.pull.copy_failed = None;
        if self.pull.showing_copy == Some(OlderCopy::NewerNotCopied) {
            self.pull.showing_copy = None;
        }
        self.pull.banners.update(cx, |banners, cx| {
            banners.hide(BannerKind::LocalCopyFailed, cx)
        });
        self.set_status(Status::Loading("Copying it locally…".into()), cx);
    }

    /// Whether the diff on screen is a local copy of an older head than the one GitHub gave
    /// this load, as when the copy shown offline waits for a newer push to be copied.
    fn shows_older_copy(&self) -> bool {
        self.pull.diff_source == Some(DiffSource::LocalCopy)
            && self.pull.api_head.is_some()
            && self.pull.head != self.pull.api_head
    }

    /// GitHub gave a newer head than the local copy on screen, and copying it failed for `why`:
    /// the older copy stays, read-only, saying so.
    fn keep_older_copy(&mut self, why: &str, cx: &mut Context<Self>) {
        self.pull.showing_copy = Some(OlderCopy::NewerNotCopied);
        let latest = self.pull.api_head.as_deref().map(short).unwrap_or_default();
        let shown = self.pull.head.as_deref().map(short).unwrap_or_default();
        let why = why.trim_end().trim_end_matches('.');
        let text = format!(
            "GitHub has {latest}, but Lo couldn't copy it: {why}. This is the local copy of \
             {shown}, read-only."
        );
        self.set_status(Status::Offline(text.into()), cx);
    }

    /// Shows that the local copy's diff is loading file `file` of its `total`, counting from 1.
    pub(crate) fn loading_diff(&mut self, file: usize, total: usize, cx: &mut Context<Self>) {
        let what = if self.pull.diff_source == Some(DiffSource::GitHub) {
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
        let github_coming = self.pull.diff.is_none()
            && self.pull._github_diff.is_some()
            && !self.pull.github_failed;
        if self.pull.diff.is_none() && !github_coming {
            self.pull.action = Some(TabAction::TryAgain);
            self.set_status(Status::Failed(format!("{error:#}").into()), cx);
            return;
        }
        if self.shows_older_copy() {
            self.pull.action = Some(TabAction::TryAgain);
            self.keep_older_copy(&format!("{error:#}"), cx);
            return;
        }
        if github_coming {
            self.pull.copy_failed = Some(format!("{error:#}").into());
            self.pull.banners.update(cx, |banners, cx| {
                banners.show(
                    BannerKind::LocalCopyFailed,
                    Severity::Error,
                    format!(
                        "Lo couldn't show the local copy's diff, so this is GitHub's diff: \
                         changed lines only. {error:#}"
                    ),
                    cx,
                )
            });
            self.set_status(Status::Loading("Reading GitHub's diff…".into()), cx);
            return;
        }
        let text = match self.pull.diff_source {
            Some(DiffSource::GitHub) => format!(
                "Lo couldn't show the local copy's diff, so this is GitHub's diff: changed \
                 lines only. {error:#}"
            ),
            _ => format!("Lo couldn't finish loading this pull request: {error:#}"),
        };
        self.pull.banners.update(cx, |banners, cx| {
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
        if let Some(old) = self.pull.diff.as_ref().filter(|old| **old != editor) {
            shown_files::release_images(old, Some(window), cx);
        }
        // Loads into a diff no longer shown stop, and let it go.
        let shown = editor.entity_id();
        self.pull.loading.retain(|(diff, _), _| *diff == shown);
        self.pull._diff_watch = vec![
            cx.observe_in(&editor, window, |_, diff, window, cx| {
                diff_decorations::sync_split(&diff, window, cx)
            }),
            cx.subscribe_in(
                &editor,
                window,
                |_, diff, event: &EditorEvent, window, cx| {
                    if let EditorEvent::BufferFoldToggled { ids, folded } = event {
                        shown_files::fold_toggled(diff, ids, *folded, window, cx);
                    }
                },
            ),
        ];
        let rhs = editor.read(cx).rhs_editor().clone();
        if let Some(history) = self.nav_history.clone() {
            rhs.update(cx, |editor, _| editor.set_nav_history(Some(history)));
        }
        // The diff's pull request's, whether the tab shows it or keeps it by then.
        let id = self.pull.id;
        self.pull._diff_cursor = Some(cx.subscribe_in(
            &rhs,
            window,
            move |this, _, event: &EditorEvent, window, cx| {
                this.with_pull(id, cx, |this, cx| match event {
                    EditorEvent::SelectionsChanged { .. } => this.diff_cursor_moved(cx),
                    EditorEvent::ScrollPositionChanged { .. } => this.select_shown_file(cx),
                    EditorEvent::BufferRangesUpdated { .. } => this.park_cursor(window, cx),
                    _ => {}
                });
            },
        ));
        self.pull.parked_cursor = None;
        self.pull.diff = Some(editor.clone());
        self.pull.diff_source = Some(source);
        if source == DiffSource::LocalCopy {
            self.pull._github_diff = None;
            self.pull.banners.update(cx, |banners, cx| {
                banners.hide(BannerKind::GithubDiffFailed, cx);
                banners.hide(BannerKind::GithubDiffCut, cx);
            });
        }
        // A diff with its files in already, as when the local copy's replaces GitHub's, sends no
        // event as they arrive, so its cursor would stay at its end, in the last file.
        self.pull.cursor_moved = false;
        self.park_cursor(window, cx);
        if focused && self.shows_pull() && !self.file_list_focus.contains_focused(window, cx) {
            window.focus(&editor.focus_handle(cx), cx);
        }
        self.emit(PrTabEvent::ComparisonChanged, cx);
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
            .pull
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
        self.pull.banners.update(cx, |banners, cx| match moved_on {
            Some(text) => banners.show(BannerKind::MovedOn, Severity::Warning, text, cx),
            None => banners.hide(BannerKind::MovedOn, cx),
        });
        let (position, cursor) = match &self.pull.diff {
            Some(diff) => (
                pr_diff::scroll_position(diff, cx),
                pr_diff::cursor_position(diff, cx),
            ),
            None => (None, None),
        };
        // Files stay open or folded as the reviewer left them, before the view scrolls to the
        // same line.
        let to_load = match self.pull.diff.clone() {
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
        diff_decorations::redecorate(&editor, Redecorate::Replaced, window, cx);
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
        if let Some((old, old_pull)) = self.pull.watched.take() {
            if old != poller || old_pull != pull {
                old.update(cx, |old, cx| old.unwatch(&old_pull, cx));
            }
            if !old
                .read(cx)
                .login()
                .eq_ignore_ascii_case(poller.read(cx).login())
            {
                self.pull.marks = ViewedMarks::default();
                self.pull.viewed_at.clear();
                self.pull.node_id = None;
                self.pull.files.set_viewed(&self.pull.marks.shown());
            }
        }
        if let Some(records) = &records {
            match records.heads(poller.read(cx).login(), &pull) {
                Ok(heads) => self.pull.viewed_at = heads,
                Err(error) => log::error!("{pull}: reading where files were viewed: {error:#}"),
            }
        }
        self.pull.records = records;
        poller.update(cx, |poller, cx| poller.watch(pull.clone(), cx));
        self.pull.poll.status = poller.read(cx).status().clone();
        // What the poller says goes to this pull request, whether the tab shows it or keeps it.
        let id = self.pull.id;
        self.pull._poller_subscriptions = vec![
            cx.observe(&poller, move |this, poller, cx| {
                let status = poller.read(cx).status().clone();
                this.with_pull(id, cx, |this, cx| {
                    if this.pull.poll.status != status {
                        this.pull.poll.status = status;
                        cx.notify();
                    }
                });
            }),
            cx.subscribe(&poller, move |this, _, event, cx| {
                this.with_pull(id, cx, |this, cx| this.poll_event(event, cx));
            }),
        ];
        self.pull.watched = Some((poller, pull));
    }

    fn poll_event(&mut self, event: &PollEvent, cx: &mut Context<Self>) {
        let Some((_, watched)) = &self.pull.watched else {
            return;
        };
        match event {
            PollEvent::Details { pull, details } if pull == watched => {
                self.pull.poll.details_at = Some(SystemTime::now());
                self.poll_succeeded(cx);
                self.set_details(details, cx);
            }
            PollEvent::Summary { pull, summary } if pull == watched => {
                self.note_latest_head(&summary.head.sha, SystemTime::now(), cx);
                self.poll_succeeded(cx);
                let title = window_title::without_bidi(&summary.title);
                if self.pull.header.title.as_ref() != Some(&title) {
                    self.pull.header.title = Some(title);
                    self.changed(cx);
                }
            }
            PollEvent::Check { pull, .. } if pull == watched => self.poll_succeeded(cx),
            PollEvent::Failed {
                target: Target::Pull(pull),
                error,
            } if pull == watched => {
                log::warn!("polling {pull}: {error}");
                self.pull.poll.failure = Some(error.clone());
                cx.notify();
            }
            _ => {}
        }
    }

    fn poll_succeeded(&mut self, cx: &mut Context<Self>) {
        if self.pull.poll.failure.take().is_some() {
            cx.notify();
        }
    }

    /// GitHub said at `at` that the pull request's head is `head`. Something it said earlier
    /// doesn't replace something it said later. Features hear of a new head.
    fn note_latest_head(&mut self, head: &str, at: SystemTime, cx: &mut Context<Self>) {
        let latest = &mut self.pull.latest_head;
        if latest.as_ref().is_some_and(|(_, seen_at)| at < *seen_at) {
            return;
        }
        let changed = latest.as_ref().is_none_or(|(latest, _)| latest != head);
        *latest = Some((head.to_string(), at));
        if changed {
            self.emit(PrTabEvent::HeadChanged, cx);
        }
    }

    /// The diff shows the pull request at `head`, which GitHub gave at `given_at`.
    fn set_head(&mut self, head: &str, given_at: SystemTime) {
        if self.pull.head.as_deref() != Some(head) {
            self.pull.head = Some(head.to_string());
            self.pull.head_at = Some(given_at);
        }
    }

    /// GitHub's latest head, when GitHub gave it no earlier than it gave the head on screen and
    /// it isn't that head.
    fn newer_head(&self) -> Option<&str> {
        let (latest, at) = self.pull.latest_head.as_ref()?;
        let newer = Some(latest) != self.pull.head.as_ref()
            && self.pull.head_at.is_none_or(|head_at| *at >= head_at);
        newer.then_some(latest.as_str())
    }

    /// The window's title while this is the active tab.
    pub fn window_title(&self) -> String {
        window_title::for_pull_request(
            &self.repo_name(),
            self.link().number,
            self.pull.header.title.as_deref(),
        )
    }

    /// Whether this is the window's active tab. The account's poller polls the active tab's pull
    /// request most often.
    fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        let Some((poller, pull)) = self.pull.watched.clone() else {
            return;
        };
        let is_open = poller.read(cx).open() == Some(&pull);
        if active && !is_open {
            let answer = poller.update(cx, |poller, cx| poller.set_open(Some(pull), cx));
            if let Some(answer) = answer {
                self.pull.poll.details_at = Some(answer.fetched_at);
                let login = poller.read(cx).login().to_string();
                self.take_details(&answer.details, answer.fetched_at, Some(&login), cx);
            }
        } else if !active && is_open {
            poller.update(cx, |poller, cx| poller.set_open(None, cx));
        }
    }

    fn apply_diff_style(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(diff) = &self.pull.diff {
            shown_files::set_style(diff, self.diff_style, window, cx);
        }
    }

    /// Brings each of the diff's files in line with whether whitespace changes are hidden.
    pub(crate) fn sync_whitespace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(diff) = self.pull.diff.clone() else {
            return;
        };
        let hide = self.hide_whitespace;
        // Replacing the task stops the one before, so no file is changed by two at once.
        self.pull._whitespace = Some(cx.spawn_in(window, async move |_, cx| {
            shown_files::hide_whitespace(diff, hide, cx).await
        }));
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
        if self.pull.loading.contains_key(&key) {
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
        let this = self.handle(cx);
        let task = cx.spawn_in(window, {
            let key = key.clone();
            async move |_, cx| {
                let loaded = pr_diff::load_large_file(diff, project, path.clone(), hide, cx).await;
                this.update_in(cx, |this, window, cx| {
                    this.pull.loading.remove(&key);
                    if let Err(error) = loaded {
                        log::error!("can't load {path}: {error:#}");
                        this.pull.banners.update(cx, |banners, cx| {
                            banners.show(
                                BannerKind::LoadFileFailed,
                                Severity::Warning,
                                format!("Lo couldn't load {path}: {error:#}"),
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
        self.pull.loading.insert(key, task);
    }

    /// The file `e` and the movement keys act on: the one the diff shows
    /// ([`diff_cursor::shown_file`]), by its position in review order and its path. `None` until
    /// the diff shows a file.
    fn shown_file(&self, cx: &mut App) -> Option<(usize, String)> {
        diff_cursor::shown_file(self.pull.diff.as_ref()?, cx)
    }

    /// The path GitHub knows the listed file at `path` by ([`ViewedMarks::github_path`]).
    fn github_path(&self, path: &str) -> Option<String> {
        let previous = self.pull.files.entry(path)?.file.previous_path.as_deref();
        self.pull.marks.github_path(path, previous)
    }

    /// Where `path`'s file starts in the diff, once it's there.
    fn location_in_diff(&self, path: &str, cx: &App) -> Option<Anchor> {
        let diff = self.pull.diff.as_ref()?;
        let position = self.pull.files.review_position(path)?;
        let key = pr_diff::path_key(position, RelPath::from_unix_str(path).ok()?.into_arc());
        let multi_buffer = diff.read(cx).rhs_editor().read(cx).buffer().clone();
        multi_buffer.read(cx).location_for_path(&key, cx)
    }

    /// The diff's cursor head, on its right side.
    fn diff_cursor(&self, cx: &App) -> Option<Anchor> {
        let diff = self.pull.diff.as_ref()?.read(cx);
        Some(diff.rhs_editor().read(cx).selections.newest_anchor().head())
    }

    /// Files arrived in the diff. Until the cursor is moved, it stays at the diff's start, in the
    /// first file in review order whichever file arrived first. An empty editor's cursor sits at
    /// its end, which would otherwise leave it in the last file.
    fn park_cursor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pull.cursor_moved {
            return;
        }
        let (Some(diff), Some(head)) = (&self.pull.diff, self.diff_cursor(cx)) else {
            return;
        };
        // Moved since it was parked, though the move's event hasn't arrived yet.
        if self.pull.parked_cursor.is_some_and(|parked| parked != head) {
            return;
        }
        let rhs = diff.read(cx).rhs_editor().clone();
        self.pull.parked_cursor = Some(rhs.update(cx, |editor, cx| {
            editor.change_selections(SelectionEffects::no_scroll(), window, cx, |selections| {
                selections.select_ranges([Point::zero()..Point::zero()])
            });
            editor.selections.newest_anchor().head()
        }));
    }

    /// The diff's cursor moved: the list selects the file the diff shows. Where the tab parked it
    /// isn't a move, but the first real move ends the parking, even one back to the same place.
    fn diff_cursor_moved(&mut self, cx: &mut Context<Self>) {
        if !self.pull.cursor_moved {
            if self.diff_cursor(cx) == self.pull.parked_cursor {
                return;
            }
            self.pull.cursor_moved = true;
            self.pull.parked_cursor = None;
        }
        self.select_shown_file(cx);
    }

    /// The list selects the file the diff shows, the one `e` acts on.
    fn select_shown_file(&mut self, cx: &mut Context<Self>) {
        let Some((_, path)) = self.shown_file(cx) else {
            return;
        };
        if self.pull.files.selected().map(FileEntry::path) != Some(path.as_str()) {
            self.pull.files.select_path(&path);
            self.scroll_to_selected();
            cx.notify();
        }
    }

    /// Focuses what the middle shows, or the tab until the diff exists.
    fn focus_diff(&self, window: &mut Window, cx: &mut App) {
        if !self.focus_middle(window, cx) {
            window.focus(&self.focus_handle, cx);
        }
    }

    /// Focuses the middle if `part`, which is about to hide or go, has focus, so keys keep
    /// reaching the tab.
    fn focus_diff_from(&self, part: Option<&PartView>, window: &mut Window, cx: &mut App) {
        if part.is_some_and(|part| part.focus.contains_focused(window, cx)) {
            self.focus_diff(window, cx);
        }
    }

    fn scroll_to_selected(&self) {
        // A kept pull request's list scrolls to its selection when it shows.
        if !self.shows_pull() {
            return;
        }
        if let Some(row) = self.pull.files.selected_row() {
            self.file_list_scroll
                .scroll_to_item(row, gpui::ScrollStrategy::Center);
        }
    }

    fn selection_moved(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.scroll_to_selected();
        self.reveal_selected(window, cx);
        cx.notify();
    }

    /// Scrolls the diff to the selected file.
    fn reveal_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(diff), Some(entry)) = (&self.pull.diff, self.pull.files.selected()) else {
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
        self.pull.header.repo.clone().unwrap_or_else(|| {
            let link = self.link();
            format!("{}/{}", link.owner, link.repo)
        })
    }

    /// GitHub's links for fixing why the tab stopped, and its button to try again or choose an
    /// account.
    fn render_actions(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let links = self
            .pull
            .help_links
            .iter()
            .enumerate()
            .map(|(index, link)| {
                let url = link.url().to_string();
                Button::new(("help-link", index), link.label())
                    .style(ButtonStyle::Filled)
                    .on_click(move |_, _, cx| cx.open_url(&url))
                    .into_any_element()
            });
        let action = self.pull.action.map(|action| {
            Button::new("tab-action", action.label())
                .on_click(cx.listener(|this, _, window, cx| this.load_again(window, cx)))
                .into_any_element()
        });
        links.chain(action).collect()
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

/// GitHub's viewed mark as Lo's review model has it.
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
    let key = tab.read(cx).pull.key.clone();
    let Some(twin) = workspace
        .items_of_type::<PrTab>(cx)
        .find(|other| other != tab && other.read(cx).pull.key.is_same_pull_request(&key))
    else {
        return false;
    };
    log::info!(
        "{} is {} by another name; keeping that tab",
        key.opened_as,
        twin.read(cx).pull.key.opened_as
    );
    twin.update(cx, |twin, cx| {
        twin.pull.key.merge(key);
        if twin.pull.status.tries_again() {
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

/// What Lo saved about `link`'s pull request, with any account.
async fn saved_details(link: &PrLink, cx: &mut AsyncWindowContext) -> Option<Saved> {
    let layer = cx.update(|_, cx| github_layer::ready(cx)).ok()?.await?;
    let pull = PullRef::new(&link.owner, &link.repo, i64::try_from(link.number).ok()?);
    cx.update(|_, cx| layer.read(cx).saved_details(&pull, cx))
        .ok()?
}

/// The tab failed to load: says why, with GitHub's links to fix it.
fn failed(this: &PullHandle, link: &PrLink, error: anyhow::Error, cx: &mut AsyncWindowContext) {
    log::error!("{link}: {error:#}");
    let waitable = is_waitable(&error);
    let text = if waitable {
        format!("{}. Lo keeps trying GitHub.", sentence(&error))
    } else {
        format!("{}.", sentence(&error))
    };
    this.update(cx, |this, cx| {
        this.pull.help_links = help_links(&error);
        this.pull.action = (!waitable).then(|| action_for(&error));
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
/// locally and shows its diff. What Lo `saved` about it shows until GitHub answers. When
/// several accounts can see it and none was chosen, it asks which to use only if it `may_ask`.
async fn load(
    this: PullHandle,
    link: PrLink,
    trust: Trust,
    opened: Instant,
    saved: &Option<Saved>,
    may_ask: bool,
    cx: &mut AsyncWindowContext,
) -> anyhow::Result<()> {
    let layer = cx.update(|_, cx| github_layer::ready(cx))?.await;
    let pull = PullRef::new(&link.owner, &link.repo, i64::try_from(link.number)?);
    // What Lo saved shows at once, and offline.
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
        let preferred = this.read_with(cx, |pull, _| pull.preferred.clone())?;
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
                        "Choose which account Lo uses for {}",
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
        let other_names = this.read_with(cx, |pull, _| pull.key.other_names())?;
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
        if this.pull.diff.is_none() {
            this.load_github_diff(reader, &pull_request, window, cx)
        }
    })?;

    // A tab opened by another of the repository's names may show it already.
    let workspace = this.update(cx, |this, _| this.workspace.clone())?;
    let tab = this.tab().upgrade().context("the tab closed")?;
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
    /// Asking which account to use, which Lo doesn't do by itself.
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

/// The tab needs the user to choose which account to use for `subject`: Lo's own load didn't
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
/// copy, if it has the head Lo `saved`, with the saved details and their age (design §11
/// "Offline"). Without a copy, it's `offline` again, and the tab shows the saved details alone.
async fn show_offline(
    this: PullHandle,
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
        this.pull.showing_copy = Some(if is_offline(&offline) {
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
            this.pull.showing_copy = None;
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
/// as GitHub names it now, so Lo asks once per owner; or for the repository alone, when the
/// owner's account can't see it. `None` when an account was added or signed in meanwhile, so
/// which accounts can see it should be checked again.
async fn ask_for_account(
    this: &PullHandle,
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
             the Lo menu, then try again"
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

/// The trusted checkouts the pull requests of `workspace`'s tabs other than `tab` use.
fn other_tabs_checkouts(
    workspace: Option<&Entity<Workspace>>,
    tab: EntityId,
    cx: &App,
) -> Vec<WorktreeId> {
    let Some(workspace) = workspace else {
        return Vec::new();
    };
    workspace
        .read(cx)
        .items_of_type::<PrTab>(cx)
        .filter(|other| other.entity_id() != tab)
        .flat_map(|other| {
            let other = other.read(cx);
            other
                .pulls()
                .filter_map(|pull| pull.checkout)
                .collect::<Vec<_>>()
        })
        .collect()
}

impl Focusable for PrTab {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PrTab {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let background = cx.theme().colors().editor_background;
        let github = self.pull.diff_source == Some(DiffSource::GitHub);
        v_flex()
            .key_context(keys::tab_context(&self.middle))
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
            .map(|this| keys::on_actions(this, self, cx))
            .size_full()
            .bg(background)
            .child(self.render_header(cx))
            .children(self.render_status(cx))
            .children(pr_banner::render(self.pull.banners.read(cx).messages()))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .children(self.render_left(cx))
                    .child(self.render_middle(cx))
                    .children(self.render_right(cx)),
            )
    }
}

impl Item for PrTab {
    type Event = PrTabEvent;

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        let repo = self.repo_name();
        let name = repo.rsplit('/').next().unwrap_or(&repo);
        tab_title(name, self.link().number, self.pull.header.title.as_deref()).into()
    }

    fn tab_content(&self, params: TabContentParams, _window: &Window, cx: &App) -> AnyElement {
        Label::new(self.tab_content_text(0, cx))
            .single_line()
            .color(params.text_color())
            .into_any_element()
    }

    fn tab_tooltip_text(&self, _: &App) -> Option<SharedString> {
        let pull = format!("{}#{}", self.repo_name(), self.link().number);
        Some(match &self.pull.header.title {
            Some(title) => format!("{pull}: {title}").into(),
            None => pull.into(),
        })
    }

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::PullRequest).color(Color::Muted))
    }

    fn to_item_events(event: &PrTabEvent, f: &mut dyn FnMut(ItemEvent)) {
        match event {
            PrTabEvent::Changed | PrTabEvent::PullChanged => f(ItemEvent::UpdateTab),
            PrTabEvent::HeadChanged | PrTabEvent::ComparisonChanged => {}
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
        Some(Box::new(self.pull.diff.clone()?))
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
            Some(self.pull.diff.clone()?.into())
        } else if type_id == std::any::TypeId::of::<Editor>() {
            Some(
                self.pull
                    .diff
                    .as_ref()?
                    .read(cx)
                    .rhs_editor()
                    .clone()
                    .into(),
            )
        } else {
            None
        }
    }

    fn deactivated(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(diff) = &self.pull.diff {
            diff.update(cx, |diff, cx| diff.deactivated(window, cx));
        }
    }

    fn navigate(
        &mut self,
        data: std::sync::Arc<dyn std::any::Any + Send>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        // Back and forward move the diff's cursor, so only while the diff shows.
        if !self.middle.shows_diff() {
            return false;
        }
        match &self.pull.diff {
            Some(diff) => diff.update(cx, |diff, cx| diff.navigate(data, window, cx)),
            None => false,
        }
    }

    fn set_nav_history(&mut self, history: ItemNavHistory, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(diff) = &self.pull.diff {
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
mod tests;
