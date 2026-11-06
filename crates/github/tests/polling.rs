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
use github::db::{AccountSource, RozedDb};
use github::poll::{
    LowLimit, Offline, PauseReason, PollConfig, PollEvent, PollStatus, Poller, RateLimitState,
    WallClock,
};
use github::queries::{PullRequestData, pull_request};
use github::replay::ReplayHttpClient;
use github::{AuthenticatedUser, PullRef, RepoRef, Target, Token, User};
use gpui::{AppContext as _, BackgroundExecutor, Entity, TestAppContext};
use http_client::http::HeaderValue;
use http_client::{AsyncBody, HttpClient, Request, Response, Url};
use parking_lot::Mutex;

/// The wall clock when each test starts: after every recorded rate-limit window has reset, so
/// only the windows a test sets count.
const START: u64 = 1_794_000_000;
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
    db: RozedDb,
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
        let db = RozedDb::open_test(name).await;
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

    /// Replaces the accounts and the poller, as when rozed starts again: no token has been read.
    fn restart_rozed(&mut self) {
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
                PollEvent::Failed { target, error } => format!("failed {target:?}: {error}"),
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
fn recorded() -> Vec<Interaction> {
    let (_, interactions) =
        cassette::read_cassette(&cassette::fixtures_dir().join("poll_playground")).unwrap();
    interactions.into_iter().map(|(_, i)| i).collect()
}

/// The recorded poll's answers for the repository's open PRs.
fn repo_answers(recorded: &[Interaction]) -> Vec<Interaction> {
    vec![
        recorded[4].clone(),
        recorded[5].clone(),
        recorded[9].clone(),
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
        ])
    );
    assert_eq!(
        t.events(),
        [
            "summary #1: feat: stack layer A",
            "check #1",
            "details #1: feat: stack layer A",
            "summary #2: feat: stack layer B",
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
    assert_eq!(seen[6].header("if-modified-since"), Some(since.as_str()));
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
        .filter(|(_, what)| !what.contains("/pulls?") && what != "OpenPullRequests")
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
        pull["reviewThreads"] =
            serde_json::json!({"totalCount": 1, "nodes": [{"isResolved": false}]});
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
            // No retry-after: a minute, doubling.
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
        ])
    );
    assert_eq!(t.status(), PollStatus::default());
    let answer = t.read(|poller| poller.pull_request(&pr(1))).unwrap();
    assert_eq!((answer.fetched_at, answer.offline_age), (wall(1200), None));
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

    // The cache forgets the check too, so a restart fetches the details in full.
    let write = t.update(post_comment);
    t.run();
    write.await.unwrap();
    t.requests();
    t.restart();
    t.update(|poller, cx| poller.set_open(Some(pr(1)), cx));
    let now = t.now();
    assert_eq!(
        t.requests()
            .into_iter()
            .filter(|(_, what)| !what.contains("/pulls?"))
            .collect::<Vec<_>>(),
        log(&[
            (now, "GET /pulls/1 (since)"),
            (now, "PullRequestCheck"),
            (now, "PullRequest"),
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
    // The Keychain is locked when rozed starts, or its prompt was dismissed.
    t.store.state.lock().fail_reads = true;
    t.restart_rozed();
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
