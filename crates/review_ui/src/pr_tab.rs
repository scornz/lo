//! The PR tab: one pull request's header, its files in review order, its side-by-side diff and its
//! threads (design §10 "Window"). rozed's window has one tab per pull request, and opening a pull
//! request that's already open switches to its tab.

use std::collections::HashMap;
use std::ops::Range;
use std::time::{Instant, SystemTime};

use anyhow::anyhow;
use editor::scroll::Autoscroll;
use editor::{Editor, SelectionEffects, SplittableEditor};
use futures::FutureExt as _;
use github::poll::{PollEvent, Poller};
use github::queries::{FileViewedState, PullRequestData};
use github::viewed::ViewedRecords;
use github::{GithubClient, GithubError, PullRef, Target};
use gpui::{
    AnyElement, App, AsyncWindowContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    KeyBinding, Render, SharedString, Subscription, Task, UniformListScrollHandle, WeakEntity,
    Window, actions, uniform_list,
};
use language::Capability;
use repo_cache::{ChangedFile, Oid};
use review::revisions::{PushRecord, Revisions, ViewedState, build_revisions};
use ui::{ListItem, ListItemSpacing, Tooltip, prelude::*};
use util::rel_path::RelPath;
use workspace::item::{Item, ItemEvent, TabContentParams};
use workspace::notifications::NotificationId;
use workspace::{ItemNavHistory, Toast, Workspace};

use crate::account::{self, AccountDecision, Acting};
use crate::file_list::{FileChange, FileEntry, FileList, FileOrder};
use crate::github_api::{self, PullRequest};
use crate::github_layer;
use crate::pr_diff::{self, Trust};
use crate::pr_link::PrLink;
use crate::tabs::{TabKey, find_tab};
use crate::viewed_marks::{Toggle, ViewedMarks, write_failed};

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
        KeyBinding::new("e", ToggleViewed, Some(KEY_CONTEXT)),
        KeyBinding::new("o", ToggleFileOrder, Some(FILE_LIST_CONTEXT)),
        // The workspace binds escape to stop following a collaborator.
        KeyBinding::new("escape", menu::Cancel, Some(FILE_LIST_CONTEXT)),
        KeyBinding::new("enter", menu::Confirm, Some(FILE_LIST_CONTEXT)),
        KeyBinding::new("j", menu::SelectNext, Some(FILE_LIST_CONTEXT)),
        KeyBinding::new("k", menu::SelectPrevious, Some(FILE_LIST_CONTEXT)),
    ]);
}

/// Opens `link`'s pull request in a tab of `workspace`, or switches to its tab if it's open. A new
/// tab starts loading: it picks the account, reads the pull request from GitHub, copies it
/// locally and shows its diff.
pub fn open(
    workspace: &mut Workspace,
    link: PrLink,
    trust: Trust,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Entity<PrTab> {
    open_with(workspace, link, trust, window, cx, PrTab::load)
}

/// [`open`], with `start` run on a new tab instead of loading it.
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
        log::info!("{link} is already open; switching to its tab");
        workspace.activate_item(&tab, true, true, window, cx);
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
    header: Header,
    created_at: Option<SystemTime>,
    files: FileList,
    /// GitHub's viewed marks, and the ones being set, kept for when the files arrive.
    marks: ViewedMarks,
    /// The pull request's GraphQL node ID, which writes name it by.
    node_id: Option<String>,
    /// The head on screen: GitHub's at first, then the one the local copy fetched.
    head: Option<String>,
    /// Where this Mac's records say each file was marked viewed, by path.
    viewed_at: HashMap<String, String>,
    /// This Mac's records of where files were marked viewed, once signed in.
    records: Option<ViewedRecords>,
    revisions: Option<Revisions>,
    status: Status,
    diff: Option<Entity<SplittableEditor>>,
    show_file_list: bool,
    show_threads: bool,
    /// The account's poller and the pull request it watches for this tab.
    watched: Option<(Entity<Poller>, PullRef)>,
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
        Self {
            key: TabKey::new(link),
            trust,
            workspace,
            focus_handle,
            file_list_focus: cx.focus_handle(),
            file_list_scroll: UniformListScrollHandle::new(),
            acting: None,
            header: Header::default(),
            created_at: None,
            files: FileList::default(),
            marks: ViewedMarks::default(),
            node_id: None,
            head: None,
            viewed_at: HashMap::new(),
            records: None,
            revisions: None,
            status: Status::Loading("Finding an account that can see it…".into()),
            diff: None,
            show_file_list: true,
            show_threads: true,
            watched: None,
            opened: Instant::now(),
            _load: None,
            _subscriptions: vec![on_focus],
        }
    }

    /// Starts loading the pull request.
    pub fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (link, trust, opened) = (self.key.opened_as.clone(), self.trust, self.opened);
        self._load = Some(cx.spawn_in(window, async move |this, cx| {
            if let Err(error) = load(this.clone(), link.clone(), trust, opened, cx).await {
                log::error!("{link}: {error:#}");
                this.update(cx, |this, cx| {
                    this.set_status(Status::Failed(format!("{error:#}").into()), cx)
                })
                .ok();
            }
        }));
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
        self.acting = Some(acting);
        self.set_status(Status::Loading("Asking GitHub for it…".into()), cx);
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
        self.head = Some(pull_request.head.sha.clone());
        self.status = Status::Loading("Copying it locally…".into());
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
        self.node_id = Some(details.id.clone());
        let marks_changed = self.marks.set_github(viewed);
        // A change of title makes the workspace report the active item changed, which comes back
        // here through `set_active`, so only a real change is reported.
        if header == self.header && !marks_changed {
            return;
        }
        self.header = header;
        self.files.set_viewed(&self.marks.shown());
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
        let mut list = FileList::new(files.iter().map(FileEntry::from_local).collect());
        list.set_order(self.files.order());
        list.set_viewed(&self.marks.shown());
        self.files = list;
        self.head = Some(head.as_str().to_string());
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

    /// Shows `editor` as the tab's diff.
    pub(crate) fn show_diff(
        &mut self,
        editor: Entity<SplittableEditor>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focused = self.focus_handle.contains_focused(window, cx);
        self.diff = Some(editor.clone());
        if focused && !self.file_list_focus.contains_focused(window, cx) {
            window.focus(&editor.focus_handle(cx), cx);
        }
        cx.notify();
    }

    /// Watches the pull request with its account's poller, which shows what's cached at once and
    /// keeps it up to date, and reads where this Mac's `records` say its files were marked viewed.
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
        let answer = poller.update(cx, |poller, cx| poller.set_open(Some(pull.clone()), cx));
        if let Some(answer) = answer {
            self.set_details(&answer.details, cx);
        }
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
                self.set_details(details, cx);
            }
            PollEvent::Summary { pull, summary }
                if pull == watched && self.header.title.as_ref() != Some(&summary.title) =>
            {
                self.header.title = Some(summary.title.clone());
                self.changed(cx);
            }
            PollEvent::Failed {
                target: Target::Pull(pull),
                error,
            } if pull == watched => {
                log::warn!("polling {pull}: {error}");
            }
            _ => {}
        }
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

    /// The file `e` and the movement keys act on: the one selected in the list, or the first in
    /// review order.
    fn current_path(&self) -> Option<String> {
        self.files
            .selected()
            .or_else(|| self.files.at_review_position(0))
            .map(|entry| entry.path().to_string())
    }

    /// `e`: marks the current file viewed and moves to the next unviewed file, or clears a viewed
    /// file's mark. The new mark shows at once, and goes back to GitHub's if the write fails.
    fn toggle_viewed(&mut self, _: &ToggleViewed, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.current_path() else {
            return;
        };
        let Some((poller, pull)) = self.watched.clone() else {
            self.toast(
                "Sign in to mark files viewed: this pull request opened without an account.".into(),
                cx,
            );
            return;
        };
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
        let toggle = self.marks.toggle(&path);
        self.files.set_viewed(&self.marks.shown());
        if toggle.mark
            && let Some(next) = self.files.next_unviewed(&path)
        {
            let next = next.path().to_string();
            self.files.select_path(&next);
            self.selection_moved(window, cx);
        }
        let mark = toggle.mark;
        let write = poller.update(cx, |poller, cx| {
            poller.write(
                pull,
                move |client: GithubClient| async move {
                    if mark {
                        client.mark_file_as_viewed(&node_id, &path).await
                    } else {
                        client.unmark_file_as_viewed(&node_id, &path).await
                    }
                },
                cx,
            )
        });
        let head = self.head.clone();
        cx.spawn(async move |this, cx| {
            let result = write.await;
            this.update(cx, |this, cx| this.viewed_written(toggle, head, result, cx))
                .ok();
        })
        .detach();
        cx.notify();
    }

    /// A toggle's write finished: on success this Mac records or forgets where the file was
    /// viewed; on failure the file shows GitHub's mark again and a toast says why.
    fn viewed_written(
        &mut self,
        toggle: Toggle,
        head: Option<String>,
        result: Result<(), GithubError>,
        cx: &mut Context<Self>,
    ) {
        self.marks.finish(&toggle, result.is_ok());
        self.files.set_viewed(&self.marks.shown());
        cx.notify();
        let Some((poller, pull)) = self.watched.clone() else {
            return;
        };
        let login = poller.read(cx).login().to_string();
        if let Err(error) = result {
            log::warn!("{pull}: {}: {error}", toggle.path);
            self.toast(write_failed(&toggle, &login, &error), cx);
            return;
        }
        let Some(records) = self.records.clone() else {
            return;
        };
        let path = toggle.path;
        let write = match head.filter(|_| toggle.mark) {
            Some(head) => {
                self.viewed_at.insert(path.clone(), head.clone());
                let now = SystemTime::now();
                async move { records.record(&login, &pull, &path, &head, now).await }.boxed_local()
            }
            None => {
                self.viewed_at.remove(&path);
                async move { records.forget(&login, &pull, &path).await }.boxed_local()
            }
        };
        cx.spawn(async move |_, _| {
            if let Err(error) = write.await {
                log::error!("recording where a file was viewed: {error:#}");
            }
        })
        .detach();
    }

    fn toast(&self, message: String, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_toast(
                    Toast::new(NotificationId::unique::<ViewedMarkToast>(), message),
                    cx,
                )
            })
            .ok();
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
                SelectionEffects::scroll(Autoscroll::top_relative(1.)),
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
        let title = self
            .header
            .title
            .clone()
            .unwrap_or_else(|| "Loading…".to_string());
        let acting = match &self.acting {
            Some(Acting::Account(login)) => format!("as {login}"),
            Some(Acting::Anonymous) => "not signed in".to_string(),
            None => String::new(),
        };
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
                    ),
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

/// The toast that says why a viewed mark couldn't be set or cleared.
struct ViewedMarkToast;

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

/// Loads `link`'s pull request into `this`: picks the account, reads the pull request, copies it
/// locally and shows its diff.
async fn load(
    this: WeakEntity<PrTab>,
    link: PrLink,
    trust: Trust,
    opened: Instant,
    cx: &mut AsyncWindowContext,
) -> anyhow::Result<()> {
    // The account that can see the repository, or none for a public one.
    let layer = cx.update(|_, cx| github_layer::ready(cx))?.await;
    let found = match &layer {
        Some(layer) => {
            let accounts = layer.read_with(cx, |layer, _| layer.accounts().clone());
            Some(
                accounts
                    .update(cx, |accounts, cx| {
                        accounts.account_for_repo(&link.owner, &link.repo, cx)
                    })
                    .await?,
            )
        }
        None => None,
    };
    let acting = match account::decide(&link.owner, &link.repo, found.as_ref()) {
        AccountDecision::Open(acting) => acting,
        AccountDecision::Ask(logins) => {
            // Until rozed asks which account to use, the first one signed in.
            log::info!(
                "{link}: several accounts can see it ({}); using {}",
                logins.join(", "),
                logins[0]
            );
            Acting::Account(logins[0].clone())
        }
        AccountDecision::Refuse(message) => return Err(anyhow!(message)),
    };
    log::info!(
        "{link}: opening {}",
        match &acting {
            Acting::Account(login) => format!("as {login}"),
            Acting::Anonymous => "without signing in".to_string(),
        }
    );
    this.update(cx, |this, cx| this.set_acting(acting.clone(), cx))?;

    // Signed in, the account's poller shows what's cached at once and keeps it up to date, and
    // its client and token read the pull request and copy it.
    let (pull_request, token) = match (&acting, &layer) {
        (Acting::Account(login), Some(layer)) => {
            let number = i64::try_from(link.number)?;
            let pull = PullRef::new(&link.owner, &link.repo, number);
            let poller = layer.update(cx, |layer, cx| layer.poller(login, cx));
            let records = layer.read_with(cx, |layer, _| layer.viewed_records().clone());
            this.update(cx, |this, cx| {
                this.watch(poller.clone(), pull, Some(records), cx)
            })?;
            let accounts = layer.read_with(cx, |layer, _| layer.accounts().clone());
            let client = accounts
                .update(cx, |accounts, cx| accounts.client(login, cx))
                .await?;
            let queue = poller.read_with(cx, |poller, _| poller.queue().clone());
            let pull_request = queue
                .read(|| github_api::fetch_as(&client, login, &link))
                .await?;
            let token = repo_cache::Token::new(client.token().expose());
            (pull_request, Some(token))
        }
        _ => {
            let http = cx.update(|_, cx| cx.http_client())?;
            match github_api::fetch_pull_request(http, &link).await {
                Ok(pull_request) => (pull_request, None),
                Err(error) => {
                    let problems = account::problems(found.as_ref());
                    if problems.is_empty() {
                        return Err(error);
                    }
                    return Err(error.context(format!(
                        "the accounts that couldn't check it said: {}",
                        problems.join("; ")
                    )));
                }
            }
        }
    };
    this.update(cx, |this, cx| this.set_pull_request(&pull_request, cx))?;

    let workspace = this.read_with(cx, |this, _| this.workspace.clone())?;
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
        let diff: AnyElement = match (&self.diff, &self.status) {
            (_, Status::Failed(error)) => v_flex()
                .size_full()
                .p_4()
                .gap_2()
                .child(Label::new("rozed couldn't open this pull request").color(Color::Error))
                .child(Label::new(error.clone()).size(LabelSize::Small))
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
            .size_full()
            .bg(colors.editor_background)
            .child(self.render_header(cx))
            .children(self.render_status())
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
            editor.update(cx, |editor, _| editor.set_nav_history(Some(history)));
        }
    }

    fn on_removed(&self, cx: &mut Context<Self>) {
        if let Some((poller, pull)) = self.watched.clone() {
            poller.update(cx, |poller, cx| poller.unwatch(&pull, cx));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration;

    use gpui::{TestAppContext, VisualTestContext};
    use project::{FakeFs, Project};
    use repo_cache::{Change, FileMode, FileVersion, LineCounts};
    use workspace::MultiWorkspace;

    use crate::github_api::{Base, BaseRepo, Head, User};

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

    fn init(cx: &mut TestAppContext) {
        crate::trust::tests::init_test(cx);
        cx.update(|cx| {
            editor::init(cx);
            crate::init_workspaces(cx);
        });
    }

    /// A window like rozed's: a workspace in a `MultiWorkspace`, which renders the workspace's
    /// actions.
    async fn workspace(cx: &mut TestAppContext) -> (Entity<Workspace>, &mut VisualTestContext) {
        init(cx);
        let project = Project::test(FakeFs::new(cx.executor()), [], cx).await;
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

    #[gpui::test]
    async fn a_pull_request_opens_in_a_tab_that_fills_in_as_it_loads(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let tab = open_unloaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);

        assert_eq!(tabs(&workspace, cx), vec![tab.clone()]);
        assert_eq!(active_tab(&workspace, cx), Some(tab.clone()));
        let title = |cx: &mut VisualTestContext| {
            tab.read_with(cx, |tab, cx| tab.tab_content_text(0, cx).to_string())
        };
        assert_eq!(title(cx), "fd#2212");
        tab.read_with(cx, |tab, _| {
            assert!(matches!(tab.status(), Status::Loading(_)));
            assert!(tab.is_file_list_shown());
            assert!(tab.is_threads_panel_shown());
        });

        tab.update(cx, |tab, cx| {
            tab.set_acting(Acting::Anonymous, cx);
            tab.set_pull_request(&fd_2212(), cx);
        });
        let files = [
            changed("Cargo.lock", Change::Modified),
            changed("src/main.rs", Change::Modified),
            changed("tests/main.rs", Change::Added),
        ];
        let head: Oid = "2".repeat(40).parse().unwrap();
        let positions = tab.update(cx, |tab, cx| tab.copied(&files, &head, cx));
        cx.run_until_parked();

        assert_eq!(title(cx), "fd#2212 Honor explicit use-jemalloc req…");
        assert_eq!(positions, [2, 0, 1]);
        tab.read_with(cx, |tab, _| {
            assert_eq!(tab.acting(), Some(&Acting::Anonymous));
            assert_eq!(
                tab.header(),
                &Header {
                    title: Some("Honor explicit use-jemalloc requests".into()),
                    author: Some("voidsipp".into()),
                    repo: Some("sharkdp/fd".into()),
                }
            );
            let paths: Vec<_> = tab.files().rows().map(FileEntry::path).collect();
            assert_eq!(paths, ["src/main.rs", "tests/main.rs", "Cargo.lock"]);
            let revisions = tab.revisions().unwrap();
            assert_eq!(revisions.len(), 1);
            assert_eq!(revisions.label(head.as_str()).as_deref(), Some("r1"));
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
        let tab = open_unloaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);
        let head: Oid = "2".repeat(40).parse().unwrap();
        let files = [
            changed("a.rs", Change::Modified),
            changed("b.rs", Change::Modified),
        ];
        tab.update(cx, |tab, cx| tab.copied(&files, &head, cx));
        // A changed title makes the workspace report the active item changed, which comes back
        // to the tab; the same details again must not, or the two would loop.
        let active_item_changes = std::rc::Rc::new(std::cell::Cell::new(0));
        let _subscription = cx.update(|_, cx| {
            let changes = active_item_changes.clone();
            cx.subscribe(&workspace, move |_, event, _| {
                if let workspace::Event::ActiveItemChanged = event {
                    changes.set(changes.get() + 1);
                }
            })
        });

        tab.update(cx, |tab, cx| {
            tab.set_details(&details("Add b", "VIEWED"), cx)
        });
        cx.run_until_parked();
        let after_first = active_item_changes.get();
        assert!(after_first > 0);
        tab.update(cx, |tab, cx| {
            tab.set_details(&details("Add b", "VIEWED"), cx)
        });
        cx.run_until_parked();
        assert_eq!(active_item_changes.get(), after_first);

        tab.read_with(cx, |tab, cx| {
            assert_eq!(tab.tab_content_text(0, cx), "playground#2 Add b");
            assert_eq!(tab.header().author.as_deref(), Some("rozed-bot"));
            let marks: Vec<_> = tab
                .files()
                .rows()
                .map(|entry| (entry.path(), entry.viewed))
                .collect();
            assert_eq!(
                marks,
                [
                    ("a.rs", Some(ViewedState::Viewed)),
                    ("b.rs", Some(ViewedState::Unviewed))
                ]
            );
        });

        // A file marked viewed elsewhere changes the marks, and only them.
        tab.update(cx, |tab, cx| {
            tab.set_details(&details("Add b", "DISMISSED"), cx)
        });
        cx.run_until_parked();
        tab.read_with(cx, |tab, _| {
            assert_eq!(
                tab.files().row(0).and_then(|entry| entry.viewed),
                Some(ViewedState::Dismissed)
            );
        });
    }

    /// [`details`] with these files and marks.
    fn details_with(files: &[(&str, &str)]) -> PullRequestData {
        let mut details = serde_json::to_value(details("Add b", "UNVIEWED")).unwrap();
        details["files"]["totalCount"] = files.len().into();
        details["files"]["nodes"] = files
            .iter()
            .map(|(path, state)| serde_json::json!({ "path": path, "viewerViewedState": state }))
            .collect();
        serde_json::from_value(details).unwrap()
    }

    /// How GitHub answers a viewed-mark mutation.
    #[derive(Clone, Copy, Debug)]
    enum Answer {
        Done,
        Forbidden,
        RateLimited,
        Offline,
    }

    /// GitHub as these tests need it: the viewed-mark mutations answer as each test says, and every
    /// other request fails as if offline, so the poller's polls change nothing.
    #[derive(Default)]
    struct FakeGithub {
        answers: std::sync::Mutex<std::collections::VecDeque<Answer>>,
        /// Each mutation's operation and variables.
        mutations: std::sync::Mutex<Vec<(String, serde_json::Value)>>,
    }

    impl FakeGithub {
        fn answer(&self, answer: Answer) {
            self.answers.lock().unwrap().push_back(answer);
        }

        fn mutations(&self) -> Vec<(String, serde_json::Value)> {
            self.mutations.lock().unwrap().clone()
        }
    }

    impl http_client::HttpClient for FakeGithub {
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
                return Box::pin(async { Err(anyhow!("the network is down")) });
            }
            self.mutations
                .lock()
                .unwrap()
                .push((operation.clone(), body["variables"].clone()));
            let answer = self.answers.lock().unwrap().pop_front().expect("an answer");
            let field = format!("{}{}", operation[..1].to_lowercase(), &operation[1..]);
            let field = field.as_str();
            let (remaining, json) = match answer {
                Answer::Offline => return Box::pin(async { Err(anyhow!("the network is down")) }),
                Answer::Done => (
                    "4999",
                    serde_json::json!({ "data": { field: { "pullRequest": { "id": "PR_x" } } } }),
                ),
                Answer::Forbidden => (
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
                Answer::RateLimited => (
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
        github: std::sync::Arc<FakeGithub>,
        records: ViewedRecords,
        pull: PullRef,
    }

    /// Signs `tab` in as the bot, with `recorded` already in its records of viewed files.
    async fn sign_in(
        tab: &Entity<PrTab>,
        name: &str,
        recorded: &[(&str, &str)],
        cx: &mut VisualTestContext,
    ) -> SignedIn {
        let github = std::sync::Arc::new(FakeGithub::default());
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
            tab.watch(poller, pull.clone(), Some(records.clone()), cx);
        });
        cx.run_until_parked();
        SignedIn {
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

    /// A tab on the playground's PR #2 with three files, signed in as the bot, with GitHub's marks.
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
        let (workspace, cx) = workspace(cx).await;
        let tab = open_unloaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);
        let signed_in = sign_in(&tab, name, recorded, cx).await;
        let files = [
            changed("a.rs", Change::Modified),
            changed("b.rs", Change::Modified),
            changed("c.rs", Change::Modified),
        ];
        let head: Oid = HEAD.parse().unwrap();
        tab.update(cx, |tab, cx| {
            tab.copied(&files, &head, cx);
            tab.set_details(&details_with(github_marks), cx);
        });
        cx.run_until_parked();
        (workspace, tab, signed_in, cx)
    }

    fn has_viewed_toast(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> bool {
        workspace.read_with(cx, |workspace, _| {
            workspace.has_notification(&NotificationId::unique::<ViewedMarkToast>())
        })
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
        github.github.answer(Answer::Done);

        // With nothing selected, the current file is the first in review order.
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
        tab.update(cx, |tab, _| tab.files.select_path("b.rs"));
        github.github.answer(Answer::Done);

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
        github.github.answer(Answer::Done);
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

        for answer in [Answer::Forbidden, Answer::Offline, Answer::RateLimited] {
            workspace.update(cx, |workspace, cx| workspace.clear_all_notifications(cx));
            tab.update(cx, |tab, _| tab.files.select_path("a.rs"));
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
        github.github.answer(Answer::Done);
        github.github.answer(Answer::Forbidden);

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
        assert_eq!(marks(&tab, cx)[1], mark("b.rs", Some(Unviewed)));
    }

    #[gpui::test]
    async fn e_says_why_it_cant_mark_without_an_account_or_githubs_details(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx).await;
        let anonymous = open_unloaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);
        let head: Oid = HEAD.parse().unwrap();
        anonymous.update(cx, |tab, cx| {
            tab.set_acting(Acting::Anonymous, cx);
            tab.copied(&[changed("src/main.rs", Change::Modified)], &head, cx);
        });

        cx.simulate_keystrokes("e");
        assert!(has_viewed_toast(&workspace, cx));
        assert_eq!(marks(&anonymous, cx), [mark("src/main.rs", None)]);

        // Signed in, but GitHub hasn't sent the pull request's details, which name it for writes.
        workspace.update(cx, |workspace, cx| workspace.clear_all_notifications(cx));
        let tab = open_unloaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);
        let github = sign_in(&tab, "e_without_details", &[], cx).await;
        tab.update(cx, |tab, cx| {
            tab.copied(&[changed("a.rs", Change::Modified)], &head, cx);
        });

        cx.simulate_keystrokes("e");
        assert!(has_viewed_toast(&workspace, cx));
        assert_eq!(marks(&tab, cx), [mark("a.rs", None)]);
        assert_eq!(github.github.mutations(), []);
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

        // Escape goes back to the diff, and the file list hides again.
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
        assert_eq!(tabs(&workspace, cx), vec![fd.clone(), playground]);
        assert_eq!(active_tab(&workspace, cx), Some(fd));
    }

    #[gpui::test]
    async fn the_window_offers_no_new_files_and_opens_editors_read_only(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;

        // A double click in the tab bar or an empty pane, which focuses the pane, sends NewFile.
        workspace.update_in(cx, |workspace, window, cx| {
            window.focus(&workspace.active_pane().focus_handle(cx), cx)
        });
        cx.run_until_parked();
        cx.dispatch_action(workspace::NewFile);
        cx.run_until_parked();
        let items = |cx: &mut VisualTestContext| {
            workspace.read_with(cx, |workspace, cx| workspace.items(cx).count())
        };
        assert_eq!(items(cx), 0);

        // What a file header's Open File opens.
        let editor = workspace.update_in(cx, |workspace, window, cx| {
            let editor = cx.new(|cx| Editor::single_line(window, cx));
            workspace.add_item_to_active_pane(Box::new(editor.clone()), None, true, window, cx);
            editor
        });
        assert_eq!(items(cx), 1);
        assert!(editor.read_with(cx, |editor, cx| editor.read_only(cx)));
    }
}
