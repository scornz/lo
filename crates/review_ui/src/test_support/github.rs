//! The fake GitHub's API, which answers per token, and signing in to it. Its REST and GraphQL
//! requests go through a list of handlers; a feature's tests add theirs in a file of their own
//! under `github/`.

mod viewed;

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures::channel::oneshot;
use futures::future::BoxFuture;
use github::accounts::Accounts;
use github::cache::PrCache;
use github::db::{AccountSource, LoDb};
use github::{AuthenticatedUser, Token, User};
use gpui::{Entity, Task, TestAppContext};
use http_client::http::HeaderValue;
use http_client::{AsyncBody, HttpClient, Request, Response, Url};
use parking_lot::Mutex;

pub use viewed::WriteAnswer;

/// A pull request the fake GitHub has.
#[derive(Clone, Debug)]
pub struct FakePull {
    /// The repository's `owner/name` now.
    pub repo: String,
    /// Names the repository had before a rename, which still find it.
    pub old_names: Vec<String>,
    pub number: u64,
    pub title: String,
    pub head: String,
    pub base_branch: String,
    pub base: String,
    /// GitHub's list of the files the pull request changes, each with its patch.
    pub files: Vec<serde_json::Value>,
    /// How many files GitHub says the pull request changes, when it's more than it lists.
    pub changed_files: Option<usize>,
}

/// An answer to one request: its status, headers and body.
#[derive(Clone, Debug)]
pub struct Answer {
    pub status: u16,
    pub headers: Vec<(&'static str, String)>,
    pub body: String,
}

impl Answer {
    pub fn new(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.into(),
        }
    }

    pub fn header(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.headers.push((name, value.into()));
        self
    }
}

#[derive(Default)]
struct State {
    pulls: Vec<FakePull>,
    /// The repositories each token can see, lowercased `owner/name`. Public ones need no token.
    sees: HashMap<String, Vec<String>>,
    public: Vec<String>,
    /// Answers that win over the rest, by token (`""` for none) and route: a path with its query,
    /// or `graphql:<Operation>`. `/graphql` answers every GraphQL request.
    answers: HashMap<(String, String), Answer>,
    offline: bool,
    /// Whether every request connects and then times out, as on a slow connection.
    timing_out: bool,
    /// Whether a proxy refuses every request, wanting a password.
    proxy_refusing: bool,
    /// Tokens whose requests can't connect, though others' can.
    cut_off: Vec<String>,
    /// Requests to these routes wait until they're let go, each with what lets it go: a path
    /// without its query, where `/graphql` holds every GraphQL request, or `graphql:<Operation>`.
    held: HashMap<String, Vec<oneshot::Sender<()>>>,
    /// How many requests without a token GitHub still allows this hour, when it says.
    anonymous_left: Option<u32>,
    /// What a request no handler answers gets, when not GitHub's 404.
    unhandled: Option<Answer>,
    /// Each feature's own data, by its type ([`State::get`]).
    features: HashMap<TypeId, Box<dyn Any + Send>>,
    /// Each request, as `GET /path as token`, or `POST /graphql Operation as token`.
    requests: Vec<String>,
}

impl State {
    /// A feature's own data, which starts as its `Default`.
    fn get<T: Any + Default + Send>(&mut self) -> &mut T {
        self.features
            .entry(TypeId::of::<T>())
            .or_insert_with(|| Box::new(T::default()))
            .downcast_mut()
            .expect("stored by its type")
    }
}

/// GitHub's REST API as far as opening a pull request needs it.
#[derive(Clone, Default)]
pub struct FakeGithub(Arc<Mutex<State>>);

impl FakeGithub {
    pub fn add_pull(&self, pull: FakePull) {
        self.0.lock().pulls.push(pull);
    }

    /// Lets `token` see `repo`, an `owner/name`.
    pub fn let_see(&self, token: &str, repo: &str) {
        self.0
            .lock()
            .sees
            .entry(token.to_string())
            .or_default()
            .push(repo.to_ascii_lowercase());
    }

    /// Moves pull request `number` of `repo` to `head`, without changing its files' links, as
    /// when a push lands between GitHub's answer for the pull request and its file list.
    pub fn set_head(&self, repo: &str, number: u64, head: &str) {
        let mut state = self.0.lock();
        for pull in &mut state.pulls {
            if pull.repo.eq_ignore_ascii_case(repo) && pull.number == number {
                pull.head = head.to_string();
            }
        }
    }

    /// Says pull request `number` of `repo` changes `count` files, more than GitHub lists.
    pub fn set_changed_files(&self, repo: &str, number: u64, count: usize) {
        let mut state = self.0.lock();
        for pull in &mut state.pulls {
            if pull.repo.eq_ignore_ascii_case(repo) && pull.number == number {
                pull.changed_files = Some(count);
            }
        }
    }

    pub fn make_public(&self, repo: &str) {
        self.0.lock().public.push(repo.to_ascii_lowercase());
    }

    /// Answers `path` asked with `token` (`""` for none) with `answer`. `path` can also be a
    /// GraphQL operation, as `graphql:<Operation>`, or `/graphql` for every one.
    pub fn answer(&self, token: &str, path: &str, answer: Answer) {
        self.0
            .lock()
            .answers
            .insert((token.to_string(), path.to_string()), answer);
    }

    /// Allows `left` more requests without a token this hour, and says how many are left in each
    /// answer to one, as GitHub does.
    pub fn limit_anonymous(&self, left: u32) {
        self.0.lock().anonymous_left = Some(left);
    }

    /// Whether every request fails as if the network were down: no connection can be made.
    pub fn set_offline(&self, offline: bool) {
        self.0.lock().offline = offline;
    }

    /// Whether every request times out after connecting, as on a slow connection.
    pub fn set_timing_out(&self, timing_out: bool) {
        self.0.lock().timing_out = timing_out;
    }

    /// Whether a proxy refuses every request, as one that wants a password does.
    pub fn set_proxy_refusing(&self, refusing: bool) {
        self.0.lock().proxy_refusing = refusing;
    }

    /// Makes `token`'s requests fail to connect, while other tokens' get answers.
    pub fn cut_off(&self, token: &str) {
        self.0.lock().cut_off.push(token.to_string());
    }

    /// Makes pull request `number` of `repo` merge into `branch`, whose tip is `base`, as
    /// changing its base on GitHub does.
    pub fn retarget(&self, repo: &str, number: u64, branch: &str, base: &str) {
        let mut state = self.0.lock();
        let pull = state
            .pulls
            .iter_mut()
            .find(|pull| pull.repo.eq_ignore_ascii_case(repo) && pull.number == number)
            .expect("no such pull request");
        pull.base_branch = branch.to_string();
        pull.base = base.to_string();
    }

    /// Undoes [`Self::cut_off`].
    pub fn reconnect(&self, token: &str) {
        self.0.lock().cut_off.retain(|cut_off| cut_off != token);
    }

    /// Makes requests to `path`, with any token, wait until [`Self::let_go`]. `path` can also be a
    /// GraphQL operation, as `graphql:<Operation>`, or `/graphql` for every one.
    pub fn hold(&self, path: &str) {
        self.0.lock().held.entry(path.to_string()).or_default();
    }

    /// Lets the requests to `path` that [`Self::hold`] held through, and any later ones.
    pub fn let_go(&self, path: &str) {
        let waiting = self.0.lock().held.remove(path).unwrap_or_default();
        for waiter in waiting {
            waiter.send(()).ok();
        }
    }

    /// Stops answering `path` asked with `token` as [`Self::answer`] said.
    pub fn forget_answer(&self, token: &str, path: &str) {
        self.0
            .lock()
            .answers
            .remove(&(token.to_string(), path.to_string()));
    }

    /// The requests made since the last call, as `GET /path as token`, or
    /// `POST /graphql Operation as token`.
    pub fn requests(&self) -> Vec<String> {
        std::mem::take(&mut self.0.lock().requests)
    }

    /// Answers the requests no handler knows with `answer` instead of GitHub's 404.
    pub fn answer_unhandled(&self, answer: Answer) {
        self.0.lock().unhandled = Some(answer);
    }

    /// Runs `f` on a feature's own data ([`State::get`]), which starts as its `Default`. Each
    /// feature's data is found by its type, so a feature stores a type of its own. The data is
    /// taken out while `f` runs, without holding the fake's lock, so `f` can call the fake; a call
    /// from inside it for the same data sees a fresh `T`.
    pub fn with<T: Any + Default + Send, R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        let mut data = std::mem::take(self.0.lock().get::<T>());
        let result = f(&mut data);
        *self.0.lock().get::<T>() = data;
        result
    }

    fn respond(&self, asked: &Asked) -> anyhow::Result<Answer> {
        let mut state = self.0.lock();
        let (path, token) = (asked.target.as_str(), asked.token.as_str());
        let who = if token.is_empty() { "nobody" } else { token };
        let operation = match &asked.graphql {
            Some(graphql) => format!(" {}", graphql.operation),
            None => String::new(),
        };
        state
            .requests
            .push(format!("{} {path}{operation} as {who}", asked.method));
        if state.offline || state.cut_off.iter().any(|cut_off| cut_off == token) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                "the network is down",
            )
            .into());
        }
        if state.proxy_refusing {
            return Err(anyhow::anyhow!("proxy authentication required")
                .context("client error (Connect)")
                .context(format!(
                    "error sending request for url (https://api.github.com{path})"
                )));
        }
        if state.timing_out {
            return Err(anyhow::anyhow!("operation timed out").context(format!(
                "error sending request for url (https://api.github.com{path})"
            )));
        }
        let Some(left) = state.anonymous_left.filter(|_| token.is_empty()) else {
            return answer_for(&mut state, asked);
        };
        let reset = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs()
            + 3_600;
        let answer = match left.checked_sub(1) {
            Some(left) => {
                state.anonymous_left = Some(left);
                answer_for(&mut state, asked)?
            }
            None => Answer::new(403, r#"{"message":"API rate limit exceeded"}"#),
        };
        Ok(answer
            .header("x-ratelimit-limit", "60")
            .header("x-ratelimit-remaining", left.saturating_sub(1).to_string())
            .header("x-ratelimit-reset", reset.to_string())
            .header("x-ratelimit-resource", "core"))
    }
}

/// What the fake GitHub answers `asked`: the answer a test set for it, or else its handler's, or
/// else GitHub's 404.
fn answer_for(state: &mut State, asked: &Asked) -> anyhow::Result<Answer> {
    let routes = [asked.graphql_route(), Some(asked.target.clone())];
    for route in routes.into_iter().flatten() {
        if let Some(answer) = state.answers.get(&(asked.token.clone(), route)) {
            return Ok(answer.clone());
        }
    }
    for handler in HANDLERS {
        if let Some(answer) = handler(state, asked) {
            return answer;
        }
    }
    Ok(state.unhandled.clone().unwrap_or_else(not_found))
}

/// A request to the fake GitHub.
struct Asked {
    method: String,
    /// The path and query as asked.
    target: String,
    /// The path, without its query.
    path: String,
    query: String,
    /// The token it came with, `""` for none.
    token: String,
    /// What it sent, as a REST write or GraphQL does.
    body: Vec<u8>,
    /// The operation and its variables, for a GraphQL request.
    graphql: Option<GraphQl>,
}

/// A GraphQL request's operation and its variables.
struct GraphQl {
    operation: String,
    variables: serde_json::Value,
}

impl Asked {
    /// `request`'s method, path, query, token and body, and for a GraphQL request, the operation
    /// and variables in its body.
    fn read(request: Request<AsyncBody>) -> Self {
        let token = request
            .headers()
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split_once(' ').map(|(_, token)| token.to_string()))
            .unwrap_or_default();
        let target = request
            .uri()
            .path_and_query()
            .map(|path| path.to_string())
            .unwrap_or_default();
        let (path, query) = target.split_once('?').unwrap_or((&target, ""));
        let (path, query) = (path.to_string(), query.to_string());
        let method = request.method().to_string();
        let mut body = Vec::new();
        {
            use futures::AsyncReadExt as _;
            futures::executor::block_on(request.into_body().read_to_end(&mut body)).unwrap();
        }
        let mut asked = Self {
            method,
            target,
            path,
            query,
            token,
            body,
            graphql: None,
        };
        if asked.path == "/graphql" {
            let json = asked.json();
            let operation = json["operationName"]
                .as_str()
                .unwrap_or_else(|| panic!("a GraphQL request without an operation name: {json}"));
            asked.graphql = Some(GraphQl {
                operation: operation.to_string(),
                variables: json["variables"].clone(),
            });
        }
        asked
    }

    /// The body, as JSON, or null when there's none.
    fn json(&self) -> serde_json::Value {
        if self.body.is_empty() {
            return serde_json::Value::Null;
        }
        serde_json::from_slice(&self.body).unwrap_or_else(|error| {
            panic!(
                "{} {}: the body isn't JSON: {error}",
                self.method, self.target
            )
        })
    }

    /// `graphql:<Operation>`, for a GraphQL request.
    fn graphql_route(&self) -> Option<String> {
        let graphql = self.graphql.as_ref()?;
        Some(format!("graphql:{}", graphql.operation))
    }
}

/// Answers the requests it knows, and leaves the rest to the next handler with `None`. It may
/// change `State`, as a write does, and keep a feature's own data there ([`State::get`]).
type Handler = fn(&mut State, &Asked) -> Option<anyhow::Result<Answer>>;

/// The fake GitHub's handlers, tried once no answer a test set fits. A request none of them
/// answers isn't found. They answer different requests, so they're listed in alphabetical order,
/// one per line, and a feature's tests add theirs in a file of their own under `github/`.
const HANDLERS: &[Handler] = &[
    get_pull,                    // GET /repos/{owner}/{name}/pulls/{number}
    get_pull_files,              // GET /repos/{owner}/{name}/pulls/{number}/files
    get_repo,                    // GET /repos/{owner}/{name}
    viewed::mark_file_as_viewed, // GraphQL markFileAsViewed and unmarkFileAsViewed
];

fn not_found() -> Answer {
    Answer::new(404, r#"{"message":"Not Found"}"#)
}

/// The repository a request under `/repos/{owner}/{name}` is for, as GitHub names it now, and the
/// path's segments after its name, when the request's token can see it. Its old names find it too.
fn repository<'a>(state: &State, asked: &'a Asked) -> Option<(String, Vec<&'a str>)> {
    let mut segments = asked.path.strip_prefix("/repos/")?.split('/');
    let (owner, repo) = (segments.next()?, segments.next()?);
    let asked_for = format!("{owner}/{repo}").to_ascii_lowercase();
    let canonical = state
        .pulls
        .iter()
        .find(|pull| {
            pull.repo.eq_ignore_ascii_case(&asked_for)
                || pull
                    .old_names
                    .iter()
                    .any(|old| old.eq_ignore_ascii_case(&asked_for))
        })
        .map(|pull| pull.repo.clone())?;
    let key = canonical.to_ascii_lowercase();
    let visible = state.public.contains(&key)
        || state
            .sees
            .get(&asked.token)
            .is_some_and(|repos| repos.contains(&key));
    visible.then(|| (canonical, segments.collect()))
}

/// Pull request `number` of `repo`, as GitHub names it now.
fn pull_request<'a>(state: &'a State, repo: &str, number: &str) -> Option<&'a FakePull> {
    state
        .pulls
        .iter()
        .find(|pull| pull.repo.eq_ignore_ascii_case(repo) && number.parse() == Ok(pull.number))
}

/// `/repos/{owner}/{name}`: the repository, by its name now.
fn get_repo(state: &mut State, asked: &Asked) -> Option<anyhow::Result<Answer>> {
    let (repo, rest) = repository(state, asked)?;
    let json = serde_json::json!({ "full_name": repo });
    rest.is_empty()
        .then(|| Ok(Answer::new(200, json.to_string())))
}

/// `/repos/{owner}/{name}/pulls/{number}`: the pull request.
fn get_pull(state: &mut State, asked: &Asked) -> Option<anyhow::Result<Answer>> {
    let (repo, rest) = repository(state, asked)?;
    let ["pulls", number] = rest.as_slice() else {
        return None;
    };
    let pull = pull_request(state, &repo, number)?;
    Some(Ok(Answer::new(200, pull_json(pull).to_string())))
}

/// `/repos/{owner}/{name}/pulls/{number}/files`: a page of the pull request's files.
fn get_pull_files(state: &mut State, asked: &Asked) -> Option<anyhow::Result<Answer>> {
    let (repo, rest) = repository(state, asked)?;
    let ["pulls", number, "files"] = rest.as_slice() else {
        return None;
    };
    let pull = pull_request(state, &repo, number)?;
    Some(Ok(files_page(pull, &asked.query)))
}

/// One page of `pull`'s files, as `query` (`per_page` and `page`) asks, with a `Link` header to
/// the next page as GitHub gives it: a full URL.
fn files_page(pull: &FakePull, query: &str) -> Answer {
    let param = |name: &str| {
        query
            .split('&')
            .find_map(|pair| pair.strip_prefix(name)?.strip_prefix('='))
            .and_then(|value| value.parse::<usize>().ok())
    };
    let per_page = param("per_page").unwrap_or(30);
    let page = param("page").unwrap_or(1);
    let start = (page - 1) * per_page;
    let files: Vec<_> = pull
        .files
        .iter()
        .skip(start)
        .take(per_page)
        .cloned()
        .collect();
    let mut answer = Answer::new(200, serde_json::Value::from(files).to_string());
    if start + per_page < pull.files.len() {
        answer = answer.header(
            "link",
            format!(
                "<https://api.github.com/repos/{}/pulls/{}/files?per_page={per_page}&page={}>; \
                 rel=\"next\"",
                pull.repo,
                pull.number,
                page + 1
            ),
        );
    }
    answer
}

/// GitHub's REST view of `pull`.
fn pull_json(pull: &FakePull) -> serde_json::Value {
    serde_json::json!({
        "number": pull.number,
        "title": pull.title,
        "state": "open",
        "draft": false,
        "merged_at": null,
        "user": { "login": "author" },
        "created_at": "2026-11-01T10:00:00Z",
        "updated_at": "2026-11-02T10:00:00Z",
        "head": { "ref": "feature", "sha": pull.head },
        "changed_files": pull.changed_files.unwrap_or(pull.files.len()),
        "base": {
            "ref": pull.base_branch,
            "sha": pull.base,
            "repo": {
                "full_name": pull.repo,
                "clone_url": format!("https://github.com/{}.git", pull.repo),
            },
        },
    })
}

impl HttpClient for FakeGithub {
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
        let asked = Asked::read(request);
        let held = {
            let mut state = self.0.lock();
            let route = [Some(asked.path.clone()), asked.graphql_route()]
                .into_iter()
                .flatten()
                .find(|route| state.held.contains_key(route));
            route.map(|route| {
                let (let_go, held) = oneshot::channel();
                state.held.entry(route).or_default().push(let_go);
                held
            })
        };
        let this = self.clone();
        Box::pin(async move {
            if let Some(held) = held {
                held.await.ok();
            }
            let answer = this.respond(&asked)?;
            let mut response = Response::builder().status(answer.status);
            for (name, value) in answer.headers {
                response = response.header(name, value);
            }
            Ok(response.body(AsyncBody::from(answer.body))?)
        })
    }
}

/// Lo's GitHub layer over `github`, with `accounts` (login, token) signed in, in that order,
/// and `github` as the app's HTTP client for reading without signing in.
pub async fn sign_in(
    github: &FakeGithub,
    accounts: &[(&str, &str)],
    cx: &mut TestAppContext,
) -> (Entity<Accounts>, PrCache) {
    static DATABASES: AtomicUsize = AtomicUsize::new(0);
    let name = format!("review_ui-{}", DATABASES.fetch_add(1, Ordering::SeqCst));
    let db = LoDb::open_test(&name).await;
    let store = Arc::new(credentials_provider::test_support::FakeCredentialsProvider::default());
    let http: Arc<dyn HttpClient> = Arc::new(github.clone());
    let entity = cx
        .update(|cx| Accounts::load(db.clone(), store, http.clone(), cx))
        .unwrap();
    for (id, (login, token)) in accounts.iter().enumerate() {
        let user = AuthenticatedUser {
            user: User {
                login: login.to_string(),
                id: id as u64 + 1,
                name: None,
                avatar_url: None,
            },
            scopes: vec!["read:org".into(), "repo".into(), "workflow".into()],
        };
        entity
            .update(cx, |accounts, cx| {
                accounts.add(user, Token::new(*token), AccountSource::DeviceFlow, cx)
            })
            .await
            .unwrap();
        // Accounts sort by when they were added, to the second.
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(1));
    }
    cx.update(|cx| {
        cx.set_http_client(http);
        crate::github_layer::init(Task::ready(Some((entity.clone(), db.clone()))), cx);
    });
    cx.run_until_parked();
    (entity, PrCache::new(db))
}

/// Signs `login` in with `token` after [`sign_in`], as Accounts… does: a new account, or one
/// signed out signing in again.
pub async fn sign_in_as(
    accounts: &Entity<Accounts>,
    login: &str,
    token: &str,
    cx: &mut TestAppContext,
) {
    let user = AuthenticatedUser {
        user: User {
            login: login.to_string(),
            id: 1_000,
            name: None,
            avatar_url: None,
        },
        scopes: vec!["read:org".into(), "repo".into(), "workflow".into()],
    };
    accounts
        .update(cx, |accounts, cx| {
            accounts.add(user, Token::new(token), AccountSource::DeviceFlow, cx)
        })
        .await
        .unwrap();
    cx.run_until_parked();
}

/// Without signing in: the app's HTTP client is `github`, and there's no accounts database.
pub fn anonymous(github: &FakeGithub, cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_http_client(Arc::new(github.clone())));
}
