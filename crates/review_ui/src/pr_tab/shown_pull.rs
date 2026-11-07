//! The pull request a tab shows: everything the tab knows about it, how its load is going, and
//! its diff, in one place, so a tab can swap one pull request for another, as a stack's tab does
//! for each layer (design §4), and keep the one it swapped out loading.
//!
//! Work that runs for a pull request, such as its load, GitHub's diff of it or a viewed mark's
//! write, holds a [`PullHandle`] to it, and writes to that pull request whether the tab shows it
//! by then or keeps it out of sight.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use anyhow::Context as _;
use editor::{Anchor, SplittableEditor};
use futures::channel::oneshot;
use github::poll::Poller;
use github::viewed::ViewedRecords;
use github::{HelpLink, PullRef};
use gpui::{
    App, AppContext, Context, Entity, EntityId, SharedString, Subscription, Task, WeakEntity,
    Window,
};
use project::{Project, WorktreeId};
use review::revisions::Revisions;

use super::{DiffSource, Header, OlderCopy, PrTab, Status, TabAction};
use crate::account::Acting;
use crate::file_list::FileList;
use crate::poll_state::PollState;
use crate::pr_banner::Banners;
use crate::pr_diff::LocalPullRequest;
use crate::pr_link::PrLink;
use crate::shown_files;
use crate::tabs::TabKey;
use crate::viewed_marks::ViewedMarks;

/// Which of a tab's pull requests something is about, unique for as long as Lo runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PullId(u64);

impl PullId {
    fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// One pull request in a tab: what GitHub said about it, its files and viewed marks, its
/// revisions, how it's polled, its local copy, how its load is going, and its diff. A tab shows
/// one at a time and can keep others out of sight ([`PrTab::show_pull`]).
///
/// Nothing runs when one is dropped: a pull request the tab lets go of is released with
/// [`PrTab::release_pull`], which stops its poller watching it and closes its checkout.
pub struct ShownPull {
    pub(super) id: PullId,
    /// The pull request's number.
    pub(super) number: u64,
    pub(super) key: TabKey,
    /// The account the tab acts as for it, once chosen.
    pub(super) acting: Option<Acting>,
    /// The account to open it with, while it can see the repository, whatever account the
    /// repository's choice is: a recent row's, for the load that row started.
    pub(super) preferred: Option<String>,
    pub(super) header: Header,
    /// When it was opened, as GitHub says.
    pub(super) created_at: Option<SystemTime>,
    pub(super) files: FileList,
    /// GitHub's viewed marks, and the ones being set, kept for when the files arrive.
    pub(super) marks: ViewedMarks,
    pub(super) revisions: Option<Revisions>,
    /// Its GraphQL node ID, which writes name it by.
    pub(super) node_id: Option<String>,
    /// The head on screen: GitHub's at first, then the one the local copy fetched.
    pub(super) head: Option<String>,
    /// When Lo learned the head on screen. What GitHub said about its head before then is
    /// older news.
    pub(super) head_at: Option<SystemTime>,
    /// GitHub's latest head for it, from its details or its summary, and when GitHub gave it.
    /// `e` marks files only while it's the head on screen.
    pub(super) latest_head: Option<(String, SystemTime)>,
    /// Where this Mac's records say each file was marked viewed, by path.
    pub(super) viewed_at: HashMap<String, String>,
    /// This Mac's records of where files were marked viewed, once signed in.
    pub(super) records: Option<ViewedRecords>,
    /// The account's poller and the pull request it watches for the tab, until it's released.
    pub(super) watched: Option<(Entity<Poller>, PullRef)>,
    /// How the poller is doing, for the header.
    pub(super) poll: PollState,
    /// What it hears from the poller it watches with, replaced when it watches with another.
    pub(super) _poller_subscriptions: Vec<Subscription>,
    /// Its local copy, once it's copied.
    pub(super) local: Option<Arc<LocalPullRequest>>,
    /// The trusted checkout's worktree, which goes with the last pull request that uses it.
    pub(super) checkout: Option<WorktreeId>,
    /// The head GitHub's REST API gave for it, which the local copy fetches.
    pub(super) api_head: Option<String>,
    /// The head GitHub's diff was listed at, once it shows and if GitHub's links say.
    pub(super) github_head: Option<String>,

    pub(super) status: Status,
    /// GitHub's links for fixing why it failed to load, such as authorizing single sign-on.
    pub(super) help_links: Vec<HelpLink>,
    /// What the tab offers once its load stopped for a reason only the user can fix.
    pub(super) action: Option<TabAction>,
    /// Why the diff shown is a local copy that isn't of GitHub's latest head, while it is.
    pub(super) showing_copy: Option<OlderCopy>,
    /// Whether the viewed marks shown are another account's than the one the tab acts as, to
    /// let go of once GitHub answers for that one.
    pub(super) viewed_are_others: bool,
    /// Whether GitHub gave this load the pull request, so anything that fails after is the local
    /// copy's.
    pub(super) answered: bool,
    /// The account and the time of the details Lo saved, while the tab shows them instead of
    /// GitHub's answer.
    pub(super) saved: Option<(String, SystemTime)>,
    /// What the tab should tell about what its diff shows, under its header.
    pub(super) banners: Entity<Banners>,
    pub(super) _load: Option<Task<()>>,
    /// Reads and shows GitHub's diff, until the local copy's replaces it.
    pub(super) _github_diff: Option<Task<()>>,
    /// Why the local copy failed for good, when it did before GitHub's diff showed.
    pub(super) copy_failed: Option<SharedString>,
    /// Whether GitHub's diff failed for good, or wasn't read, so it won't show.
    pub(super) github_failed: bool,
    /// Tries copying again, when Lo stopped trying after failures it doesn't know.
    pub(super) retry_copy: Option<oneshot::Sender<()>>,

    pub(super) diff: Option<Entity<SplittableEditor>>,
    pub(super) diff_source: Option<DiffSource>,
    /// Brings the diff's files in line with whether whitespace changes are hidden.
    pub(super) _whitespace: Option<Task<()>>,
    /// Loads large files on a key, one task a file of a diff, each until it's done.
    pub(super) loading: HashMap<(EntityId, String), Task<()>>,
    /// Decorates the diff again as it goes side by side and back, and stops decoding a file's
    /// images when it folds.
    pub(super) _diff_watch: Vec<Subscription>,
    /// Where the tab put the diff's cursor while files arrive, until it's moved.
    pub(super) parked_cursor: Option<Anchor>,
    /// Whether the diff's cursor was moved, by a key, a click or the file list. From then on the
    /// tab leaves it where it is.
    pub(super) cursor_moved: bool,
    /// Keeps the file list's selection on the file the diff's cursor is in, and the cursor at the
    /// start until it's moved.
    pub(super) _diff_cursor: Option<Subscription>,
    pub(super) _subscriptions: Vec<Subscription>,
}

impl ShownPull {
    /// The pull request `link` points at, for `cx`'s tab, before GitHub has said anything about
    /// it. [`PrTab::load`] loads it once the tab shows it.
    pub fn new(link: PrLink, cx: &mut Context<PrTab>) -> Self {
        let banners = cx.new(|_| Banners::default());
        let on_banners = cx.observe(&banners, |_, _, cx| cx.notify());
        Self {
            id: PullId::next(),
            number: link.number,
            key: TabKey::new(link),
            acting: None,
            preferred: None,
            header: Header::default(),
            created_at: None,
            files: FileList::default(),
            marks: ViewedMarks::default(),
            revisions: None,
            node_id: None,
            head: None,
            head_at: None,
            latest_head: None,
            viewed_at: HashMap::new(),
            records: None,
            watched: None,
            poll: PollState::default(),
            _poller_subscriptions: Vec::new(),
            local: None,
            checkout: None,
            api_head: None,
            github_head: None,
            status: Status::Loading("Finding an account that can see it…".into()),
            help_links: Vec::new(),
            action: None,
            showing_copy: None,
            viewed_are_others: false,
            answered: false,
            saved: None,
            banners,
            _load: None,
            _github_diff: None,
            copy_failed: None,
            github_failed: false,
            retry_copy: None,
            diff: None,
            diff_source: None,
            _whitespace: None,
            loading: HashMap::new(),
            _diff_watch: Vec::new(),
            parked_cursor: None,
            cursor_moved: false,
            _diff_cursor: None,
            _subscriptions: vec![on_banners],
        }
    }

    pub fn id(&self) -> PullId {
        self.id
    }

    pub fn number(&self) -> u64 {
        self.number
    }

    pub fn key(&self) -> &TabKey {
        &self.key
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

    /// GitHub's latest head for the pull request, once GitHub said.
    pub fn latest_head(&self) -> Option<&str> {
        self.latest_head.as_ref().map(|(head, _)| head.as_str())
    }

    /// The head the diff shows, once one does.
    pub fn head(&self) -> Option<&str> {
        self.head.as_deref()
    }

    /// Its diff, once one shows.
    pub fn diff(&self) -> Option<&Entity<SplittableEditor>> {
        self.diff.as_ref()
    }

    /// The banners about its diff.
    pub fn banners(&self) -> &Entity<Banners> {
        &self.banners
    }

    /// Where this Mac records `path` as viewed: the head on screen when it was marked.
    pub fn viewed_at(&self, path: &str) -> Option<&str> {
        self.viewed_at.get(path).map(String::as_str)
    }

    /// Lets go of what the pull request holds outside its tab: its poller stops watching it, its
    /// diff's images go, and its trusted checkout leaves `project`, with its language servers,
    /// unless `used_elsewhere` says another pull request uses it too. What it runs stops with it.
    pub(super) fn release(
        &mut self,
        project: Option<&Entity<Project>>,
        used_elsewhere: impl Fn(WorktreeId) -> bool,
        cx: &mut App,
    ) {
        self._load = None;
        self._github_diff = None;
        self.retry_copy = None;
        self.loading.clear();
        if let Some(diff) = &self.diff {
            shown_files::release_images(diff, None, cx);
        }
        if let Some((poller, pull)) = self.watched.take() {
            poller.update(cx, |poller, cx| poller.unwatch(&pull, cx));
        }
        self._poller_subscriptions.clear();
        let (Some(checkout), Some(project)) = (self.checkout.take(), project) else {
            return;
        };
        if used_elsewhere(checkout) {
            return;
        }
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
}

/// One of a tab's pull requests, as work started for it holds it: writes go to that pull
/// request, whether the tab shows it or keeps it out of sight by then, and fail once the tab has
/// let go of it, as they fail once the tab closes.
#[derive(Clone)]
pub struct PullHandle {
    tab: WeakEntity<PrTab>,
    id: PullId,
}

impl PullHandle {
    pub(super) fn new(tab: WeakEntity<PrTab>, id: PullId) -> Self {
        Self { tab, id }
    }

    pub fn tab(&self) -> &WeakEntity<PrTab> {
        &self.tab
    }

    pub fn id(&self) -> PullId {
        self.id
    }

    /// Updates the tab with the pull request as its `pull` ([`PrTab::with_pull`]).
    pub fn update<C, R>(
        &self,
        cx: &mut C,
        update: impl FnOnce(&mut PrTab, &mut Context<PrTab>) -> R,
    ) -> anyhow::Result<R>
    where
        C: AppContext,
    {
        let id = self.id;
        self.tab
            .update(cx, |tab, cx| tab.with_pull(id, cx, update))?
            .context("the tab let go of the pull request")
    }

    /// [`Self::update`], with the tab's window.
    pub fn update_in<C, R>(
        &self,
        cx: &mut C,
        update: impl FnOnce(&mut PrTab, &mut Window, &mut Context<PrTab>) -> R,
    ) -> anyhow::Result<R>
    where
        C: AppContext,
    {
        let id = self.id;
        self.tab
            .update_in(cx, |tab, window, cx| {
                tab.with_pull(id, cx, |tab, cx| update(tab, window, cx))
            })?
            .context("the tab let go of the pull request")
    }

    /// Reads the pull request, shown or kept.
    pub fn read_with<C, R>(
        &self,
        cx: &C,
        read: impl FnOnce(&ShownPull, &App) -> R,
    ) -> anyhow::Result<R>
    where
        C: AppContext,
    {
        let id = self.id;
        self.tab
            .read_with(cx, |tab, cx| tab.pull_by_id(id).map(|pull| read(pull, cx)))?
            .context("the tab let go of the pull request")
    }

    /// Whether the tab shows the pull request now.
    pub fn is_shown(&self, cx: &App) -> bool {
        self.tab
            .upgrade()
            .is_some_and(|tab| tab.read(cx).pull.id == self.id)
    }
}
