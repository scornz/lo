//! One account's poller: what it watches, when each poll is due, and what each answer changed.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;
use futures::StreamExt as _;
use futures::channel::{mpsc, oneshot};
use futures::future::{BoxFuture, FutureExt as _};
use gpui::{
    App, AppContext as _, AsyncApp, Context, Entity, EventEmitter, Subscription, Task, WeakEntity,
};

use super::governor::{
    Cooldown, PauseReason, PollConfig, RateLimitState, Tier, backoff, interval, rate_limit_state,
};
use super::queue::RequestQueue;
use super::reads::{CachedRead, ReadJob, ReadState, ReadType, Value, stamp};
use crate::accounts::Accounts;
use crate::cache::{CacheEntry, Cached, PrCache, RecentPr};
use crate::client::{Conditional, GithubClient};
use crate::error::GithubError;
use crate::pulls::{ListedPage, ListedPull, ListedPulls, PullRef, PullSummary, RepoRef, Target};
use crate::push_history::{PushHistory, read_push_history};
use crate::queries::open_pull_requests::RECENTLY_UPDATED_MERGED;
use crate::queries::pending_review::read_pending_review;
use crate::queries::review_threads::read_review_threads;
use crate::queries::stack_layers::read_stack_layers;
use crate::queries::timeline::read_timeline;
use crate::queries::{
    MAX_OPEN_PULL_REQUESTS, OpenPullRequestList, PAGE_SIZE, PullRequestCheckData, PullRequestData,
    PullRequestTimeline, ReviewThreadList, StackLayerList, TrunkRuleList, ViewerPendingReview,
};
use crate::rate_limit::RateLimits;
use crate::revision_marks::RevisionMarks;

/// What a poll found.
#[derive(Clone, Debug)]
pub enum PollEvent {
    /// GitHub's REST view of a watched pull request changed: a push, a retitle, a merge or a
    /// close. Sent for every watched pull request, the open one included.
    Summary {
        pull: PullRef,
        summary: Arc<PullSummary>,
    },
    /// The open pull request's cheap check changed, or was made for the first time: its checks'
    /// state, review decision, merge state, review threads or merge queue entry, or anything that
    /// means its details changed.
    Check {
        pull: PullRef,
        check: Arc<PullRequestCheckData>,
    },
    /// The open pull request's details changed, or were fetched for the first time.
    Details {
        pull: PullRef,
        details: Arc<PullRequestData>,
    },
    /// A watched repository's open pull requests changed.
    OpenPullRequests {
        repo: RepoRef,
        list: Arc<OpenPullRequestList>,
    },
    /// A watched repository's branch rulesets changed, or were read for the first time:
    /// [`Poller::trunk_rules`] has them.
    TrunkRules { repo: RepoRef },
    /// A poll failed for a reason that isn't the network, the rate limit or the token, such as a
    /// pull request that's gone or an organization's SSO. It's tried again at its next interval.
    Failed {
        target: Target,
        error: Arc<GithubError>,
    },
    /// A read [`Poller::read_cached`] keeps for a pull request changed, or was read for the first
    /// time: [`Poller::cached_read`] has it. `name` is its [`CacheEntry::NAME`].
    Read { pull: PullRef, name: &'static str },
    /// A read [`Poller::read_cached`] keeps failed for a reason that isn't the network, the rate
    /// limit or the token. It's tried again at the open pull request's next interval.
    ReadFailed {
        pull: PullRef,
        name: &'static str,
        error: Arc<GithubError>,
    },
}

/// How polling is going, for banners (design §3 "Rate limit", §11 "Offline", "Rate limit used
/// up", "Signed out").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PollStatus {
    pub rate_limit: RateLimitState,
    /// Set while GitHub can't be reached, or the account's token can't be read.
    pub offline: Option<Offline>,
    /// GitHub refused the account's token, so nothing is polled until it signs in again.
    pub signed_out: bool,
}

/// GitHub couldn't be reached, or the account's token couldn't be read (a locked Keychain).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offline {
    /// When the first request failed.
    pub since: SystemTime,
    /// When the next request will try.
    pub retry_at: SystemTime,
    pub error: String,
}

/// A pull request's details as Lo has them.
#[derive(Clone, Debug, PartialEq)]
pub struct PullRequestAnswer {
    pub details: Arc<PullRequestData>,
    /// When GitHub last confirmed them.
    pub fetched_at: SystemTime,
    /// While GitHub can't be reached, how old the details are: they're from the cache, and the
    /// app shows them read-only with when they're from (design §11 "Offline").
    pub offline_age: Option<Duration>,
}

/// When a pull request or a repository was polled, and whether it's owed a poll at once.
#[derive(Default)]
struct Schedule {
    last_polled: Option<Instant>,
    /// How many times it was asked to be polled at once: opened, Lo came to the foreground, or
    /// a write changed it.
    forced: u64,
    /// `forced` as it was when the last poll to finish was chosen. A poll asked for while another
    /// was in flight is still owed when that one finishes.
    served: u64,
}

impl Schedule {
    fn force(&mut self) {
        self.forced += 1;
    }

    fn is_forced(&self) -> bool {
        self.forced > self.served
    }

    /// A poll chosen when `forced` was `ticket` finished.
    fn polled(&mut self, at: Instant, ticket: u64) {
        self.last_polled = Some(at);
        self.served(ticket);
    }

    /// A poll chosen when `forced` was `ticket` finished without an answer, so it isn't owed
    /// again at once.
    fn served(&mut self, ticket: u64) {
        self.served = self.served.max(ticket);
    }
}

#[derive(Default)]
struct PullState {
    schedule: Schedule,
    summary: Option<Cached<Arc<PullSummary>>>,
    /// The check the details were last fetched in full at, and when. A different check means
    /// they're stale, and none means they must be fetched again whatever the check says.
    check: Option<Cached<Arc<PullRequestCheckData>>>,
    /// The latest check, as [`Poller::check`] returns it and [`PollEvent::Check`] sends it.
    latest_check: Option<Arc<PullRequestCheckData>>,
    /// The latest check as JSON, which reads' stamps come from.
    check_json: Option<serde_json::Value>,
    details: Option<Cached<Arc<PullRequestData>>>,
    /// The reads [`Poller::read_cached`] keeps, by name.
    reads: BTreeMap<&'static str, ReadState>,
}

impl PullState {
    fn set_latest_check(&mut self, check: Option<Arc<PullRequestCheckData>>) {
        self.check_json = (check.as_deref())
            .and_then(|check| serde_json::to_value(check).ok())
            .filter(|json| !json.is_null());
        self.latest_check = check;
    }
}

#[derive(Default)]
struct RepoState {
    schedule: Schedule,
    /// GitHub's REST list of the open PRs, page by page with their ETags: what tells whether
    /// `list` changed.
    listed: Option<Cached<Arc<ListedPulls>>>,
    list: Option<Cached<Arc<OpenPullRequestList>>>,
    rules: Option<Cached<Arc<TrunkRuleList>>>,
    /// How many reads of `rules` in a row couldn't read the rulesets, and when they may be read
    /// again.
    rules_failures: u32,
    rules_retry_at: Option<Instant>,
    /// How many times in a row the lookups of merged PRs stopped short, and when they may run
    /// again without the list changing.
    lookup_failures: u32,
    lookup_retry_at: Option<Instant>,
}

struct OfflineState {
    attempts: u32,
    retry_at: Instant,
    status: Offline,
}

/// One poll: a few requests run back to back in the account's queue.
#[derive(Clone, Debug)]
enum Job {
    /// The open PR: a free conditional GET, the 1-point check, then its details only when either
    /// says something changed.
    Open {
        pull: PullRef,
        summary: Option<Arc<PullSummary>>,
        check: Option<Arc<PullRequestCheckData>>,
        have_details: bool,
    },
    /// Another watched PR: the conditional GET alone.
    Watched {
        pull: PullRef,
        summary: Option<Arc<PullSummary>>,
    },
    /// A watched repository: conditional GETs of its open PRs, a page at a time, then the full
    /// list when they changed, and the merged PRs its stacks link through, carried over from
    /// `list` where their branches haven't moved.
    Repo {
        repo: RepoRef,
        listed: Option<Arc<ListedPulls>>,
        list: Option<Arc<OpenPullRequestList>>,
        /// Whether `list` is recent enough to carry its lookups over from.
        carry: bool,
        /// Whether to look up merged PRs again for `list` while the PRs haven't changed, because
        /// its lookups stopped short.
        retry_lookups: bool,
        rules: Option<Arc<TrunkRuleList>>,
        /// Whether to read the rulesets again.
        read_rules: bool,
    },
    /// A read [`Poller::read_cached`] keeps for the open PR.
    Read { pull: PullRef, read: ReadJob },
}

enum Outcome {
    Open(Box<OpenOutcome>),
    Watched {
        summary: Conditional<PullSummary>,
    },
    Repo {
        listed: ListedPulls,
        list: Option<OpenPullRequestList>,
        rules: Option<TrunkRuleList>,
    },
    Read {
        name: &'static str,
        value: Value,
        stamp: String,
        generation: u64,
        started: Instant,
    },
}

/// A conditional GET of a pull request, asking about changes since GitHub last updated the
/// summary Lo has (not its ETag, which a push to any branch in the repository changes).
async fn get_summary(
    client: &GithubClient,
    pull: &PullRef,
    known: Option<&PullSummary>,
) -> Result<Conditional<PullSummary>, GithubError> {
    let since = known.and_then(PullSummary::last_modified);
    client
        .get_modified_since(&pull.rest_path(), since.as_deref())
        .await
}

/// Whether GitHub's answer holds a summary other than the one Lo has. A `200` can bring the
/// same one back.
fn summary_changed(answer: &Conditional<PullSummary>, known: Option<&PullSummary>) -> bool {
    match answer {
        Conditional::NotModified => false,
        Conditional::Modified { value, .. } => known != Some(value),
    }
}

/// The most pages of open PRs read, as many as the open PR list holds.
const MAX_LISTED_PAGES: usize = MAX_OPEN_PULL_REQUESTS / PAGE_SIZE as usize;

/// Conditional GETs of a repository's open PRs, a page at a time while pages are full, each with
/// the ETag Lo has for it.
async fn get_listed(
    client: &GithubClient,
    repo: &RepoRef,
    known: Option<&ListedPulls>,
) -> Result<ListedPulls, GithubError> {
    let mut listed = ListedPulls::default();
    for number in 1..=MAX_LISTED_PAGES {
        let known = known.and_then(|known| known.pages.get(number - 1));
        let answer = client
            .get_conditional::<Vec<ListedPull>>(
                &repo.open_pulls_path(number),
                known.and_then(|page| page.etag.as_deref()),
            )
            .await?;
        let page = match answer {
            Conditional::Modified { value, etag } => ListedPage { etag, pulls: value },
            Conditional::NotModified => known.cloned().ok_or_else(|| {
                GithubError::Inconsistent(format!(
                    "GitHub said page {number} of {repo}'s open pull requests hadn't changed, \
                     but Lo never read it"
                ))
            })?,
        };
        let full = page.pulls.len() >= PAGE_SIZE as usize;
        listed.pages.push(page);
        if !full {
            break;
        }
    }
    Ok(listed)
}

struct OpenOutcome {
    summary: Conditional<PullSummary>,
    check: PullRequestCheckData,
    details: Option<PullRequestData>,
}

impl Job {
    fn target(&self) -> Target {
        match self {
            Self::Open { pull, .. } | Self::Watched { pull, .. } | Self::Read { pull, .. } => {
                Target::Pull(pull.clone())
            }
            Self::Repo { repo, .. } => Target::Repo(repo.clone()),
        }
    }

    /// The name of the read it runs, if it runs one.
    fn read(&self) -> Option<&'static str> {
        match self {
            Self::Read { read, .. } => Some(read.name),
            _ => None,
        }
    }

    async fn run(self, client: GithubClient) -> Result<Outcome, GithubError> {
        match self {
            Self::Open {
                pull,
                summary: known,
                check,
                have_details,
            } => {
                let summary = get_summary(&client, &pull, known.as_deref()).await?;
                let (owner, name) = (&pull.repo.owner, &pull.repo.name);
                let new_check = client.pull_request_check(owner, name, pull.number).await?;
                let changed = !have_details
                    || summary_changed(&summary, known.as_deref())
                    || check.as_deref() != Some(&new_check);
                let details = if changed {
                    Some(client.pull_request(owner, name, pull.number).await?)
                } else {
                    None
                };
                Ok(Outcome::Open(Box::new(OpenOutcome {
                    summary,
                    check: new_check,
                    details,
                })))
            }
            Self::Watched { pull, summary } => Ok(Outcome::Watched {
                summary: get_summary(&client, &pull, summary.as_deref()).await?,
            }),
            Self::Repo {
                repo,
                listed: known,
                list,
                ..
            } => {
                let listed = get_listed(&client, &repo, known.as_deref()).await?;
                let changed = known.is_none_or(|known| !known.lists_the_same(&listed));
                let list = if changed || list.is_none() {
                    let page = PAGE_SIZE;
                    let merged = RECENTLY_UPDATED_MERGED;
                    Some(client.open_pull_requests_paged(&repo, page, merged).await?)
                } else {
                    None
                };
                Ok(Outcome::Repo {
                    listed,
                    list,
                    rules: None,
                })
            }
            Self::Read { read, .. } => Ok(Outcome::Read {
                name: read.name,
                value: (read.run)(client, read.known).await?,
                stamp: read.stamp,
                generation: read.generation,
                started: read.started,
            }),
        }
    }
}

enum Step {
    /// Run a poll, chosen when its target's `Schedule::forced` was the number given.
    Run(Box<Job>, u64),
    /// Nothing is due: wait this long, or until woken. The loop waits at most
    /// [`PollConfig::max_wait`] before asking again.
    Wait(Option<Duration>),
}

type CacheWrite = BoxFuture<'static, Result<()>>;

/// A read [`Poller::read_cached`] keeps, to run again after a write
/// ([`Poller::write_reading_again`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Reread {
    /// The read's [`CacheEntry::NAME`].
    pub name: &'static str,
    /// Whether it reads everything again rather than what's new, as for an edit of an item a read
    /// of the newest page doesn't reach.
    pub afresh: bool,
}

/// Polls GitHub for one account (research §7 "Recommended scheme"): the open PR every 20 s,
/// other watched PRs and their repositories' open PRs every 2 minutes, and everything every
/// 5 minutes while Lo is in the background. Each poll asks first whether anything changed,
/// with requests that are free when nothing did, and fetches details only when something did.
/// Requests run one at a time, slowed or paused by the rate limit and backed off while offline.
///
/// The app tells it whether Lo is in the foreground ([`Poller::set_foreground`]), which PRs
/// are in tabs ([`Poller::watch`]) and which tab is active ([`Poller::set_open`]). What changed
/// arrives as [`PollEvent`]s; [`Poller::status`] is for banners and is observable.
pub struct Poller {
    accounts: Entity<Accounts>,
    login: String,
    cache: PrCache,
    marks: RevisionMarks,
    config: PollConfig,
    queue: RequestQueue,
    foreground: bool,
    open: Option<PullRef>,
    /// When the open PR was opened.
    opened_at: Option<SystemTime>,
    pulls: BTreeMap<PullRef, PullState>,
    repos: BTreeMap<RepoRef, RepoState>,
    /// The sets of layers each pull request's stack trees asked about, and when each was last
    /// asked ([`Poller::stack_layers`]).
    layer_asks: BTreeMap<PullRef, BTreeMap<BTreeSet<String>, SystemTime>>,
    /// The layers each pull request's read of them reads: the sets asked about lately.
    layer_union: BTreeMap<PullRef, BTreeSet<String>>,
    limits: RateLimits,
    cooldown: Option<Cooldown>,
    secondary_attempts: u32,
    offline: Option<OfflineState>,
    /// The error each target's polls keep failing with, so it's logged once.
    failures: BTreeMap<Target, String>,
    signed_out: bool,
    status: PollStatus,
    wake: mpsc::UnboundedSender<()>,
    _poll: Task<()>,
    _accounts: Subscription,
}

impl EventEmitter<PollEvent> for Poller {}

impl Poller {
    /// Starts polling for `login`, an account in `accounts`, in the foreground and with nothing
    /// watched.
    pub fn new(
        accounts: Entity<Accounts>,
        login: impl Into<String>,
        cache: PrCache,
        config: PollConfig,
        cx: &mut Context<Self>,
    ) -> Self {
        let (wake, woken) = mpsc::unbounded();
        let poll = cx.spawn(async move |this, cx| {
            if let Err(error) = poll(this, woken, cx).await {
                log::debug!("stopped polling: {error:#}");
            }
        });
        // An account that signs in again resumes polling.
        let observe = cx.observe(&accounts, |this, accounts, cx| {
            let accounts = accounts.read(cx);
            if this.signed_out
                && accounts.account(&this.login).is_some()
                && !accounts.is_signed_out(&this.login)
            {
                this.signed_out = false;
                this.wake();
            }
        });
        // Long-closed pull requests' revision marks and drafts go at launch, as unused clones do.
        let marks = RevisionMarks::new(cache.db().clone());
        let drafts = crate::drafts::DraftStore::new(cache.db().clone());
        let (pruning, now) = (marks.clone(), config.wall_clock.now());
        cx.background_spawn(async move {
            if let Err(error) = pruning.prune(now).await {
                log::warn!("forgetting closed pull requests' revisions: {error:#}");
            }
            if let Err(error) = drafts.prune_closed(now).await {
                log::warn!("forgetting closed pull requests' drafts: {error:#}");
            }
        })
        .detach();
        Self {
            accounts,
            login: login.into(),
            marks,
            cache,
            queue: RequestQueue::new(cx.background_executor().clone(), config.write_spacing),
            config,
            foreground: true,
            open: None,
            opened_at: None,
            pulls: BTreeMap::new(),
            repos: BTreeMap::new(),
            layer_asks: BTreeMap::new(),
            layer_union: BTreeMap::new(),
            limits: RateLimits::default(),
            cooldown: None,
            secondary_attempts: 0,
            offline: None,
            failures: BTreeMap::new(),
            signed_out: false,
            status: PollStatus::default(),
            wake,
            _poll: poll,
            _accounts: observe,
        }
    }

    pub fn login(&self) -> &str {
        &self.login
    }

    pub fn status(&self) -> &PollStatus {
        &self.status
    }

    /// The account's request queue, for requests made outside polls.
    pub fn queue(&self) -> &RequestQueue {
        &self.queue
    }

    pub fn is_foreground(&self) -> bool {
        self.foreground
    }

    /// Whether Lo is in the foreground. Coming back polls the open PR at once.
    pub fn set_foreground(&mut self, foreground: bool, cx: &mut Context<Self>) {
        if self.foreground == foreground {
            return;
        }
        self.foreground = foreground;
        if foreground && let Some(state) = self.open.as_ref().and_then(|o| self.pulls.get_mut(o)) {
            state.schedule.force();
        }
        self.wake();
        cx.notify();
    }

    /// Starts watching a pull request, such as one in a tab, and the open pull requests of its
    /// repository. What's cached about them is loaded at once.
    pub fn watch(&mut self, pull: PullRef, cx: &mut Context<Self>) {
        if self.pulls.contains_key(&pull) {
            return;
        }
        let target = Target::Pull(pull.clone());
        let check = self
            .cached::<PullRequestCheckData>(&target)
            .map(|c| c.map(Arc::new));
        let mut state = PullState {
            summary: self.cached::<PullSummary>(&target).map(|c| c.map(Arc::new)),
            ..PullState::default()
        };
        state.set_latest_check(check.as_ref().map(|c| c.value.clone()));
        state.check = check;
        if !self.repos.contains_key(&pull.repo) {
            let repo = Target::Repo(pull.repo.clone());
            let state = RepoState {
                schedule: Schedule::default(),
                listed: self.cached::<ListedPulls>(&repo).map(|c| c.map(Arc::new)),
                list: self
                    .cached::<OpenPullRequestList>(&repo)
                    .map(|c| c.map(Arc::new)),
                rules: self.cached::<TrunkRuleList>(&repo).map(|c| c.map(Arc::new)),
                rules_failures: 0,
                rules_retry_at: None,
                lookup_failures: 0,
                lookup_retry_at: None,
            };
            self.repos.insert(pull.repo.clone(), state);
        }
        self.pulls.insert(pull, state);
        self.wake();
        cx.notify();
    }

    /// Stops watching a pull request, and its repository when no other watched PR is in it.
    pub fn unwatch(&mut self, pull: &PullRef, cx: &mut Context<Self>) {
        if self.pulls.remove(pull).is_none() {
            return;
        }
        self.failures.remove(&Target::Pull(pull.clone()));
        self.layer_asks.remove(pull);
        self.layer_union.remove(pull);
        if self.open.as_ref() == Some(pull) {
            self.open = None;
            self.opened_at = None;
        }
        if !self.pulls.keys().any(|other| other.repo == pull.repo) {
            self.repos.remove(&pull.repo);
            self.failures.remove(&Target::Repo(pull.repo.clone()));
        }
        self.wake();
        cx.notify();
    }

    pub fn open(&self) -> Option<&PullRef> {
        self.open.as_ref()
    }

    /// Makes `pull` the open PR, the one in the active tab, watching it if it wasn't. Returns its
    /// cached details at once, if there are any, and refreshes them.
    pub fn set_open(
        &mut self,
        pull: Option<PullRef>,
        cx: &mut Context<Self>,
    ) -> Option<PullRequestAnswer> {
        if self.open == pull {
            return pull.and_then(|pull| self.pull_request(&pull));
        }
        self.open = pull.clone();
        self.opened_at = None;
        let Some(pull) = pull else {
            self.wake();
            cx.notify();
            return None;
        };
        self.watch(pull.clone(), cx);
        let details = match self.pulls.get(&pull).and_then(|s| s.details.clone()) {
            Some(details) => Some(details),
            None => self
                .cached::<PullRequestData>(&Target::Pull(pull.clone()))
                .map(|c| c.map(Arc::new)),
        };
        let state = self.pulls.entry(pull.clone()).or_default();
        state.schedule.force();
        state.details = details;
        let opened_at = self.config.wall_clock.now();
        self.opened_at = Some(opened_at);
        self.record_recent(&pull, cx);
        self.wake();
        cx.notify();
        self.pull_request(&pull)
    }

    /// A watched pull request's details, as last fetched.
    pub fn pull_request(&self, pull: &PullRef) -> Option<PullRequestAnswer> {
        let details = self.pulls.get(pull)?.details.as_ref()?;
        let offline_age = self
            .offline
            .is_some()
            .then(|| details.age_at(self.config.wall_clock.now()));
        Some(PullRequestAnswer {
            details: details.value.clone(),
            fetched_at: details.fetched_at,
            offline_age,
        })
    }

    /// The open pull request's cheap check, as of its details: its checks' state, review
    /// decision, merge state, review threads and merge queue entry.
    pub fn check(&self, pull: &PullRef) -> Option<Arc<PullRequestCheckData>> {
        self.pulls.get(pull)?.latest_check.clone()
    }

    /// GitHub's REST view of a watched pull request, as last fetched.
    pub fn summary(&self, pull: &PullRef) -> Option<Arc<PullSummary>> {
        Some(self.pulls.get(pull)?.summary.as_ref()?.value.clone())
    }

    /// A watched repository's open pull requests, as last fetched.
    pub fn open_pull_requests(&self, repo: &RepoRef) -> Option<Cached<Arc<OpenPullRequestList>>> {
        self.repos.get(repo)?.list.clone()
    }

    /// What this account's poller and the others saw of pull requests' revisions, and where you
    /// last left each.
    pub fn revision_marks(&self) -> &RevisionMarks {
        &self.marks
    }

    /// Reads something about `pull` with `request`, keeping it in the cache as `T`, watching
    /// `pull` if it wasn't. Returns what Lo has at once, if anything: what was read before, or
    /// what's in the cache, even from before a restart.
    ///
    /// While `pull` is the open pull request, `request` runs in the account's queue, after the
    /// open PR's poll: when Lo has nothing yet, when a field `T` watches in the open PR's check
    /// ([`CachedRead::WATCHED`]) is no longer what it was when `T` was read, and, like the details,
    /// on the 5-minute refresh in the foreground. `request` is given the value Lo has, so it can
    /// ask GitHub whether that changed. Each time the value changes, [`PollEvent::Read`] says so.
    /// A read is kept for as long as `pull` is watched; asking again replaces its `request`.
    pub fn read_cached<T, F, Fut>(
        &mut self,
        pull: PullRef,
        request: F,
        cx: &mut Context<Self>,
    ) -> Option<Cached<Arc<T>>>
    where
        T: CachedRead,
        F: Fn(GithubClient, Option<Arc<T>>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<T, GithubError>> + Send + 'static,
    {
        self.watch(pull.clone(), cx);
        let run: super::reads::ReadFn = Arc::new(move |client, known: Option<Value>| {
            let known = known.and_then(|known| known.downcast::<T>().ok());
            let read = request(client, known);
            async move { Ok(Arc::new(read.await?) as Value) }.boxed()
        });
        let (cache, login) = (self.cache.clone(), self.login.clone());
        let state = self.pulls.get_mut(&pull).expect("watched above");
        match state.reads.get_mut(T::NAME) {
            Some(read) => read.run = run,
            None => {
                let ty = ReadType::of::<T>();
                let loaded = (ty.load)(&cache, &login, &Target::Pull(pull.clone()));
                let (value, stamp, fetched_at) = match loaded {
                    Some((value, stamp, fetched_at)) => (Some(value), stamp, Some(fetched_at)),
                    None => (None, None, None),
                };
                state.reads.insert(
                    T::NAME,
                    ReadState {
                        ty,
                        run,
                        value,
                        stamp,
                        fetched_at,
                        failures: 0,
                        retry_at: None,
                        generation: 0,
                        again_at: None,
                        afresh: false,
                    },
                );
                self.wake();
                cx.notify();
            }
        }
        self.cached_read::<T>(&pull)
    }

    /// What a read [`Self::read_cached`] keeps for `pull` holds, and when GitHub last confirmed
    /// it.
    pub fn cached_read<T: CachedRead>(&self, pull: &PullRef) -> Option<Cached<Arc<T>>> {
        let read = self.pulls.get(pull)?.reads.get(T::NAME)?;
        Some(Cached {
            value: read.value.clone()?.downcast::<T>().ok()?,
            etag: None,
            fetched_at: read.fetched_at?,
        })
    }

    /// `pull`'s push history, as [`Self::read_cached`] keeps it, read as this account.
    pub fn push_history(
        &mut self,
        pull: PullRef,
        cx: &mut Context<Self>,
    ) -> Option<Cached<Arc<PushHistory>>> {
        let (login, target) = (self.login.clone(), pull.clone());
        self.read_cached(
            pull,
            move |client, known| read_push_history(client, target.clone(), login.clone(), known),
            cx,
        )
    }

    /// `pull`'s review threads, each with every comment, as [`Self::read_cached`] keeps them, read
    /// as this account. They're read again after a push, when a thread is added, deleted,
    /// resolved or unresolved, or gains or loses a comment ([`ReviewThreadList`]'s
    /// [`CachedRead::WATCHED`]), and on the 5-minute refresh.
    pub fn review_threads(
        &mut self,
        pull: PullRef,
        cx: &mut Context<Self>,
    ) -> Option<Cached<Arc<ReviewThreadList>>> {
        let target = pull.clone();
        self.read_cached(
            pull,
            move |client, known| read_review_threads(client, target.clone(), known),
            cx,
        )
    }

    /// This account's pending review on `pull`, as [`Self::read_cached`] keeps it. It's read
    /// again when the check's count of its comments changes, from Lo or anywhere else
    /// ([`ViewerPendingReview`]'s [`CachedRead::WATCHED`]), and on the 5-minute refresh.
    pub fn pending_review(
        &mut self,
        pull: PullRef,
        cx: &mut Context<Self>,
    ) -> Option<Cached<Arc<ViewerPendingReview>>> {
        let (login, target) = (self.login.clone(), pull.clone());
        self.read_cached(
            pull,
            move |client, known| read_pending_review(client, target.clone(), login.clone(), known),
            cx,
        )
    }

    /// `pull`'s timeline, as [`Self::read_cached`] keeps it, read as this account. It's read
    /// again when an item it shows is added or deleted, the pull request is updated (as by an
    /// edited comment), the latest commit's checks change or the description is edited
    /// ([`PullRequestTimeline`]'s [`CachedRead::WATCHED`]), and on the 5-minute refresh: each time
    /// only its newest page. So a change to an older item shows at the hourly full read, unless
    /// Lo made it: an edit or a deletion of Lo's reads it in full ([`Self::edit_comment`],
    /// [`Self::delete_comment`]).
    pub fn timeline(
        &mut self,
        pull: PullRef,
        cx: &mut Context<Self>,
    ) -> Option<Cached<Arc<PullRequestTimeline>>> {
        let target = pull.clone();
        let clock = self.config.wall_clock.clone();
        self.read_cached(
            pull,
            move |client, known| read_timeline(client, target.clone(), known, clock.now()),
            cx,
        )
    }

    /// What the stack tree shows of the layers whose node IDs are `ids`, kept for `pull`, the
    /// layer that's open, as [`Self::read_cached`] keeps it. It reads the layers of every set
    /// asked about in the last 5 minutes, so two trees asking about different sets for one pull
    /// share a read rather than take turns. It's read again:
    ///
    /// - when the open layer's state, head, review decision, checks, queue entry or retargets
    ///   change ([`StackLayerList`]'s [`CachedRead::WATCHED`]);
    /// - a couple of seconds after Lo last writes to another layer it shows, once for a burst of
    ///   writes. Writes to the open layer itself, such as viewed marks, aren't read back: the tab
    ///   holds them, and shows them over what this holds;
    /// - on the 5-minute refresh;
    /// - at once when asked about a layer it doesn't read, whatever the order.
    ///
    /// Until then it returns what it holds, which may be about other layers too.
    pub fn stack_layers(
        &mut self,
        pull: PullRef,
        ids: Vec<String>,
        cx: &mut Context<Self>,
    ) -> Option<Cached<Arc<StackLayerList>>> {
        let wall = self.config.wall_clock.now();
        let max_age = self.config.details_max_age;
        let asks = self.layer_asks.entry(pull.clone()).or_default();
        asks.insert(ids.iter().cloned().collect(), wall);
        asks.retain(|_, at| wall.duration_since(*at).unwrap_or_default() < max_age);
        // These layers first, in the order asked, then the other sets'.
        let mut asked = ids;
        for id in asks.keys().flatten() {
            if !asked.contains(id) {
                asked.push(id.clone());
            }
        }
        let union: BTreeSet<String> = asked.iter().cloned().collect();
        let repo = pull.repo.clone();
        let clock = self.config.wall_clock.clone();
        let cached = self.read_cached(
            pull.clone(),
            move |client, known| {
                read_stack_layers(client, repo.clone(), asked.clone(), known, clock.now())
            },
            cx,
        );
        let held = (cached.as_ref())
            .map(|cached| cached.value.ids.iter().cloned().collect::<BTreeSet<_>>());
        let reading = match self.layer_union.insert(pull.clone(), union.clone()) {
            Some(before) => before,
            None => held.unwrap_or_default(),
        };
        if !union.is_subset(&reading) {
            self.read_again::<StackLayerList>(&pull, cx);
        }
        cached
    }

    /// The branch rulesets that name trunks in a watched repository, read with its open PRs and
    /// again every 30 minutes, or `None` until they've been read once: a stack tree is "loading"
    /// until then ([`OpenPullRequestList::stacks`]). When they can't be read, they're tried
    /// again after a wait, and what they hold says why ([`TrunkRuleList::unavailable`]).
    pub fn trunk_rules(&self, repo: &RepoRef) -> Option<Cached<Arc<TrunkRuleList>>> {
        self.repos.get(repo)?.rules.clone()
    }

    /// Makes a read [`Self::read_cached`] keeps for `pull` due at once, whatever the fields it
    /// watches say, and whatever wait its failures set. A run already under way doesn't stamp
    /// what it reads, so it runs again.
    fn read_again<T: CachedRead>(&mut self, pull: &PullRef, cx: &mut Context<Self>) {
        self.read_again_named(pull, T::NAME, false, cx);
    }

    /// [`Self::read_again`] for the read named `name`. `afresh`, its next run isn't given the
    /// value Lo has, so it reads everything rather than what's new.
    fn read_again_named(
        &mut self,
        pull: &PullRef,
        name: &str,
        afresh: bool,
        cx: &mut Context<Self>,
    ) {
        let state = self.pulls.get_mut(pull);
        if let Some(read) = state.and_then(|state| state.reads.get_mut(name)) {
            read.afresh |= afresh;
            read.stamp = None;
            read.generation += 1;
            read.failures = 0;
            read.retry_at = None;
            self.wake();
            cx.notify();
        }
    }

    /// After a write to `pull`, makes every stack tree that shows it read its layers again a
    /// couple of seconds after the last write of a burst, so the burst is read back once. The open
    /// pull request's own tree doesn't read back a write to itself: its tab holds what it wrote.
    fn layer_written(&mut self, pull: &PullRef, cx: &mut Context<Self>) {
        let soon = cx.background_executor().now() + WRITES_READ_BACK_AFTER;
        let open = self.open.clone();
        for (other, state) in &mut self.pulls {
            if other.repo != pull.repo || (other == pull && open.as_ref() == Some(pull)) {
                continue;
            }
            let Some(read) = state.reads.get_mut(StackLayerList::NAME) else {
                continue;
            };
            let layers =
                (read.value.as_ref()).and_then(|value| value.downcast_ref::<StackLayerList>());
            if layers.is_some_and(|layers| (layers.layers.iter()).any(|l| l.number == pull.number))
            {
                read.again_at = Some(soon);
            }
        }
        self.wake();
        cx.notify();
    }

    /// Runs a write (a comment, a review, viewed marks, a merge) that changes `target`, with the
    /// account's client, in the account's queue: after any poll in flight, ahead of waiting
    /// polls, and at least a second after the previous write.
    ///
    /// Afterwards what Lo knows about `target` is out of date, whether or not the write
    /// succeeded (a failed one may still have reached GitHub): a pull request's details are
    /// fetched in full again, and so are the layers of every stack tree that shows it, such as
    /// its viewed files; a repository's open PRs are listed again; and either is polled at once
    /// if it's watched. A rate limit the write runs into pauses polling as a poll's would.
    ///
    /// The write goes on if the returned task is dropped, so closing what asked for it can't cut
    /// a merge or a review off halfway.
    pub fn write<T, F, Fut>(
        &mut self,
        target: impl Into<Target>,
        request: F,
        cx: &mut Context<Self>,
    ) -> Task<Result<T, GithubError>>
    where
        T: 'static,
        F: FnOnce(GithubClient) -> Fut + 'static,
        Fut: Future<Output = Result<T, GithubError>> + 'static,
    {
        self.write_reading_again(target, request, |_| Vec::new(), cx)
    }

    /// [`Self::write`], then makes the reads [`Self::read_cached`] keeps for the pull request it
    /// wrote to that `again` names due at once, whatever the fields they watch say: for a change
    /// the check can't see, such as an edited comment. `again` is given the write's result.
    pub(super) fn write_reading_again<T, F, Fut>(
        &mut self,
        target: impl Into<Target>,
        request: F,
        again: impl FnOnce(&Result<T, GithubError>) -> Vec<Reread> + 'static,
        cx: &mut Context<Self>,
    ) -> Task<Result<T, GithubError>>
    where
        T: 'static,
        F: FnOnce(GithubClient) -> Fut + 'static,
        Fut: Future<Output = Result<T, GithubError>> + 'static,
    {
        let target = target.into();
        let login = self.login.clone();
        let client = self
            .accounts
            .update(cx, |accounts, cx| accounts.client(&login, cx));
        let queue = self.queue.clone();
        let (done, result) = oneshot::channel();
        cx.spawn(async move |this, cx| {
            let client = match client.await {
                Ok(client) => client,
                Err(error) => {
                    let signed_out = this.read_with(cx, |this, cx| this.account_signed_out(cx));
                    let message = format!("{error:#}");
                    done.send(Err(if signed_out.unwrap_or(true) {
                        GithubError::Unauthorized { message }
                    } else {
                        GithubError::TokenUnavailable(message)
                    }))
                    .ok();
                    return;
                }
            };
            let result = queue.write(|| request(client.clone())).await;
            let forget = this.update(cx, |this, cx| {
                if answered(&result) {
                    this.offline = None;
                }
                this.limits = client.rate_limits();
                if let Err(GithubError::RateLimited {
                    secondary,
                    reset,
                    retry_after,
                    resource,
                    message,
                }) = &result
                {
                    log::info!("GitHub's rate limit, writing: {message}");
                    let wall = this.config.wall_clock.now();
                    this.rate_limited(*secondary, *reset, *retry_after, resource.clone(), wall);
                }
                this.refresh_status(cx);
                if let Target::Pull(pull) = &target {
                    for reread in again(&result) {
                        this.read_again_named(pull, reread.name, reread.afresh, cx);
                    }
                }
                this.invalidate(&target, cx)
            });
            if let Ok(forget) = forget
                && let Err(error) = forget.await
            {
                log::warn!("writing the cache: {error:#}");
            }
            done.send(result).ok();
        })
        .detach();
        cx.foreground_executor().spawn(async move {
            result.await.unwrap_or_else(|_| {
                Err(GithubError::Network(anyhow::anyhow!(
                    "Lo stopped before the write finished"
                )))
            })
        })
    }

    /// Forgets what makes `target` look current, so it's fetched again, at once if it's watched.
    /// Returns the cache write.
    fn invalidate(&mut self, target: &Target, cx: &mut Context<Self>) -> CacheWrite {
        let (cache, login, key) = (self.cache.clone(), self.login.clone(), target.clone());
        let forget = match target {
            Target::Pull(pull) => {
                if let Some(state) = self.pulls.get_mut(pull) {
                    state.check = None;
                    state.schedule.force();
                }
                self.layer_written(pull, cx);
                async move { cache.remove::<PullRequestCheckData>(&login, &key).await }.boxed()
            }
            Target::Repo(repo) => {
                if let Some(state) = self.repos.get_mut(repo) {
                    state.listed = None;
                    state.schedule.force();
                }
                async move { cache.remove::<ListedPulls>(&login, &key).await }.boxed()
            }
        };
        self.wake();
        cx.notify();
        forget
    }

    fn wake(&self) {
        self.wake.unbounded_send(()).ok();
    }

    fn cached<T: CacheEntry>(&self, target: &Target) -> Option<Cached<T>> {
        self.cache
            .get(&self.login, target)
            .inspect_err(|error| log::warn!("reading the cache: {error:#}"))
            .ok()
            .flatten()
    }

    fn record_recent(&self, pull: &PullRef, cx: &mut Context<Self>) {
        let Some(opened_at) = self.opened_at else {
            return;
        };
        let title = self
            .pulls
            .get(pull)
            .and_then(|state| state.details.as_ref())
            .map(|details| details.value.title.clone());
        let recent = RecentPr {
            pull: pull.clone(),
            title,
            account: self.login.clone(),
            opened_at,
        };
        let cache = self.cache.clone();
        cx.background_spawn(async move {
            if let Err(error) = cache.record_recent(recent).await {
                log::warn!("recording a recent PR: {error:#}");
            }
        })
        .detach();
    }

    fn refresh_status(&mut self, cx: &mut Context<Self>) {
        let now = self.config.wall_clock.now();
        let status = PollStatus {
            rate_limit: rate_limit_state(&self.limits, self.cooldown.as_ref(), now, &self.config),
            offline: self.offline.as_ref().map(|offline| offline.status.clone()),
            signed_out: self.signed_out,
        };
        if status != self.status {
            self.status = status;
            cx.notify();
        }
    }

    /// The next poll to run, or how long to wait for one.
    fn next_step(&mut self, cx: &mut Context<Self>) -> Step {
        self.refresh_status(cx);
        if self.signed_out {
            return Step::Wait(None);
        }
        let now = cx.background_executor().now();
        if let RateLimitState::Paused { until, .. } = &self.status.rate_limit {
            let wait = until
                .duration_since(self.config.wall_clock.now())
                .unwrap_or_default();
            return Step::Wait(Some(wait.max(Duration::from_millis(1))));
        }
        let due = |schedule: &Schedule, tier: Tier| -> Option<Instant> {
            let interval = interval(tier, self.foreground, &self.status.rate_limit, &self.config)?;
            Some(match schedule.last_polled {
                Some(last) if !schedule.is_forced() => last + interval,
                _ => now,
            })
        };

        // The open PR first, then the other PRs, then the repositories; earliest due first.
        let mut next: Option<(Instant, Job, u64, bool)> = None;
        let mut consider = |schedule: &Schedule, tier: Tier, job: &dyn Fn() -> Job| {
            if let Some(at) = due(schedule, tier)
                && next.as_ref().is_none_or(|(best, ..)| at < *best)
            {
                next = Some((at, job(), schedule.forced, schedule.is_forced()));
            }
        };
        // In the foreground, and unless the rate limit is very low, details older than
        // `details_max_age` are fetched again whatever the check says.
        let wall = self.config.wall_clock.now();
        let refresh_details =
            self.foreground && !matches!(self.status.rate_limit, RateLimitState::OpenOnly(_));
        if let Some(open) = &self.open
            && let Some(state) = self.pulls.get(open)
        {
            consider(&state.schedule, Tier::Open, &|| Job::Open {
                pull: open.clone(),
                summary: state.summary.as_ref().map(|s| s.value.clone()),
                check: state
                    .check
                    .as_ref()
                    .filter(|check| {
                        !refresh_details || check.age_at(wall) < self.config.details_max_age
                    })
                    .map(|check| check.value.clone()),
                have_details: state.details.is_some(),
            });
        }
        for (pull, state) in &self.pulls {
            if Some(pull) == self.open.as_ref() {
                continue;
            }
            consider(&state.schedule, Tier::Watched, &|| Job::Watched {
                pull: pull.clone(),
                summary: state.summary.as_ref().map(|s| s.value.clone()),
            });
        }
        for (repo, state) in &self.repos {
            let ready = |retry_at: Option<Instant>| retry_at.is_none_or(|at| at <= now);
            let rules_due = match &state.rules {
                None => true,
                Some(rules) if rules.value.unavailable.is_some() => ready(state.rules_retry_at),
                Some(rules) => rules.age_at(wall) >= TRUNK_RULES_MAX_AGE,
            };
            let truncated = (state.list.as_ref()).is_some_and(|list| list.value.merged_truncated);
            consider(&state.schedule, Tier::Watched, &|| Job::Repo {
                repo: repo.clone(),
                listed: state.listed.as_ref().map(|l| l.value.clone()),
                list: state.list.as_ref().map(|list| list.value.clone()),
                carry: (state.list.as_ref()).is_some_and(|list| list.age_at(wall) < CARRY_MAX_AGE),
                retry_lookups: truncated && ready(state.lookup_retry_at),
                rules: state.rules.as_ref().map(|rules| rules.value.clone()),
                read_rules: rules_due,
            });
        }
        // The open PR's reads, once its check says what they watch.
        if let Some(open) = &self.open
            && let Some(state) = self.pulls.get(open)
            && let Some(check) = &state.check_json
            && interval(
                Tier::Open,
                self.foreground,
                &self.status.rate_limit,
                &self.config,
            )
            .is_some()
        {
            for (name, read) in &state.reads {
                let current = stamp(check, read.ty.watched);
                let at = if read.value.is_none() || read.stamp.as_deref() != Some(current.as_str())
                {
                    Some(now)
                } else if refresh_details && let Some(fetched_at) = read.fetched_at {
                    let age = wall.duration_since(fetched_at).unwrap_or_default();
                    Some(now + self.config.details_max_age.saturating_sub(age))
                } else {
                    None
                };
                // A write reads it back a little later.
                let at = match (at, read.again_at) {
                    (Some(at), Some(again)) => Some(at.min(again)),
                    (at, again) => at.or(again),
                };
                // A read that keeps failing waits longer each time, whatever made it due.
                let at = at.map(|at| read.retry_at.map_or(at, |retry_at| at.max(retry_at)));
                if let Some(at) = at
                    && next.as_ref().is_none_or(|(best, ..)| at < *best)
                {
                    let read = ReadJob {
                        name,
                        run: read.run.clone(),
                        known: (read.value.clone()).filter(|_| !read.afresh),
                        stamp: current,
                        generation: read.generation,
                        started: now,
                    };
                    next = Some((
                        at,
                        Job::Read {
                            pull: open.clone(),
                            read,
                        },
                        0,
                        false,
                    ));
                }
            }
        }
        // While the limit is low, intervals are longer and some tiers aren't polled at all. Wake at
        // its reset, when they're back.
        let until_reset = match &self.status.rate_limit {
            RateLimitState::Slowed(low) | RateLimitState::OpenOnly(low) => Some(
                low.reset
                    .duration_since(self.config.wall_clock.now())
                    .unwrap_or_default()
                    .max(Duration::from_millis(1)),
            ),
            _ => None,
        };
        let Some((mut at, job, ticket, forced)) = next else {
            return Step::Wait(until_reset);
        };
        // Opening a PR or coming back to the foreground tries at once, whatever the backoff.
        if let Some(offline) = &mut self.offline
            && !forced
        {
            at = at.max(offline.retry_at);
            // The banner says when the next try really is, which can be after the backoff.
            let retry_at = wall + at.saturating_duration_since(now);
            if offline.status.retry_at != retry_at {
                offline.status.retry_at = retry_at;
                self.refresh_status(cx);
            }
        }
        if at <= now {
            Step::Run(Box::new(job), ticket)
        } else {
            let wait = at - now;
            Step::Wait(Some(until_reset.map_or(wait, |reset| wait.min(reset))))
        }
    }

    /// Takes in a poll's answer: updates what's known, sends events, and returns the cache
    /// writes to make.
    fn finish(
        &mut self,
        job: Job,
        ticket: u64,
        result: Result<Outcome, GithubError>,
        client: &GithubClient,
        cx: &mut Context<Self>,
    ) -> Vec<CacheWrite> {
        self.limits = client.rate_limits();
        let now = cx.background_executor().now();
        let wall = self.config.wall_clock.now();
        let target = job.target();
        let read = job.read();
        let mut writes = Vec::new();
        if answered(&result) {
            self.offline = None;
        }
        match result {
            Ok(outcome) => {
                self.secondary_attempts = 0;
                self.failures.remove(&target);
                // A read keeps its own times, apart from its pull request's polls.
                if read.is_none() {
                    self.schedule(&target, |schedule| schedule.polled(now, ticket));
                }
                writes = self.apply(outcome, &target, wall, cx);
            }
            Err(GithubError::Network(error)) => {
                if read.is_none() {
                    self.schedule(&target, |schedule| schedule.served(ticket));
                }
                self.back_off(format!("{error:#}"), now, wall, cx);
            }
            Err(GithubError::RateLimited {
                secondary,
                reset,
                retry_after,
                resource,
                message,
            }) => {
                log::info!("GitHub's rate limit: {message}");
                self.rate_limited(secondary, reset, retry_after, resource, wall);
            }
            // The client's hook has already read a newer token from gh for an account imported
            // from it, and signed the account out when there was none.
            Err(GithubError::Unauthorized { .. })
                if self.accounts.read(cx).is_signed_out(&self.login) =>
            {
                self.signed_out = true;
            }
            Err(error) => match (read, &target) {
                (Some(name), Target::Pull(pull)) => {
                    log::info!("reading {name} for {pull}: {error:#}");
                    self.read_failed(pull, name, now);
                    if self.is_watched(&target) {
                        cx.emit(PollEvent::ReadFailed {
                            pull: pull.clone(),
                            name,
                            error: Arc::new(error),
                        });
                    }
                }
                _ => {
                    // Logged once while it keeps failing the same way.
                    let message = format!("{error:#}");
                    if self.failures.get(&target) == Some(&message) {
                        log::debug!("polling {target:?}: {message}");
                    } else {
                        log::info!("polling {target:?}: {message}");
                        self.failures.insert(target.clone(), message);
                    }
                    if self.is_watched(&target) {
                        self.schedule(&target, |schedule| schedule.polled(now, ticket));
                        cx.emit(PollEvent::Failed {
                            target,
                            error: Arc::new(error),
                        });
                    }
                }
            },
        }
        self.refresh_status(cx);
        writes
    }

    /// Notes that a read failed, or gave a value that isn't complete, so it waits before it's
    /// read again: the open pull request's interval, doubling with each failure in a row, up to
    /// the 5-minute refresh's.
    fn read_failed(&mut self, pull: &PullRef, name: &'static str, now: Instant) {
        let (first, max, jitter) = (
            self.config.open_interval,
            self.config.details_max_age,
            self.config.jitter,
        );
        let state = self.pulls.get_mut(pull);
        if let Some(read) = state.and_then(|state| state.reads.get_mut(name)) {
            read.failures += 1;
            let wait = backoff(read.failures, first, max, jitter, jitter_sample());
            read.retry_at = Some(now + wait);
        }
    }

    /// Pauses polling as a rate-limit error asks: until the limit that was hit resets, or, for
    /// GitHub's secondary limits, for as long as GitHub asked or a minute doubling.
    fn rate_limited(
        &mut self,
        secondary: bool,
        reset: Option<u64>,
        retry_after: Option<Duration>,
        resource: Option<String>,
        wall: SystemTime,
    ) {
        let used_up = |resource: &str| self.limits.get(resource).is_some_and(|l| l.remaining == 0);
        // The limit GitHub names, else whichever is at zero. With none at zero, it can only have
        // been a secondary limit.
        let used_up = (!secondary)
            .then(|| {
                resource.filter(|r| used_up(r)).or_else(|| {
                    ["graphql", "core"]
                        .into_iter()
                        .find(|r| used_up(r))
                        .map(str::to_string)
                })
            })
            .flatten();
        self.cooldown = Some(match used_up {
            Some(resource) => {
                let until = reset
                    .or_else(|| self.limits.get(&resource).map(|limit| limit.reset))
                    .map(|reset| SystemTime::UNIX_EPOCH + Duration::from_secs(reset))
                    .filter(|until| *until > wall)
                    .unwrap_or(wall + self.config.secondary_first_wait);
                Cooldown {
                    until,
                    reason: PauseReason::UsedUp { resource },
                }
            }
            None => {
                self.secondary_attempts += 1;
                let wait = retry_after.unwrap_or_else(|| {
                    backoff(
                        self.secondary_attempts,
                        self.config.secondary_first_wait,
                        self.config.secondary_max_wait,
                        0.0,
                        0.0,
                    )
                });
                Cooldown {
                    until: wall + wait,
                    reason: PauseReason::Secondary,
                }
            }
        });
    }

    fn is_watched(&self, target: &Target) -> bool {
        match target {
            Target::Pull(pull) => self.pulls.contains_key(pull),
            Target::Repo(repo) => self.repos.contains_key(repo),
        }
    }

    /// Updates a pull request's or a repository's schedule, if it's still watched.
    fn schedule(&mut self, target: &Target, update: impl FnOnce(&mut Schedule)) {
        let schedule = match target {
            Target::Pull(pull) => self.pulls.get_mut(pull).map(|state| &mut state.schedule),
            Target::Repo(repo) => self.repos.get_mut(repo).map(|state| &mut state.schedule),
        };
        if let Some(schedule) = schedule {
            update(schedule);
        }
    }

    fn apply(
        &mut self,
        outcome: Outcome,
        target: &Target,
        wall: SystemTime,
        cx: &mut Context<Self>,
    ) -> Vec<CacheWrite> {
        let mut writes = Vec::new();
        let put = |writes: &mut Vec<CacheWrite>, cache: &PrCache, login: &str, write: Put| {
            let (cache, login, target) = (cache.clone(), login.to_string(), target.clone());
            writes.push(
                async move {
                    match write {
                        Put::Summary(value) => {
                            cache.put(&login, &target, &*value, None, wall).await
                        }
                        Put::TouchSummary => {
                            cache.touch::<PullSummary>(&login, &target, wall).await
                        }
                        Put::Details(details, check) => {
                            let rows =
                                vec![PrCache::row(&*details, None)?, PrCache::row(&*check, None)?];
                            cache.put_together(&login, &target, rows, wall).await
                        }
                        Put::Listed(value) => cache.put(&login, &target, &*value, None, wall).await,
                        Put::TouchListed => cache.touch::<ListedPulls>(&login, &target, wall).await,
                        Put::List(value) => cache.put(&login, &target, &*value, None, wall).await,
                        Put::Rules(value) => cache.put(&login, &target, &*value, None, wall).await,
                        Put::TouchDetails => {
                            cache.touch::<PullRequestData>(&login, &target, wall).await
                        }
                        Put::TouchList => {
                            cache
                                .touch::<OpenPullRequestList>(&login, &target, wall)
                                .await
                        }
                        Put::ForgetCheck => {
                            cache.remove::<PullRequestCheckData>(&login, &target).await
                        }
                    }
                }
                .boxed(),
            );
        };
        let (cache, login) = (self.cache.clone(), self.login.clone());
        // Every head a poll brings is noted as seen, once for each answer that brings it anew.
        let marks = self.marks.clone();
        let see = |writes: &mut Vec<CacheWrite>, pull: &PullRef, heads: Vec<&str>| {
            let mut heads: Vec<String> = heads.into_iter().map(str::to_string).collect();
            heads.sort();
            heads.dedup();
            let (marks, login, pull) = (marks.clone(), login.clone(), pull.clone());
            writes.push(
                async move {
                    for head in heads {
                        marks.record_seen(&login, &pull, &head, wall).await?;
                    }
                    Ok(())
                }
                .boxed(),
            );
        };
        match (outcome, target) {
            (Outcome::Open(open), Target::Pull(pull)) => {
                let OpenOutcome {
                    summary,
                    check,
                    details,
                } = *open;
                let Some(state) = self.pulls.get_mut(pull) else {
                    return writes;
                };
                let summary = Self::take_summary(state, summary, wall);
                let check = Arc::new(check);
                let changed_check = (state.latest_check.as_ref() != Some(&check)).then(|| {
                    state.set_latest_check(Some(check.clone()));
                    check.clone()
                });
                let changed_details = match details {
                    Some(details) => {
                        let changed = state
                            .details
                            .as_ref()
                            .is_none_or(|known| *known.value != details);
                        let details = Arc::new(details);
                        state.details = Some(Cached {
                            value: details.clone(),
                            etag: None,
                            fetched_at: wall,
                        });
                        state.check = Some(Cached {
                            value: check.clone(),
                            etag: None,
                            fetched_at: wall,
                        });
                        // Together, so a restart never pairs old details with a newer check.
                        put(
                            &mut writes,
                            &cache,
                            &login,
                            Put::Details(details.clone(), check),
                        );
                        changed.then_some(details)
                    }
                    None => {
                        // Nothing changed: the details are current as of now.
                        if let Some(known) = &mut state.details {
                            known.fetched_at = wall;
                        }
                        put(&mut writes, &cache, &login, Put::TouchDetails);
                        None
                    }
                };
                let heads: Vec<&str> = [
                    summary.as_ref().map(|summary| summary.head.sha.as_str()),
                    changed_check
                        .as_ref()
                        .map(|check| check.head_ref_oid.as_str()),
                    changed_details
                        .as_ref()
                        .map(|details| details.head_ref_oid.as_str()),
                ]
                .into_iter()
                .flatten()
                .collect();
                if !heads.is_empty() {
                    see(&mut writes, pull, heads);
                }
                match summary {
                    Some(summary) => {
                        put(&mut writes, &cache, &login, Put::Summary(summary.clone()));
                        cx.emit(PollEvent::Summary {
                            pull: pull.clone(),
                            summary,
                        });
                    }
                    None => put(&mut writes, &cache, &login, Put::TouchSummary),
                }
                if let Some(check) = changed_check {
                    cx.emit(PollEvent::Check {
                        pull: pull.clone(),
                        check,
                    });
                }
                if let Some(details) = changed_details {
                    if self.open.as_ref() == Some(pull) {
                        self.record_recent(pull, cx);
                    }
                    cx.emit(PollEvent::Details {
                        pull: pull.clone(),
                        details,
                    });
                }
            }
            (Outcome::Watched { summary }, Target::Pull(pull)) => {
                let Some(state) = self.pulls.get_mut(pull) else {
                    return writes;
                };
                match Self::take_summary(state, summary, wall) {
                    Some(summary) => {
                        // The details need fetching again when it's opened, even if the check
                        // doesn't show what changed.
                        state.check = None;
                        see(&mut writes, pull, vec![&summary.head.sha]);
                        put(&mut writes, &cache, &login, Put::ForgetCheck);
                        put(&mut writes, &cache, &login, Put::Summary(summary.clone()));
                        cx.emit(PollEvent::Summary {
                            pull: pull.clone(),
                            summary,
                        });
                    }
                    None => put(&mut writes, &cache, &login, Put::TouchSummary),
                }
            }
            (
                Outcome::Repo {
                    listed,
                    list,
                    rules,
                },
                Target::Repo(repo),
            ) => {
                let (first, jitter) = (self.config.watched_interval, self.config.jitter);
                let now = cx.background_executor().now();
                let Some(state) = self.repos.get_mut(repo) else {
                    return writes;
                };
                if let Some(rules) = rules {
                    if rules.unavailable.is_some() {
                        state.rules_failures += 1;
                        let wait = backoff(
                            state.rules_failures,
                            first,
                            TRUNK_RULES_MAX_AGE,
                            jitter,
                            jitter_sample(),
                        );
                        state.rules_retry_at = Some(now + wait);
                    } else {
                        state.rules_failures = 0;
                        state.rules_retry_at = None;
                    }
                    let changed = (state.rules.as_ref()).is_none_or(|known| *known.value != rules);
                    let rules = Arc::new(rules);
                    state.rules = Some(Cached {
                        value: rules.clone(),
                        etag: None,
                        fetched_at: wall,
                    });
                    put(&mut writes, &cache, &login, Put::Rules(rules));
                    if changed {
                        cx.emit(PollEvent::TrunkRules { repo: repo.clone() });
                    }
                }
                // Lookups that stopped short run again after a wait, if the PRs don't change.
                if let Some(list) = &list {
                    if list.merged_truncated {
                        state.lookup_failures += 1;
                        let wait = backoff(
                            state.lookup_failures,
                            first,
                            TRUNK_RULES_MAX_AGE,
                            jitter,
                            jitter_sample(),
                        );
                        state.lookup_retry_at = Some(now + wait);
                    } else {
                        state.lookup_failures = 0;
                        state.lookup_retry_at = None;
                    }
                }
                if state
                    .listed
                    .as_ref()
                    .is_some_and(|known| *known.value == listed)
                {
                    if let Some(known) = &mut state.listed {
                        known.fetched_at = wall;
                    }
                    put(&mut writes, &cache, &login, Put::TouchListed);
                } else {
                    // New ETags, if nothing else.
                    let listed = Arc::new(listed);
                    state.listed = Some(Cached {
                        value: listed.clone(),
                        etag: None,
                        fetched_at: wall,
                    });
                    put(&mut writes, &cache, &login, Put::Listed(listed));
                }
                match list {
                    Some(list) => {
                        let changed = state.list.as_ref().is_none_or(|known| *known.value != list);
                        let list = Arc::new(list);
                        state.list = Some(Cached {
                            value: list.clone(),
                            etag: None,
                            fetched_at: wall,
                        });
                        put(&mut writes, &cache, &login, Put::List(list.clone()));
                        if changed {
                            cx.emit(PollEvent::OpenPullRequests {
                                repo: repo.clone(),
                                list,
                            });
                        }
                    }
                    None => {
                        if let Some(known) = &mut state.list {
                            known.fetched_at = wall;
                        }
                        put(&mut writes, &cache, &login, Put::TouchList);
                    }
                }
            }
            (
                Outcome::Read {
                    name,
                    value,
                    stamp,
                    generation,
                    started,
                },
                Target::Pull(pull),
            ) => {
                let state = self.pulls.get_mut(pull);
                let Some(read) = state.and_then(|state| state.reads.get_mut(name)) else {
                    return writes;
                };
                let changed =
                    (read.value.as_ref()).is_none_or(|known| !(read.ty.same)(known, &value));
                // A value that isn't complete is kept without a stamp, so it's read again once
                // its wait is over. So is one whose read started before it was asked to read
                // again, which it then does at once.
                let complete = (read.ty.complete)(&value);
                let stamp = (complete && generation == read.generation).then_some(stamp);
                let restamped = read.stamp != stamp;
                if generation == read.generation {
                    read.afresh = false;
                }
                read.value = Some(value.clone());
                read.stamp = stamp.clone();
                read.fetched_at = Some(wall);
                // A write's read-back is done once a read started after it was due.
                if read.again_at.is_some_and(|again| again <= started) {
                    read.again_at = None;
                }
                if complete {
                    read.failures = 0;
                    read.retry_at = None;
                }
                let target = target.clone();
                writes.push(if changed {
                    (read.ty.put)(cache, login, target, stamp, value, wall)
                } else if restamped {
                    (read.ty.restamp)(cache, login, target, stamp, wall)
                } else {
                    (read.ty.touch)(cache, login, target, wall)
                });
                if !complete {
                    let now = cx.background_executor().now();
                    self.read_failed(pull, name, now);
                }
                if changed {
                    cx.emit(PollEvent::Read {
                        pull: pull.clone(),
                        name,
                    });
                }
            }
            _ => {}
        }
        writes
    }

    /// Takes in GitHub's answer about a pull request, returning its summary when it changed. A
    /// `304`, or a `200` with the same summary, only confirms the one Lo has.
    fn take_summary(
        state: &mut PullState,
        answer: Conditional<PullSummary>,
        wall: SystemTime,
    ) -> Option<Arc<PullSummary>> {
        let known = state.summary.as_ref().map(|known| &*known.value);
        if !summary_changed(&answer, known) {
            if let Some(known) = &mut state.summary {
                known.fetched_at = wall;
            }
            return None;
        }
        let Conditional::Modified { value, .. } = answer else {
            return None;
        };
        let value = Arc::new(value);
        state.summary = Some(Cached {
            value: value.clone(),
            etag: None,
            fetched_at: wall,
        });
        Some(value)
    }

    /// Waits longer each time before trying again, while GitHub can't be reached or the
    /// account's token can't be read, and says so in the status.
    fn back_off(&mut self, error: String, now: Instant, wall: SystemTime, cx: &mut Context<Self>) {
        let attempts = self.offline.as_ref().map_or(0, |o| o.attempts) + 1;
        let wait = backoff(
            attempts,
            self.config.offline_first_wait,
            self.config.offline_max_wait,
            self.config.jitter,
            jitter_sample(),
        );
        log::info!("{error}; trying again in {wait:?}");
        let since = self.offline.as_ref().map_or(wall, |o| o.status.since);
        self.offline = Some(OfflineState {
            attempts,
            retry_at: now + wait,
            status: Offline {
                since,
                retry_at: wall + wait,
                error,
            },
        });
        // Details shown from the cache now carry their age.
        cx.notify();
    }

    /// Whether `Accounts` says the account is signed out, or it's gone. Only then is it signed out
    /// here.
    fn account_signed_out(&self, cx: &App) -> bool {
        let accounts = self.accounts.read(cx);
        accounts.account(&self.login).is_none() || accounts.is_signed_out(&self.login)
    }

    /// There was no client for a poll. A signed-out account stops polling until it signs in again;
    /// anything else, such as a locked Keychain, is tried again later.
    fn lost_client(
        &mut self,
        target: &Target,
        ticket: u64,
        error: anyhow::Error,
        cx: &mut Context<Self>,
    ) {
        if self.account_signed_out(cx) {
            log::info!("not polling for {}: {error:#}", self.login);
            self.signed_out = true;
        } else {
            self.schedule(target, |schedule| schedule.served(ticket));
            let now = cx.background_executor().now();
            let wall = self.config.wall_clock.now();
            self.back_off(format!("{error:#}"), now, wall, cx);
        }
        self.refresh_status(cx);
    }
}

/// Whether GitHub answered, even with an error: it can be reached.
fn answered<T>(result: &Result<T, GithubError>) -> bool {
    !matches!(
        result,
        Err(GithubError::Network(_)
            | GithubError::RefusedUrl { .. }
            | GithubError::TokenUnavailable(_))
    )
}

/// A number in `0..1` to jitter a backoff with. Retries only need to spread out, not to be
/// unpredictable, so the clock's nanoseconds will do.
fn jitter_sample() -> f64 {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    f64::from(nanos % 1_000_000) / 1_000_000.0
}

/// A cache write.
enum Put {
    Summary(Arc<PullSummary>),
    TouchSummary,
    /// The details and the check they were fetched at.
    Details(Arc<PullRequestData>, Arc<PullRequestCheckData>),
    Listed(Arc<ListedPulls>),
    TouchListed,
    List(Arc<OpenPullRequestList>),
    Rules(Arc<TrunkRuleList>),
    TouchDetails,
    TouchList,
    ForgetCheck,
}

/// How long after the last of a burst of writes the stack trees that show its pull requests read
/// their layers again, so the burst is read back once.
const WRITES_READ_BACK_AFTER: Duration = Duration::from_secs(2);
/// How long a repository's rulesets are kept before they're read again.
const TRUNK_RULES_MAX_AGE: Duration = Duration::from_secs(30 * 60);
/// How old a list of open PRs may be for its lookups of merged PRs to carry over, while their
/// branches stay put: a list kept from before a restart may have missed a merge.
const CARRY_MAX_AGE: Duration = Duration::from_secs(60 * 60);

/// The rest of a repository's poll, after its open PRs: its rulesets, when they're due, then the
/// merged PRs its stacks link through, for a new list or for one whose lookups stopped short.
/// Each request takes its own turn in the queue, through `client`, so writes can go between.
async fn finish_repo(
    client: GithubClient,
    job: Job,
    result: Result<Outcome, GithubError>,
    now: SystemTime,
) -> Result<Outcome, GithubError> {
    let Job::Repo {
        repo,
        list: known,
        carry,
        retry_lookups,
        rules: known_rules,
        read_rules,
        ..
    } = job
    else {
        return result;
    };
    let Outcome::Repo {
        listed, mut list, ..
    } = result?
    else {
        unreachable!("a repository's poll gives a repository's outcome");
    };
    if list.is_none() && retry_lookups {
        list = known.as_deref().cloned().map(|mut list| {
            list.lookups.clear();
            list.merged_truncated = false;
            list
        });
    }
    let rules = if read_rules || known_rules.is_none() {
        let basis = list.as_ref().or(known.as_deref());
        let branches = basis.map(OpenPullRequestList::branches_to_check);
        let branches = branches.unwrap_or_default();
        Some((client.trunk_rules(&repo, known_rules.as_deref(), &branches)).await?)
    } else {
        None
    };
    if let Some(list) = &mut list {
        let in_force = rules.as_ref().or(known_rules.as_deref());
        let in_force = in_force.cloned().unwrap_or_default();
        let carried = known.as_deref().filter(|_| carry);
        (client.look_up_merged(&repo, list, carried, &in_force, now)).await?;
    }
    Ok(Outcome::Repo {
        listed,
        list,
        rules,
    })
}

/// The poll loop: runs whatever is due, one poll at a time, and sleeps until the next is due or
/// something changes.
async fn poll(
    this: WeakEntity<Poller>,
    mut woken: mpsc::UnboundedReceiver<()>,
    cx: &mut AsyncApp,
) -> Result<()> {
    let executor = cx.background_executor().clone();
    let max_wait = this.read_with(cx, |this, _| this.config.max_wait)?;
    loop {
        match this.update(cx, |this, cx| this.next_step(cx))? {
            Step::Wait(wait) => {
                // Timers stop while the Mac sleeps, and waits are worked out from the wall clock,
                // so none is trusted for longer than `max_wait`.
                let wait = wait.map_or(max_wait, |wait| wait.min(max_wait));
                futures::future::select(pin!(executor.timer(wait)), woken.next()).await;
                while woken.try_recv().is_ok() {}
            }
            Step::Run(job, ticket) => {
                let (client, queue) = this.update(cx, |this, cx| {
                    let login = this.login.clone();
                    let client = this
                        .accounts
                        .update(cx, |accounts, cx| accounts.client(&login, cx));
                    (client, this.queue.clone())
                })?;
                let client = match client.await {
                    Ok(client) => client,
                    Err(error) => {
                        this.update(cx, |this, cx| {
                            this.lost_client(&job.target(), ticket, error, cx)
                        })?;
                        continue;
                    }
                };
                // The poll's requests start only once it holds the queue. A read, which can be many
                // requests, takes a turn for each, so a write can go between them.
                let result = match &*job {
                    Job::Read { .. } => {
                        let gated = client.gated(queue.gate());
                        executor.spawn((*job).clone().run(gated)).await
                    }
                    Job::Repo { .. } => {
                        let result = queue
                            .read(|| executor.spawn((*job).clone().run(client.clone())))
                            .await;
                        let (gated, job) = (client.gated(queue.gate()), (*job).clone());
                        let wall = this.read_with(cx, |this, _| this.config.wall_clock.now())?;
                        executor
                            .spawn(async move { finish_repo(gated, job, result, wall).await })
                            .await
                    }
                    _ => {
                        queue
                            .read(|| executor.spawn((*job).clone().run(client.clone())))
                            .await
                    }
                };
                let writes = this.update(cx, |this, cx| {
                    this.finish(*job, ticket, result, &client, cx)
                })?;
                // Off the main thread: encoding a PR's details can take a while.
                executor
                    .spawn(async move {
                        for write in writes {
                            if let Err(error) = write.await {
                                log::warn!("writing the cache: {error:#}");
                            }
                        }
                    })
                    .await;
            }
        }
    }
}
