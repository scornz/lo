//! One account's poller: what it watches, when each poll is due, and what each answer changed.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;
use futures::StreamExt as _;
use futures::channel::mpsc;
use futures::future::{BoxFuture, FutureExt as _};
use gpui::{
    AppContext as _, AsyncApp, Context, Entity, EventEmitter, Subscription, Task, WeakEntity,
};
use serde::de::IgnoredAny;

use super::governor::{
    Cooldown, PauseReason, PollConfig, RateLimitState, Tier, backoff, interval, rate_limit_state,
};
use super::queue::RequestQueue;
use crate::accounts::Accounts;
use crate::cache::{CacheEntry, Cached, PrCache, RecentPr};
use crate::client::{Conditional, GithubClient};
use crate::error::GithubError;
use crate::pulls::{PullRef, PullSummary, RepoRef, Target};
use crate::queries::{OpenPullRequestList, PullRequestCheckData, PullRequestData};
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

/// GitHub couldn't be reached.
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

#[derive(Default)]
struct PullState {
    last_polled: Option<Instant>,
    /// Poll as soon as possible: just opened, or rozed just came to the foreground.
    force: bool,
    summary: Option<Cached<Arc<PullSummary>>>,
    /// The check the details were last fetched at. A different check means they're stale.
    check: Option<PullRequestCheckData>,
    details: Option<Cached<Arc<PullRequestData>>>,
}

#[derive(Default)]
struct RepoState {
    last_polled: Option<Instant>,
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
        etag: Option<String>,
        check: Option<PullRequestCheckData>,
        have_details: bool,
    },
    /// Another watched PR: the conditional GET alone.
    Watched { pull: PullRef, etag: Option<String> },
    /// A watched repository: a conditional GET of its open PRs, then the full list when it
    /// changed.
    Repo {
        repo: RepoRef,
        etag: Option<String>,
        have_list: bool,
    },
}

enum Outcome {
    Open(Box<OpenOutcome>),
    Watched {
        summary: Conditional<PullSummary>,
    },
    Repo {
        etag: Option<String>,
        list: Option<OpenPullRequestList>,
    },
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
                etag,
                check,
                have_details,
            } => {
                let summary = client
                    .get_conditional::<PullSummary>(&pull.rest_path(), etag.as_deref())
                    .await?;
                let (owner, name) = (&pull.repo.owner, &pull.repo.name);
                let new_check = client.pull_request_check(owner, name, pull.number).await?;
                let changed = !have_details
                    || matches!(summary, Conditional::Modified { .. })
                    || check.as_ref() != Some(&new_check);
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
            Self::Watched { pull, etag } => Ok(Outcome::Watched {
                summary: client
                    .get_conditional(&pull.rest_path(), etag.as_deref())
                    .await?,
            }),
            Self::Repo {
                repo,
                etag,
                have_list,
            } => {
                let answer = client
                    .get_conditional::<Vec<IgnoredAny>>(&repo.open_pulls_path(), etag.as_deref())
                    .await?;
                let (changed, etag) = match answer {
                    Conditional::NotModified => (false, etag),
                    Conditional::Modified { etag, .. } => (true, etag),
                };
                let list = if changed || !have_list {
                    Some(client.open_pull_requests(&repo.owner, &repo.name).await?)
                } else {
                    None
                };
                Ok(Outcome::Repo { etag, list })
            }
        }
    }
}

enum Step {
    Run(Box<Job>),
    /// Nothing is due: wait this long, or until woken.
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
            state.force = true;
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
        let state = PullState {
            summary: self.cached::<PullSummary>(&target).map(|c| c.map(Arc::new)),
            check: self
                .cached::<PullRequestCheckData>(&target)
                .map(|c| c.value),
            ..PullState::default()
        };
        if !self.repos.contains_key(&pull.repo) {
            let repo = Target::Repo(pull.repo.clone());
            let list = self
                .cached::<OpenPullRequestList>(&repo)
                .map(|c| c.map(Arc::new));
            self.repos.insert(
                pull.repo.clone(),
                RepoState {
                    last_polled: None,
                    list,
                },
            );
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
        if self.open.as_ref() == Some(pull) {
            self.open = None;
            self.opened_at = None;
        }
        if !self.pulls.keys().any(|other| other.repo == pull.repo) {
            self.repos.remove(&pull.repo);
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
        state.force = true;
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

    /// GitHub's REST view of a watched pull request, as last fetched.
    pub fn summary(&self, pull: &PullRef) -> Option<Arc<PullSummary>> {
        Some(self.pulls.get(pull)?.summary.as_ref()?.value.clone())
    }

    /// A watched repository's open pull requests, as last fetched.
    pub fn open_pull_requests(&self, repo: &RepoRef) -> Option<Cached<Arc<OpenPullRequestList>>> {
        self.repos.get(repo)?.list.clone()
    }

    /// Runs a write (a comment, a review, a merge) with the account's client, in the account's
    /// queue: after any poll in flight, and at least a second after the previous write.
    pub fn write<T, F, Fut>(
        &mut self,
        request: F,
        cx: &mut Context<Self>,
    ) -> Task<Result<T, GithubError>>
    where
        T: 'static,
        F: FnOnce(GithubClient) -> Fut + 'static,
        Fut: Future<Output = Result<T, GithubError>> + 'static,
    {
        let login = self.login.clone();
        let client = self
            .accounts
            .update(cx, |accounts, cx| accounts.client(&login, cx));
        let queue = self.queue.clone();
        cx.spawn(async move |this, cx| {
            let client = client.await.map_err(|error| GithubError::Unauthorized {
                message: format!("{error:#}"),
            })?;
            let result = queue.write(request(client.clone())).await;
            this.update(cx, |this, cx| {
                this.limits = client.rate_limits();
                this.refresh_status(cx);
            })
            .ok();
            result
        })
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
        let due = |last_polled: Option<Instant>, force: bool, tier: Tier| -> Option<Instant> {
            let interval = interval(tier, self.foreground, &self.status.rate_limit, &self.config)?;
            Some(match last_polled {
                Some(last) if !force => last + interval,
                _ => now,
            })
        };

        // The open PR first, then the other PRs, then the repositories; earliest due first.
        let mut next: Option<(Instant, Job)> = None;
        let mut consider = |at: Option<Instant>, job: &dyn Fn() -> Job| {
            if let Some(at) = at
                && next.as_ref().is_none_or(|(best, _)| at < *best)
            {
                next = Some((at, job()));
            }
        };
        if let Some(open) = &self.open
            && let Some(state) = self.pulls.get(open)
        {
            consider(due(state.last_polled, state.force, Tier::Open), &|| {
                Job::Open {
                    pull: open.clone(),
                    etag: state.summary.as_ref().and_then(|s| s.etag.clone()),
                    check: state.check.clone(),
                    have_details: state.details.is_some(),
                }
            });
        }
        for (pull, state) in &self.pulls {
            if Some(pull) == self.open.as_ref() {
                continue;
            }
            consider(due(state.last_polled, false, Tier::Watched), &|| {
                Job::Watched {
                    pull: pull.clone(),
                    etag: state.summary.as_ref().and_then(|s| s.etag.clone()),
                }
            });
        }
        for (repo, state) in &self.repos {
            consider(due(state.last_polled, false, Tier::Watched), &|| {
                Job::Repo {
                    repo: repo.clone(),
                    etag: state.list.as_ref().and_then(|l| l.etag.clone()),
                    have_list: state.list.is_some(),
                }
            });
        }
        let Some((mut at, job)) = next else {
            return Step::Wait(None);
        };
        if let Some(offline) = &self.offline {
            at = at.max(offline.retry_at);
        }
        if at <= now {
            Step::Run(Box::new(job))
        } else {
            Step::Wait(Some(at - now))
        }
    }

    /// Takes in a poll's answer: updates what's known, sends events, and returns the cache
    /// writes to make.
    fn finish(
        &mut self,
        job: Job,
        result: Result<Outcome, GithubError>,
        client: &GithubClient,
        cx: &mut Context<Self>,
    ) -> Vec<CacheWrite> {
        self.limits = client.rate_limits();
        let now = cx.background_executor().now();
        let wall = self.config.wall_clock.now();
        let target = job.target();
        let mut writes = Vec::new();
        match result {
            Ok(outcome) => {
                self.offline = None;
                self.secondary_attempts = 0;
                self.mark_polled(&target, now);
                writes = self.apply(outcome, &target, wall, cx);
            }
            Err(GithubError::Network(error)) => {
                let attempts = self.offline.as_ref().map_or(0, |o| o.attempts) + 1;
                let wait = backoff(
                    attempts,
                    self.config.offline_first_wait,
                    self.config.offline_max_wait,
                    self.config.jitter,
                    jitter_sample(),
                );
                log::info!("couldn't reach GitHub ({error:#}); trying again in {wait:?}");
                let since = self.offline.as_ref().map_or(wall, |o| o.status.since);
                self.offline = Some(OfflineState {
                    attempts,
                    retry_at: now + wait,
                    status: Offline {
                        since,
                        retry_at: wall + wait,
                        error: format!("{error:#}"),
                    },
                });
                // Details shown from the cache now carry their age.
                cx.notify();
            }
            Err(GithubError::RateLimited {
                secondary,
                reset,
                retry_after,
                message,
            }) => {
                log::info!("GitHub's rate limit: {message}");
                self.cooldown = Some(if secondary {
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
                } else {
                    let until = reset
                        .map(|reset| SystemTime::UNIX_EPOCH + Duration::from_secs(reset))
                        .filter(|until| *until > wall)
                        .unwrap_or(wall + self.config.secondary_first_wait);
                    let resource = ["graphql", "core"]
                        .into_iter()
                        .find(|r| self.limits.get(r).is_some_and(|l| l.remaining == 0))
                        .unwrap_or("core");
                    Cooldown {
                        until,
                        reason: PauseReason::UsedUp {
                            resource: resource.to_string(),
                        },
                    }
                });
            }
            Err(error @ GithubError::Unauthorized { .. }) => {
                let login = self.login.clone();
                self.accounts
                    .update(cx, |accounts, cx| accounts.report_error(&login, &error, cx));
                self.signed_out = true;
            }
            Err(error) => {
                log::info!("polling {target:?}: {error:#}");
                self.mark_polled(&target, now);
                cx.emit(PollEvent::Failed {
                    target,
                    error: Arc::new(error),
                });
            }
        }
        self.refresh_status(cx);
        writes
    }

    fn mark_polled(&mut self, target: &Target, now: Instant) {
        match target {
            Target::Pull(pull) => {
                if let Some(state) = self.pulls.get_mut(pull) {
                    state.last_polled = Some(now);
                    state.force = false;
                }
            }
            Target::Repo(repo) => {
                if let Some(state) = self.repos.get_mut(repo) {
                    state.last_polled = Some(now);
                }
            }
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
                        Put::Summary(value, etag) => {
                            cache
                                .put(&login, &target, &*value, etag.as_deref(), wall)
                                .await
                        }
                        Put::Check(value) => cache.put(&login, &target, &value, None, wall).await,
                        Put::Details(value) => {
                            cache.put(&login, &target, &*value, None, wall).await
                        }
                        Put::List(value, etag) => {
                            cache
                                .put(&login, &target, &*value, etag.as_deref(), wall)
                                .await
                        }
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
                        state.check = Some(check.clone());
                        put(&mut writes, &cache, &login, Put::Details(details.clone()));
                        put(&mut writes, &cache, &login, Put::Check(check));
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
                if let Some((summary, etag)) = summary {
                    put(
                        &mut writes,
                        &cache,
                        &login,
                        Put::Summary(summary.clone(), etag),
                    );
                    cx.emit(PollEvent::Summary {
                        pull: pull.clone(),
                        summary,
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
                if let Some((summary, etag)) = Self::take_summary(state, summary, wall) {
                    // The details need fetching again when it's opened, even if the check
                    // doesn't show what changed.
                    state.check = None;
                    put(&mut writes, &cache, &login, Put::ForgetCheck);
                    put(
                        &mut writes,
                        &cache,
                        &login,
                        Put::Summary(summary.clone(), etag),
                    );
                    cx.emit(PollEvent::Summary {
                        pull: pull.clone(),
                        summary,
                    });
                }
            }
            (Outcome::Repo { etag, list }, Target::Repo(repo)) => {
                let Some(state) = self.repos.get_mut(repo) else {
                    return writes;
                };
                match list {
                    Some(list) => {
                        let changed = state.list.as_ref().is_none_or(|known| *known.value != list);
                        let list = Arc::new(list);
                        state.list = Some(Cached {
                            value: list.clone(),
                            etag: etag.clone(),
                            fetched_at: wall,
                        });
                        put(&mut writes, &cache, &login, Put::List(list.clone(), etag));
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

    /// Stores a REST answer that changed, returning it and its ETag.
    fn take_summary(
        state: &mut PullState,
        summary: Conditional<PullSummary>,
        wall: SystemTime,
    ) -> Option<(Arc<PullSummary>, Option<String>)> {
        let Conditional::Modified { value, etag } = summary else {
            return None;
        };
        let value = Arc::new(value);
        state.summary = Some(Cached {
            value: value.clone(),
            etag: etag.clone(),
            fetched_at: wall,
        });
        Some((value, etag))
    }

    fn lost_client(&mut self, error: anyhow::Error, cx: &mut Context<Self>) {
        log::info!("not polling for {}: {error:#}", self.login);
        self.signed_out = true;
        self.refresh_status(cx);
    }
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
    Summary(Arc<PullSummary>, Option<String>),
    Check(PullRequestCheckData),
    Details(Arc<PullRequestData>),
    List(Arc<OpenPullRequestList>, Option<String>),
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
    loop {
        match this.update(cx, |this, cx| this.next_step(cx))? {
            Step::Wait(wait) => {
                let timer = async {
                    match wait {
                        Some(wait) => executor.timer(wait).await,
                        None => futures::future::pending().await,
                    }
                };
                futures::future::select(pin!(timer), woken.next()).await;
                while woken.try_recv().is_ok() {}
            }
            Step::Run(job) => {
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
                        this.update(cx, |this, cx| this.lost_client(error, cx))?;
                        continue;
                    }
                };
                let run = cx.background_spawn((*job).clone().run(client.clone()));
                let result = queue.read(run).await;
                let writes = this.update(cx, |this, cx| this.finish(*job, result, &client, cx))?;
                for write in writes {
                    if let Err(error) = write.await {
                        log::warn!("writing the cache: {error:#}");
                    }
                }
            }
        }
    }
}
