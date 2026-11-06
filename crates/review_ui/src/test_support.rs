//! A fake GitHub for the tab's tests: its REST API, answering per token, and its git server, real
//! bare repositories served over `file://`. With them, tests open pull requests the way rozed
//! does, through the GitHub layer, the local copy and the diff.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures::future::BoxFuture;
use github::accounts::Accounts;
use github::cache::PrCache;
use github::db::{AccountSource, RozedDb};
use github::{AuthenticatedUser, Token, User};
use gpui::{Entity, Task, TestAppContext, VisualTestContext};
use http_client::http::HeaderValue;
use http_client::{AsyncBody, HttpClient, Request, Response, Url};
use parking_lot::Mutex;
use project::{FakeFs, Project};
use repo_cache::{Git, RepoCache};
use tempfile::TempDir;
use workspace::{MultiWorkspace, Workspace};

/// Sets the app up as rozed does: the command palette, the editor, the tab's keys over Zed's
/// keymap, and rozed's changes to every workspace.
pub fn init(cx: &mut TestAppContext) {
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
pub async fn workspace(cx: &mut TestAppContext) -> (Entity<Workspace>, &mut VisualTestContext) {
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
    /// Answers that win over the rest, by token (`""` for none) and path.
    answers: HashMap<(String, String), Answer>,
    offline: bool,
    /// Each request, as `GET /path as token`.
    requests: Vec<String>,
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

    pub fn make_public(&self, repo: &str) {
        self.0.lock().public.push(repo.to_ascii_lowercase());
    }

    /// Answers `path` asked with `token` (`""` for none) with `answer`.
    pub fn answer(&self, token: &str, path: &str, answer: Answer) {
        self.0
            .lock()
            .answers
            .insert((token.to_string(), path.to_string()), answer);
    }

    /// Whether every request fails as if the network were down.
    pub fn set_offline(&self, offline: bool) {
        self.0.lock().offline = offline;
    }

    /// Stops answering `path` asked with `token` as [`Self::answer`] said.
    pub fn forget_answer(&self, token: &str, path: &str) {
        self.0
            .lock()
            .answers
            .remove(&(token.to_string(), path.to_string()));
    }

    /// The requests made since the last call, as `GET /path as token`.
    pub fn requests(&self) -> Vec<String> {
        std::mem::take(&mut self.0.lock().requests)
    }

    fn respond(&self, method: &str, path: &str, token: &str) -> anyhow::Result<Answer> {
        let mut state = self.0.lock();
        let who = if token.is_empty() { "nobody" } else { token };
        state.requests.push(format!("{method} {path} as {who}"));
        if state.offline {
            anyhow::bail!("the network is down");
        }
        if let Some(answer) = state.answers.get(&(token.to_string(), path.to_string())) {
            return Ok(answer.clone());
        }
        let not_found = || Answer::new(404, r#"{"message":"Not Found"}"#);
        let Some(rest) = path.strip_prefix("/repos/") else {
            return Ok(not_found());
        };
        let segments: Vec<&str> = rest.split('/').collect();
        let (owner, repo) = match segments.as_slice() {
            [owner, repo, ..] => (*owner, *repo),
            _ => return Ok(not_found()),
        };
        let asked = format!("{owner}/{repo}").to_ascii_lowercase();
        let Some(canonical) = state
            .pulls
            .iter()
            .find(|pull| {
                pull.repo.eq_ignore_ascii_case(&asked)
                    || pull
                        .old_names
                        .iter()
                        .any(|old| old.eq_ignore_ascii_case(&asked))
            })
            .map(|pull| pull.repo.to_ascii_lowercase())
        else {
            return Ok(not_found());
        };
        let visible = state.public.contains(&canonical)
            || state
                .sees
                .get(token)
                .is_some_and(|repos| repos.contains(&canonical));
        if !visible {
            return Ok(not_found());
        }
        match segments.as_slice() {
            [_, _] => Ok(Answer::new(
                200,
                serde_json::json!({ "full_name": canonical }).to_string(),
            )),
            [_, _, "pulls", number] => {
                let Some(pull) = state.pulls.iter().find(|pull| {
                    pull.repo.eq_ignore_ascii_case(&canonical) && number.parse() == Ok(pull.number)
                }) else {
                    return Ok(not_found());
                };
                Ok(Answer::new(200, pull_json(pull).to_string()))
            }
            _ => Ok(not_found()),
        }
    }
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
        let token = request
            .headers()
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split_once(' ').map(|(_, token)| token.to_string()))
            .unwrap_or_default();
        let path = request
            .uri()
            .path_and_query()
            .map(|path| path.to_string())
            .unwrap_or_default();
        let answer = self.respond(request.method().as_str(), &path, &token);
        Box::pin(async move {
            let answer = answer?;
            let mut response = Response::builder().status(answer.status);
            for (name, value) in answer.headers {
                response = response.header(name, value);
            }
            Ok(response.body(AsyncBody::from(answer.body))?)
        })
    }
}

/// rozed's GitHub layer over `github`, with `accounts` (login, token) signed in, in that order,
/// and `github` as the app's HTTP client for reading without signing in.
pub async fn sign_in(
    github: &FakeGithub,
    accounts: &[(&str, &str)],
    cx: &mut TestAppContext,
) -> (Entity<Accounts>, PrCache) {
    static DATABASES: AtomicUsize = AtomicUsize::new(0);
    let name = format!("review_ui-{}", DATABASES.fetch_add(1, Ordering::SeqCst));
    let db = RozedDb::open_test(&name).await;
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

/// Without signing in: the app's HTTP client is `github`, and there's no accounts database.
pub fn anonymous(github: &FakeGithub, cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_http_client(Arc::new(github.clone())));
}

/// GitHub's git server: bare repositories at `<root>/<owner>/<repo>.git`.
pub struct GitServer {
    dir: TempDir,
    _cache: TempDir,
}

impl GitServer {
    /// A server, and local copies in a folder of their own that fetch from it.
    pub fn start(cx: &mut TestAppContext) -> Self {
        let dir = TempDir::new().unwrap();
        let cache_dir = TempDir::new().unwrap();
        let cache = RepoCache::new(cache_dir.path(), Git::find().unwrap())
            .unwrap()
            .with_remote_base(format!("file://{}", dir.path().display()));
        cx.update(|cx| crate::local_copies::init_with(cache, cx));
        Self {
            dir,
            _cache: cache_dir,
        }
    }

    /// Pull request `number` of `repo`, an `owner/name`: `base`, a new commit on `main`, and
    /// `head`, a commit on top of it, as `refs/pull/<number>/head`. Each is a list of files and
    /// their text.
    pub fn pull_request(
        &self,
        repo: &str,
        number: u64,
        base: &[(&str, &str)],
        head: &[(&str, &str)],
    ) -> FakePull {
        let git_dir = self.dir.path().join(format!("{repo}.git"));
        if !git_dir.join("HEAD").exists() {
            std::fs::create_dir_all(&git_dir).unwrap();
            git(&git_dir, &["init", "-q", "--bare"], None, None);
            git(
                &git_dir,
                &["config", "uploadpack.allowFilter", "true"],
                None,
                None,
            );
            git(
                &git_dir,
                &["config", "uploadpack.allowAnySHA1InWant", "true"],
                None,
                None,
            );
        }
        let index = self.dir.path().join("index");
        // Each pull request's base goes on top of the branch, so every pull request shares its
        // history.
        let main = std::fs::read_to_string(git_dir.join("refs/heads/main"))
            .ok()
            .map(|main| main.trim().to_string());
        let base_commit = commit(&git_dir, &index, base, main.as_deref());
        let head_commit = commit(&git_dir, &index, head, Some(&base_commit));
        git(
            &git_dir,
            &["update-ref", "refs/heads/main", &base_commit],
            None,
            None,
        );
        let pull_ref = format!("refs/pull/{number}/head");
        git(
            &git_dir,
            &["update-ref", &pull_ref, &head_commit],
            None,
            None,
        );
        FakePull {
            repo: repo.to_string(),
            old_names: Vec::new(),
            number,
            title: format!("Pull request {number}"),
            head: head_commit,
            base_branch: "main".into(),
            base: base_commit,
        }
    }
}

/// Writes a commit holding exactly `files`, and returns its ID.
fn commit(git_dir: &Path, index: &Path, files: &[(&str, &str)], parent: Option<&str>) -> String {
    let _ = std::fs::remove_file(index);
    for (path, text) in files {
        let id = git(
            git_dir,
            &["hash-object", "-w", "--stdin"],
            Some(text.as_bytes()),
            None,
        );
        let cacheinfo = format!("100644,{id},{path}");
        git(
            git_dir,
            &["update-index", "--add", "--cacheinfo", &cacheinfo],
            None,
            Some(index),
        );
    }
    let tree = git(git_dir, &["write-tree"], None, Some(index));
    let mut args = vec!["commit-tree", tree.as_str(), "-m", "fixture"];
    if let Some(parent) = parent {
        args.extend(["-p", parent]);
    }
    git(git_dir, &args, None, None)
}

fn git(dir: &Path, args: &[&str], stdin: Option<&[u8]>, index: Option<&Path>) -> String {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "rozed test")
        .env("GIT_AUTHOR_EMAIL", "test@rozed.invalid")
        .env("GIT_COMMITTER_NAME", "rozed test")
        .env("GIT_COMMITTER_EMAIL", "test@rozed.invalid")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
    }
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.unwrap_or_default())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}
