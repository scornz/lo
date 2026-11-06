//! Real git repositories in temporary folders, served over `file://` the way GitHub serves them:
//! partial clones allowed, and any commit fetchable by ID.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use repo_cache::{Git, Oid, RepoCache};
use tempfile::TempDir;

/// Runs git in `dir` without the user's config, as a test's own git, and returns its output.
pub fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "rozed test")
        .env("GIT_AUTHOR_EMAIL", "test@rozed.invalid")
        .env("GIT_COMMITTER_NAME", "rozed test")
        .env("GIT_COMMITTER_EMAIL", "test@rozed.invalid")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

/// A folder of bare repositories at `<root>/<owner>/<repo>.git`, standing in for github.com.
pub struct Server {
    dir: TempDir,
}

impl Server {
    pub fn new() -> Self {
        Self {
            dir: TempDir::new().unwrap(),
        }
    }

    /// What [`RepoCache::with_remote_base`] takes to fetch from here.
    pub fn base_url(&self) -> String {
        format!("file://{}", self.remotes().display())
    }

    fn remotes(&self) -> PathBuf {
        self.dir.path().join("remotes")
    }

    /// A new, empty repository `owner/name`, with a working copy to make commits in.
    pub fn repo(&self, owner: &str, name: &str) -> Remote {
        let git_dir = self.remotes().join(owner).join(format!("{name}.git"));
        fs::create_dir_all(&git_dir).unwrap();
        git(&git_dir, &["init", "-q", "--bare", "-b", "trunk"]);
        // As GitHub does: clients may filter, and may fetch any object by ID.
        git(&git_dir, &["config", "uploadpack.allowFilter", "true"]);
        git(
            &git_dir,
            &["config", "uploadpack.allowAnySHA1InWant", "true"],
        );
        let work = self.dir.path().join("work").join(owner).join(name);
        fs::create_dir_all(&work).unwrap();
        git(&work, &["init", "-q", "-b", "trunk"]);
        Remote { git_dir, work }
    }

    /// A cache in a folder of its own, fetching from this server.
    pub fn cache(&self) -> (TempDir, RepoCache) {
        let dir = TempDir::new().unwrap();
        let cache = RepoCache::new(dir.path(), Git::find().unwrap())
            .unwrap()
            .with_remote_base(self.base_url());
        (dir, cache)
    }
}

/// A repository on the [`Server`], and a working copy where tests make its commits.
pub struct Remote {
    pub git_dir: PathBuf,
    pub work: PathBuf,
}

impl Remote {
    pub fn write(&self, path: &str, contents: impl AsRef<[u8]>) -> &Self {
        let path = self.work.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
        self
    }

    pub fn remove(&self, path: &str) -> &Self {
        fs::remove_file(self.work.join(path)).unwrap();
        self
    }

    /// Commits everything in the working copy and returns the commit.
    pub fn commit(&self, message: &str) -> Oid {
        git(&self.work, &["add", "-A"]);
        git(
            &self.work,
            &["commit", "-q", "--allow-empty", "-m", message],
        );
        self.head()
    }

    /// Replaces the last commit with one holding everything in the working copy.
    pub fn amend(&self, message: &str) -> Oid {
        git(&self.work, &["add", "-A"]);
        git(&self.work, &["commit", "-q", "--amend", "-m", message]);
        self.head()
    }

    pub fn head(&self) -> Oid {
        git(&self.work, &["rev-parse", "HEAD"]).parse().unwrap()
    }

    /// Switches the working copy to `branch`, creating it at the current commit if it's new.
    pub fn switch(&self, branch: &str) -> &Self {
        let exists = Command::new("git")
            .args([
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ])
            .current_dir(&self.work)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap()
            .status
            .success();
        if exists {
            git(&self.work, &["checkout", "-q", branch]);
        } else {
            git(&self.work, &["checkout", "-q", "-b", branch]);
        }
        self
    }

    /// Starts `branch` with no history.
    pub fn orphan(&self, branch: &str) -> &Self {
        git(&self.work, &["checkout", "-q", "--orphan", branch]);
        git(
            &self.work,
            &["rm", "-rq", "--cached", "--ignore-unmatch", "."],
        );
        self
    }

    /// Points `reference` on the server at the working copy's current commit, force-pushing if
    /// it was somewhere else.
    pub fn publish(&self, reference: &str) -> &Self {
        let work = self.work.to_str().unwrap();
        git(
            &self.git_dir,
            &["fetch", "-q", work, &format!("+HEAD:{reference}")],
        );
        self
    }

    /// Copies `reference` from `other` (a fork) into this repository, as GitHub does with a pull
    /// request's head from a fork.
    pub fn take_from(&self, other: &Remote, from: &str, reference: &str) -> &Self {
        let other = other.git_dir.to_str().unwrap();
        git(
            &self.git_dir,
            &["fetch", "-q", other, &format!("+{from}:{reference}")],
        );
        self
    }

    /// Deletes everything the server's refs don't reach, as if it had lost force-pushed commits.
    pub fn forget_unreachable(&self) {
        git(
            &self.git_dir,
            &["reflog", "expire", "--expire=now", "--all"],
        );
        git(&self.git_dir, &["gc", "-q", "--prune=now"]);
    }

    /// Whether the server still has `oid`.
    pub fn has(&self, oid: &Oid) -> bool {
        Command::new("git")
            .args(["cat-file", "-e", oid.as_str()])
            .current_dir(&self.git_dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .unwrap()
            .success()
    }
}

/// The objects reachable from `rev` that the clone at `git_dir` hasn't downloaded.
pub fn missing_objects(git_dir: &Path, rev: &str) -> Vec<String> {
    git(
        git_dir,
        &["rev-list", "--objects", "--missing=print", rev, "--"],
    )
    .lines()
    .filter_map(|line| line.strip_prefix('?'))
    .map(str::to_string)
    .collect()
}

/// The blob at `path` in `rev`, by ID, from the test's own git in `work`.
pub fn blob_id(work: &Path, rev: &Oid, path: &str) -> String {
    git(work, &["rev-parse", &format!("{rev}:{path}")])
}

/// Lines long enough that a small edit still reads as a rename.
pub fn rust_source(extra: &str) -> String {
    let mut source = String::new();
    for i in 0..20 {
        source.push_str(&format!("pub fn function_{i}() -> u32 {{ {i} }}\n"));
    }
    source.push_str(extra);
    source
}
