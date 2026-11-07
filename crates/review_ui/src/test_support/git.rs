//! The fake GitHub's git server: real bare repositories, served over `file://`.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use gpui::TestAppContext;
use repo_cache::{Git, RepoCache};
use tempfile::TempDir;

use super::FakePull;

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

    /// Takes `repo`, an `owner/name`, off the server, so fetching from it fails, as offline,
    /// until [`Self::restore`].
    pub fn hide(&self, repo: &str) {
        let git_dir = self.dir.path().join(format!("{repo}.git"));
        std::fs::rename(&git_dir, git_dir.with_extension("hidden")).unwrap();
    }

    /// Puts `repo` back on the server, as when GitHub can be reached again.
    pub fn restore(&self, repo: &str) {
        let git_dir = self.dir.path().join(format!("{repo}.git"));
        std::fs::rename(git_dir.with_extension("hidden"), &git_dir).unwrap();
    }

    /// Pushes a commit of `files`, the whole tree, on top of pull request `number`'s head in
    /// `repo`, as its new `refs/pull/<number>/head`, and returns the commit.
    pub fn push(&self, repo: &str, number: u64, files: &[(&str, &str)]) -> String {
        let git_dir = self.dir.path().join(format!("{repo}.git"));
        let index = self.dir.path().join("index");
        let pull_ref = format!("refs/pull/{number}/head");
        let head = git(&git_dir, &["rev-parse", &pull_ref], None, None);
        let pushed = commit(&git_dir, &index, files, Some(head.trim()));
        git(&git_dir, &["update-ref", &pull_ref, &pushed], None, None);
        pushed
    }

    /// Pull request `number` of `repo` on top of a stack's lower layer: `base`, a new commit on
    /// `main`; `lower`, a commit on top of it, as branch `stack-a`; and `head`, a commit on top of
    /// that, as `refs/pull/<number>/head`. It merges into `main` to start with. Each is a list of
    /// files and their text. Returns the pull request and the lower layer's commit.
    pub fn stacked_pull_request(
        &self,
        repo: &str,
        number: u64,
        base: &[(&str, &str)],
        lower: &[(&str, &str)],
        head: &[(&str, &str)],
    ) -> (FakePull, String) {
        let mut pull = self.pull_request(repo, number, base, lower);
        let git_dir = self.dir.path().join(format!("{repo}.git"));
        let index = self.dir.path().join("index");
        let lower_commit = pull.head.clone();
        let head_commit = commit(&git_dir, &index, head, Some(&lower_commit));
        git(
            &git_dir,
            &["update-ref", "refs/heads/stack-a", &lower_commit],
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
        pull.head = head_commit;
        (pull, lower_commit)
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
        let files = pull_files(&git_dir, repo, &base_commit, &head_commit);
        FakePull {
            repo: repo.to_string(),
            old_names: Vec::new(),
            number,
            title: format!("Pull request {number}"),
            head: head_commit,
            base_branch: "main".into(),
            base: base_commit,
            files,
            changed_files: None,
        }
    }
}

/// GitHub's list of the files changed from `base` to `head`, with their patches, as git makes
/// them. A removed file's links name the base, as GitHub's do.
fn pull_files(git_dir: &Path, repo: &str, base: &str, head: &str) -> Vec<serde_json::Value> {
    let statuses = git(git_dir, &["diff", "--name-status", base, head], None, None);
    statuses
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(status, path)| {
            let status = match status {
                "A" => "added",
                "D" => "removed",
                _ => "modified",
            };
            let diff = git(
                git_dir,
                &["diff", "-U3", "--no-color", base, head, "--", path],
                None,
                None,
            );
            let patch = diff
                .find("\n@@ ")
                .map(|start| diff[start + 1..].to_string());
            let count = |prefix: char| {
                patch.as_deref().map_or(0, |patch| {
                    patch
                        .lines()
                        .filter(|line| line.starts_with(prefix))
                        .count()
                })
            };
            let (additions, deletions) = (count('+'), count('-'));
            let at = if status == "removed" { base } else { head };
            serde_json::json!({
                "filename": path,
                "status": status,
                "additions": additions,
                "deletions": deletions,
                "changes": additions + deletions,
                "patch": patch,
                "blob_url": format!("https://github.com/{repo}/blob/{at}/{path}"),
                "raw_url": format!("https://github.com/{repo}/raw/{at}/{path}"),
                "contents_url": format!(
                    "https://api.github.com/repos/{repo}/contents/{path}?ref={at}"
                ),
            })
        })
        .collect()
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
        .env("GIT_AUTHOR_NAME", "Lo test")
        .env("GIT_AUTHOR_EMAIL", "test@lo.invalid")
        .env("GIT_COMMITTER_NAME", "Lo test")
        .env("GIT_COMMITTER_EMAIL", "test@lo.invalid")
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
