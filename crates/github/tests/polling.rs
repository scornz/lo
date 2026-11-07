//! The poller on GPUI's fake clock, replaying `poll_playground` (two polls of
//! `rozed-dev/playground`, recorded in the poller's order) and edited copies of its answers.
//!
//! Each test steps the clock a second at a time, so every request is logged at the second it was
//! sent.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use credentials_provider::test_support::FakeCredentialsProvider;
use futures::AsyncReadExt as _;
use futures::future::BoxFuture;
use github::accounts::Accounts;
use github::cache::PrCache;
use github::cassette::{self, Interaction};
use github::db::{AccountSource, LoDb};
use github::poll::{
    CachedRead, LowLimit, Offline, PauseReason, PollConfig, PollEvent, PollStatus, Poller,
    RateLimitState, WallClock,
};
use github::push_history::PushHistory;
use github::queries::{PullRequestData, StackLayerList, pull_request};
use github::replay::ReplayHttpClient;
use github::{AuthenticatedUser, GithubError, PullRef, RepoRef, Target, Token, User};
use gpui::{AppContext as _, BackgroundExecutor, Entity, TestAppContext};
use http_client::http::HeaderValue;
use http_client::{AsyncBody, HttpClient, Request, Response, Url};
use parking_lot::Mutex;

/// The wall clock when each test starts: after every recorded rate-limit window has reset, so
/// only the windows a test sets count.
const START: u64 = 1_794_100_000;
const LOGIN: &str = "rozed-bot";

fn wall(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(START + secs)
}

fn pr(number: i64) -> PullRef {
    PullRef::new("rozed-dev", "playground", number)
}

fn playground() -> RepoRef {
    RepoRef::new("rozed-dev", "playground")
}

/// Replays a cassette, logging when each request was sent and what it was. It can pretend the
/// network is down, and make every request take a while.
struct Logged {
    replay: Arc<ReplayHttpClient>,
    executor: BackgroundExecutor,
    start: Instant,
    log: Mutex<Vec<(u64, String)>>,
    offline: AtomicBool,
    delay: Option<Duration>,
}

/// `GET /pulls?state=open&per_page=100 (etag)` for a request that sent an ETag, `GET /pulls/1
/// (since)` for one that asked about changes since a time, or a GraphQL operation's name.
fn describe(
    method: &str,
    url: &Url,
    headers: &http_client::http::HeaderMap,
    body: &[u8],
) -> String {
    if url.path() == "/graphql" {
        let body: serde_json::Value = serde_json::from_slice(body).unwrap();
        return body["operationName"].as_str().unwrap().to_string();
    }
    let path = url.path().trim_start_matches("/repos/rozed-dev/playground");
    let query = url.query().map(|q| format!("?{q}")).unwrap_or_default();
    let condition = if headers.contains_key("if-none-match") {
        " (etag)"
    } else if headers.contains_key("if-modified-since") {
        " (since)"
    } else {
        ""
    };
    format!("{method} {path}{query}{condition}")
}

impl HttpClient for Logged {
    fn user_agent(&self) -> Option<&HeaderValue> {
        None
    }

    fn proxy(&self) -> Option<&Url> {
        None
    }

    fn send(
        &self,
        request: Request<AsyncBody>,
    ) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>> {
        let (parts, mut body) = request.into_parts();
        let sent = (self.executor.now() - self.start).as_secs();
        let offline = self.offline.load(Ordering::SeqCst);
        let (replay, executor, delay) = (self.replay.clone(), self.executor.clone(), self.delay);
        let url = Url::parse(&parts.uri.to_string()).unwrap();
        let mut bytes = Vec::new();
        futures::executor::block_on(body.read_to_end(&mut bytes)).unwrap();
        self.log.lock().push((
            sent,
            describe(parts.method.as_str(), &url, &parts.headers, &bytes),
        ));
        Box::pin(async move {
            if let Some(delay) = delay {
                executor.timer(delay).await;
            }
            if offline {
                anyhow::bail!("the network is down");
            }
            replay
                .send(Request::from_parts(parts, AsyncBody::from(bytes)))
                .await
        })
    }
}

struct Polling {
    poller: Entity<Poller>,
    accounts: Entity<Accounts>,
    http: Arc<Logged>,
    db: LoDb,
    store: Arc<FakeCredentialsProvider>,
    cache: PrCache,
    events: Rc<RefCell<Vec<PollEvent>>>,
    executor: BackgroundExecutor,
    clock: WallClock,
    /// How long the Mac has slept, in seconds: wall-clock time that GPUI's clock didn't see.
    slept: Arc<std::sync::atomic::AtomicU64>,
    cx: TestAppContext,
}

fn user() -> AuthenticatedUser {
    AuthenticatedUser {
        user: User {
            login: LOGIN.into(),
            id: 353226052,
            name: None,
            avatar_url: None,
        },
        scopes: vec!["read:org".into(), "repo".into(), "workflow".into()],
    }
}

impl Polling {
    async fn new(name: &str, interactions: Vec<Interaction>, cx: &mut TestAppContext) -> Self {
        Self::with(name, interactions, None, AccountSource::DeviceFlow, cx).await
    }

    /// A poller whose requests each take `delay`, for an account added from `source`.
    async fn with(
        name: &str,
        interactions: Vec<Interaction>,
        delay: Option<Duration>,
        source: AccountSource,
        cx: &mut TestAppContext,
    ) -> Self {
        let executor = cx.executor();
        let http = Arc::new(Logged {
            replay: Arc::new(ReplayHttpClient::from_interactions(name, interactions).unwrap()),
            executor: executor.clone(),
            start: executor.now(),
            log: Mutex::default(),
            offline: AtomicBool::new(false),
            delay,
        });
        let db = LoDb::open_test(name).await;
        let store = Arc::new(FakeCredentialsProvider::default());
        let accounts = cx
            .update(|cx| Accounts::load(db.clone(), store.clone(), http.clone(), cx))
            .unwrap();
        accounts
            .update(cx, |accounts, cx| {
                accounts.add(user(), Token::new("gho_FAKE"), source, cx)
            })
            .await
            .unwrap();
        let cache = PrCache::new(db.clone());
        let slept = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let clock = {
            let (executor, slept) = (executor.clone(), slept.clone());
            let origin = executor.now();
            WallClock::new(move || wall(slept.load(Ordering::SeqCst)) + (executor.now() - origin))
        };
        let mut this = Self {
            poller: cx
                .new(|cx| Poller::new(accounts.clone(), LOGIN, cache.clone(), config(&clock), cx)),
            accounts,
            http,
            db,
            store,
            cache,
            events: Rc::default(),
            executor,
            clock,
            slept,
            cx: cx.clone(),
        };
        this.subscribe();
        this
    }

    fn subscribe(&mut self) {
        let events = self.events.clone();
        self.cx.update(|cx| {
            cx.subscribe(&self.poller, move |_, event: &PollEvent, _| {
                events.borrow_mut().push(event.clone())
            })
            .detach()
        });
    }

    /// Replaces the poller with a new one on the same accounts and cache, as after a restart.
    fn restart(&mut self) {
        let (accounts, cache, clock) = (
            self.accounts.clone(),
            self.cache.clone(),
            self.clock.clone(),
        );
        self.poller = self
            .cx
            .new(|cx| Poller::new(accounts, LOGIN, cache, config(&clock), cx));
        self.subscribe();
        self.executor.run_until_parked();
    }

    /// Replaces the accounts and the poller, as when Lo starts again: no token has been read.
    fn restart_lo(&mut self) {
        let (db, store, http) = (self.db.clone(), self.store.clone(), self.http.clone());
        self.accounts = self
            .cx
            .update(|cx| Accounts::load(db, store, http, cx))
            .unwrap();
        self.restart();
    }

    fn update<R>(&mut self, f: impl FnOnce(&mut Poller, &mut gpui::Context<Poller>) -> R) -> R {
        let result = self.poller.update(&mut self.cx, f);
        self.executor.run_until_parked();
        result
    }

    fn read<R>(&self, f: impl FnOnce(&Poller) -> R) -> R {
        self.poller.read_with(&self.cx, |poller, _| f(poller))
    }

    fn status(&self) -> PollStatus {
        self.read(|poller| poller.status().clone())
    }

    /// The Mac sleeps for `secs`: the wall clock moves on, GPUI's clock and its timers don't.
    fn sleep(&self, secs: u64) {
        self.slept.fetch_add(secs, Ordering::SeqCst);
    }

    /// Runs what's ready at the current second.
    fn run(&self) {
        self.executor.run_until_parked();
    }

    /// Moves the clock to `secs` after the start, a second at a time.
    fn run_until(&self, secs: u64) {
        while self.now() < secs {
            self.executor.advance_clock(Duration::from_secs(1));
            self.executor.run_until_parked();
        }
    }

    fn now(&self) -> u64 {
        (self.executor.now() - self.http.start).as_secs()
    }

    /// The requests sent since the last call.
    fn requests(&self) -> Vec<(u64, String)> {
        std::mem::take(&mut *self.http.log.lock())
    }

    /// The events sent since the last call, briefly.
    fn events(&self) -> Vec<String> {
        self.events
            .borrow_mut()
            .drain(..)
            .map(|event| match event {
                PollEvent::Summary { pull, summary } => {
                    format!("summary #{}: {}", pull.number, summary.title)
                }
                PollEvent::Details { pull, details } => {
                    format!("details #{}: {}", pull.number, details.title)
                }
                PollEvent::Check { pull, .. } => format!("check #{}", pull.number),
                PollEvent::OpenPullRequests { repo, list } => {
                    format!("open PRs of {repo}: {}", list.pull_requests.len())
                }
                PollEvent::TrunkRules { repo } => format!("rulesets of {repo}"),
                PollEvent::Failed { target, error } => format!("failed {target:?}: {error}"),
                PollEvent::Read { pull, name } => format!("read {name} #{}", pull.number),
                PollEvent::ReadFailed { pull, name, error } => {
                    format!("failed to read {name} #{}: {error}", pull.number)
                }
            })
            .collect()
    }
}

fn config(clock: &WallClock) -> PollConfig {
    PollConfig {
        jitter: 0.0,
        wall_clock: clock.clone(),
        ..PollConfig::default()
    }
}

fn log(entries: &[(u64, &str)]) -> Vec<(u64, String)> {
    entries
        .iter()
        .map(|(secs, what)| (*secs, what.to_string()))
        .collect()
}

/// The recorded poll: `[0]` GET /pulls/1, `[1]` its check, `[2]` its details, `[3]` GET
/// /pulls/2, `[4]` GET of the open PRs, `[5]` OpenPullRequests, then the second poll: `[6]` a
/// 304 for /pulls/1, `[7]` its check, `[8]` a 304 for /pulls/2, `[9]` a 304 for the open PRs.
/// Last, `[10]` TrunkRules, which the first poll reads after OpenPullRequests.
fn recorded() -> Vec<Interaction> {
    let (_, interactions) =
        cassette::read_cassette(&cassette::fixtures_dir().join("poll_playground")).unwrap();
    interactions.into_iter().map(|(_, i)| i).collect()
}

/// The recorded poll's answers for the repository's open PRs, and its rulesets.
fn repo_answers(recorded: &[Interaction]) -> Vec<Interaction> {
    vec![
        recorded[4].clone(),
        recorded[5].clone(),
        recorded[9].clone(),
        recorded[10].clone(),
    ]
}

fn with_headers(mut interaction: Interaction, headers: &[(&str, String)]) -> Interaction {
    for (name, value) in headers {
        interaction
            .response
            .headers
            .insert(name.to_string(), value.clone());
    }
    interaction
}

/// `x-ratelimit-*` headers for a window that resets `reset` seconds after the start.
fn rate_limit(resource: &str, remaining: u32, reset: u64) -> Vec<(&'static str, String)> {
    vec![
        ("x-ratelimit-limit", "5000".into()),
        ("x-ratelimit-remaining", remaining.to_string()),
        ("x-ratelimit-used", (5000 - remaining).to_string()),
        ("x-ratelimit-reset", (START + reset).to_string()),
        ("x-ratelimit-resource", resource.into()),
    ]
}

/// The answer to a conditional request that sent `etag`, now carrying `new_etag`.
fn answering(mut interaction: Interaction, etag: &str, new_etag: &str) -> Interaction {
    interaction.request.if_none_match = Some(etag.into());
    interaction
        .response
        .headers
        .insert("etag".into(), new_etag.into());
    interaction
}

fn etag(interaction: &Interaction) -> String {
    interaction.response.headers["etag"].clone()
}

/// The answer to a GET of a pull request that asked about changes since `since`.
fn asked_since(mut interaction: Interaction, since: &str) -> Interaction {
    interaction.request.if_none_match = None;
    interaction.request.if_modified_since = Some(since.into());
    interaction
}

/// When GitHub last updated a pull request, from its answer.
fn last_modified(interaction: &Interaction) -> String {
    interaction.response.headers["last-modified"].clone()
}

/// The time a pull request edited by [`updated`] was updated at, and its HTTP date.
const NOON: (&str, &str) = ("2026-11-06T12:00:00Z", "Fri, 06 Nov 2026 12:00:00 GMT");

/// A pull request's answer, updated at [`NOON`] with `edit`.
fn updated(interaction: Interaction, edit: impl FnOnce(&mut serde_json::Value)) -> Interaction {
    let mut interaction = with_body(interaction, |body| {
        body["updated_at"] = NOON.0.into();
        edit(body);
    });
    interaction
        .response
        .headers
        .insert("last-modified".into(), NOON.1.into());
    interaction
}

fn with_body(
    mut interaction: Interaction,
    edit: impl FnOnce(&mut serde_json::Value),
) -> Interaction {
    edit(interaction.response.body.as_mut().unwrap());
    interaction
}

/// An answer with a status and a JSON body, for the request `like` made.
fn answer(like: &Interaction, status: u16, headers: &[(&str, &str)], body: &str) -> Interaction {
    let mut interaction = like.clone();
    interaction.response.status = status;
    interaction.response.headers = headers
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    interaction.response.body = serde_json::from_str(body).ok();
    interaction.response.body_text = None;
    interaction
}

/// The recorded details of PR #1, retitled.
fn details_titled(recorded: &[Interaction], title: &str) -> Interaction {
    with_body(recorded[2].clone(), |body| {
        body["data"]["repository"]["pullRequest"]["title"] = title.into();
    })
}

/// A recorded GraphQL request for PR #1, asked about PR `number` instead.
fn for_pull(interaction: &Interaction, number: i64) -> Interaction {
    let mut interaction = interaction.clone();
    interaction.request.variables.as_mut().unwrap()["number"] = number.into();
    interaction
}

fn decoded_details(recorded: &[Interaction]) -> PullRequestData {
    let body = recorded[2].response.body.clone().unwrap();
    let data: pull_request::ResponseData = serde_json::from_value(body["data"].clone()).unwrap();
    data.repository.unwrap().pull_request.unwrap()
}

#[gpui::test]
async fn the_first_poll_fetches_everything_and_later_ones_only_ask(cx: &mut TestAppContext) {
    let mut t = Polling::new("first_poll", recorded(), cx).await;
    let cached = t.update(|poller, cx| {
        poller.watch(pr(2), cx);
        poller.set_open(Some(pr(1)), cx)
    });
    assert_eq!(cached, None);
    t.run();
    assert_eq!(
        t.requests(),
        log(&[
            (0, "GET /pulls/1"),
            (0, "PullRequestCheck"),
            (0, "PullRequest"),
            (0, "GET /pulls/2"),
            (0, "GET /pulls?state=open&per_page=100"),
            (0, "OpenPullRequests"),
            (0, "TrunkRules"),
        ])
    );
    assert_eq!(
        t.events(),
        [
            "summary #1: feat: stack layer A",
            "check #1",
            "details #1: feat: stack layer A",
            "summary #2: feat: stack layer B",
            "rulesets of rozed-dev/playground",
            "open PRs of rozed-dev/playground: 2",
        ]
    );

    // The open PR every 20 s: a free conditional GET and the 1-point check. Everything else every
    // 2 minutes: free conditional GETs.
    t.run_until(120);
    let mut expected = Vec::new();
    for secs in [20, 40, 60, 80, 100, 120] {
        expected.push((secs, "GET /pulls/1 (since)"));
        expected.push((secs, "PullRequestCheck"));
    }
    expected.push((120, "GET /pulls/2 (since)"));
    expected.push((120, "GET /pulls?state=open&per_page=100 (etag)"));
    assert_eq!(t.requests(), log(&expected));
    assert_eq!(t.events(), Vec::<String>::new());

    // The open PR is asked about changes since GitHub last updated it.
    let seen = t.http.replay.seen();
    let since = last_modified(&recorded()[0]);
    assert_eq!(seen[7].header("if-modified-since"), Some(since.as_str()));
    assert_eq!(t.status(), PollStatus::default());

    let answer = t.read(|poller| poller.pull_request(&pr(1))).unwrap();
    assert_eq!(answer.details.title, "feat: stack layer A");
    assert_eq!(answer.fetched_at, wall(120));
    assert_eq!(answer.offline_age, None);
    assert_eq!(
        t.read(|poller| poller.summary(&pr(2))).unwrap().head.name,
        "stack-b"
    );
    let open = t
        .read(|poller| poller.open_pull_requests(&playground()))
        .unwrap();
    assert_eq!(open.value.pull_requests.len(), 2);
    assert_eq!(open.fetched_at, wall(120));
}

#[gpui::test]
async fn details_are_fetched_again_only_when_something_changed(cx: &mut TestAppContext) {
    let r = recorded();
    let changed_rest = updated(asked_since(r[0].clone(), &last_modified(&r[0])), |body| {
        body["title"] = "retitled again".into()
    });
    let moved_timeline = with_body(r[1].clone(), |body| {
        body["data"]["repository"]["pullRequest"]["timelineItems"]["updatedAt"] =
            "2026-11-06T12:00:00Z".into();
    });
    let mut interactions = vec![
        // GET /pulls/1: 200, two 304s, a 200 for a retitle, then 304s since it.
        r[0].clone(),
        r[6].clone(),
        r[6].clone(),
        changed_rest,
        asked_since(r[6].clone(), NOON.1),
        // The check: the same twice, then the timeline moves.
        r[1].clone(),
        r[1].clone(),
        moved_timeline,
        // The details, as they change.
        r[2].clone(),
        details_titled(&r, "retitled"),
        details_titled(&r, "retitled again"),
    ];
    interactions.extend(repo_answers(&r));
    let mut t = Polling::new("change_detection", interactions, cx).await;
    t.update(|poller, cx| poller.set_open(Some(pr(1)), cx));
    t.run_until(80);
    let pull_one: Vec<_> = t
        .requests()
        .into_iter()
        .filter(|(_, what)| {
            !what.contains("/pulls?") && what != "OpenPullRequests" && what != "TrunkRules"
        })
        .collect();
    assert_eq!(
        pull_one,
        log(&[
            (0, "GET /pulls/1"),
            (0, "PullRequestCheck"),
            (0, "PullRequest"),
            // Nothing changed.
            (20, "GET /pulls/1 (since)"),
            (20, "PullRequestCheck"),
            // The check changed.
            (40, "GET /pulls/1 (since)"),
            (40, "PullRequestCheck"),
            (40, "PullRequest"),
            // The REST answer changed.
            (60, "GET /pulls/1 (since)"),
            (60, "PullRequestCheck"),
            (60, "PullRequest"),
            (80, "GET /pulls/1 (since)"),
            (80, "PullRequestCheck"),
        ])
    );
    assert_eq!(
        t.events(),
        [
            "summary #1: feat: stack layer A",
            "check #1",
            "details #1: feat: stack layer A",
            "rulesets of rozed-dev/playground",
            "open PRs of rozed-dev/playground: 2",
            "check #1",
            "details #1: retitled",
            "summary #1: retitled again",
            "details #1: retitled again",
        ]
    );
    // The next poll asks about changes since the retitle.
    let last = t.http.replay.seen().into_iter().rev().nth(1).unwrap();
    assert_eq!(last.header("if-modified-since"), Some(NOON.1));
}

#[gpui::test]
async fn a_watched_pr_that_changed_is_fetched_in_full_when_opened_again(cx: &mut TestAppContext) {
    let r = recorded();
    let pushed = updated(asked_since(r[3].clone(), &last_modified(&r[3])), |body| {
        body["head"]["sha"] = "0123456789abcdef0123456789abcdef01234567".into()
    });
    let mut interactions = vec![
        r[0].clone(),
        r[1].clone(),
        r[2].clone(),
        r[6].clone(),
        // GET /pulls/2: 200, then a push, then 304s since it.
        r[3].clone(),
        pushed,
        asked_since(r[8].clone(), NOON.1),
        // PR #2's check never changes; its details do.
        for_pull(&r[1], 2),
        for_pull(&details_titled(&r, "layer B"), 2),
        for_pull(&details_titled(&r, "layer B, pushed"), 2),
    ];
    interactions.extend(repo_answers(&r));
    let mut t = Polling::new("watched_then_opened", interactions, cx).await;
    t.update(|poller, cx| poller.set_open(Some(pr(2)), cx));
    t.run_until(10);
    t.update(|poller, cx| poller.set_open(Some(pr(1)), cx));
    t.run_until(120);
    t.events();
    t.requests();
    // At 120 s, #2 is watched: only its conditional GET, which shows the push.
    assert_eq!(
        t.read(|poller| poller.summary(&pr(2))).unwrap().head.sha,
        "0123456789abcdef0123456789abcdef01234567"
    );

    t.run_until(130);
    t.requests();
    t.update(|poller, cx| poller.set_open(Some(pr(2)), cx));
    assert_eq!(
        t.requests(),
        log(&[
            (130, "GET /pulls/2 (since)"),
            (130, "PullRequestCheck"),
            // The check is the same, but the push means the details are stale.
            (130, "PullRequest"),
        ])
    );
    // The check is the same, so no check event.
    assert_eq!(t.events(), ["details #2: layer B, pushed"]);
}

/// GitHub's REST view of a pull request, as it would answer after a push to another branch in the
/// repository: only the embedded repositories changed.
fn after_a_push_elsewhere(interaction: Interaction) -> Interaction {
    with_body(interaction, |body| {
        let pushed_at = "2026-11-06T12:49:58Z";
        if let Some(pulls) = body.as_array_mut() {
            for pull in pulls {
                pull["base"]["repo"]["pushed_at"] = pushed_at.into();
                pull["head"]["repo"]["pushed_at"] = pushed_at.into();
            }
        } else {
            body["base"]["repo"]["pushed_at"] = pushed_at.into();
            body["head"]["repo"]["pushed_at"] = pushed_at.into();
        }
    })
}

#[gpui::test]
async fn a_pull_request_that_answers_200_unchanged_isnt_fetched_again(cx: &mut TestAppContext) {
    let r = recorded();
    let since = last_modified(&r[0]);
    let mut interactions = vec![
        r[0].clone(),
        // GitHub answers 200 although the pull request is the same.
        after_a_push_elsewhere(asked_since(r[0].clone(), &since)),
        asked_since(r[6].clone(), &since),
        r[1].clone(),
        r[2].clone(),
        // PR #2, watched: the same.
        r[3].clone(),
        after_a_push_elsewhere(asked_since(r[3].clone(), &last_modified(&r[3]))),
        asked_since(r[8].clone(), &last_modified(&r[3])),
        for_pull(&r[1], 2),
        for_pull(&details_titled(&r, "layer B"), 2),
    ];
    interactions.extend(repo_answers(&r));
    let mut t = Polling::new("unchanged_200", interactions, cx).await;
    t.update(|poller, cx| {
        poller.watch(pr(2), cx);
        poller.set_open(Some(pr(1)), cx)
    });
    t.run_until(5);
    t.requests();
    t.events();
    t.run_until(130);
    let pulls: Vec<_> = t
        .requests()
        .into_iter()
        .filter(|(_, what)| !what.contains("/pulls?"))
        .take(4)
        .collect();
    assert_eq!(
        pulls,
        log(&[
            // A 200, but the same pull request: no details.
            (20, "GET /pulls/1 (since)"),
            (20, "PullRequestCheck"),
            (40, "GET /pulls/1 (since)"),
            (40, "PullRequestCheck"),
        ])
    );
    assert_eq!(t.events(), Vec::<String>::new());

    // PR #2 answered 200 unchanged at 120 s: its check stands, so opening it fetches its
    // details only because they were never fetched, and sends no summary.
    t.update(|poller, cx| poller.set_open(Some(pr(2)), cx));
    assert_eq!(t.events(), ["check #2", "details #2: layer B"]);
}

#[gpui::test]
async fn a_check_that_changes_is_sent(cx: &mut TestAppContext) {
    let r = recorded();
    // Checks start running and a review thread is opened; nothing else moves.
    let moved = with_body(r[1].clone(), |body| {
        let pull = &mut body["data"]["repository"]["pullRequest"];
        pull["statusCheckRollup"] = serde_json::json!({"state": "PENDING"});
        pull["reviewThreads"] = serde_json::json!({
            "totalCount": 1,
            "nodes": [{"isResolved": false, "comments": {"totalCount": 1}}]
        });
    });
    let mut interactions = vec![
        r[0].clone(),
        r[6].clone(),
        r[1].clone(),
        moved,
        r[2].clone(),
    ];
    interactions.extend(repo_answers(&r));
    let mut t = Polling::new("check_changes", interactions, cx).await;
    t.update(|poller, cx| poller.set_open(Some(pr(1)), cx));
    t.run_until(5);
    t.events();
    t.run_until(20);
    assert_eq!(t.events(), ["check #1"]);
    let check = t.read(|poller| poller.check(&pr(1))).unwrap();
    assert_eq!(check.unresolved_threads(), 1);
    assert_eq!(
        check
            .status_check_rollup
            .as_ref()
            .map(|rollup| &rollup.state),
        Some(&github::queries::StatusState::PENDING)
    );
}

#[gpui::test]
async fn a_list_that_answers_200_unchanged_isnt_fetched_again(cx: &mut TestAppContext) {
    let r = recorded();
    let mut moved = after_a_push_elsewhere(answering(r[4].clone(), &etag(&r[4]), "\"moved\""));
    moved.response.status = 200;
    let interactions = vec![
        r[4].clone(),
        r[5].clone(),
        moved,
        answering(r[9].clone(), "\"moved\"", "\"moved\""),
        r[3].clone(),
        r[8].clone(),
        r[10].clone(),
    ];
    let mut t = Polling::new("unchanged_list", interactions, cx).await;
    t.update(|poller, cx| poller.watch(pr(2), cx));
    t.run_until(5);
    t.requests();
    t.events();
    t.run_until(250);
    assert_eq!(
        t.requests()
            .into_iter()
            .filter(|(_, what)| !what.contains("/pulls/2"))
            .collect::<Vec<_>>(),
        log(&[
            // A new ETag, but the same open PRs: no OpenPullRequests.
            (120, "GET /pulls?state=open&per_page=100 (etag)"),
            (240, "GET /pulls?state=open&per_page=100 (etag)"),
        ])
    );
    assert_eq!(t.events(), Vec::<String>::new());
    // The new ETag is the one sent next.
    let seen = t.http.replay.seen();
    let last = seen
        .iter()
        .rev()
        .find(|s| s.url.contains("/pulls?"))
        .unwrap();
    assert_eq!(last.header("if-none-match"), Some("\"moved\""));
}

/// A page of the playground's open PRs: copies of the recorded first one, numbered `numbers`.
fn page_of(
    r: &[Interaction],
    page: usize,
    numbers: std::ops::Range<i64>,
    etag: &str,
) -> Interaction {
    let mut interaction = r[4].clone();
    if page > 1 {
        interaction.request.url = format!("{}&page={page}", interaction.request.url);
    }
    let first = interaction.response.body.as_ref().unwrap()[0].clone();
    let pulls: Vec<serde_json::Value> = numbers
        .rev()
        .map(|number| {
            let mut pull = first.clone();
            pull["number"] = number.into();
            pull
        })
        .collect();
    interaction.response.body = Some(pulls.into());
    interaction
        .response
        .headers
        .insert("etag".into(), etag.into());
    interaction
}

#[gpui::test]
async fn every_page_of_open_prs_is_watched(cx: &mut TestAppContext) {
    let r = recorded();
    let page_two = page_of(&r, 2, 1..3, "\"two\"");
    let mut closed = page_of(&r, 2, 1..2, "\"two, one closed\"");
    closed.request.if_none_match = Some("\"two\"".into());
    let mut unchanged_one = r[9].clone();
    unchanged_one.request.if_none_match = Some("\"one\"".into());
    let interactions = vec![
        page_of(&r, 1, 3..103, "\"one\""),
        page_two,
        r[5].clone(),
        unchanged_one,
        closed,
        r[3].clone(),
        r[8].clone(),
        r[10].clone(),
    ];
    let mut t = Polling::new("pages", interactions, cx).await;
    t.update(|poller, cx| poller.watch(pr(2), cx));
    t.run_until(130);
    let repo: Vec<_> = t
        .requests()
        .into_iter()
        .filter(|(_, what)| !what.contains("/pulls/2"))
        .collect();
    assert_eq!(
        repo,
        log(&[
            (0, "GET /pulls?state=open&per_page=100"),
            (0, "GET /pulls?state=open&per_page=100&page=2"),
            (0, "OpenPullRequests"),
            (0, "TrunkRules"),
            // The first page is the same, but an older PR on the second closed.
            (120, "GET /pulls?state=open&per_page=100 (etag)"),
            (120, "GET /pulls?state=open&per_page=100&page=2 (etag)"),
            (120, "OpenPullRequests"),
        ])
    );
}

#[gpui::test]
async fn opening_a_pr_while_its_watched_poll_is_in_flight_polls_it_at_once(
    cx: &mut TestAppContext,
) {
    let r = recorded();
    let mut interactions = vec![
        r[0].clone(),
        r[1].clone(),
        r[2].clone(),
        r[6].clone(),
        r[3].clone(),
        r[8].clone(),
        for_pull(&r[1], 2),
        for_pull(&details_titled(&r, "layer B"), 2),
    ];
    interactions.extend(repo_answers(&r));
    let mut t = Polling::with(
        "open_in_flight",
        interactions,
        Some(Duration::from_secs(1)),
        AccountSource::DeviceFlow,
        cx,
    )
    .await;
    t.update(|poller, cx| {
        poller.watch(pr(2), cx);
        poller.set_open(Some(pr(1)), cx)
    });
    // Step until PR #2's second watched GET is in flight, then open it.
    while !t
        .http
        .log
        .lock()
        .iter()
        .any(|(_, what)| what == "GET /pulls/2 (since)")
    {
        t.run_until(t.now() + 1);
    }
    let opened_at = t.now();
    t.requests();
    t.events();
    t.update(|poller, cx| poller.set_open(Some(pr(2)), cx));
    t.run_until(opened_at + 5);
    // As soon as the watched GET is back: the open PR's poll, with its details.
    assert_eq!(
        t.requests()
            .into_iter()
            .filter(|(_, what)| !what.contains("/pulls?"))
            .collect::<Vec<_>>(),
        log(&[
            (opened_at + 1, "GET /pulls/2 (since)"),
            (opened_at + 2, "PullRequestCheck"),
            (opened_at + 3, "PullRequest"),
        ])
    );
    assert_eq!(t.events(), ["check #2", "details #2: layer B"]);
}

#[gpui::test]
async fn in_the_background_everything_polls_every_five_minutes(cx: &mut TestAppContext) {
    let mut t = Polling::new("background", recorded(), cx).await;
    t.update(|poller, cx| poller.watch(pr(2), cx));
    t.update(|poller, cx| poller.set_open(Some(pr(1)), cx));
    t.run_until(5);
    t.requests();
    t.update(|poller, cx| poller.set_foreground(false, cx));
    t.run_until(610);
    let all = |secs| {
        [
            (secs, "GET /pulls/1 (since)"),
            (secs, "PullRequestCheck"),
            (secs, "GET /pulls/2 (since)"),
            (secs, "GET /pulls?state=open&per_page=100 (etag)"),
        ]
    };
    assert_eq!(t.requests(), log(&[all(300), all(600)].concat()));

    // Coming back polls the open PR at once, with its details, older than 5 minutes, then every
    // 20 s.
    t.update(|poller, cx| poller.set_foreground(true, cx));
    t.run_until(630);
    assert_eq!(
        t.requests(),
        log(&[
            (610, "GET /pulls/1 (since)"),
            (610, "PullRequestCheck"),
            (610, "PullRequest"),
            (630, "GET /pulls/1 (since)"),
            (630, "PullRequestCheck"),
        ])
    );
}

/// The recorded poll, with `low` as GitHub's answer to PR #1's second conditional GET.
fn with_second_answer(low: Interaction) -> Vec<Interaction> {
    let r = recorded();
    let mut interactions = r.clone();
    interactions.insert(6, low);
    interactions
}

#[gpui::test]
async fn a_low_rate_limit_doubles_every_interval(cx: &mut TestAppContext) {
    let low = with_headers(recorded()[6].clone(), &rate_limit("core", 900, 3600));
    let mut t = Polling::new("slowed", with_second_answer(low), cx).await;
    t.update(|poller, cx| poller.watch(pr(2), cx));
    t.update(|poller, cx| poller.set_open(Some(pr(1)), cx));
    t.run_until(20);
    assert_eq!(
        t.status().rate_limit,
        RateLimitState::Slowed(LowLimit {
            resource: "core".into(),
            remaining: 900,
            limit: 5000,
            reset: wall(3600),
        })
    );
    t.requests();
    t.run_until(240);
    assert_eq!(
        t.requests(),
        log(&[
            (60, "GET /pulls/1 (since)"),
            (60, "PullRequestCheck"),
            (100, "GET /pulls/1 (since)"),
            (100, "PullRequestCheck"),
            (140, "GET /pulls/1 (since)"),
            (140, "PullRequestCheck"),
            (180, "GET /pulls/1 (since)"),
            (180, "PullRequestCheck"),
            (220, "GET /pulls/1 (since)"),
            (220, "PullRequestCheck"),
            (240, "GET /pulls/2 (since)"),
            (240, "GET /pulls?state=open&per_page=100 (etag)"),
        ])
    );
}

#[gpui::test]
async fn a_very_low_rate_limit_polls_only_the_open_pr(cx: &mut TestAppContext) {
    let low = with_headers(recorded()[6].clone(), &rate_limit("core", 100, 3600));
    let mut t = Polling::new("open_only", with_second_answer(low), cx).await;
    t.update(|poller, cx| poller.watch(pr(2), cx));
    t.update(|poller, cx| poller.set_open(Some(pr(1)), cx));
    t.run_until(20);
    assert!(matches!(
        t.status().rate_limit,
        RateLimitState::OpenOnly(LowLimit { remaining: 100, .. })
    ));
    t.requests();
    t.run_until(600);
    let mut expected = Vec::new();
    for secs in [140, 260, 380, 500] {
        expected.push((secs, "GET /pulls/1 (since)"));
        expected.push((secs, "PullRequestCheck"));
    }
    assert_eq!(t.requests(), log(&expected));
}

#[gpui::test]
async fn a_used_up_rate_limit_pauses_polling_until_the_reset(cx: &mut TestAppContext) {
    let r = recorded();
    let used_up = with_headers(r[6].clone(), &rate_limit("core", 0, 600));
    let next_window = with_headers(r[6].clone(), &rate_limit("core", 4999, 4200));
    let mut interactions = r.clone();
    interactions.insert(6, used_up);
    interactions.insert(7, next_window);
    let mut t = Polling::new("paused", interactions, cx).await;
    t.update(|poller, cx| poller.watch(pr(2), cx));
    t.update(|poller, cx| poller.set_open(Some(pr(1)), cx));
    t.run_until(20);
    assert_eq!(
        t.status().rate_limit,
        RateLimitState::Paused {
            until: wall(600),
            reason: PauseReason::UsedUp {
                resource: "core".into()
            },
        }
    );
    t.requests();
    t.run_until(599);
    assert_eq!(t.requests(), []);
    // At the reset, everything that's due is polled.
    t.run_until(620);
    assert_eq!(
        t.requests(),
        log(&[
            (600, "GET /pulls/1 (since)"),
            (600, "PullRequestCheck"),
            // Its details are older than 5 minutes.
            (600, "PullRequest"),
            (600, "GET /pulls/2 (since)"),
            (600, "GET /pulls?state=open&per_page=100 (etag)"),
            (620, "GET /pulls/1 (since)"),
            (620, "PullRequestCheck"),
        ])
    );
    assert_eq!(t.status().rate_limit, RateLimitState::Normal);
}

#[gpui::test]
async fn a_pause_ends_on_time_after_the_mac_sleeps(cx: &mut TestAppContext) {
    let r = recorded();
    let mut interactions = r.clone();
    interactions.insert(6, with_headers(r[6].clone(), &rate_limit("core", 0, 600)));
    let mut t = Polling::new("sleep", interactions, cx).await;
    t.update(|poller, cx| poller.set_open(Some(pr(1)), cx));
    t.run_until(30);
    assert!(t.status().rate_limit.is_paused());
    t.requests();
    // Asleep from 30 s for 1,000 s, well past the reset.
    t.sleep(1000);
    t.run_until(100);
    let requests = t.requests();
    assert!(!requests.is_empty(), "still paused");
    assert!(requests[0].0 <= 80, "{requests:?}");
    assert_eq!(t.status().rate_limit, RateLimitState::Normal);
}

#[gpui::test]
async fn an_open_only_limit_with_nothing_open_wakes_at_the_reset(cx: &mut TestAppContext) {
    let low = with_headers(recorded()[6].clone(), &rate_limit("core", 100, 3600));
    let mut t = Polling::new("open_only_reset", with_second_answer(low), cx).await;
    t.update(|poller, cx| {
        poller.watch(pr(2), cx);
        poller.set_open(Some(pr(1)), cx)
    });
    t.run_until(20);
    t.update(|poller, cx| poller.set_open(None, cx));
    t.requests();
    t.run_until(3599);
    assert_eq!(t.requests(), []);
    assert!(matches!(t.status().rate_limit, RateLimitState::OpenOnly(_)));
    // The window resets at 3,600 s: everything is polled, and the banner goes.
    t.run_until(3600);
    assert_eq!(
        t.requests(),
        log(&[
            (3600, "GET /pulls/2 (since)"),
            (3600, "GET /pulls?state=open&per_page=100 (etag)"),
            // Half an hour since the rulesets were read.
            (3600, "TrunkRules"),
            (3600, "GET /pulls/1 (since)"),
        ])
    );
    assert_eq!(t.status().rate_limit, RateLimitState::Normal);
}

#[gpui::test]
async fn a_secondary_limit_waits_as_long_as_github_asks_then_backs_off(cx: &mut TestAppContext) {
    let r = recorded();
    let message = r#"{"message":"You have exceeded a secondary rate limit. Please wait a few minutes before you try again.","documentation_url":"https://docs.github.com/rest"}"#;
    let mut interactions = r.clone();
    interactions.splice(
        6..6,
        [
            answer(&r[6], 403, &[("retry-after", "90")], message),
            answer(&r[6], 429, &[], message),
            answer(&r[6], 429, &[], message),
        ],
    );
    let mut t = Polling::new("secondary", interactions, cx).await;
    t.update(|poller, cx| poller.set_open(Some(pr(1)), cx));
    t.run_until(20);
    assert_eq!(
        t.status().rate_limit,
        RateLimitState::Paused {
            until: wall(110),
            reason: PauseReason::Secondary,
        }
    );
    t.requests();
    t.run_until(500);
    let pull_one: Vec<_> = t
        .requests()
        .into_iter()
        .filter(|(_, what)| !what.contains("/pulls?"))
        .collect();
    assert_eq!(
        pull_one,
        log(&[
            // retry-after: 90.
            (110, "GET /pulls/1 (since)"),
            // No retry-after: a minute, doubling with each secondary limit in a row, the one with
            // a retry-after included. This is the second, so 2 minutes, then 4.
            (230, "GET /pulls/1 (since)"),
            (470, "GET /pulls/1 (since)"),
            (470, "PullRequestCheck"),
            // Its details are older than 5 minutes.
            (470, "PullRequest"),
            (490, "GET /pulls/1 (since)"),
            (490, "PullRequestCheck"),
        ])
    );
}

/// The recorded poll with the open PR's second check answered by a GraphQL rate-limit error,
/// with `remaining` of GraphQL's limit left in a window that resets at 600 s.
fn with_graphql_rate_limit(remaining: u32) -> Vec<Interaction> {
    let r = recorded();
    let limited = answer(
        &r[7],
        200,
        &[],
        r#"{"data":null,"errors":[{"type":"RATE_LIMITED","message":"API rate limit exceeded for user ID 1."}]}"#,
    );
    let mut interactions = r.clone();
    interactions.insert(
        7,
        with_headers(limited, &rate_limit("graphql", remaining, 600)),
    );
    interactions
}

#[gpui::test]
async fn a_used_up_graphql_limit_is_named(cx: &mut TestAppContext) {
    let mut t = Polling::new("graphql_used_up", with_graphql_rate_limit(0), cx).await;
    t.update(|poller, cx| poller.set_open(Some(pr(1)), cx));
    t.run_until(20);
    assert_eq!(
        t.status().rate_limit,
        RateLimitState::Paused {
            until: wall(600),
            reason: PauseReason::UsedUp {
                resource: "graphql".into()
            },
        }
    );
}

#[gpui::test]
async fn a_graphql_rate_limit_with_some_left_is_a_secondary_limit(cx: &mut TestAppContext) {
    let mut t = Polling::new("graphql_secondary", with_graphql_rate_limit(4000), cx).await;
    t.update(|poller, cx| poller.set_open(Some(pr(1)), cx));
    t.run_until(20);
    // A minute, not until the window resets, and not "core used up".
    assert_eq!(
        t.status().rate_limit,
        RateLimitState::Paused {
            until: wall(80),
            reason: PauseReason::Secondary,
        }
    );
}

#[gpui::test]
async fn offline_it_backs_off_and_answers_from_the_cache_with_its_age(cx: &mut TestAppContext) {
    let r = recorded();
    let mut t = Polling::new("offline", r.clone(), cx).await;
    let three_hours_ago = SystemTime::UNIX_EPOCH + Duration::from_secs(START - 3 * 3600);
    t.cache
        .put(
            LOGIN,
            &Target::Pull(pr(1)),
            &decoded_details(&r),
            None,
            three_hours_ago,
        )
        .await
        .unwrap();
    t.http.offline.store(true, Ordering::SeqCst);

    // Opening shows the cached copy at once.
    let answer = t
        .update(|poller, cx| poller.set_open(Some(pr(1)), cx))
        .unwrap();
    assert_eq!(answer.details.title, "feat: stack layer A");
    assert_eq!(answer.fetched_at, three_hours_ago);

    // The refresh fails: the copy is now known to be offline, 3 hours old.
    assert_eq!(
        t.status().offline,
        Some(Offline {
            since: wall(0),
            retry_at: wall(20),
            error: "the network is down".into(),
        })
    );
    let answer = t.read(|poller| poller.pull_request(&pr(1))).unwrap();
    assert_eq!(answer.offline_age, Some(Duration::from_secs(3 * 3600)));
    t.run_until(10);
    let answer = t.read(|poller| poller.pull_request(&pr(1))).unwrap();
    assert_eq!(answer.offline_age, Some(Duration::from_secs(3 * 3600 + 10)));

    // Retries back off from 20 s, doubling up to 5 minutes.
    t.run_until(900);
    assert_eq!(
        t.requests(),
        log(&[
            (0, "GET /pulls/1"),
            (20, "GET /pulls/1"),
            (60, "GET /pulls/1"),
            (140, "GET /pulls/1"),
            (300, "GET /pulls/1"),
            (600, "GET /pulls/1"),
            (900, "GET /pulls/1"),
        ])
    );
    assert_eq!(t.status().offline.unwrap().retry_at, wall(1200));

    // Back online, the next retry succeeds and everything is current again.
    t.http.offline.store(false, Ordering::SeqCst);
    t.run_until(1200);
    assert_eq!(
        t.requests(),
        log(&[
            (1200, "GET /pulls/1"),
            (1200, "PullRequestCheck"),
            (1200, "PullRequest"),
            (1200, "GET /pulls?state=open&per_page=100"),
            (1200, "OpenPullRequests"),
            (1200, "TrunkRules"),
        ])
    );
    assert_eq!(t.status(), PollStatus::default());
    let answer = t.read(|poller| poller.pull_request(&pr(1))).unwrap();
    assert_eq!((answer.fetched_at, answer.offline_age), (wall(1200), None));
}

#[gpui::test]
async fn offline_says_when_the_next_try_really_is(cx: &mut TestAppContext) {
    let mut t = Polling::new("offline_retry_at", recorded(), cx).await;
    t.update(|poller, cx| poller.set_open(Some(pr(1)), cx));
    t.run_until(10);
    t.http.offline.store(true, Ordering::SeqCst);
    t.run_until(20);
    assert_eq!(t.status().offline.unwrap().retry_at, wall(40));
    // In the background the open PR is next polled 5 minutes after it last answered.
    t.update(|poller, cx| poller.set_foreground(false, cx));
    assert_eq!(t.status().offline.unwrap().retry_at, wall(300));
}

#[gpui::test]
async fn any_answer_from_github_ends_offline(cx: &mut TestAppContext) {
    let r = recorded();
    let message = r#"{"message":"You have exceeded a secondary rate limit.","documentation_url":"https://docs.github.com/rest"}"#;
    let mut interactions = r.clone();
    interactions.insert(0, answer(&r[0], 403, &[("retry-after", "90")], message));
    let mut t = Polling::new("offline_ends", interactions, cx).await;
    t.http.offline.store(true, Ordering::SeqCst);
    t.update(|poller, cx| poller.set_open(Some(pr(1)), cx));
    assert!(t.status().offline.is_some());
    // Back online, GitHub answers, if only to say to wait.
    t.http.offline.store(false, Ordering::SeqCst);
    t.run_until(20);
    let status = t.status();
    assert_eq!(status.offline, None);
    assert!(status.rate_limit.is_paused());
}

#[gpui::test]
async fn opening_a_pr_while_offline_tries_at_once(cx: &mut TestAppContext) {
    let r = recorded();
    let mut interactions = r.clone();
    interactions.extend([
        for_pull(&r[1], 2),
        for_pull(&details_titled(&r, "layer B"), 2),
    ]);
    let mut t = Polling::new("offline_open", interactions, cx).await;
    t.http.offline.store(true, Ordering::SeqCst);
    t.update(|poller, cx| poller.set_open(Some(pr(1)), cx));
    t.run_until(70);
    assert_eq!(t.status().offline.unwrap().retry_at, wall(140));
    t.requests();
    // The network is back, and the user opens another PR: it's tried now, not at 140 s, and
    // then everything else that's due.
    t.http.offline.store(false, Ordering::SeqCst);
    t.update(|poller, cx| poller.set_open(Some(pr(2)), cx));
    assert_eq!(
        t.requests(),
        log(&[
            (70, "GET /pulls/2"),
            (70, "PullRequestCheck"),
            (70, "PullRequest"),
            (70, "GET /pulls/1"),
            (70, "GET /pulls?state=open&per_page=100"),
            (70, "OpenPullRequests"),
            (70, "TrunkRules"),
        ])
    );
    assert_eq!(t.status().offline, None);
}

#[gpui::test]
async fn reopening_shows_the_cache_at_once_and_its_first_poll_is_free(cx: &mut TestAppContext) {
    let mut t = Polling::new("reopen", recorded(), cx).await;
    t.update(|poller, cx| poller.set_open(Some(pr(1)), cx));
    t.run_until(5);
    t.requests();
    t.events();

    t.restart();
    let answer = t
        .update(|poller, cx| {
            let answer = poller.set_open(Some(pr(1)), cx);
            // Before any request, the cached copy and the repository's open PRs are there.
            assert!(poller.open_pull_requests(&playground()).is_some());
            answer
        })
        .unwrap();
    assert_eq!(answer.details.title, "feat: stack layer A");
    assert_eq!(answer.fetched_at, wall(0));
    // Its first poll sends the cached ETags, so it costs one GraphQL point and no details.
    assert_eq!(
        t.requests(),
        log(&[
            (5, "GET /pulls/1 (since)"),
            (5, "PullRequestCheck"),
            (5, "GET /pulls?state=open&per_page=100 (etag)"),
        ])
    );
    assert_eq!(t.events(), Vec::<String>::new());
    let recent = t.cache.recent(10).unwrap();
    assert_eq!(recent.len(), 1);
    assert_eq!(
        (recent[0].pull.clone(), recent[0].title.as_deref()),
        (pr(1), Some("feat: stack layer A"))
    );
    assert_eq!(recent[0].opened_at, wall(5));
}

/// `POST /issues/1/comments`, answered `201`.
fn comment(r: &[Interaction]) -> Interaction {
    Interaction {
        request: cassette::RecordedRequest {
            method: "POST".into(),
            url: "https://api.github.com/repos/rozed-dev/playground/issues/1/comments".into(),
            body: Some(r#"{"body":"hi"}"#.into()),
            ..Default::default()
        },
        response: answer(&r[0], 201, &[], r#"{"id":1}"#).response,
    }
}

fn post_comment(
    poller: &mut Poller,
    cx: &mut gpui::Context<Poller>,
) -> gpui::Task<Result<serde_json::Value, github::GithubError>> {
    poller.write(
        pr(1),
        |client| async move {
            client
                .post::<serde_json::Value, _>(
                    "/repos/rozed-dev/playground/issues/1/comments",
                    &serde_json::json!({"body": "hi"}),
                )
                .await
        },
        cx,
    )
}

/// Asserts no request was sent before the one before it finished, when each takes a second.
fn assert_one_at_a_time(requests: &[(u64, String)]) {
    for pair in requests.windows(2) {
        assert!(pair[1].0 > pair[0].0, "{requests:?}");
    }
}

#[gpui::test]
async fn a_write_waits_for_the_poll_in_flight_and_the_previous_write(cx: &mut TestAppContext) {
    let r = recorded();
    let mut interactions = r.clone();
    interactions.push(comment(&r));
    let mut t = Polling::with(
        "write",
        interactions,
        Some(Duration::from_secs(1)),
        AccountSource::DeviceFlow,
        cx,
    )
    .await;
    t.update(|poller, cx| poller.set_open(Some(pr(1)), cx));
    t.run_until(1);
    let first = t.update(post_comment);
    let second = t.update(post_comment);
    t.run_until(20);
    assert_eq!(first.await.unwrap()["id"], 1);
    assert_eq!(second.await.unwrap()["id"], 1);

    let requests = t.requests();
    // Each request takes a second, and none overlaps another.
    assert_one_at_a_time(&requests);
    let posts: Vec<u64> = requests
        .iter()
        .filter(|(_, what)| what.starts_with("POST /issues"))
        .map(|(secs, _)| *secs)
        .collect();
    // The first waits for the open PR's poll (0–3 s); the second starts a second after the first
    // finished.
    assert_eq!(posts.len(), 2);
    assert!(posts[0] >= 3, "{requests:?}");
    assert!(posts[1] >= posts[0] + 2, "{requests:?}");
}

#[gpui::test]
async fn a_poll_that_comes_due_while_a_write_holds_the_queue_waits(cx: &mut TestAppContext) {
    let r = recorded();
    let mut interactions = r.clone();
    interactions.push(comment(&r));
    let mut t = Polling::with(
        "poll_waits_for_write",
        interactions,
        Some(Duration::from_secs(1)),
        AccountSource::DeviceFlow,
        cx,
    )
    .await;
    t.update(|poller, cx| poller.set_open(Some(pr(1)), cx));
    t.run_until(21);
    t.requests();
    // The second write holds the queue through its second of spacing, while the open PR's next
    // poll comes due.
    let first = t.update(post_comment);
    let second = t.update(post_comment);
    t.run_until(30);
    first.await.unwrap();
    second.await.unwrap();
    let requests = t.requests();
    assert_one_at_a_time(&requests);
    assert_eq!(
        requests
            .iter()
            .filter(|(_, what)| what.starts_with("POST"))
            .count(),
        2
    );
}

#[gpui::test]
async fn a_waiting_write_goes_before_the_polls_that_come_due(cx: &mut TestAppContext) {
    let r = recorded();
    let mut interactions = r.clone();
    interactions.push(comment(&r));
    let mut t = Polling::with(
        "write_before_polls",
        interactions,
        Some(Duration::from_secs(1)),
        AccountSource::DeviceFlow,
        cx,
    )
    .await;
    t.update(|poller, cx| {
        poller.watch(pr(2), cx);
        poller.set_open(Some(pr(1)), cx)
    });
    t.run_until(1);
    // Sent while the open PR's poll is in flight, with the other PR and the repository due.
    let write = t.update(post_comment);
    t.run_until(15);
    write.await.unwrap();
    let requests = t.requests();
    let what: Vec<&str> = requests.iter().map(|(_, what)| what.as_str()).collect();
    assert_eq!(
        what,
        [
            "GET /pulls/1",
            "PullRequestCheck",
            "PullRequest",
            "POST /issues/1/comments",
            "GET /pulls/2",
            // The comment was on PR #1, so it's fetched again.
            "GET /pulls/1 (since)",
            "PullRequestCheck",
            "PullRequest",
            "GET /pulls?state=open&per_page=100",
            "OpenPullRequests",
            "TrunkRules",
        ],
        "{requests:?}"
    );
}

#[gpui::test]
async fn a_write_fetches_what_it_changed_again(cx: &mut TestAppContext) {
    let r = recorded();
    let mut interactions = r.clone();
    interactions.push(comment(&r));
    let mut t = Polling::new("write_invalidates", interactions, cx).await;
    t.update(|poller, cx| poller.set_open(Some(pr(1)), cx));
    t.run_until(5);
    t.requests();
    let write = t.update(post_comment);
    t.run();
    write.await.unwrap();
    t.run();
    // At once, and in full, although the check is the same.
    assert_eq!(
        t.requests(),
        log(&[
            (5, "POST /issues/1/comments"),
            (5, "GET /pulls/1 (since)"),
            (5, "PullRequestCheck"),
            (5, "PullRequest"),
        ])
    );

    // Not watched, it's forgotten in the cache, so it's fetched in full when it's opened again.
    t.update(|poller, cx| {
        poller.set_open(None, cx);
        poller.unwatch(&pr(1), cx)
    });
    let write = t.update(post_comment);
    t.run();
    write.await.unwrap();
    t.run();
    assert_eq!(t.requests(), log(&[(6, "POST /issues/1/comments")]));
    t.update(|poller, cx| poller.set_open(Some(pr(1)), cx));
    assert_eq!(
        t.requests()
            .into_iter()
            .filter(|(_, what)| !what.contains("/pulls?"))
            .collect::<Vec<_>>(),
        log(&[
            (6, "GET /pulls/1 (since)"),
            (6, "PullRequestCheck"),
            (6, "PullRequest"),
        ])
    );
}

#[gpui::test]
async fn the_open_prs_details_are_fetched_in_full_every_five_minutes(cx: &mut TestAppContext) {
    let mut t = Polling::new("details_refresh", recorded(), cx).await;
    t.update(|poller, cx| poller.set_open(Some(pr(1)), cx));
    t.run_until(5);
    t.requests();
    t.run_until(620);
    let details: Vec<u64> = t
        .requests()
        .into_iter()
        .filter(|(_, what)| what == "PullRequest")
        .map(|(secs, _)| secs)
        .collect();
    // Viewed marks set on github.com move nothing the check sees.
    assert_eq!(details, [300, 600]);

    // Not in the background.
    t.update(|poller, cx| poller.set_foreground(false, cx));
    t.run_until(1300);
    assert!(
        !t.requests().iter().any(|(_, what)| what == "PullRequest"),
        "fetched details in the background"
    );
    // Back in the foreground, details older than 5 minutes are fetched at once.
    t.update(|poller, cx| poller.set_foreground(true, cx));
    assert_eq!(
        t.requests(),
        log(&[
            (1300, "GET /pulls/1 (since)"),
            (1300, "PullRequestCheck"),
            (1300, "PullRequest"),
        ])
    );
}

#[gpui::test]
async fn a_write_finishes_when_its_caller_stops_waiting(cx: &mut TestAppContext) {
    let r = recorded();
    let mut interactions = r.clone();
    interactions.push(comment(&r));
    let mut t = Polling::with(
        "write_dropped",
        interactions,
        Some(Duration::from_secs(1)),
        AccountSource::DeviceFlow,
        cx,
    )
    .await;
    t.update(|poller, cx| poller.set_open(Some(pr(1)), cx));
    t.run_until(6);
    t.requests();
    // The tab that asked for it closes while it's in flight.
    let write = t.update(post_comment);
    assert_eq!(t.requests(), log(&[(6, "POST /issues/1/comments")]));
    drop(write);
    t.run_until(11);
    // It finished: what it changed is fetched again.
    assert_eq!(
        t.requests(),
        log(&[
            (7, "GET /pulls/1 (since)"),
            (8, "PullRequestCheck"),
            (9, "PullRequest"),
        ])
    );
}

#[gpui::test]
async fn a_write_that_hits_a_rate_limit_pauses_polling(cx: &mut TestAppContext) {
    let r = recorded();
    let mut limited = comment(&r);
    limited.response = answer(
        &r[0],
        403,
        &[("retry-after", "60")],
        r#"{"message":"You have exceeded a secondary rate limit."}"#,
    )
    .response;
    let mut interactions = r.clone();
    interactions.push(limited);
    let mut t = Polling::new("write_rate_limited", interactions, cx).await;
    t.update(|poller, cx| poller.set_open(Some(pr(1)), cx));
    t.run_until(5);
    let write = t.update(post_comment);
    t.run();
    assert!(matches!(
        write.await,
        Err(github::GithubError::RateLimited {
            secondary: true,
            ..
        })
    ));
    assert_eq!(
        t.status().rate_limit,
        RateLimitState::Paused {
            until: wall(65),
            reason: PauseReason::Secondary,
        }
    );
}

#[gpui::test]
async fn a_refused_token_signs_the_account_out_until_it_signs_in_again(cx: &mut TestAppContext) {
    let r = recorded();
    let mut interactions = r.clone();
    interactions.insert(6, answer(&r[6], 401, &[], BAD_CREDENTIALS));
    let mut t = Polling::new("signed_out", interactions, cx).await;
    t.update(|poller, cx| poller.set_open(Some(pr(1)), cx));
    t.run_until(20);
    assert!(t.status().signed_out);
    assert!(
        t.accounts
            .read_with(&t.cx, |accounts, _| accounts.is_signed_out(LOGIN))
    );
    t.requests();
    t.run_until(300);
    assert_eq!(t.requests(), []);

    // Signing in again resumes polling at once.
    let added = t.accounts.update(&mut t.cx, |accounts, cx| {
        accounts.add(user(), Token::new("gho_NEW"), AccountSource::DeviceFlow, cx)
    });
    t.run();
    added.await.unwrap();
    t.run();
    assert!(!t.status().signed_out);
    assert_eq!(
        t.requests(),
        log(&[
            (300, "GET /pulls/1 (since)"),
            (300, "PullRequestCheck"),
            // Its details are older than 5 minutes.
            (300, "PullRequest"),
            (300, "GET /pulls?state=open&per_page=100 (etag)"),
        ])
    );
    assert_eq!(
        t.http.replay.seen().last().unwrap().header("authorization"),
        Some("token gho_NEW")
    );
}

#[gpui::test]
async fn a_token_that_cant_be_read_is_tried_again(cx: &mut TestAppContext) {
    let mut t = Polling::new("token_unreadable", recorded(), cx).await;
    // The Keychain is locked when Lo starts, or its prompt was dismissed.
    t.store.state.lock().fail_reads = true;
    t.restart_lo();
    t.update(|poller, cx| poller.set_open(Some(pr(1)), cx));
    let status = t.status();
    assert!(!status.signed_out);
    let offline = status.offline.unwrap();
    assert_eq!(offline.retry_at, wall(20));
    assert!(offline.error.contains("token"), "{}", offline.error);
    // A write says so, rather than that GitHub refused the token.
    let write = t.update(post_comment);
    t.run();
    let error = write.await.unwrap_err();
    assert!(!error.is_unauthorized(), "{error:?}");

    // Once it can be read, polling goes on.
    t.store.state.lock().fail_reads = false;
    t.run_until(20);
    assert_eq!(
        t.requests(),
        log(&[
            (20, "GET /pulls/1"),
            (20, "PullRequestCheck"),
            (20, "PullRequest"),
            (20, "GET /pulls?state=open&per_page=100"),
            (20, "OpenPullRequests"),
            (20, "TrunkRules"),
        ])
    );
    assert_eq!(t.status(), PollStatus::default());
}

const BAD_CREDENTIALS: &str = r#"{"message":"Bad credentials","documentation_url":"https://docs.github.com/rest","status":"401"}"#;

/// A `gh` that has a newer token for rozed-bot.
const FAKE_GH: &str = r#"#!/bin/sh
case "$*" in
  "auth token --hostname github.com --user rozed-bot")
    printf 'gho_FAKE_FROM_GH\n' ;;
  *)
    echo "no oauth token found for github.com" >&2
    exit 1 ;;
esac
"#;

#[gpui::test]
async fn a_refused_gh_token_is_read_from_gh_again_and_polling_goes_on(cx: &mut TestAppContext) {
    use std::os::unix::fs::PermissionsExt as _;

    let r = recorded();
    let mut interactions = r.clone();
    interactions.insert(6, answer(&r[6], 401, &[], BAD_CREDENTIALS));
    // GitHub says the token gh hands over is rozed-bot's.
    let (_, user) = cassette::read_cassette(&cassette::fixtures_dir().join("user")).unwrap();
    interactions.extend(user.into_iter().map(|(_, i)| i));
    let mut t = Polling::with("gh_reread", interactions, None, AccountSource::Gh, cx).await;
    let dir = tempfile::tempdir().unwrap();
    let gh = dir.path().join("gh");
    std::fs::write(&gh, FAKE_GH).unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    t.accounts
        .update(&mut t.cx, |accounts, _| accounts.set_gh_path(gh));

    t.update(|poller, cx| poller.set_open(Some(pr(1)), cx));
    t.run_until(5);
    t.requests();
    t.run_until(40);
    assert_eq!(
        t.requests(),
        log(&[
            // Refused, read from gh again and checked, then sent again with gh's token.
            (20, "GET /pulls/1 (since)"),
            (20, "GET /user"),
            (20, "GET /pulls/1 (since)"),
            (20, "PullRequestCheck"),
            (40, "GET /pulls/1 (since)"),
            (40, "PullRequestCheck"),
        ])
    );
    assert!(!t.status().signed_out);
    assert_eq!(
        t.http.replay.seen().last().unwrap().header("authorization"),
        Some("token gho_FAKE_FROM_GH")
    );
}

#[gpui::test]
async fn a_failed_poll_is_reported_and_tried_again_at_its_interval(cx: &mut TestAppContext) {
    let r = recorded();
    let mut missing = answer(
        &r[3],
        404,
        &[],
        r#"{"message":"Not Found","documentation_url":"https://docs.github.com/rest","status":"404"}"#,
    );
    missing.request.url = "https://api.github.com/repos/rozed-dev/playground/pulls/99".into();
    let mut interactions = repo_answers(&r);
    interactions.push(missing);
    let mut t = Polling::new("failed", interactions, cx).await;
    t.update(|poller, cx| poller.watch(pr(99), cx));
    t.run_until(120);
    assert_eq!(
        t.requests(),
        log(&[
            (0, "GET /pulls/99"),
            (0, "GET /pulls?state=open&per_page=100"),
            (0, "OpenPullRequests"),
            (0, "TrunkRules"),
            (120, "GET /pulls/99"),
            (120, "GET /pulls?state=open&per_page=100 (etag)"),
        ])
    );
    let failed = t
        .events()
        .into_iter()
        .filter(|event| event.starts_with("failed"))
        .count();
    assert_eq!(failed, 2);
    assert_eq!(t.status(), PollStatus::default());
}

#[gpui::test]
async fn a_poll_that_fails_after_its_pr_is_unwatched_isnt_reported(cx: &mut TestAppContext) {
    let r = recorded();
    let mut missing = answer(&r[3], 404, &[], r#"{"message":"Not Found"}"#);
    missing.request.url = "https://api.github.com/repos/rozed-dev/playground/pulls/99".into();
    let mut t = Polling::with(
        "failed_unwatched",
        vec![missing],
        Some(Duration::from_secs(1)),
        AccountSource::DeviceFlow,
        cx,
    )
    .await;
    t.update(|poller, cx| {
        poller.watch(pr(99), cx);
        // Its repository isn't polled in this test.
        poller.set_open(None, cx);
    });
    assert_eq!(t.requests(), log(&[(0, "GET /pulls/99")]));
    t.update(|poller, cx| poller.unwatch(&pr(99), cx));
    t.run_until(5);
    assert_eq!(t.events(), Vec::<String>::new());
}

#[gpui::test]
async fn unwatching_stops_polling(cx: &mut TestAppContext) {
    let mut t = Polling::new("unwatching", recorded(), cx).await;
    t.update(|poller, cx| poller.watch(pr(2), cx));
    t.update(|poller, cx| poller.set_open(Some(pr(1)), cx));
    t.run_until(5);
    t.update(|poller, cx| poller.unwatch(&pr(2), cx));
    t.requests();
    t.run_until(120);
    // #1 and the repository are still watched.
    assert_eq!(
        t.requests()
            .into_iter()
            .filter(|(_, what)| what.contains("/pulls/2"))
            .count(),
        0
    );
    t.update(|poller, cx| {
        poller.set_open(None, cx);
        poller.unwatch(&pr(1), cx);
    });
    t.run_until(1000);
    assert_eq!(t.requests(), []);
    assert_eq!(
        t.read(|poller| poller.open_pull_requests(&playground())),
        None
    );
}

// Reads the poller keeps in the cache, with a made-up read whose requests send nothing: each
// counts itself and answers what the test set.

/// What [`counted`] reads: the value the test set.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
struct Counted(u32);

impl github::cache::CacheEntry for Counted {
    const NAME: &'static str = "Counted";

    fn shape() -> String {
        github::cache::schema_shape::<Self>()
    }
}

impl CachedRead for Counted {
    /// What a push history watches.
    const WATCHED: &'static [&'static str] = PushHistory::WATCHED;

    /// Zero stands for a value one of whose parts couldn't be read.
    fn is_complete(&self) -> bool {
        self.0 != 0
    }
}

/// How many times the read ran, and what it answers next: a value, or an error.
#[derive(Clone)]
struct Answers {
    runs: Arc<std::sync::atomic::AtomicU32>,
    next: Arc<Mutex<Result<u32, String>>>,
}

impl Default for Answers {
    fn default() -> Self {
        Self {
            runs: Arc::default(),
            next: Arc::new(Mutex::new(Ok(1))),
        }
    }
}

impl Answers {
    fn runs(&self) -> u32 {
        self.runs.load(Ordering::SeqCst)
    }

    fn answer(&self, next: Result<u32, &str>) {
        *self.next.lock() = next.map_err(str::to_string);
    }
}

/// Keeps a [`Counted`] read for `pull`, returning what the poller has at once.
fn counted(
    poller: &mut Poller,
    pull: PullRef,
    answers: &Answers,
    cx: &mut gpui::Context<Poller>,
) -> Option<u32> {
    let answers = answers.clone();
    poller
        .read_cached(
            pull,
            move |_, _known| {
                answers.runs.fetch_add(1, Ordering::SeqCst);
                let next = answers.next.lock().clone();
                async move { next.map(Counted).map_err(GithubError::Decode) }
            },
            cx,
        )
        .map(|cached| cached.value.0)
}

fn cached_count(t: &Polling) -> Option<u32> {
    t.read(|poller| poller.cached_read::<Counted>(&pr(1)))
        .map(|cached| cached.value.0)
}

/// PR #1's recorded check with `edit` made to it.
fn check_with(r: &[Interaction], edit: impl FnOnce(&mut serde_json::Value)) -> Interaction {
    with_body(r[1].clone(), |body| {
        edit(&mut body["data"]["repository"]["pullRequest"])
    })
}

#[gpui::test]
async fn a_cached_read_runs_after_the_open_prs_poll_and_again_when_a_field_it_watches_changes(
    cx: &mut TestAppContext,
) {
    let r = recorded();
    let rollup = check_with(&r, |pull| {
        pull["statusCheckRollup"] = serde_json::json!({"state": "PENDING"});
    });
    let commented = check_with(&r, |pull| {
        pull["statusCheckRollup"] = serde_json::json!({"state": "PENDING"});
        pull["timelineItems"]["updatedAt"] = "2026-11-06T12:00:00Z".into();
        pull["comments"]["totalCount"] = 1.into();
    });
    let force_pushed = check_with(&r, |pull| {
        pull["statusCheckRollup"] = serde_json::json!({"state": "PENDING"});
        pull["timelineItems"]["updatedAt"] = "2026-11-06T12:01:00Z".into();
        pull["comments"]["totalCount"] = 1.into();
        pull["revisionEvents"]["filteredCount"] = 1.into();
    });
    // You submit the review you had pending: the count of every review, which held it already,
    // stays as it was.
    let submitted = check_with(&r, |pull| {
        pull["statusCheckRollup"] = serde_json::json!({"state": "PENDING"});
        pull["timelineItems"]["updatedAt"] = "2026-11-06T12:02:00Z".into();
        pull["comments"]["totalCount"] = 1.into();
        pull["revisionEvents"]["filteredCount"] = 1.into();
        let submitted = pull["submittedReviews"]["totalCount"].as_i64().unwrap();
        pull["submittedReviews"]["totalCount"] = (submitted + 1).into();
    });
    let mut interactions = vec![
        r[0].clone(),
        r[6].clone(),
        r[1].clone(),
        r[1].clone(),
        rollup,
        commented,
        force_pushed,
        submitted,
        r[2].clone(),
    ];
    interactions.extend(repo_answers(&r));
    let mut t = Polling::new("cached_read", interactions, cx).await;
    let answers = Answers::default();
    answers.answer(Ok(1));
    let at_once = t.update(|poller, cx| {
        poller.set_open(Some(pr(1)), cx);
        counted(poller, pr(1), &answers, cx)
    });
    assert_eq!(at_once, None);
    t.run();
    assert_eq!(answers.runs(), 1);
    assert_eq!(cached_count(&t), Some(1));
    assert!(t.events().contains(&"read Counted #1".to_string()));

    // The checks' state isn't a field it watches, and a comment, which moves the timeline,
    // changes none.
    t.run_until(40);
    assert_eq!(answers.runs(), 1);
    assert_eq!(t.events(), ["check #1"]);
    t.run_until(60);
    assert_eq!(answers.runs(), 1);
    assert_eq!(t.events(), ["check #1"]);

    // A force-push: it's read again, after the poll. An unchanged answer sends nothing.
    t.run_until(80);
    assert_eq!(answers.runs(), 2);
    assert_eq!(t.events(), ["check #1"]);
    assert_eq!(cached_count(&t), Some(1));
    // A submitted review: it's read again.
    t.run_until(100);
    assert_eq!(answers.runs(), 3);
    t.run_until(120);
    assert_eq!(answers.runs(), 3);

    // The cache took the new stamp, so after a restart it isn't read again.
    t.restart();
    t.update(|poller, cx| {
        poller.set_open(Some(pr(1)), cx);
        counted(poller, pr(1), &answers, cx);
    });
    t.run_until(125);
    assert_eq!(answers.runs(), 3);
}

#[gpui::test]
async fn a_cached_read_is_refreshed_every_five_minutes_and_kept_across_restarts(
    cx: &mut TestAppContext,
) {
    let mut t = Polling::new("cached_read_refresh", recorded(), cx).await;
    let answers = Answers::default();
    answers.answer(Ok(1));
    t.update(|poller, cx| {
        poller.set_open(Some(pr(1)), cx);
        counted(poller, pr(1), &answers, cx);
    });
    t.run();
    t.events();

    // Like the details, every 5 minutes in the foreground. A new answer is sent.
    answers.answer(Ok(2));
    t.run_until(299);
    assert_eq!(answers.runs(), 1);
    t.run_until(300);
    assert_eq!(answers.runs(), 2);
    assert_eq!(t.events(), ["read Counted #1"]);
    let refreshed = t.read(|poller| poller.cached_read::<Counted>(&pr(1)).unwrap());
    assert_eq!((refreshed.value.0, refreshed.fetched_at), (2, wall(300)));
    // Not in the background.
    t.update(|poller, cx| poller.set_foreground(false, cx));
    t.run_until(1000);
    assert_eq!(answers.runs(), 2);
    t.update(|poller, cx| poller.set_foreground(true, cx));
    t.run_until(1001);
    assert_eq!(answers.runs(), 3);

    // After a restart, it's there at once, and since the check hasn't moved, it isn't read again.
    t.restart();
    let at_once = t.update(|poller, cx| {
        poller.set_open(Some(pr(1)), cx);
        counted(poller, pr(1), &answers, cx)
    });
    assert_eq!(at_once, Some(2));
    t.run_until(1010);
    assert_eq!(answers.runs(), 3);
}

#[gpui::test]
async fn a_cached_read_that_fails_says_so_and_is_tried_again_at_the_open_prs_interval(
    cx: &mut TestAppContext,
) {
    let mut t = Polling::new("cached_read_fails", recorded(), cx).await;
    let answers = Answers::default();
    answers.answer(Err("no good"));
    t.update(|poller, cx| {
        poller.set_open(Some(pr(1)), cx);
        counted(poller, pr(1), &answers, cx);
    });
    t.run();
    assert_eq!(answers.runs(), 1);
    assert!(
        t.events()
            .contains(&"failed to read Counted #1: couldn't read GitHub's answer: no good".into())
    );
    // The pull request itself is fine, and polled as ever.
    assert!(t.read(|poller| poller.check(&pr(1))).is_some());

    answers.answer(Ok(7));
    t.run_until(19);
    assert_eq!(answers.runs(), 1);
    t.run_until(20);
    assert_eq!(answers.runs(), 2);
    assert_eq!(t.events(), ["read Counted #1"]);
    assert_eq!(cached_count(&t), Some(7));
}

#[gpui::test]
async fn a_cached_read_whose_refresh_keeps_failing_waits_longer_each_time(cx: &mut TestAppContext) {
    let mut t = Polling::new("cached_read_refresh_fails", recorded(), cx).await;
    let answers = Answers::default();
    t.update(|poller, cx| {
        poller.set_open(Some(pr(1)), cx);
        counted(poller, pr(1), &answers, cx);
    });
    t.run();
    assert_eq!(answers.runs(), 1);

    // GitHub starts failing it, from its 5-minute refresh on.
    answers.answer(Err("bad gateway"));
    t.run_until(300);
    assert_eq!(answers.runs(), 2);
    t.run_until(319);
    assert_eq!(answers.runs(), 2);
    // It waits the open PR's 20 s, then 40, 80 and 160, then the 5-minute refresh's 300 each
    // time: tries at 320, 360, 440, 600, 900, 1200, 1500, 1800 and 2100 s.
    t.run_until(300 + 30 * 60);
    assert_eq!(answers.runs(), 11);
    assert_eq!(cached_count(&t), Some(1));

    // One that works again is read as before.
    answers.answer(Ok(5));
    t.run_until(2400);
    assert_eq!(answers.runs(), 12);
    assert_eq!(cached_count(&t), Some(5));
    t.run_until(2690);
    assert_eq!(answers.runs(), 12);
}

#[gpui::test]
async fn a_cached_read_that_isnt_complete_is_kept_and_read_again_after_a_wait(
    cx: &mut TestAppContext,
) {
    let mut t = Polling::new("cached_read_incomplete", recorded(), cx).await;
    let answers = Answers::default();
    answers.answer(Ok(0));
    t.update(|poller, cx| {
        poller.set_open(Some(pr(1)), cx);
        counted(poller, pr(1), &answers, cx);
    });
    t.run();
    assert_eq!(answers.runs(), 1);
    assert_eq!(cached_count(&t), Some(0));
    assert!(t.events().contains(&"read Counted #1".to_string()));

    // Nothing it watches changed, but it's read again after the open PR's interval, then
    // waiting longer each time, as a read that failed is.
    t.run_until(19);
    assert_eq!(answers.runs(), 1);
    t.run_until(20);
    assert_eq!(answers.runs(), 2);
    t.run_until(59);
    assert_eq!(answers.runs(), 2);
    answers.answer(Ok(4));
    t.run_until(60);
    assert_eq!(answers.runs(), 3);
    assert_eq!(cached_count(&t), Some(4));
    // Complete, it waits for its watched fields or the 5-minute refresh.
    t.run_until(359);
    assert_eq!(answers.runs(), 3);
}

#[gpui::test]
async fn a_cached_read_waits_while_its_pull_request_isnt_open(cx: &mut TestAppContext) {
    let mut t = Polling::new("cached_read_watched", recorded(), cx).await;
    let answers = Answers::default();
    answers.answer(Ok(1));
    t.update(|poller, cx| {
        counted(poller, pr(1), &answers, cx);
    });
    t.run_until(200);
    assert_eq!(answers.runs(), 0);

    t.update(|poller, cx| poller.set_open(Some(pr(1)), cx));
    t.run_until(201);
    assert_eq!(answers.runs(), 1);
}

#[gpui::test]
async fn a_write_goes_between_the_requests_of_a_long_cached_read(cx: &mut TestAppContext) {
    let r = recorded();
    let mut interactions = r.clone();
    interactions.push(comment(&r));
    let mut t = Polling::with(
        "read_yields",
        interactions,
        Some(Duration::from_secs(1)),
        AccountSource::DeviceFlow,
        cx,
    )
    .await;
    let runs = Arc::new(std::sync::atomic::AtomicU32::new(0));
    t.update(|poller, cx| {
        poller.set_open(Some(pr(1)), cx);
        let runs = runs.clone();
        // A read of three requests, a second each.
        poller.read_cached(
            pr(1),
            move |client, _| {
                runs.fetch_add(1, Ordering::SeqCst);
                async move {
                    for _ in 0..3 {
                        client.get::<serde_json::Value>(&pr(1).rest_path()).await?;
                    }
                    Ok(Counted(1))
                }
            },
            cx,
        );
    });
    // The open PR's poll and the repository's, with its rulesets, take the first six seconds;
    // the read starts at 6.
    t.run_until(7);
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    let write = t.update(post_comment);
    t.run_until(13);
    write.await.unwrap();

    let requests: Vec<(u64, String)> = (t.requests().into_iter())
        .filter(|(secs, _)| (6..=9).contains(secs))
        .collect();
    assert_eq!(
        requests,
        log(&[
            (6, "GET /pulls/1"),
            (7, "GET /pulls/1"),
            (8, "POST /issues/1/comments"),
            (9, "GET /pulls/1"),
        ])
    );
}

#[gpui::test]
async fn the_poller_notes_every_head_it_sees(cx: &mut TestAppContext) {
    let r = recorded();
    let pushed = "1111111111111111111111111111111111111111";
    let moved = updated(asked_since(r[3].clone(), &last_modified(&r[3])), |body| {
        body["head"]["sha"] = pushed.into();
    });
    let mut interactions = r.clone();
    // Ahead of the recorded 304, so it's the answer to the second poll.
    interactions.insert(8, moved);
    let mut t = Polling::new("seen_heads", interactions, cx).await;
    t.update(|poller, cx| {
        poller.watch(pr(2), cx);
        poller.set_open(Some(pr(1)), cx);
    });
    t.run();
    let marks = t.read(|poller| poller.revision_marks().clone());
    let summary = |n| t.read(|poller| poller.summary(&pr(n)).unwrap());
    let seen = |n| -> Vec<(String, SystemTime)> {
        marks
            .seen_heads(&pr(n))
            .unwrap()
            .into_iter()
            .map(|seen| (seen.head, seen.first_seen))
            .collect()
    };
    // The open PR's head from its summary, check and details, which agree; the other PR's from
    // its summary.
    let head_1 = summary(1).head.sha.clone();
    assert_eq!(seen(1), [(head_1.clone(), wall(0))]);
    let head_2 = summary(2).head.sha.clone();
    assert_eq!(seen(2), [(head_2.clone(), wall(0))]);

    // A push to PR #2, seen at its next poll.
    t.run_until(120);
    assert_eq!(summary(2).head.sha, pushed);
    assert_eq!(
        seen(2),
        [(head_2, wall(0)), (pushed.to_string(), wall(120))]
    );
    assert_eq!(seen(1), [(head_1, wall(0))]);
}

/// `stacks_playground`'s recorded StackLayers answer, for PR #1 and #2, and the IDs it asked for.
fn stack_layers_answer() -> (Interaction, Vec<String>) {
    let (_, interactions) =
        cassette::read_cassette(&cassette::fixtures_dir().join("stacks_playground")).unwrap();
    let (_, answer) = (interactions.into_iter())
        .find(|(_, i)| i.request.operation.as_deref() == Some("StackLayers"))
        .unwrap();
    let ids =
        serde_json::from_value(answer.request.variables.as_ref().unwrap()["ids"].clone()).unwrap();
    (answer, ids)
}

fn layer_numbers(layers: &StackLayerList) -> Vec<i64> {
    layers.layers.iter().map(|layer| layer.number).collect()
}

#[gpui::test]
async fn stack_layers_are_read_after_the_open_prs_poll_and_at_once_for_other_layers(
    cx: &mut TestAppContext,
) {
    let r = recorded();
    let (both, ids) = stack_layers_answer();
    // The same answer, about PR #2 alone.
    let mut top = with_body(both.clone(), |body| {
        let nodes = body["data"]["nodes"].as_array_mut().unwrap();
        nodes.remove(0);
    });
    top.request.variables.as_mut().unwrap()["ids"] = serde_json::json!([ids[1]]);
    let mut interactions = vec![r[0].clone(), r[6].clone(), r[1].clone(), r[2].clone()];
    interactions.extend([both, top]);
    interactions.extend(repo_answers(&r));
    let mut t = Polling::new("stack_layers", interactions, cx).await;
    let at_once = t.update(|poller, cx| {
        poller.set_open(Some(pr(1)), cx);
        poller.stack_layers(pr(1), vec![ids[1].clone()], cx)
    });
    assert_eq!(at_once, None);
    t.run();
    let stack_layers = |requests: Vec<(u64, String)>| -> Vec<u64> {
        (requests.into_iter())
            .filter(|(_, what)| what == "StackLayers")
            .map(|(secs, _)| secs)
            .collect()
    };
    assert_eq!(stack_layers(t.requests()), [0]);
    assert!(t.events().contains(&"read StackLayers #1".to_string()));
    let read = |t: &Polling| t.read(|poller| poller.cached_read::<StackLayerList>(&pr(1)));
    assert_eq!(layer_numbers(&read(&t).unwrap().value), [2]);

    // Asking about the same layer again reads nothing new, while the open PR's check doesn't
    // change.
    let held = t.update(|poller, cx| poller.stack_layers(pr(1), vec![ids[1].clone()], cx));
    assert_eq!(layer_numbers(&held.unwrap().value), [2]);
    t.run_until(60);
    assert_eq!(stack_layers(t.requests()), Vec::<u64>::new());

    // Asking about a layer it doesn't read gives what it holds, and reads both at once.
    let held = t.update(|poller, cx| poller.stack_layers(pr(1), ids.clone(), cx));
    assert_eq!(held.unwrap().value.ids, ids[1..]);
    assert_eq!(stack_layers(t.requests()), [60]);
    let layers = read(&t).unwrap().value;
    assert_eq!(
        (layers.ids.as_slice(), layer_numbers(&layers)),
        (&ids[..], vec![1, 2])
    );

    // Another tree asking about one of them, or both in another order, shares that read.
    t.update(|poller, cx| poller.stack_layers(pr(1), vec![ids[1].clone()], cx));
    let reversed = ids.iter().rev().cloned().collect();
    t.update(|poller, cx| poller.stack_layers(pr(1), reversed, cx));
    t.run_until(120);
    assert_eq!(stack_layers(t.requests()), Vec::<u64>::new());
}

/// When each `StackLayers` request in `requests` was sent.
fn stack_layer_reads(requests: Vec<(u64, String)>) -> Vec<u64> {
    (requests.into_iter())
        .filter(|(_, what)| what == "StackLayers")
        .map(|(secs, _)| secs)
        .collect()
}

/// The recorded `StackLayers` answer about PR #2 alone, and the one about both, with their IDs.
fn top_and_both() -> (Interaction, Interaction, Vec<String>) {
    let (both, ids) = stack_layers_answer();
    let mut top = with_body(both.clone(), |body| {
        body["data"]["nodes"].as_array_mut().unwrap().remove(0);
    });
    top.request.variables.as_mut().unwrap()["ids"] = serde_json::json!([ids[1]]);
    (top, both, ids)
}

#[gpui::test]
async fn asking_about_other_layers_while_they_are_read_reads_them_again(cx: &mut TestAppContext) {
    let r = recorded();
    let (top, both, ids) = top_and_both();
    let mut interactions = r.clone();
    interactions.extend([both, top]);
    let mut t = Polling::with(
        "stack_layers_in_flight",
        interactions,
        Some(Duration::from_secs(1)),
        AccountSource::DeviceFlow,
        cx,
    )
    .await;
    t.update(|poller, cx| {
        poller.set_open(Some(pr(1)), cx);
        poller.stack_layers(pr(1), vec![ids[1].clone()], cx);
    });
    // The open PR's poll and the repository's, with its rulesets, take the first six seconds;
    // PR #2's layer is read from 6 to 7.
    t.run_until(6);
    assert_eq!(stack_layer_reads(t.requests()), [6]);
    t.update(|poller, cx| {
        poller.stack_layers(pr(1), ids.clone(), cx);
    });
    t.run_until(10);
    // The answer about PR #2 alone isn't taken as the answer about both.
    assert_eq!(stack_layer_reads(t.requests()), [7]);
    let layers = t
        .read(|poller| poller.cached_read::<StackLayerList>(&pr(1)))
        .unwrap()
        .value;
    assert_eq!(layers.ids, ids);
    t.run_until(60);
    assert_eq!(stack_layer_reads(t.requests()), Vec::<u64>::new());
}

/// A comment on PR `number`, as the recorded answer to `r`'s first request says.
fn comment_on(r: &[Interaction], number: i64) -> Interaction {
    let mut interaction = comment(r);
    interaction.request.url =
        format!("https://api.github.com/repos/rozed-dev/playground/issues/{number}/comments");
    interaction
}

fn post_comment_on(
    number: i64,
) -> impl FnOnce(
    &mut Poller,
    &mut gpui::Context<Poller>,
) -> gpui::Task<Result<serde_json::Value, github::GithubError>> {
    move |poller, cx| {
        poller.write(
            pr(number),
            move |client| async move {
                let path = format!("/repos/rozed-dev/playground/issues/{number}/comments");
                (client.post::<serde_json::Value, _>(&path, &serde_json::json!({"body": "hi"})))
                    .await
            },
            cx,
        )
    }
}

#[gpui::test]
async fn writes_to_another_layer_read_the_layers_again_once_a_burst_is_over(
    cx: &mut TestAppContext,
) {
    let r = recorded();
    let (both, ids) = stack_layers_answer();
    let mut interactions = r.clone();
    interactions.extend([both, comment(&r), comment_on(&r, 2)]);
    let mut t = Polling::new("stack_layers_written", interactions, cx).await;
    t.update(|poller, cx| {
        poller.set_open(Some(pr(1)), cx);
        poller.stack_layers(pr(1), ids.clone(), cx);
    });
    t.run_until(30);
    assert_eq!(stack_layer_reads(t.requests()), [0]);

    // Writes to the open PR, such as viewed marks, aren't read back: its tab holds them.
    let write = t.update(post_comment);
    t.run_until(40);
    write.await.unwrap();
    assert_eq!(stack_layer_reads(t.requests()), Vec::<u64>::new());

    // Three comments on PR #2, the layer above, a second apart: one read, two seconds after the
    // last.
    for _ in 0..3 {
        let write = t.update(post_comment_on(2));
        t.run_until(t.now() + 1);
        write.await.unwrap();
    }
    t.run_until(50);
    assert_eq!(stack_layer_reads(t.requests()), [44]);
}

#[gpui::test]
async fn each_lookup_takes_a_turn_in_the_queue(cx: &mut TestAppContext) {
    // PR #1 targets `b`, which merged into `a`, which merged into `main`: two lookups.
    let r = recorded();
    let open = with_body(r[5].clone(), |body| {
        let nodes = &mut body["data"]["repository"]["pullRequests"]["nodes"];
        nodes[1]["baseRefName"] = "b".into();
    });
    let merged = |base: &str, head: &str, number: i64| {
        serde_json::json!({
            "number": number, "createdAt": "2026-11-06T07:00:00Z", "baseRefName": base,
            "baseRef": null, "headRefName": head, "headRefOid": format!("{number:040}"),
            "isCrossRepository": false, "commits": {"nodes": []},
            "mergeCommit": null, "stackEntry": null,
        })
    };
    let looked_up = |branch: &str, found: serde_json::Value| {
        let mut variables =
            serde_json::json!({"owner": "rozed-dev", "name": "playground", "first": 5});
        let mut repository = serde_json::json!({});
        for index in 0..10 {
            variables[format!("b{index}")] = branch.into();
            let nodes = if index == 0 {
                vec![found.clone()]
            } else {
                vec![]
            };
            repository[format!("b{index}")] = serde_json::json!({ "nodes": nodes });
        }
        let mut answer = r[5].clone();
        answer.request.operation = Some("MergedPullRequests".into());
        answer.request.variables = Some(variables);
        answer.request.query_sha256 = None;
        answer.response.body = Some(serde_json::json!({"data": {
            "rateLimit": null, "repository": repository,
        }}));
        answer
    };
    let mut interactions = r.clone();
    interactions[5] = open;
    interactions.extend([
        looked_up("b", merged("a", "b", 902)),
        looked_up("a", merged("main", "a", 901)),
        comment(&r),
    ]);
    let mut t = Polling::with(
        "lookups_yield",
        interactions,
        Some(Duration::from_secs(1)),
        AccountSource::DeviceFlow,
        cx,
    )
    .await;
    t.update(|poller, cx| poller.set_open(Some(pr(1)), cx));
    // The open PR's poll takes 0–3 s, the repository's 3–5 s, its rulesets 5–6 s, and the first
    // lookup 6–7 s. A comment sent then goes before the second lookup.
    t.run_until(6);
    let write = t.update(post_comment);
    t.run_until(12);
    write.await.unwrap();
    let requests: Vec<String> = (t.requests().into_iter())
        .map(|(_, what)| what)
        .filter(|what| what == "MergedPullRequests" || what.starts_with("POST"))
        .collect();
    assert_eq!(
        requests,
        [
            "MergedPullRequests",
            "POST /issues/1/comments",
            "MergedPullRequests"
        ]
    );
    let list = t
        .read(|poller| poller.open_pull_requests(&playground()))
        .unwrap();
    let merged: Vec<u64> = list.value.merged.iter().map(|pr| pr.number).collect();
    assert_eq!(merged, [902, 901]);
}

#[gpui::test]
async fn rulesets_are_read_with_the_repository_and_again_every_half_hour(cx: &mut TestAppContext) {
    let mut t = Polling::new("trunk_rules", recorded(), cx).await;
    assert_eq!(t.read(|poller| poller.trunk_rules(&playground())), None);
    t.update(|poller, cx| poller.set_open(Some(pr(1)), cx));
    let reads = |t: &Polling| -> Vec<u64> {
        (t.requests().into_iter())
            .filter(|(_, what)| what == "TrunkRules")
            .map(|(secs, _)| secs)
            .collect()
    };
    assert_eq!(reads(&t), [0]);
    assert!(
        t.events()
            .contains(&"rulesets of rozed-dev/playground".to_string())
    );
    let rules = t.read(|poller| poller.trunk_rules(&playground())).unwrap();
    assert_eq!(rules.value.unavailable, None);
    // Every PR in the repository has them, not only the open one.
    t.update(|poller, cx| poller.watch(pr(2), cx));
    assert!(t.read(|poller| poller.trunk_rules(&playground())).is_some());
    t.run_until(1799);
    assert_eq!(reads(&t), Vec::<u64>::new());
    t.run_until(1920);
    assert_eq!(reads(&t), [1800]);

    // After a restart, they're there at once.
    t.restart();
    t.update(|poller, cx| poller.watch(pr(1), cx));
    assert!(t.read(|poller| poller.trunk_rules(&playground())).is_some());
}

#[gpui::test]
async fn lookups_that_stop_short_run_again_while_the_list_stays_the_same(cx: &mut TestAppContext) {
    // PR #1 targets `gone`, and GitHub fails to look it up at first.
    let r = recorded();
    let open = with_body(r[5].clone(), |body| {
        let nodes = &mut body["data"]["repository"]["pullRequests"]["nodes"];
        nodes[1]["baseRefName"] = "gone".into();
    });
    let looked_up = |status: u16| {
        let mut variables =
            serde_json::json!({"owner": "rozed-dev", "name": "playground", "first": 5});
        let mut repository = serde_json::json!({});
        for index in 0..10 {
            variables[format!("b{index}")] = "gone".into();
            repository[format!("b{index}")] = serde_json::json!({ "nodes": [] });
        }
        let mut answer = r[5].clone();
        answer.request.operation = Some("MergedPullRequests".into());
        answer.request.variables = Some(variables);
        answer.request.query_sha256 = None;
        answer.response.status = status;
        answer.response.body = Some(serde_json::json!({"data": {
            "rateLimit": null, "repository": repository,
        }}));
        answer
    };
    let mut interactions = r.clone();
    interactions[5] = open;
    interactions.extend([looked_up(502), looked_up(200)]);
    let mut t = Polling::new("lookups_retried", interactions, cx).await;
    t.update(|poller, cx| poller.set_open(Some(pr(1)), cx));
    t.run();
    let list = t.read(|poller| poller.open_pull_requests(&playground()));
    assert!(list.unwrap().value.merged_truncated);
    let lookups = |t: &Polling| -> Vec<u64> {
        (t.requests().into_iter())
            .filter(|(_, what)| what == "MergedPullRequests")
            .map(|(secs, _)| secs)
            .collect()
    };
    assert_eq!(lookups(&t), [0]);
    // The PRs don't change, but the lookups run again at the repository's next poll.
    t.run_until(130);
    assert_eq!(lookups(&t), [120]);
    let list = t.read(|poller| poller.open_pull_requests(&playground()));
    let list = list.unwrap().value;
    assert!(!list.merged_truncated);
    assert_eq!(list.lookups[0].branch, "gone");
    t.run_until(400);
    assert_eq!(lookups(&t), Vec::<u64>::new());
}

// Comments and reviews (`github::review_writes`): what each write reads again, with the review
// threads, your pending review and the timeline kept by made-up reads that count their runs.

/// How often each of PR #1's review reads ran, and, for the timeline, whether each run was given
/// what Lo had.
#[derive(Clone, Default)]
struct ReviewRuns {
    threads: Arc<std::sync::atomic::AtomicU32>,
    pending: Arc<std::sync::atomic::AtomicU32>,
    timeline: Arc<Mutex<Vec<bool>>>,
}

impl ReviewRuns {
    fn threads(&self) -> u32 {
        self.threads.load(Ordering::SeqCst)
    }

    fn pending(&self) -> u32 {
        self.pending.load(Ordering::SeqCst)
    }

    /// Whether each timeline run since the last call was given what Lo had.
    fn timeline(&self) -> Vec<bool> {
        std::mem::take(&mut *self.timeline.lock())
    }
}

/// PR #1's review threads: `count` of them, `PRRT_0` onwards, each with one posted comment,
/// `PRRC_0` onwards.
fn review_threads(count: usize) -> github::queries::ReviewThreadList {
    pending_review_threads(count, 0)
}

/// [`review_threads`], the first `pending` of them in your pending review.
fn pending_review_threads(count: usize, pending: usize) -> github::queries::ReviewThreadList {
    let nodes: Vec<serde_json::Value> = (0..count)
        .map(|n| {
            serde_json::json!({
                "id": format!("PRRT_{n}"), "path": "a.rs", "subjectType": "LINE",
                "diffSide": "RIGHT", "startDiffSide": null, "line": 1, "startLine": null,
                "originalLine": 1, "originalStartLine": null, "isOutdated": false,
                "isResolved": false, "viewerCanReply": true, "viewerCanResolve": true,
                "viewerCanUnresolve": false,
                "comments": {
                    "totalCount": 1,
                    "pageInfo": {"hasNextPage": false, "endCursor": null},
                    "nodes": [{
                        "id": format!("PRRC_{n}"),
                        "author": {"__typename": "User", "login": LOGIN},
                        "body": "Hm.", "state": if n < pending { "PENDING" } else { "SUBMITTED" },
                        "createdAt": "2026-11-06T12:00:00Z",
                        "url": "https://github.com/rozed-dev/playground/pull/1",
                        "isMinimized": false, "minimizedReason": null, "viewerCanUpdate": true,
                        "viewerCanDelete": true, "commit": {"oid": "a"},
                        "originalCommit": {"oid": "a"}
                    }]
                }
            })
        })
        .collect();
    github::queries::ReviewThreadList {
        nodes: serde_json::from_value(nodes.into()).unwrap(),
        missing: 0,
    }
}

/// Keeps PR #1's review threads, holding `threads`, your pending review and its timeline, with
/// made-up requests that count their runs in `runs`.
fn keep_review_reads(
    poller: &mut Poller,
    runs: &ReviewRuns,
    threads: github::queries::ReviewThreadList,
    cx: &mut gpui::Context<Poller>,
) {
    use github::queries::timeline::Description;
    use github::queries::{PullRequestTimeline, ViewerPendingReview};

    let counted = runs.threads.clone();
    poller.read_cached(
        pr(1),
        move |_, _| {
            counted.fetch_add(1, Ordering::SeqCst);
            let threads = threads.clone();
            async move { Ok(threads) }
        },
        cx,
    );
    let counted = runs.pending.clone();
    poller.read_cached(
        pr(1),
        move |_, _| {
            counted.fetch_add(1, Ordering::SeqCst);
            async move { Ok(ViewerPendingReview::default()) }
        },
        cx,
    );
    let given = runs.timeline.clone();
    poller.read_cached(
        pr(1),
        move |_, known: Option<Arc<PullRequestTimeline>>| {
            given.lock().push(known.is_some());
            async move {
                Ok(PullRequestTimeline {
                    description: Description {
                        author: None,
                        body: String::new(),
                        created_at: chrono::DateTime::UNIX_EPOCH,
                        last_edited_at: None,
                    },
                    items: Vec::new(),
                    left_out: 0,
                    count: 0,
                    shown_updated_at: None,
                    read_in_full_at: chrono::DateTime::UNIX_EPOCH,
                })
            }
        },
        cx,
    );
}

/// A mutation `operation` with `variables`, answered with `answer`, or the errors in it, for the
/// GraphQL request `like` made.
fn mutation(
    like: &Interaction,
    operation: &str,
    variables: serde_json::Value,
    answer: serde_json::Value,
) -> Interaction {
    let mut interaction = like.clone();
    interaction.request.operation = Some(operation.into());
    interaction.request.variables = Some(variables);
    interaction.request.query_sha256 = None;
    interaction.response.status = 200;
    interaction.response.headers.clear();
    interaction.response.body = Some(answer);
    interaction
}

/// The recorded poll of PR #1 with its review reads kept, run until they've each run once.
async fn reviewing(
    name: &str,
    answers: Vec<Interaction>,
    threads: github::queries::ReviewThreadList,
    cx: &mut TestAppContext,
) -> (Polling, ReviewRuns) {
    let mut interactions = recorded();
    interactions.extend(answers);
    let mut t = Polling::new(name, interactions, cx).await;
    let runs = ReviewRuns::default();
    t.update(|poller, cx| {
        poller.set_open(Some(pr(1)), cx);
        keep_review_reads(poller, &runs, threads, cx);
    });
    t.run_until(10);
    assert_eq!((runs.threads(), runs.pending()), (1, 1));
    assert_eq!(runs.timeline(), [false]);
    (t, runs)
}

#[gpui::test]
async fn an_edit_reads_the_threads_again_or_the_whole_timeline(cx: &mut TestAppContext) {
    use github::review_writes::CommentId;

    let r = recorded();
    let edited = |operation: &str, id: &str, field: &str| {
        let answer = serde_json::json!({"data": {
            operation[..1].to_lowercase() + &operation[1..]: {field: {"id": id}}
        }});
        mutation(
            &r[1],
            operation,
            serde_json::json!({"id": id, "body": "Edited."}),
            answer,
        )
    };
    let answers = vec![
        edited(
            "UpdatePullRequestReviewComment",
            "PRRC_0",
            "pullRequestReviewComment",
        ),
        edited("UpdateIssueComment", "IC_1", "issueComment"),
    ];
    let (mut t, runs) = reviewing("edits_read_again", answers, review_threads(1), cx).await;

    // The check sees no edit of a comment, so an edit in a thread reads the threads again.
    let thread = CommentId::Thread("PRRC_0".into());
    let edit = t.update(|poller, cx| poller.edit_comment(pr(1), thread, "Edited.".into(), cx));
    t.run_until(20);
    edit.await.unwrap();
    assert_eq!((runs.threads(), runs.pending()), (2, 1));
    assert_eq!(runs.timeline(), Vec::<bool>::new());

    // One on the pull request itself reads the timeline again, in full: not given what Lo has,
    // since the comment may be older than its newest page.
    let comment = CommentId::Conversation("IC_1".into());
    let edit = t.update(|poller, cx| poller.edit_comment(pr(1), comment, "Edited.".into(), cx));
    t.run_until(30);
    edit.await.unwrap();
    assert_eq!(runs.threads(), 2);
    assert_eq!(runs.timeline(), [false]);
    // Its next refresh reads only what's new again.
    t.run_until(340);
    assert_eq!(runs.timeline(), [true]);
}

#[gpui::test]
async fn a_write_to_a_thread_the_check_doesnt_read_reads_the_threads_again(
    cx: &mut TestAppContext,
) {
    use github::review_writes::CommentId;
    use review::pending::Mutation;

    let r = recorded();
    let resolved = |thread: &str| {
        mutation(
            &r[1],
            "ResolveReviewThread",
            serde_json::json!({"threadId": thread}),
            serde_json::json!({"data": {"resolveReviewThread": {
                "thread": {"id": thread, "isResolved": true}
            }}}),
        )
    };
    let answers = vec![
        resolved("PRRT_0"),
        resolved("PRRT_100"),
        mutation(
            &r[1],
            "DeletePullRequestReviewComment",
            serde_json::json!({"id": "PRRC_100"}),
            serde_json::json!({"data": {"deletePullRequestReviewComment": {
                "pullRequestReview": {"id": "PRR_1"}
            }}}),
        ),
        mutation(
            &r[1],
            "AddPullRequestReviewThreadReply",
            serde_json::json!({"pullRequestReviewThreadId": "PRRT_100", "body": "Agreed."}),
            serde_json::json!({"data": {"addPullRequestReviewThreadReply": {"comment": {
                "id": "PRRC_200", "state": "SUBMITTED",
                "pullRequestReview": {"id": "PRR_2", "state": "COMMENTED"}
            }}}}),
        ),
    ];
    let (mut t, runs) = reviewing("threads_past_the_check", answers, review_threads(101), cx).await;

    // The check reads the first 100 threads' state, so resolving one of them is left to it,
    // which this replay doesn't change.
    let resolve = t.update(|poller, cx| poller.resolve_thread(pr(1), "PRRT_0".into(), true, cx));
    t.run_until(20);
    resolve.await.unwrap();
    assert_eq!(runs.threads(), 1);
    // Past them, the threads are read again.
    let resolve = t.update(|poller, cx| poller.resolve_thread(pr(1), "PRRT_100".into(), true, cx));
    t.run_until(30);
    resolve.await.unwrap();
    assert_eq!(runs.threads(), 2);
    // So are they after deleting a comment there, with the timeline in full: it counts the
    // comments of the comment's review.
    let comment = CommentId::Thread("PRRC_100".into());
    let delete = t.update(|poller, cx| poller.delete_comment(pr(1), comment, cx));
    t.run_until(40);
    delete.await.unwrap();
    assert_eq!(runs.threads(), 3);
    assert_eq!(runs.timeline(), [false]);
    // And after a reply posted there at once.
    let reply = Mutation::PostReply {
        thread: "PRRT_100".into(),
        body: "Agreed.".into(),
    };
    let sent = t.update(|poller, cx| poller.send_review(pr(1), "PR_1".into(), reply, cx));
    t.run_until(50);
    assert_eq!(sent.await.unwrap().comment.as_deref(), Some("PRRC_200"));
    assert_eq!(runs.threads(), 4);
    assert_eq!(runs.pending(), 1);
}

#[gpui::test]
async fn a_refused_second_pending_review_reads_your_pending_review_again(cx: &mut TestAppContext) {
    use github::review_writes::Rejection;
    use review::pending::Mutation;

    let r = recorded();
    let refused = mutation(
        &r[1],
        "AddPullRequestReview",
        serde_json::json!({"pullRequestId": "PR_1", "commitOID": "a3"}),
        serde_json::json!({
            "data": {"addPullRequestReview": null},
            "errors": [{
                "type": "UNPROCESSABLE", "path": ["addPullRequestReview"],
                "message": "User can only have one pending review per pull request"
            }]
        }),
    );
    let (mut t, runs) = reviewing("pending_elsewhere", vec![refused], review_threads(1), cx).await;

    // Lo planned a new review: one was started elsewhere since it last read yours.
    let start = Mutation::StartReview {
        commit: "a3".into(),
        thread: None,
    };
    let sent = t.update(|poller, cx| poller.send_review(pr(1), "PR_1".into(), start, cx));
    t.run_until(20);
    assert!(matches!(
        sent.await,
        Err(GithubError::Rejected(Rejection::PendingReviewExists))
    ));
    assert_eq!((runs.threads(), runs.pending()), (2, 2));
    assert_eq!(runs.timeline(), Vec::<bool>::new());
}

#[gpui::test]
async fn a_reply_that_joined_a_pending_review_lo_didnt_know_reads_it_again(
    cx: &mut TestAppContext,
) {
    use review::pending::Mutation;

    let r = recorded();
    let joined = mutation(
        &r[1],
        "AddPullRequestReviewThreadReply",
        serde_json::json!({"pullRequestReviewThreadId": "PRRT_0", "body": "Agreed."}),
        serde_json::json!({"data": {"addPullRequestReviewThreadReply": {"comment": {
            "id": "PRRC_9", "state": "PENDING",
            "pullRequestReview": {"id": "PRR_elsewhere", "state": "PENDING"}
        }}}}),
    );
    let (mut t, runs) = reviewing("joined_pending", vec![joined], review_threads(1), cx).await;

    // Sent to be posted at once, it went into a pending review started elsewhere.
    let reply = Mutation::PostReply {
        thread: "PRRT_0".into(),
        body: "Agreed.".into(),
    };
    let sent = t.update(|poller, cx| poller.send_review(pr(1), "PR_1".into(), reply, cx));
    t.run_until(20);
    let sent = sent.await.unwrap();
    assert_eq!(sent.joined_pending.as_deref(), Some("PRR_elsewhere"));
    assert_eq!((runs.threads(), runs.pending()), (2, 2));
    assert_eq!(runs.timeline(), Vec::<bool>::new());
}

#[gpui::test]
async fn a_write_refused_because_its_review_changed_elsewhere_reads_it_again(
    cx: &mut TestAppContext,
) {
    use github::review_writes::Rejection;
    use review::pending::{Mutation, NewThread};
    use review::threads::{Line, Lines};

    let r = recorded();
    let thread = |line: u32| NewThread {
        path: "a.rs".into(),
        lines: Some(Lines::one(Line::right(line))),
        body: "Hm.".into(),
    };
    let added = |line: u32| {
        serde_json::json!({
            "pullRequestReviewId": "PRR_1", "path": "a.rs", "body": "Hm.",
            "subjectType": "LINE", "line": line, "side": "RIGHT"
        })
    };
    let answers = vec![
        // Deleted elsewhere: "not found".
        mutation(
            &r[1],
            "AddPullRequestReviewThread",
            added(1),
            serde_json::json!({
                "data": {"addPullRequestReviewThread": null},
                "errors": [{
                    "type": "NOT_FOUND", "path": ["addPullRequestReviewThread"],
                    "message": "Could not resolve to a node with the global id of 'PRR_1'."
                }]
            }),
        ),
        // A line outside the diff, in a review still pending.
        mutation(
            &r[1],
            "AddPullRequestReviewThread",
            added(9),
            serde_json::json!({"data": {"addPullRequestReviewThread": {"thread": null}}}),
        ),
        mutation(
            &r[1],
            "ReviewState",
            serde_json::json!({"id": "PRR_1"}),
            serde_json::json!({"data": {"node": {
                "__typename": "PullRequestReview", "state": "PENDING"
            }}}),
        ),
    ];
    let (mut t, runs) = reviewing("review_changed", answers, review_threads(1), cx).await;

    let add = |line| Mutation::AddThread {
        review: "PRR_1".into(),
        thread: thread(line),
    };
    let sent = t.update(|poller, cx| poller.send_review(pr(1), "PR_1".into(), add(1), cx));
    t.run_until(20);
    assert!(matches!(
        sent.await,
        Err(GithubError::Rejected(Rejection::Gone))
    ));
    assert_eq!((runs.threads(), runs.pending()), (2, 2));
    // Refused as outside the diff, it may still have started a review elsewhere: read again.
    let sent = t.update(|poller, cx| poller.send_review(pr(1), "PR_1".into(), add(9), cx));
    t.run_until(30);
    assert!(matches!(
        sent.await,
        Err(GithubError::Rejected(Rejection::NotInDiff))
    ));
    assert_eq!((runs.threads(), runs.pending()), (3, 3));
    assert_eq!(runs.timeline(), Vec::<bool>::new());
}

#[gpui::test]
async fn deleting_a_pending_comment_leaves_the_timeline_alone(cx: &mut TestAppContext) {
    use github::review_writes::CommentId;

    let r = recorded();
    let deleted = |id: &str| {
        mutation(
            &r[1],
            "DeletePullRequestReviewComment",
            serde_json::json!({"id": id}),
            serde_json::json!({"data": {"deletePullRequestReviewComment": {
                "pullRequestReview": {"id": "PRR_1"}
            }}}),
        )
    };
    let answers = vec![deleted("PRRC_0"), deleted("PRRC_1")];
    let threads = pending_review_threads(2, 1);
    let (mut t, runs) = reviewing("delete_pending", answers, threads, cx).await;

    // Your pending review isn't in the timeline: the check's count of its comments reads it again.
    let pending = CommentId::Thread("PRRC_0".into());
    let delete = t.update(|poller, cx| poller.delete_comment(pr(1), pending, cx));
    t.run_until(20);
    delete.await.unwrap();
    assert_eq!(runs.timeline(), Vec::<bool>::new());
    assert_eq!(runs.threads(), 1);
    // A posted one's review is, and may be past the newest page.
    let posted = CommentId::Thread("PRRC_1".into());
    let delete = t.update(|poller, cx| poller.delete_comment(pr(1), posted, cx));
    t.run_until(30);
    delete.await.unwrap();
    assert_eq!(runs.timeline(), [false]);
}
