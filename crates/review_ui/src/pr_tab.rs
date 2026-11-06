//! The PR tab: one pull request's header, its files in review order, its side-by-side diff and its
//! threads (design §10 "Window"). rozed's window has one tab per pull request, and opening a pull
//! request that's already open switches to its tab.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::time::{Instant, SystemTime};

use anyhow::Context as _;

use editor::scroll::Autoscroll;
use editor::{Anchor, Direction, Editor, EditorEvent, SelectionEffects, SplittableEditor};
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
use repo_cache::{ChangedFile, Oid};
use review::revisions::{PushRecord, Revisions, ViewedState, build_revisions};
use ui::{ListItem, ListItemSpacing, Tooltip, prelude::*};
use util::rel_path::RelPath;
use workspace::item::{Item, ItemEvent, TabContentParams};
use workspace::notifications::NotificationId;
use workspace::{ItemNavHistory, Toast, Workspace};

use crate::account::{self, AccountDecision, Acting, Refusal};
use crate::diff_cursor;
use crate::file_list::{FileChange, FileEntry, FileList, FileOrder};
use crate::github_api::{self, CanSignIn, PullRequest};
use crate::github_layer::{self, Saved};
use crate::poll_state::{self, PollNote, PollState};
use crate::pr_banner::{self, BannerKind, Banners};
use crate::pr_diff::{self, Trust};
use crate::pr_link::PrLink;
use crate::tabs::{TabKey, find_tab};
use crate::viewed_marks::{Toggle, ViewedMarks, changed_since_viewed, write_failed};

actions!(
    review_ui,
    [
        /// Shows or hides the file list.
        ToggleFileList,
        /// Shows or hides the threads panel.
        ToggleThreads,
        /// Switches the file list between review order and path order.
        ToggleFileOrder,
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
/// The tab outside any text box in it, such as a comment box: keys that type a character work
/// here, so a text box still types them. The diff's own editors are the only full-size ones.
/// Milestone 12 binds rozed's diff keys in a context deeper than vim's.
const READING_CONTEXT: &str = "PrTab && !(Editor && mode != full)";

/// The tab's keys. Panels toggle with Zed's keys for its docks.
pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys([
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
    Failed(SharedString),
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
    github_head: Option<(String, SystemTime)>,
    /// Where this Mac's records say each file was marked viewed, by path.
    viewed_at: HashMap<String, String>,
    /// This Mac's records of where files were marked viewed, once signed in.
    records: Option<ViewedRecords>,
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
    /// Where the tab put the diff's cursor while files arrive, until it's moved.
    parked_cursor: Option<Anchor>,
    /// Whether the diff's cursor was moved, by a key, a click or the file list. From then on the
    /// tab leaves it where it is.
    cursor_moved: bool,
    /// Keeps the file list's selection on the file the diff's cursor is in, and the cursor at the
    /// start until it's moved.
    _diff_cursor: Option<Subscription>,
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
            marks: ViewedMarks::default(),
            node_id: None,
            head: None,
            head_at: None,
            github_head: None,
            viewed_at: HashMap::new(),
            records: None,
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
            parked_cursor: None,
            cursor_moved: false,
            _diff_cursor: None,
            _load: None,
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
                    this.set_status(Status::Failed(format!("{error:#}").into()), cx)
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
            self.marks.set_github(HashMap::new());
            self.files.set_viewed(&self.marks.shown());
        }
        self.acting = Some(acting);
        self.set_status(Status::Loading("Asking GitHub for it…".into()), cx);
    }

    /// Shows what rozed saved about the pull request, at once and online or not, until GitHub
    /// answers.
    pub(crate) fn show_saved(&mut self, saved: &Saved, cx: &mut Context<Self>) {
        self.take_details(&saved.details, saved.fetched_at, cx);
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
        self.set_head(&pull_request.head.sha);
        self.status = Status::Loading("Copying it locally…".into());
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
        self.take_details(details, SystemTime::now(), cx);
    }

    /// Takes the pull request's details as GitHub gave them at `fetched_at`, from the poller or
    /// from what rozed saved.
    fn take_details(
        &mut self,
        details: &PullRequestData,
        fetched_at: SystemTime,
        cx: &mut Context<Self>,
    ) {
        self.note_github_head(&details.head_ref_oid, fetched_at);
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
        self.node_id = Some(details.id.clone());
        // Details from before a write finished, such as the ones a tab switch hands back, don't
        // undo it.
        let viewed = self.marks.without_stale(viewed, fetched_at);
        self.follow_marks_set_elsewhere(&viewed, &details.head_ref_oid, cx);
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

    /// The local copy listed the pull request's files: lists them, builds its revisions, and
    /// returns where each file goes in the diff, by its index in `files`.
    pub(crate) fn copied(
        &mut self,
        files: &[ChangedFile],
        head: &Oid,
        cx: &mut Context<Self>,
    ) -> Vec<usize> {
        let mut list = FileList::new(files.iter().map(FileEntry::from_local).collect());
        list.set_order(self.files.order());
        list.set_viewed(&self.marks.shown());
        self.files = list;
        self.set_head(head.as_str());
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

    /// Shows `editor` as the tab's diff. Its cursor goes where it was in the diff it replaces, if
    /// it was moved there, or else to its start.
    pub(crate) fn show_diff(
        &mut self,
        editor: Entity<SplittableEditor>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focused = self.focus_handle.contains_focused(window, cx);
        let carried = match (&self.diff, self.cursor_moved) {
            (Some(diff), true) => diff_cursor::cursor_position(diff, cx),
            _ => None,
        };
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
        let carried = carried
            .is_some_and(|position| diff_cursor::move_cursor_to(&editor, &position, window, cx));
        // A diff with its files in already sends no event as they arrive, so its cursor would
        // stay at its end.
        if !carried {
            self.cursor_moved = false;
            self.park_cursor(window, cx);
        }
        if focused && !self.file_list_focus.contains_focused(window, cx) {
            window.focus(&editor.focus_handle(cx), cx);
        }
        cx.notify();
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
        if let Some(records) = &records {
            match records.heads(poller.read(cx).login(), &pull) {
                Ok(heads) => self.viewed_at = heads,
                Err(error) => log::error!("{pull}: reading where files were viewed: {error:#}"),
            }
        }
        self.records = records;
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
                self.note_github_head(&summary.head.sha, SystemTime::now());
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

    /// GitHub said at `at` that the pull request's head is `head`. Something it said earlier
    /// doesn't replace something it said later.
    fn note_github_head(&mut self, head: &str, at: SystemTime) {
        if self
            .github_head
            .as_ref()
            .is_none_or(|(_, seen_at)| at >= *seen_at)
        {
            self.github_head = Some((head.to_string(), at));
        }
    }

    /// The diff shows the pull request at `head`.
    fn set_head(&mut self, head: &str) {
        if self.head.as_deref() != Some(head) {
            self.head = Some(head.to_string());
            self.head_at = Some(SystemTime::now());
        }
    }

    /// GitHub's latest head, when GitHub gave it after rozed learned the head on screen and it
    /// isn't that head.
    fn newer_head(&self) -> Option<&str> {
        let (latest, at) = self.github_head.as_ref()?;
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
                self.take_details(&answer.details, answer.fetched_at, cx);
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

    fn render_status(&self) -> Option<AnyElement> {
        let (icon, color, text) = match &self.status {
            Status::Ready => return None,
            Status::Loading(step) => (IconName::ArrowCircle, Color::Muted, step.clone()),
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
        v_flex()
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::toggle_file_list))
            .on_action(cx.listener(Self::toggle_threads))
            .on_action(cx.listener(Self::toggle_file_order))
            .on_action(cx.listener(Self::toggle_viewed))
            .on_action(cx.listener(Self::next_file))
            .on_action(cx.listener(Self::previous_file))
            .on_action(cx.listener(Self::next_change))
            .on_action(cx.listener(Self::previous_change))
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
    use project::{FakeFs, Project};
    use repo_cache::{Change, FileMode, FileVersion, LineCounts};
    use workspace::MultiWorkspace;

    use crate::github_api::{Base, BaseRepo, Head, User};
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
                accounts,
                BOT,
                cache,
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
            tab.copied(&[changed("a.rs", Change::Modified)], &head, cx);
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
            tab.show_diff(editor.clone(), window, cx)
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
            tab.show_diff(editor.clone(), window, cx)
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
            tab.take_details(&details_with(&[("a.rs", "UNVIEWED")]), older, cx)
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
            tab.github_head = None;
            tab.take_details(
                &details_at(
                    "1111111111111111111111111111111111111111",
                    &[("a.rs", "UNVIEWED")],
                ),
                saved_at,
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
            tab.copied(&[changed("a.rs", Change::Modified), renamed], &head, cx);
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
        tab.update(cx, |tab, cx| tab.take_details(&stale, before_the_write, cx));
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
            tab.show_diff(editor.clone(), window, cx)
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
        tab.update(cx, |tab, cx| tab.copied(&changed, &head, cx))
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
            tab.show_diff(editor.clone(), window, cx)
        });
        cx.run_until_parked();
        editor
    }

    fn cursor_position(
        editor: &Entity<SplittableEditor>,
        cx: &mut VisualTestContext,
    ) -> Option<(String, u32)> {
        cx.update(|_, cx| crate::diff_cursor::cursor_position(editor, cx))
            .map(|position| (position.path, position.point.row))
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
    async fn a_diff_that_replaces_another_keeps_the_cursors_file_and_line(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let tab = open_unloaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);
        let files = [
            ("src/a.rs", lines(30, &[]), lines(30, &[2, 25])),
            ("src/b.rs", lines(5, &[]), lines(5, &[3])),
        ];
        let first = show_test_diff(&tab, &workspace, &files, cx).await;

        // A cursor that wasn't moved starts at the new diff's start.
        let second = show_built_diff(&tab, &workspace, &files, cx).await;
        assert_eq!(
            cursor(&tab, &second, cx),
            (Some("src/a.rs".into()), 0, None)
        );
        drop(first);

        // A moved one stays on its file and line.
        cx.simulate_keystrokes("] c ] c");
        assert_eq!(cursor_position(&second, cx), Some(("src/a.rs".into(), 24)));
        let third = show_built_diff(&tab, &workspace, &files, cx).await;
        assert_eq!(cursor_position(&third, cx), Some(("src/a.rs".into(), 24)));
        assert_eq!(selected(&tab, cx).as_deref(), Some("src/a.rs"));
        // And it was moved, so files arriving leave it there.
        cx.simulate_keystrokes("] c");
        assert_eq!(cursor_position(&third, cx), Some(("src/b.rs".into(), 2)));
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
