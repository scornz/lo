//! One account's poller: what it watches, when each poll is due, and what each answer changed.

use std::collections::BTreeMap;
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
use crate::accounts::Accounts;
use crate::cache::{CacheEntry, Cached, PrCache, RecentPr};
use crate::client::{Conditional, GithubClient};
use crate::error::GithubError;
use crate::pulls::{ListedPage, ListedPull, ListedPulls, PullRef, PullSummary, RepoRef, Target};
use crate::queries::{
    MAX_OPEN_PULL_REQUESTS, OpenPullRequestList, PAGE_SIZE, PullRequestCheckData, PullRequestData,
};
use crate::rate_limit::RateLimits;

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
    /// A poll failed for a reason that isn't the network, the rate limit or the token, such as a
    /// pull request that's gone or an organization's SSO. It's tried again at its next interval.
    Failed {
        target: Target,
        error: Arc<GithubError>,
    },
}

/// How polling is going, for banners (design §3 "Rate limit", §11 "Offline", "Rate limit used
/// up", "Signed out").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PollStatus {
    pub rate_limit: RateLimitState,
    /// Set while GitHub can't be reached.
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

/// A pull request's details as rozed has them.
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
    /// How many times it was asked to be polled at once: opened, or rozed came to the foreground.
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
    details: Option<Cached<Arc<PullRequestData>>>,
}

#[derive(Default)]
struct RepoState {
    schedule: Schedule,
    /// GitHub's REST list of the open PRs, page by page with their ETags: what tells whether
    /// `list` changed.
    listed: Option<Cached<Arc<ListedPulls>>>,
    list: Option<Cached<Arc<OpenPullRequestList>>>,
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
    /// list when they changed.
    Repo {
        repo: RepoRef,
        listed: Option<Arc<ListedPulls>>,
        have_list: bool,
    },
}

enum Outcome {
    Open(Box<OpenOutcome>),
    Watched {
        summary: Conditional<PullSummary>,
    },
    Repo {
        listed: ListedPulls,
        list: Option<OpenPullRequestList>,
    },
}

/// A conditional GET of a pull request, asking about changes since GitHub last updated the
/// summary rozed has (not its ETag, which a push to any branch in the repository changes).
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

/// Whether GitHub's answer holds a summary other than the one rozed has. A `200` can bring the
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
/// the ETag rozed has for it.
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
                     but rozed never read it"
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
            Self::Open { pull, .. } | Self::Watched { pull, .. } => Target::Pull(pull.clone()),
            Self::Repo { repo, .. } => Target::Repo(repo.clone()),
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
                have_list,
            } => {
                let listed = get_listed(&client, &repo, known.as_deref()).await?;
                let changed = known.is_none_or(|known| !known.lists_the_same(&listed));
                let list = if changed || !have_list {
                    Some(client.open_pull_requests(&repo.owner, &repo.name).await?)
                } else {
                    None
                };
                Ok(Outcome::Repo { listed, list })
            }
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

/// Polls GitHub for one account (research §7 "Recommended scheme"): the open PR every 20 s,
/// other watched PRs and their repositories' open PRs every 2 minutes, and everything every
/// 5 minutes while rozed is in the background. Each poll asks first whether anything changed,
/// with requests that are free when nothing did, and fetches details only when something did.
/// Requests run one at a time, slowed or paused by the rate limit and backed off while offline.
///
/// The app tells it whether rozed is in the foreground ([`Poller::set_foreground`]), which PRs
/// are in tabs ([`Poller::watch`]) and which tab is active ([`Poller::set_open`]). What changed
/// arrives as [`PollEvent`]s; [`Poller::status`] is for banners and is observable.
pub struct Poller {
    accounts: Entity<Accounts>,
    login: String,
    cache: PrCache,
    config: PollConfig,
    queue: RequestQueue,
    foreground: bool,
    open: Option<PullRef>,
    /// When the open PR was opened.
    opened_at: Option<SystemTime>,
    pulls: BTreeMap<PullRef, PullState>,
    repos: BTreeMap<RepoRef, RepoState>,
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
        Self {
            accounts,
            login: login.into(),
            cache,
            queue: RequestQueue::new(cx.background_executor().clone(), config.write_spacing),
            config,
            foreground: true,
            open: None,
            opened_at: None,
            pulls: BTreeMap::new(),
            repos: BTreeMap::new(),
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

    /// Whether rozed is in the foreground. Coming back polls the open PR at once.
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
        let state = PullState {
            summary: self.cached::<PullSummary>(&target).map(|c| c.map(Arc::new)),
            latest_check: check.as_ref().map(|c| c.value.clone()),
            check,
            ..PullState::default()
        };
        if !self.repos.contains_key(&pull.repo) {
            let repo = Target::Repo(pull.repo.clone());
            let state = RepoState {
                schedule: Schedule::default(),
                listed: self.cached::<ListedPulls>(&repo).map(|c| c.map(Arc::new)),
                list: self
                    .cached::<OpenPullRequestList>(&repo)
                    .map(|c| c.map(Arc::new)),
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

    /// Runs a write (a comment, a review, viewed marks, a merge) that changes `target`, with the
    /// account's client, in the account's queue: after any poll in flight, ahead of waiting
    /// polls, and at least a second after the previous write.
    ///
    /// Afterwards what rozed knows about `target` is out of date, whether or not the write
    /// succeeded (a failed one may still have reached GitHub): a pull request's details are
    /// fetched in full again, a repository's open PRs are listed again, and either is polled at
    /// once if it's watched. A rate limit the write runs into pauses polling as a poll's would.
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
                this.invalidate(&target, cx)
            });
            done.send(result).ok();
            if let Ok(forget) = forget
                && let Err(error) = forget.await
            {
                log::warn!("writing the cache: {error:#}");
            }
        })
        .detach();
        cx.foreground_executor().spawn(async move {
            result.await.unwrap_or_else(|_| {
                Err(GithubError::Network(anyhow::anyhow!(
                    "rozed stopped before the write finished"
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
            consider(&state.schedule, Tier::Watched, &|| Job::Repo {
                repo: repo.clone(),
                listed: state.listed.as_ref().map(|l| l.value.clone()),
                have_list: state.list.is_some(),
            });
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
        if let Some(offline) = &self.offline
            && !forced
        {
            at = at.max(offline.retry_at);
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
        let mut writes = Vec::new();
        if answered(&result) {
            self.offline = None;
        }
        match result {
            Ok(outcome) => {
                self.secondary_attempts = 0;
                self.failures.remove(&target);
                self.schedule(&target, |schedule| schedule.polled(now, ticket));
                writes = self.apply(outcome, &target, wall, cx);
            }
            Err(GithubError::Network(error)) => {
                self.schedule(&target, |schedule| schedule.served(ticket));
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
            Err(error) => {
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
        }
        self.refresh_status(cx);
        writes
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
                    state.latest_check = Some(check.clone());
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
            (Outcome::Repo { listed, list }, Target::Repo(repo)) => {
                let Some(state) = self.repos.get_mut(repo) else {
                    return writes;
                };
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
            _ => {}
        }
        writes
    }

    /// Takes in GitHub's answer about a pull request, returning its summary when it changed. A
    /// `304`, or a `200` with the same summary, only confirms the one rozed has.
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
    TouchDetails,
    TouchList,
    ForgetCheck,
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
                // The poll's requests start only once it holds the queue.
                let result = queue
                    .read(|| executor.spawn((*job).clone().run(client.clone())))
                    .await;
                let writes = this.update(cx, |this, cx| {
                    this.finish(*job, ticket, result, &client, cx)
                })?;
                for write in writes {
                    if let Err(error) = write.await {
                        log::warn!("writing the cache: {error:#}");
                    }
                }
            }
        }
    }
}
