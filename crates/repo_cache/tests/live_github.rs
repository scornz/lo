//! A live test against github.com: fetching a pull request from a private repository with a
//! token, which git gets from the credential helper scoped to `https://github.com`.
//!
//! It's ignored by default, so CI never runs it. Run it with a file holding a token that can read
//! `rozed-dev/playground`:
//!
//! ```sh
//! LO_LIVE_TOKEN_FILE=<file> cargo test -p repo_cache --test live_github -- --ignored --nocapture
//! ```
//!
//! The test reads the token from that file and never prints it. Without the variable it skips.

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use repo_cache::{
    BaseSource, Change, Error, FetchFailure, Git, Oid, PullRequestRefs, RepoCache, Token,
};
use tempfile::TempDir;

const OWNER: &str = "rozed-dev";
const REPO: &str = "playground";
/// `stack-b` into `stack-a`, the head of pull request #1, which goes into `main`. Each adds one
/// file.
const PULL: u64 = 2;
const HEAD_BRANCH: &str = "stack-b";
const BASE_BRANCH: &str = "stack-a";
/// The file pull request #2 adds.
const ADDED: &str = "stack-b.md";
/// The file pull request #1 adds, which #2's diff, from its merge base with `stack-a`, leaves out.
const BELOW: &str = "stack-a.md";

/// The variable rozed's credential helper reads the token from.
const TOKEN_VAR: &str = "LO_GIT_TOKEN";

const JSON: &str = "application/vnd.github+json";
const RAW: &str = "application/vnd.github.raw+json";

#[test]
#[ignore = "fetches a private repository from github.com; needs LO_LIVE_TOKEN_FILE"]
fn fetches_a_private_repository_through_the_credential_helper() {
    let Some(token_file) = std::env::var_os("LO_LIVE_TOKEN_FILE") else {
        eprintln!("skipped: LO_LIVE_TOKEN_FILE isn't set");
        return;
    };
    let token_file = PathBuf::from(token_file);
    let token = Token::new(
        fs::read_to_string(&token_file)
            .unwrap_or_else(|error| panic!("can't read {}: {error}", token_file.display())),
    );
    let secret = token.expose().as_bytes();
    assert!(
        secret.len() >= 20,
        "{} doesn't hold a token",
        token_file.display()
    );

    // The repository is private: GitHub's API doesn't show it without a token.
    let (status, _) = api(&format!("/repos/{OWNER}/{REPO}"), JSON, None);
    assert_eq!(status, 404, "{OWNER}/{REPO} should be private");

    // The pull request as GitHub's API describes it, which is what the app passes to fetch_pull.
    let (status, body) = api(
        &format!("/repos/{OWNER}/{REPO}/pulls/{PULL}"),
        JSON,
        Some(&token),
    );
    assert_eq!(status, 200, "reading pull request #{PULL} from the API");
    let body = String::from_utf8(body).unwrap();
    assert_eq!(json_string(&body, &["head", "ref"]), HEAD_BRANCH);
    assert_eq!(json_string(&body, &["base", "ref"]), BASE_BRANCH);
    let head: Oid = json_string(&body, &["head", "sha"]).parse().unwrap();
    let refs = PullRequestRefs {
        number: PULL,
        head: Some(head.clone()),
        base_branch: BASE_BRANCH.into(),
        base: Some(json_string(&body, &["base", "sha"]).parse().unwrap()),
    };
    println!("pull request #{PULL}: {HEAD_BRANCH} {head} into {BASE_BRANCH}");

    let root = TempDir::new().unwrap();
    let signed_in_git = TracedGit::new();
    let anonymous_git = TracedGit::new();

    // With the token: fetch, then read the diff and the file it adds.
    {
        let cache = RepoCache::new(root.path(), signed_in_git.git.clone()).unwrap();
        let repo = check(
            cache.open(OWNER, REPO, Some(token.clone())),
            &token,
            "opening",
        );
        let fetched = check(repo.fetch_pull(&refs), &token, "fetching with the token");
        println!("fetched: {fetched:?}");
        assert_eq!(fetched.head, head);
        assert_eq!(fetched.base_source, BaseSource::Branch);

        // The head is pinned under the pull request's revisions.
        let pinned = for_each_ref(repo.git_dir(), &format!("refs/lo/pull/{PULL}/"));
        println!("refs:\n{pinned}");
        assert!(
            pinned
                .lines()
                .any(|line| line == format!("{head} refs/lo/pull/{PULL}/revs/{head}")),
            "{pinned}"
        );
        let revisions = check(repo.pull_revisions(PULL), &token, "listing revisions");
        assert_eq!(revisions, vec![head.clone()]);

        let merge_base = check(
            repo.merge_base(&fetched.head, &fetched.base_tip),
            &token,
            "finding the merge base",
        )
        .expect("the head and the base share history");
        let files = check(
            repo.changed_files(&merge_base, &fetched.head),
            &token,
            "listing changed files",
        );
        for file in &files {
            println!("changed: {:?} {}", file.change, file.display_path());
        }
        let added = files
            .iter()
            .find(|file| file.path() == ADDED.as_bytes())
            .unwrap_or_else(|| panic!("{ADDED} isn't among the changed files"));
        assert_eq!(added.change, Change::Added);
        assert!(added.old.is_none());
        assert!(
            files.iter().all(|file| file.path() != BELOW.as_bytes()),
            "{BELOW}, from the pull request below, is in the diff"
        );

        let content = check(
            repo.read_file(&fetched.head, ADDED.as_bytes()),
            &token,
            "reading the file",
        )
        .unwrap_or_else(|| panic!("{ADDED} isn't in the head"));
        let (status, from_api) = api(
            &format!("/repos/{OWNER}/{REPO}/contents/{ADDED}?ref={head}"),
            RAW,
            Some(&token),
        );
        assert_eq!(status, 200, "reading {ADDED} from the API");
        assert!(!content.is_empty());
        assert_eq!(content, from_api, "{ADDED} differs from GitHub's copy");
        println!(
            "{ADDED} ({} bytes, as GitHub has it):\n{}",
            content.len(),
            String::from_utf8_lossy(&content).trim_end()
        );
    }

    // Without a token: the same fetch fails as GitHub refusing it, and nothing prompts.
    {
        let cache = RepoCache::new(root.path(), anonymous_git.git.clone()).unwrap();
        let repo = cache.open(OWNER, REPO, None).unwrap();
        let (sender, receiver) = mpsc::channel();
        let pull = refs.clone();
        let fetching = thread::spawn(move || sender.send(repo.fetch_pull(&pull)));
        let result = receiver
            .recv_timeout(Duration::from_secs(120))
            .expect("the fetch without a token never finished, as if waiting at a prompt");
        fetching.join().unwrap().unwrap();
        let error = match result {
            Ok(fetched) => panic!("fetched without a token: {fetched:?}"),
            Err(error) => error,
        };
        println!("without a token: {}", redact(&error.to_string(), &token));
        let Error::Fetch { failure, .. } = &error else {
            panic!("not a fetch error: {}", redact(&error.to_string(), &token));
        };
        assert!(
            matches!(failure, FetchFailure::Auth | FetchFailure::RepoNotFound),
            "{failure:?}"
        );
        assert!(!error.is_retryable());
    }

    // The token is in no file in the cache: not in a clone's config, logs or refs, nor in git's
    // home folder.
    let files = files_under(root.path());
    let config = root
        .path()
        .join(format!("repos/github.com/{OWNER}/{REPO}.git/config"));
    assert!(
        files.contains(&config),
        "{} wasn't scanned",
        config.display()
    );
    let mut bytes = 0;
    for file in &files {
        let contents = fs::read(file).unwrap();
        bytes += contents.len();
        assert!(
            !contains(&contents, secret),
            "the token is in {}",
            file.display()
        );
    }
    let has_logs = files.iter().any(|file| {
        let file = file.strip_prefix(root.path()).unwrap();
        file.components().any(|part| part.as_os_str() == "logs")
    });
    println!(
        "cache: no token in {} files ({bytes} bytes){}",
        files.len(),
        if has_logs {
            ", logs included"
        } else {
            ", and git kept no logs"
        }
    );

    // No git, and nothing git started, was given the token as an argument, and git traced
    // nothing with the token in it.
    for traced in [&signed_in_git, &anonymous_git] {
        for log in traced.logs() {
            let contents = fs::read(&log).unwrap_or_default();
            assert!(!contents.is_empty(), "{} is empty", log.display());
            assert!(
                !contains(&contents, secret),
                "the token is in {}",
                log.display()
            );
        }
    }
    let signed_in = signed_in_git.summary();
    let anonymous = anonymous_git.summary();
    println!("with the token: {signed_in}");
    println!("without a token: {anonymous}");
    // With the token, git asked rozed's helper for credentials, which read it from the variable
    // set on that git.
    assert!(signed_in.helper_gets > 0, "{signed_in:?}");
    assert!(signed_in.with_token_var > 0, "{signed_in:?}");
    // Without one, no git had the variable or the helper, and git ran no other helper or askpass.
    assert_eq!(anonymous.with_token_var, 0, "{anonymous:?}");
    assert_eq!(anonymous.helper_gets, 0, "{anonymous:?}");
    assert_eq!(anonymous.credential_programs, 0, "{anonymous:?}");
    assert!(!anonymous_git.trace2().contains(TOKEN_VAR));
}

/// Fails with what failed and why, without the token, if `result` is an error.
fn check<T>(result: Result<T, Error>, token: &Token, what: &str) -> T {
    result.unwrap_or_else(|error| panic!("{what}: {}", redact(&error.to_string(), token)))
}

fn redact(text: &str, token: &Token) -> String {
    text.replace(token.expose(), "<token>")
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// GETs `path` from GitHub's REST API, with `token` when there is one, and returns the status
/// and the body. curl reads the `Authorization` header from its stdin, so the token is in no
/// argument, and `--disable` keeps it from reading the user's `.curlrc`.
fn api(path: &str, accept: &str, token: Option<&Token>) -> (u16, Vec<u8>) {
    let mut child = Command::new("curl")
        .args([
            "--disable",
            "--silent",
            "--show-error",
            "--proto",
            "=https",
            "--max-time",
            "60",
            "--header",
            &format!("Accept: {accept}"),
            "--header",
            "X-GitHub-Api-Version: 2022-11-28",
            "--header",
            "@-",
            "--write-out",
            "\n%{http_code}",
            &format!("https://api.github.com{path}"),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("running curl");
    let mut stdin = child.stdin.take().unwrap();
    if let Some(token) = token {
        stdin
            .write_all(format!("Authorization: Bearer {}\n", token.expose()).as_bytes())
            .unwrap();
    }
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "curl {path}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut body = output.stdout;
    let newline = body.iter().rposition(|byte| *byte == b'\n').unwrap();
    let status = String::from_utf8_lossy(&body[newline + 1..])
        .parse()
        .unwrap();
    body.truncate(newline);
    (status, body)
}

/// The string at `keys` in a JSON object from GitHub's API: `["head", "sha"]` is `head.sha`.
/// Enough for the few plain values this test reads, without a JSON parser.
fn json_string<'a>(json: &'a str, keys: &[&str]) -> &'a str {
    let mut rest = json;
    for key in keys {
        let quoted = format!("\"{key}\"");
        rest = loop {
            let at = rest
                .find(&quoted)
                .unwrap_or_else(|| panic!("no {keys:?} in the API's answer"));
            let after = rest[at + quoted.len()..].trim_start();
            if let Some(value) = after.strip_prefix(':') {
                break value.trim_start();
            }
            rest = after;
        };
    }
    let value = rest
        .strip_prefix('"')
        .unwrap_or_else(|| panic!("{keys:?} isn't a string"));
    &value[..value.find('"').unwrap()]
}

/// `<oid> <ref>` for each ref under `prefix` in the clone at `git_dir`, read by a git of the
/// test's own, which reads only refs and never fetches.
fn for_each_ref(git_dir: &Path, prefix: &str) -> String {
    let output = Command::new("git")
        .arg(format!("--git-dir={}", git_dir.display()))
        .args(["for-each-ref", "--format=%(objectname) %(refname)", prefix])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap()
}

/// Every file under `dir`, without following symlinks.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut folders = vec![dir.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for entry in fs::read_dir(&folder).unwrap() {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                folders.push(entry.path());
            } else if kind.is_file() {
                files.push(entry.path());
            }
        }
    }
    files
}

/// A git that records how every git ran: a script that appends its arguments, and whether the
/// credential helper's variable was set, to `argv`, then runs the real git with git's tracing on.
/// `GIT_TRACE` records the command line of every program git starts, and `GIT_TRACE2_EVENT`
/// records those and the arguments of every git process, including the ones git starts itself.
struct TracedGit {
    dir: TempDir,
    git: Git,
}

/// What a [`TracedGit`] saw.
#[derive(Debug)]
struct Summary {
    /// Times rozed ran git.
    runs: usize,
    /// Of those, the runs with the credential helper's variable set.
    with_token_var: usize,
    /// git processes, counting the ones git started, such as `git remote-https`.
    git_processes: usize,
    /// Programs git started.
    children: usize,
    /// Times git asked rozed's credential helper for credentials.
    helper_gets: usize,
    /// Programs git started that ask for credentials: any credential helper or askpass.
    credential_programs: usize,
}

impl std::fmt::Display for Summary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "rozed ran git {} times, {} with the helper's variable set; {} git processes; \
             git started {} programs, asked rozed's helper for credentials {} times, and ran {} \
             other credential helpers or askpass",
            self.runs,
            self.with_token_var,
            self.git_processes,
            self.children,
            self.helper_gets,
            self.credential_programs,
        )
    }
}

impl TracedGit {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let real = Git::find().unwrap();
        let path = |name: &str| quote(&dir.path().join(name));
        let (argv, trace, trace2) = (path("argv"), path("trace"), path("trace2"));
        let script = format!(
            "#!/bin/sh\n\
             if [ -n \"${{{TOKEN_VAR}+set}}\" ]; then printf 'token-var:set\\0'; \
             else printf 'token-var:unset\\0'; fi >> {argv}\n\
             printf '%s\\0' \"$@\" >> {argv}\n\
             printf '\\n' >> {argv}\n\
             GIT_TRACE={trace}\n\
             GIT_TRACE2_EVENT={trace2}\n\
             export GIT_TRACE GIT_TRACE2_EVENT\n\
             exec {} \"$@\"\n",
            quote(real.program()),
        );
        let wrapper = dir.path().join("git");
        fs::write(&wrapper, script).unwrap();
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
        let git = Git::at(&wrapper).unwrap();
        Self { dir, git }
    }

    fn logs(&self) -> [PathBuf; 3] {
        ["argv", "trace", "trace2"].map(|name| self.dir.path().join(name))
    }

    fn trace2(&self) -> String {
        fs::read_to_string(self.dir.path().join("trace2")).unwrap()
    }

    fn summary(&self) -> Summary {
        let argv = fs::read(self.dir.path().join("argv")).unwrap();
        let runs: Vec<_> = argv
            .split(|byte| *byte == b'\n')
            .filter(|run| !run.is_empty())
            .collect();
        let trace2 = self.trace2();
        let events = |event: &str| {
            let event = format!("\"event\":\"{event}\"");
            trace2
                .lines()
                .filter(move |line| line.contains(&event))
                .collect::<Vec<_>>()
        };
        let children = events("child_start");
        Summary {
            runs: runs.len(),
            with_token_var: runs
                .iter()
                .filter(|run| run.starts_with(b"token-var:set\0"))
                .count(),
            git_processes: events("start").len(),
            children: children.len(),
            helper_gets: children
                .iter()
                .filter(|line| line.contains(TOKEN_VAR) && line.contains("f get\""))
                .count(),
            credential_programs: children
                .iter()
                .filter(|line| {
                    let line = line.to_ascii_lowercase();
                    line.contains("credential") || line.contains("askpass")
                })
                .count(),
        }
    }
}

/// `path` quoted for sh.
fn quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}
