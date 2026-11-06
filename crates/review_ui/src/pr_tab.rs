//! The PR tab: one pull request's header, its files in review order, its side-by-side diff and its
//! threads (design §10 "Window"). rozed's window has one tab per pull request, and opening a pull
//! request that's already open switches to its tab.

use std::collections::HashMap;
use std::ops::Range;
use std::time::{Instant, SystemTime};

use anyhow::anyhow;
use editor::scroll::Autoscroll;
use editor::{Editor, SelectionEffects, SplittableEditor};
use github::poll::{PollEvent, Poller};
use github::queries::{FileViewedState, PullRequestData};
use github::{PullRef, Target};
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
use workspace::{ItemNavHistory, Workspace};

use crate::account::{self, AccountDecision, Acting};
use crate::file_list::{FileChange, FileEntry, FileList, FileOrder};
use crate::github_api::{self, PullRequest};
use crate::github_layer;
use crate::pr_banner::{self, Banners};
use crate::pr_diff::{self, Trust};
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
    /// GitHub's viewed marks, by path, kept for when the files arrive.
    viewed: HashMap<String, ViewedState>,
    revisions: Option<Revisions>,
    status: Status,
    /// What the tab should tell about what its diff shows, under its header.
    banners: Entity<Banners>,
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
            header: Header::default(),
            created_at: None,
            files: FileList::default(),
            viewed: HashMap::new(),
            revisions: None,
            status: Status::Loading("Finding an account that can see it…".into()),
            banners,
            diff: None,
            show_file_list: true,
            show_threads: true,
            watched: None,
            opened: Instant::now(),
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
        let mut list = FileList::new(files.iter().map(FileEntry::from_local).collect());
        list.set_order(self.files.order());
        list.set_viewed(&self.viewed);
        self.files = list;
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
    /// keeps it up to date.
    fn watch(&mut self, poller: Entity<Poller>, pull: PullRef, cx: &mut Context<Self>) {
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
            this.update(cx, |this, cx| this.watch(poller.clone(), pull, cx))?;
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
            editor.update(cx, |editor, _| editor.set_nav_history(Some(history)));
        }
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

    fn on_removed(&self, cx: &mut Context<Self>) {
        if let Some((poller, pull)) = self.watched.clone() {
            poller.update(cx, |poller, cx| poller.unwatch(&pull, cx));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use gpui::{TestAppContext, VisualTestContext};
    use project::{FakeFs, Project};
    use repo_cache::{Change, FileMode, FileVersion, LineCounts};
    use workspace::MultiWorkspace;

    use crate::github_api::{Base, BaseRepo, Head, User};
    use crate::test_support::{FakeGithub, FakePull, GitServer, anonymous, sign_in};

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
        assert_eq!(tabs(&workspace, cx), vec![fd.clone(), playground]);
        assert_eq!(active_tab(&workspace, cx), Some(fd));
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
