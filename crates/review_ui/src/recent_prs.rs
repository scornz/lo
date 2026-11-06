//! What the window shows with no pull request open: the pull requests opened recently, with any
//! account, newest first (design §10 "Window"). Enter opens the selected one.
//!
//! The list is a tab of its own that shows exactly while no pull request is open: opening one
//! closes it, and closing the last one brings it back. It's read again when an account is added,
//! removed or signed out, and says how long ago each was opened, minute by minute.

use std::ops::Range;
use std::time::{Duration, SystemTime};

use github::accounts::Accounts;
use github::cache::RecentPr;
use github::poll::WallClock;
use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyBinding, Render,
    SharedString, Task, UniformListScrollHandle, WeakEntity, Window, uniform_list,
};
use language::Capability;
use ui::{ListItem, ListItemSpacing, prelude::*};
use workspace::Workspace;
use workspace::item::{Item, TabContentParams};

use crate::github_layer;
use crate::open_box::OpenPullRequest;
use crate::poll_state;
use crate::pr_diff::Trust;
use crate::pr_link::PrLink;
use crate::pr_tab::{self, PrTab};

const KEY_CONTEXT: &str = "RecentPullRequests";

/// The list's keys: `j`/`k` or the arrows move, and enter opens the selected pull request.
/// `cmd-o` opens one from its link, as everywhere in the window.
pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("j", menu::SelectNext, Some(KEY_CONTEXT)),
        KeyBinding::new("k", menu::SelectPrevious, Some(KEY_CONTEXT)),
        KeyBinding::new("down", menu::SelectNext, Some(KEY_CONTEXT)),
        KeyBinding::new("up", menu::SelectPrevious, Some(KEY_CONTEXT)),
        KeyBinding::new("enter", menu::Confirm, Some(KEY_CONTEXT)),
        // Nothing else is open, so closing the list closes the window, as closing an empty pane
        // does. Closing it any other way brings it back.
        KeyBinding::new("cmd-w", workspace::CloseWindow, Some(KEY_CONTEXT)),
    ]);
}

/// Shows the recent pull requests in `workspace`, focused, unless they show already.
pub fn show(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let shown = workspace.items_of_type::<RecentPrs>(cx).next();
    if let Some(list) = shown {
        workspace.activate_item(&list, true, true, window, cx);
        return;
    }
    let weak = workspace.weak_handle();
    let list = cx.new(|cx| RecentPrs::new(weak, window, cx));
    workspace.add_item_to_active_pane(Box::new(list), None, true, window, cx);
}

/// Closes the list once a pull request is open.
pub(crate) fn pull_request_opened(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if workspace.items_of_type::<PrTab>(cx).next().is_none() {
        return;
    }
    let lists: Vec<Entity<RecentPrs>> = workspace.items_of_type::<RecentPrs>(cx).collect();
    for list in lists {
        if let Some(pane) = workspace.pane_for(&list) {
            pane.update(cx, |pane, cx| {
                pane.remove_item(list.entity_id(), false, true, window, cx)
            });
        }
    }
}

/// Shows the list again once no pull request is open.
pub(crate) fn tab_closed(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let any_open = workspace.items_of_type::<PrTab>(cx).next().is_some()
        || workspace.items_of_type::<RecentPrs>(cx).next().is_some();
    if !any_open {
        show(workspace, window, cx);
    }
}

/// How often the rows say again how long ago each was opened.
const AGES_EVERY: Duration = Duration::from_secs(60);

/// What a row of the list says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// `owner/repo#n`.
    pub pull: String,
    /// Its title, once GitHub gave it.
    pub title: Option<String>,
    /// The account it was opened with.
    pub account: String,
    /// Whether that account is signed out, so opening it uses whichever account can see it.
    pub signed_out: bool,
    /// When it was last opened, in words.
    pub opened: String,
}

impl Row {
    pub fn new(recent: &RecentPr, signed_out: bool, now: SystemTime) -> Self {
        Self {
            pull: recent.pull.to_string(),
            title: recent
                .title
                .as_deref()
                .map(crate::window_title::without_bidi),
            account: recent.account.clone(),
            signed_out,
            opened: poll_state::ago(recent.opened_at, now),
        }
    }
}

/// Whether the list was read yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Loaded {
    Not,
    Yes,
    /// rozed's database couldn't be opened, so there's nothing to read.
    NoDatabase,
}

/// The pull requests opened recently, as a tab.
pub struct RecentPrs {
    workspace: WeakEntity<Workspace>,
    focus_handle: FocusHandle,
    scroll: UniformListScrollHandle,
    recent: Vec<RecentPr>,
    rows: Vec<Row>,
    selected: usize,
    loaded: Loaded,
    accounts: Option<Entity<Accounts>>,
    /// What says how long ago each was opened.
    clock: WallClock,
    _load: Task<()>,
    _ages: Task<()>,
}

impl EventEmitter<()> for RecentPrs {}

impl RecentPrs {
    fn new(workspace: WeakEntity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let load = cx.spawn_in(window, async move |this, cx| {
            let Ok(ready) = cx.update(|_, cx| github_layer::ready(cx)) else {
                return;
            };
            let layer = ready.await;
            this.update(cx, |this, cx| {
                let Some(layer) = layer else {
                    // Without rozed's database, nothing was recorded.
                    this.loaded = Loaded::NoDatabase;
                    cx.notify();
                    return;
                };
                // Accounts added, removed or signed out change what's listed and how.
                let accounts = layer.read(cx).accounts().clone();
                cx.observe(&accounts, |this, _, cx| this.reload(cx))
                    .detach();
                this.accounts = Some(accounts);
                this.loaded = Loaded::Yes;
                this.reload(cx);
            })
            .ok();
        });
        let ages = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(AGES_EVERY).await;
                if this.update(cx, |this, cx| this.refresh_rows(cx)).is_err() {
                    return;
                }
            }
        });
        Self {
            workspace,
            focus_handle: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            recent: Vec::new(),
            rows: Vec::new(),
            selected: 0,
            loaded: Loaded::Not,
            accounts: None,
            clock: WallClock::system(),
            _load: load,
            _ages: ages,
        }
    }

    /// Reads the list again from rozed's database, keeping the selected pull request selected.
    fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(layer) = github_layer::loaded(cx) else {
            return;
        };
        let selected = self.selected().map(|recent| recent.pull.clone());
        self.recent = layer.read(cx).recent();
        self.selected = selected
            .and_then(|selected| {
                self.recent
                    .iter()
                    .position(|recent| recent.pull == selected)
            })
            .unwrap_or(0);
        self.refresh_rows(cx);
    }

    /// Says again what each row says, with how long ago it was opened as of now.
    fn refresh_rows(&mut self, cx: &mut Context<Self>) {
        let now = self.clock.now();
        let accounts = self.accounts.as_ref().map(|accounts| accounts.read(cx));
        self.rows = self
            .recent
            .iter()
            .map(|recent| {
                let signed_out = accounts.is_some_and(|accounts| {
                    accounts.account(&recent.account).is_none()
                        || accounts.is_signed_out(&recent.account)
                });
                Row::new(recent, signed_out, now)
            })
            .collect();
        cx.notify();
    }

    /// Tells the time by `clock`, for tests.
    #[cfg(test)]
    pub(crate) fn set_clock(&mut self, clock: WallClock, cx: &mut Context<Self>) {
        self.clock = clock;
        self.refresh_rows(cx);
    }

    /// The pull requests listed, newest first.
    pub fn recent(&self) -> &[RecentPr] {
        &self.recent
    }

    /// What each row says, as shown.
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn selected(&self) -> Option<&RecentPr> {
        self.recent.get(self.selected)
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.recent.len() {
            self.selected = index;
            self.scroll
                .scroll_to_item(index, gpui::ScrollStrategy::Nearest);
            cx.notify();
        }
    }

    fn select_next(&mut self, _: &menu::SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        self.select(self.selected + 1, cx);
    }

    fn select_previous(
        &mut self,
        _: &menu::SelectPrevious,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(index) = self.selected.checked_sub(1) {
            self.select(index, cx);
        }
    }

    fn select_first(&mut self, _: &menu::SelectFirst, _: &mut Window, cx: &mut Context<Self>) {
        self.select(0, cx);
    }

    fn select_last(&mut self, _: &menu::SelectLast, _: &mut Window, cx: &mut Context<Self>) {
        self.select(self.recent.len().saturating_sub(1), cx);
    }

    /// Opens the selected pull request in a tab, with the account it was opened with while
    /// that's signed in, else with whichever account can see it.
    fn confirm(&mut self, _: &menu::Confirm, window: &mut Window, cx: &mut Context<Self>) {
        let Some(recent) = self.selected() else {
            return;
        };
        let signed_out = self
            .rows
            .get(self.selected)
            .is_none_or(|row| row.signed_out);
        let login = (!signed_out).then(|| recent.account.clone());
        let Ok(number) = u64::try_from(recent.pull.number) else {
            return;
        };
        let link = PrLink {
            owner: recent.pull.repo.owner.clone(),
            repo: recent.pull.repo.name.clone(),
            number,
        };
        let workspace = self.workspace.clone();
        // Opening it closes this list, so after this update.
        window.defer(cx, move |window, cx| {
            workspace
                .update(cx, |workspace, cx| match login {
                    Some(login) => {
                        pr_tab::open_as(workspace, link, Trust::Untrusted, login, window, cx);
                    }
                    None => {
                        pr_tab::open(workspace, link, Trust::Untrusted, window, cx);
                    }
                })
                .ok();
        });
    }

    fn render_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let row = self.rows[index].clone();
        let account_color = if row.signed_out {
            Color::Muted
        } else {
            Color::Default
        };
        let title = match row.title {
            Some(title) => Label::new(title).truncate(),
            None => Label::new("No title yet").color(Color::Muted),
        };
        ListItem::new(("recent-pr", index))
            .spacing(ListItemSpacing::Sparse)
            .toggle_state(index == self.selected)
            .start_slot(
                Icon::new(IconName::PullRequest)
                    .size(IconSize::Small)
                    .color(Color::Muted),
            )
            .child(
                h_flex()
                    .min_w_0()
                    .gap_3()
                    .child(
                        div()
                            .flex_shrink_0()
                            .child(Label::new(row.pull).color(Color::Muted)),
                    )
                    .child(div().min_w_0().child(title)),
            )
            .end_slot(
                h_flex()
                    .gap_1p5()
                    .child(
                        Label::new(row.account)
                            .size(LabelSize::Small)
                            .color(account_color),
                    )
                    .when(row.signed_out, |this| {
                        this.child(
                            Label::new("signed out")
                                .size(LabelSize::Small)
                                .color(Color::Warning),
                        )
                    })
                    .child(Label::new("·").size(LabelSize::Small).color(Color::Muted))
                    .child(
                        Label::new(row.opened)
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.select(index, cx);
                this.confirm(&menu::Confirm, window, cx);
            }))
            .into_any_element()
    }

    fn render_empty(&self) -> AnyElement {
        let text = match self.loaded {
            Loaded::Not => "Loading…",
            Loaded::Yes => "No pull requests opened yet. Open one from its link.",
            Loaded::NoDatabase => {
                "rozed couldn't open its database, so it can't list the pull requests opened \
                 before. Open one from its link."
            }
        };
        div()
            .px_2()
            .py_4()
            .child(Label::new(text).color(Color::Muted))
            .into_any_element()
    }

    fn render_hint(
        &self,
        action: &dyn gpui::Action,
        text: &'static str,
        cx: &App,
    ) -> impl IntoElement {
        h_flex()
            .gap_1()
            .child(ui::KeyBinding::for_action_in(
                action,
                &self.focus_handle,
                cx,
            ))
            .child(Label::new(text).size(LabelSize::Small).color(Color::Muted))
    }
}

impl Focusable for RecentPrs {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for RecentPrs {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let list = if self.rows.is_empty() {
            self.render_empty()
        } else {
            uniform_list(
                "recent-prs",
                self.rows.len(),
                cx.processor(|this, range: Range<usize>, _window, cx| {
                    range
                        .map(|index| this.render_row(index, cx))
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(&self.scroll)
            .flex_1()
            .into_any_element()
        };
        v_flex()
            .id("recent-prs-screen")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::select_first))
            .on_action(cx.listener(Self::select_last))
            .on_action(cx.listener(Self::confirm))
            .size_full()
            .items_center()
            .bg(colors.editor_background)
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(900.))
                    .h_full()
                    .px_6()
                    .pt_10()
                    .pb_4()
                    .gap_3()
                    .child(
                        h_flex()
                            .px_2()
                            .justify_between()
                            .items_end()
                            .child(
                                v_flex()
                                    .gap_0p5()
                                    .child(Headline::new("Recent pull requests"))
                                    .child(
                                        Label::new(
                                            "Opened with any of your accounts, newest first",
                                        )
                                        .size(LabelSize::Small)
                                        .color(Color::Muted),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .gap_4()
                                    .child(self.render_hint(&menu::Confirm, "open", cx))
                                    .child(self.render_hint(&OpenPullRequest, "open a link", cx)),
                            ),
                    )
                    .child(div().border_t_1().border_color(colors.border_variant))
                    .child(list),
            )
    }
}

impl Item for RecentPrs {
    type Event = ();

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        "Recent pull requests".into()
    }

    fn tab_content(&self, params: TabContentParams, _window: &Window, cx: &App) -> AnyElement {
        Label::new(self.tab_content_text(0, cx))
            .single_line()
            .color(params.text_color())
            .into_any_element()
    }

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::HistoryRerun).color(Color::Muted))
    }

    fn show_toolbar(&self) -> bool {
        false
    }

    fn capability(&self, _: &App) -> Capability {
        Capability::ReadOnly
    }

    /// Keeps files and folders dropped on the list from the pane, which would open them.
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

    use std::time::Duration;

    use github::PullRef;
    use github::cache::PrCache;
    use gpui::{TestAppContext, VisualTestContext};

    use crate::account::Acting;
    use crate::account_picker::AccountPicker;
    use crate::test_support::{FakeGithub, GitServer, sign_in, workspace};

    fn recent(pull: (&str, &str, i64), title: Option<&str>, account: &str, ago: u64) -> RecentPr {
        RecentPr {
            pull: PullRef::new(pull.0, pull.1, pull.2),
            title: title.map(str::to_string),
            account: account.to_string(),
            opened_at: SystemTime::now() - Duration::from_secs(ago),
        }
    }

    #[test]
    fn a_row_names_the_pull_request_its_title_account_and_when_it_was_opened() {
        let now = SystemTime::now();
        let mut opened = recent(
            ("rozed-dev", "playground", 2),
            Some("Add b"),
            "rozed-bot",
            0,
        );
        opened.opened_at = now - Duration::from_secs(2 * 3_600 + 5);
        assert_eq!(
            Row::new(&opened, false, now),
            Row {
                pull: "rozed-dev/playground#2".into(),
                title: Some("Add b".into()),
                account: "rozed-bot".into(),
                signed_out: false,
                opened: "2 hours ago".into(),
            }
        );
    }

    /// Pull requests on the fake GitHub that `home` and `rozed-bot` can see, opened before with
    /// one or the other: `acme/app#1` ten minutes ago, `rozed-dev/playground#2` three hours ago
    /// and `rozed-dev/playground#3` a day ago.
    async fn opened_before(
        github: &FakeGithub,
        git: &GitServer,
        cx: &mut TestAppContext,
    ) -> PrCache {
        for (repo, number, token) in [
            ("acme/app", 1, "gho_HOME"),
            ("rozed-dev/playground", 2, "gho_BOT"),
            ("rozed-dev/playground", 3, "gho_BOT"),
        ] {
            let mut pull = git.pull_request(repo, number, &[], &[("a.rs", "a\n")]);
            pull.title = format!("Change {number}");
            github.add_pull(pull);
            github.let_see(token, repo);
        }
        let (_, cache) = sign_in(
            github,
            &[("home", "gho_HOME"), ("rozed-bot", "gho_BOT")],
            cx,
        )
        .await;
        for opened in [
            recent(
                ("rozed-dev", "playground", 3),
                Some("Change 3"),
                "rozed-bot",
                86_400,
            ),
            recent(("acme", "app", 1), None, "home", 600),
            recent(
                ("rozed-dev", "playground", 2),
                Some("Change 2"),
                "rozed-bot",
                3 * 3_600,
            ),
        ] {
            cache.record_recent(opened).await.unwrap();
        }
        cache
    }

    fn list(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> Option<Entity<RecentPrs>> {
        workspace.read_with(cx, |workspace, cx| {
            workspace.active_item_as::<RecentPrs>(cx)
        })
    }

    fn listed(list: &Entity<RecentPrs>, cx: &mut VisualTestContext) -> Vec<String> {
        list.read_with(cx, |list, _| {
            list.recent()
                .iter()
                .map(|recent| recent.pull.to_string())
                .collect()
        })
    }

    fn selected(list: &Entity<RecentPrs>, cx: &mut VisualTestContext) -> Option<String> {
        list.read_with(cx, |list, _| {
            list.selected().map(|recent| recent.pull.to_string())
        })
    }

    fn tabs(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Vec<Entity<PrTab>> {
        workspace.read_with(cx, |workspace, cx| {
            workspace.items_of_type::<PrTab>(cx).collect()
        })
    }

    #[gpui::test]
    async fn with_no_pull_request_open_the_window_lists_recent_ones_newest_first(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        opened_before(&github, &git, cx).await;

        workspace.update_in(cx, show);
        cx.run_until_parked();

        let list = list(&workspace, cx).expect("the list shows");
        assert_eq!(
            listed(&list, cx),
            [
                "acme/app#1",
                "rozed-dev/playground#2",
                "rozed-dev/playground#3"
            ]
        );
        assert_eq!(selected(&list, cx).as_deref(), Some("acme/app#1"));
        assert_eq!(cx.window_title().as_deref(), Some("rozed"));
        let in_list = list.read_with(cx, |list, _| list.focus_handle.clone());
        assert!(cx.update(|window, cx| in_list.contains_focused(window, cx)));

        // j and k, and the arrows, move.
        cx.simulate_keystrokes("j j j");
        assert_eq!(
            selected(&list, cx).as_deref(),
            Some("rozed-dev/playground#3")
        );
        cx.simulate_keystrokes("k up");
        assert_eq!(selected(&list, cx).as_deref(), Some("acme/app#1"));
        cx.simulate_keystrokes("down");
        assert_eq!(
            selected(&list, cx).as_deref(),
            Some("rozed-dev/playground#2")
        );
    }

    #[gpui::test]
    async fn enter_opens_the_selected_pull_request_and_closing_it_lists_them_again(
        cx: &mut TestAppContext,
    ) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        opened_before(&github, &git, cx).await;
        workspace.update_in(cx, show);
        cx.run_until_parked();

        cx.simulate_keystrokes("j enter");
        cx.run_until_parked();

        // The pull request opens in a tab with the account that can see it, and the list goes.
        let tabs = tabs(&workspace, cx);
        assert_eq!(tabs.len(), 1);
        let tab = tabs[0].clone();
        tab.read_with(cx, |tab, _| {
            assert_eq!(tab.link().to_string(), "rozed-dev/playground#2");
            assert_eq!(tab.status(), &pr_tab::Status::Ready);
            assert_eq!(tab.acting_text(), "as rozed-bot");
        });
        assert_eq!(
            workspace.read_with(cx, |workspace, cx| workspace.items(cx).count()),
            1
        );
        assert_eq!(
            cx.window_title().as_deref(),
            Some("rozed-dev/playground#2 — Change 2")
        );

        // Closing the last tab brings the list back, focused, with the one just opened first.
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

        let list = list(&workspace, cx).expect("the list shows again");
        assert_eq!(
            listed(&list, cx),
            [
                "rozed-dev/playground#2",
                "acme/app#1",
                "rozed-dev/playground#3"
            ]
        );
        let in_list = list.read_with(cx, |list, _| list.focus_handle.clone());
        assert!(cx.update(|window, cx| in_list.contains_focused(window, cx)));
        assert_eq!(cx.window_title().as_deref(), Some("rozed"));
    }

    #[gpui::test]
    async fn cmd_o_opens_a_link_from_the_list(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        opened_before(&github, &git, cx).await;
        workspace.update_in(cx, show);
        cx.run_until_parked();

        cx.simulate_keystrokes("cmd-o");
        cx.simulate_input("https://github.com/acme/app/pull/1");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        let tabs = tabs(&workspace, cx);
        assert_eq!(tabs.len(), 1);
        assert_eq!(
            tabs[0].read_with(cx, |tab, _| tab.link().to_string()),
            "acme/app#1"
        );
        assert!(list(&workspace, cx).is_none());
    }

    #[gpui::test]
    async fn closing_the_list_some_other_way_brings_it_back(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        workspace.update_in(cx, show);
        cx.run_until_parked();
        let first = list(&workspace, cx).unwrap();
        assert_eq!(listed(&first, cx), Vec::<String>::new());

        let pane = workspace.read_with(cx, |workspace, _| workspace.active_pane().clone());
        pane.update_in(cx, |pane, window, cx| {
            pane.close_item_by_id(first.entity_id(), workspace::SaveIntent::Skip, window, cx)
        })
        .detach();
        cx.run_until_parked();

        let again = list(&workspace, cx).expect("the list shows again");
        assert_ne!(again, first);
        assert_eq!(
            workspace.read_with(cx, |workspace, cx| workspace.items(cx).count()),
            1
        );
    }

    /// What the list's rows say: each pull request, its account, whether that's signed out, and
    /// how long ago it was opened.
    fn rows(
        list: &Entity<RecentPrs>,
        cx: &mut VisualTestContext,
    ) -> Vec<(String, String, bool, String)> {
        list.read_with(cx, |list, _| {
            list.rows()
                .iter()
                .map(|row| {
                    (
                        row.pull.clone(),
                        row.account.clone(),
                        row.signed_out,
                        row.opened.clone(),
                    )
                })
                .collect()
        })
    }

    fn row(
        pull: &str,
        account: &str,
        signed_out: bool,
        opened: &str,
    ) -> (String, String, bool, String) {
        (pull.into(), account.into(), signed_out, opened.into())
    }

    #[gpui::test]
    async fn the_list_follows_the_accounts_and_the_time(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        opened_before(&github, &git, cx).await;
        workspace.update_in(cx, show);
        cx.run_until_parked();
        let list = list(&workspace, cx).unwrap();
        let clock = WallClock::following(cx.executor(), SystemTime::now());
        list.update(cx, |list, cx| list.set_clock(clock, cx));
        assert_eq!(
            rows(&list, cx),
            [
                row("acme/app#1", "home", false, "10 minutes ago"),
                row("rozed-dev/playground#2", "rozed-bot", false, "3 hours ago"),
                row("rozed-dev/playground#3", "rozed-bot", false, "a day ago"),
            ]
        );

        // Minutes later, it says so by itself.
        cx.executor().advance_clock(Duration::from_secs(5 * 60));
        cx.run_until_parked();
        assert_eq!(rows(&list, cx)[0].3, "15 minutes ago");

        // A signed-out account's rows say so.
        let accounts = cx.update(|_, cx| {
            let layer = github_layer::loaded(cx).unwrap();
            layer.read(cx).accounts().clone()
        });
        accounts.update(cx, |accounts, cx| {
            accounts.mark_signed_out("home", github::accounts::SignedOut::Refused, cx)
        });
        cx.run_until_parked();
        assert!(rows(&list, cx)[0].2);

        // A removed account's go.
        accounts
            .update(cx, |accounts, cx| accounts.remove("rozed-bot", cx))
            .await
            .unwrap();
        cx.run_until_parked();
        assert_eq!(
            rows(&list, cx),
            [row("acme/app#1", "home", true, "15 minutes ago")]
        );
    }

    #[gpui::test]
    async fn a_row_opens_with_its_account_while_its_signed_in(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        let pull = git.pull_request("acme/app", 1, &[], &[("a.rs", "a\n")]);
        github.add_pull(pull);
        for token in ["gho_HOME", "gho_WORK"] {
            github.let_see(token, "acme/app");
        }
        let (accounts, cache) =
            sign_in(&github, &[("home", "gho_HOME"), ("work", "gho_WORK")], cx).await;
        accounts
            .update(cx, |accounts, cx| {
                accounts.remember_account_for_owner("acme", "work", cx)
            })
            .await
            .unwrap();
        cache
            .record_recent(recent(("acme", "app", 1), Some("Change 1"), "home", 600))
            .await
            .unwrap();
        workspace.update_in(cx, show);
        cx.run_until_parked();

        // It was opened as home, which is still signed in, though acme's account is work.
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        let tab = tabs(&workspace, cx).pop().unwrap();
        assert!(workspace.read_with(cx, |workspace, cx| {
            workspace.active_modal::<AccountPicker>(cx).is_none()
        }));
        assert_eq!(
            tab.read_with(cx, |tab, _| tab.acting().cloned()),
            Some(Acting::Account("home".into()))
        );

        // Once home is signed out, the row opens with acme's account.
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
        accounts.update(cx, |accounts, cx| {
            accounts.mark_signed_out("home", github::accounts::SignedOut::Refused, cx)
        });
        cx.run_until_parked();
        let list = list(&workspace, cx).unwrap();
        assert!(rows(&list, cx)[0].2);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        let tab = tabs(&workspace, cx).pop().unwrap();
        assert_eq!(
            tab.read_with(cx, |tab, _| tab.acting().cloned()),
            Some(Acting::Account("work".into()))
        );
    }

    #[gpui::test]
    async fn a_row_whose_account_lost_access_opens_with_one_that_has_it(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        let pull = git.pull_request("acme/app", 1, &[], &[("a.rs", "a\n")]);
        github.add_pull(pull);
        // home opened it once, but only work can see it now.
        github.let_see("gho_WORK", "acme/app");
        let (_, cache) = sign_in(&github, &[("home", "gho_HOME"), ("work", "gho_WORK")], cx).await;
        cache
            .record_recent(recent(("acme", "app", 1), Some("Change 1"), "home", 600))
            .await
            .unwrap();
        workspace.update_in(cx, show);
        cx.run_until_parked();

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        let tab = tabs(&workspace, cx).pop().unwrap();
        assert_eq!(
            tab.read_with(cx, |tab, _| tab.status().clone()),
            pr_tab::Status::Ready
        );
        assert_eq!(
            tab.read_with(cx, |tab, _| tab.acting().cloned()),
            Some(Acting::Account("work".into()))
        );
    }

    #[gpui::test]
    async fn a_renamed_repository_is_listed_once_by_its_name_now(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        let github = FakeGithub::default();
        let git = GitServer::start(cx);
        let mut pull = git.pull_request("rozed-dev/renamed", 2, &[], &[("a.rs", "a\n")]);
        pull.old_names = vec!["rozed-dev/playground".into()];
        github.add_pull(pull);
        github.let_see("gho_BOT", "rozed-dev/renamed");
        let (_, cache) = sign_in(&github, &[("rozed-bot", "gho_BOT")], cx).await;
        cache
            .record_recent(recent(
                ("rozed-dev", "playground", 2),
                None,
                "rozed-bot",
                3_600,
            ))
            .await
            .unwrap();
        workspace.update_in(cx, show);
        cx.run_until_parked();
        assert_eq!(
            listed(&list(&workspace, cx).unwrap(), cx),
            ["rozed-dev/playground#2"]
        );

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        let tab = tabs(&workspace, cx).pop().unwrap();
        assert_eq!(
            tab.read_with(cx, |tab, _| tab.status().clone()),
            pr_tab::Status::Ready
        );
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

        assert_eq!(
            listed(&list(&workspace, cx).unwrap(), cx),
            ["rozed-dev/renamed#2"]
        );
    }

    #[gpui::test]
    async fn without_rozeds_database_the_list_says_so(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx).await;
        workspace.update_in(cx, show);
        cx.run_until_parked();
        let list = list(&workspace, cx).unwrap();
        assert_eq!(
            list.read_with(cx, |list, _| list.loaded),
            Loaded::NoDatabase
        );
    }
}
