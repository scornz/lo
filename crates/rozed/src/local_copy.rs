//! rozed's local copy of a pull request: a bare partial clone of its repository and a checkout of
//! its head. Spike version; milestone 4's `repo_cache` crate replaces it.
//!
//! Layout under the cache folder (`~/Library/Caches/rozed`):
//!
//! - `repos/github.com/<owner>/<repo>.git`: a bare clone with `--filter=blob:none`, so file
//!   contents download only when something reads them. Fetched revisions go under rozed's own refs:
//!   `refs/rozed/pull/<n>/head` and `refs/rozed/base/<branch>`.
//! - `checkouts/github.com/<owner>/<repo>/pull-<n>`: a detached `git worktree` of the clone at the
//!   pull request's head, for language servers.
//!
//! git runs without the user's or the system's git config, so nothing there (URL rewrites,
//! credential helpers, filters, hooks) changes what rozed fetches or runs, and it never prompts.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use anyhow::{Context as _, bail};

/// The folder that holds rozed's clones and checkouts.
pub struct RepoCache {
    root: PathBuf,
}

/// Which pull request to copy, and from where.
pub struct PullRequestSource<'a> {
    pub owner: &'a str,
    pub repo: &'a str,
    pub number: u64,
    /// The repository to fetch from, e.g. `https://github.com/sharkdp/fd.git`.
    pub remote_url: &'a str,
    /// The branch the pull request merges into, from GitHub's API.
    pub base_branch: &'a str,
}

/// A pull request copied locally.
#[derive(Debug)]
pub struct LocalPullRequest {
    pub repo: Repo,
    /// A checkout of `head`.
    pub checkout: PathBuf,
    /// The commit the diff starts from: where the head branched from the base branch.
    pub base: String,
    /// The pull request's head commit.
    pub head: String,
    /// What changed between `base` and `head`, in git's order.
    pub files: Vec<ChangedFile>,
}

/// A file the pull request changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    /// The path at the base. `None` when the pull request adds the file.
    pub old_path: Option<String>,
    /// The path at the head. `None` when the pull request deletes the file.
    pub new_path: Option<String>,
    /// Whether git considers either side binary.
    pub binary: bool,
}

impl ChangedFile {
    /// The path to show: the new path, or the old one for a deleted file.
    pub fn path(&self) -> &str {
        self.new_path
            .as_deref()
            .or(self.old_path.as_deref())
            .unwrap_or_default()
    }
}

/// A bare git repository in rozed's cache.
#[derive(Debug, Clone)]
pub struct Repo {
    git_dir: PathBuf,
}

/// The only host rozed clones from, for now.
const HOST: &str = "github.com";

impl RepoCache {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Where the clone of `owner/repo` lives. GitHub names are case-insensitive, so the folder
    /// names are lowercase.
    pub fn repo_dir(&self, owner: &str, repo: &str) -> PathBuf {
        self.root
            .join("repos")
            .join(HOST)
            .join(owner.to_ascii_lowercase())
            .join(format!("{}.git", repo.to_ascii_lowercase()))
    }

    /// Where the checkout of pull request `number` lives.
    pub fn checkout_dir(&self, owner: &str, repo: &str, number: u64) -> PathBuf {
        self.root
            .join("checkouts")
            .join(HOST)
            .join(owner.to_ascii_lowercase())
            .join(repo.to_ascii_lowercase())
            .join(format!("pull-{number}"))
    }

    /// Clones the repository if it isn't cloned yet, fetches the pull request's head and base
    /// branch, checks the head out, and lists what changed. Blocks while git runs.
    pub fn prepare(&self, source: &PullRequestSource) -> anyhow::Result<LocalPullRequest> {
        let branch = source.base_branch;
        check_branch_name(branch)?;
        let repo = Repo {
            git_dir: self.repo_dir(source.owner, source.repo),
        };
        repo.init(source.remote_url)?;

        let number = source.number;
        let head_ref = format!("refs/rozed/pull/{number}/head");
        let base_ref = format!("refs/rozed/base/{branch}");
        repo.fetch(&[
            format!("+refs/pull/{number}/head:{head_ref}"),
            format!("+refs/heads/{branch}:{base_ref}"),
        ])
        .with_context(|| format!("fetching pull request #{number} and its base branch {branch}"))?;
        let head = repo.commit(&head_ref)?;
        let base = repo.merge_base(&head, &repo.commit(&base_ref)?)?;
        let files = repo.changed_files(&base, &head)?;

        let checkout = self.checkout_dir(source.owner, source.repo, number);
        repo.check_out(&checkout, &head)?;
        Ok(LocalPullRequest {
            repo,
            checkout,
            base,
            head,
            files,
        })
    }
}

impl Repo {
    /// The contents of `path` at revision `rev`.
    pub fn read_file(&self, rev: &str, path: &str) -> anyhow::Result<Vec<u8>> {
        self.git(&["cat-file", "blob", &format!("{rev}:{path}")])
    }

    /// Creates the bare repository if it doesn't exist, and points `origin` at `url`. The first
    /// fetch makes it a partial clone.
    fn init(&self, url: &str) -> anyhow::Result<()> {
        if !self.git_dir.join("HEAD").is_file() {
            std::fs::create_dir_all(&self.git_dir)
                .with_context(|| format!("creating {}", self.git_dir.display()))?;
            self.git(&["init", "--quiet", "--bare"])?;
        }
        self.git(&["config", "remote.origin.url", url])?;
        // A promisor remote downloads file contents only when they're read.
        self.git(&["config", "remote.origin.promisor", "true"])?;
        self.git(&["config", "remote.origin.partialclonefilter", "blob:none"])?;
        Ok(())
    }

    fn fetch(&self, refspecs: &[String]) -> anyhow::Result<()> {
        let mut args = vec![
            "fetch",
            "--quiet",
            "--no-tags",
            "--no-write-fetch-head",
            "--recurse-submodules=no",
            "--filter=blob:none",
            "origin",
        ];
        args.extend(refspecs.iter().map(String::as_str));
        self.git(&args)?;
        Ok(())
    }

    /// The commit `rev` names.
    fn commit(&self, rev: &str) -> anyhow::Result<String> {
        let output = self.git(&[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{rev}^{{commit}}"),
        ])?;
        Ok(String::from_utf8(output)?.trim().to_string())
    }

    fn merge_base(&self, head: &str, base_tip: &str) -> anyhow::Result<String> {
        let output = self
            .git(&["merge-base", head, base_tip])
            .context("the pull request shares no history with its base branch")?;
        Ok(String::from_utf8(output)?.trim().to_string())
    }

    /// What changed from `base` to `head`, with renames detected.
    fn changed_files(&self, base: &str, head: &str) -> anyhow::Result<Vec<ChangedFile>> {
        let diff =
            |format: &str| self.git(&["diff", "--no-ext-diff", "-z", "-M", format, base, head]);
        // --numstat reads both sides of every changed file, so git downloads them all in one
        // batch here instead of one at a time later.
        let stats = parse_numstat(&diff("--numstat")?)?;
        let paths = parse_name_status(&diff("--name-status")?)?;
        anyhow::ensure!(
            stats.len() == paths.len(),
            "git listed {} files with --numstat and {} with --name-status",
            stats.len(),
            paths.len()
        );
        paths
            .into_iter()
            .zip(stats)
            .map(|((old_path, new_path), (path, binary))| {
                let file = ChangedFile {
                    old_path,
                    new_path,
                    binary,
                };
                anyhow::ensure!(
                    file.path() == path,
                    "git listed {path} with --numstat but {} with --name-status",
                    file.path()
                );
                Ok(file)
            })
            .collect()
    }

    /// Checks `rev` out, detached, at `dir`: a worktree of this repository. Symlinks are checked
    /// out as plain files holding their target, so nothing in the checkout points outside it.
    fn check_out(&self, dir: &Path, rev: &str) -> anyhow::Result<()> {
        if self.is_worktree(dir) {
            git_in(
                dir,
                &[
                    "-c",
                    "core.symlinks=false",
                    "checkout",
                    "--quiet",
                    "--force",
                    "--detach",
                    rev,
                ],
            )?;
            return Ok(());
        }
        if dir.exists() {
            std::fs::remove_dir_all(dir).with_context(|| format!("removing {}", dir.display()))?;
        }
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        self.git(&["worktree", "prune"])?;
        let dir = dir.to_str().context("the checkout path isn't UTF-8")?;
        self.git(&[
            "-c",
            "core.symlinks=false",
            "worktree",
            "add",
            "--quiet",
            "--force",
            "--detach",
            dir,
            rev,
        ])?;
        Ok(())
    }

    /// Whether `dir` is a working worktree of this repository, judged from its `.git` file
    /// rather than by running git there, which could find some other repository further up.
    fn is_worktree(&self, dir: &Path) -> bool {
        let Ok(dot_git) = std::fs::read_to_string(dir.join(".git")) else {
            return false;
        };
        let Some(worktree_git_dir) = dot_git.strip_prefix("gitdir:") else {
            return false;
        };
        let (Ok(worktree_git_dir), Ok(worktrees)) = (
            std::fs::canonicalize(worktree_git_dir.trim()),
            std::fs::canonicalize(self.git_dir.join("worktrees")),
        ) else {
            return false;
        };
        worktree_git_dir.starts_with(worktrees)
    }

    fn git(&self, args: &[&str]) -> anyhow::Result<Vec<u8>> {
        git_in(&self.git_dir, args)
    }
}

/// Fails unless `name` is a valid branch name, which also keeps it from changing the meaning of
/// a refspec.
fn check_branch_name(name: &str) -> anyhow::Result<()> {
    let valid = git_command()
        .args(["check-ref-format", &format!("refs/heads/{name}")])
        .status()
        .context("running git")?
        .success();
    anyhow::ensure!(valid, "`{name}` isn't a valid branch name");
    Ok(())
}

/// Environment variables that would point git at another repository than the one asked for.
const REPO_ENV_VARS: [&str; 7] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
];

/// A git command that reads neither the user's nor the system's git config and never prompts.
fn git_command() -> Command {
    let mut command = Command::new("git");
    command
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null());
    for var in REPO_ENV_VARS {
        command.env_remove(var);
    }
    command
}

/// Runs git in `dir` and returns its output, or an error with git's message.
fn git_in(dir: &Path, args: &[&str]) -> anyhow::Result<Vec<u8>> {
    let started = Instant::now();
    let output = git_command()
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .context("running git")?;
    log::debug!("git {} took {:?}", args.join(" "), started.elapsed());
    if !output.status.success() {
        bail!(
            "`git {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

/// Splits `-z` output into its NUL-terminated fields.
fn nul_fields(output: &[u8]) -> anyhow::Result<Vec<String>> {
    let output = output.strip_suffix(b"\0").unwrap_or(output);
    if output.is_empty() {
        return Ok(Vec::new());
    }
    output
        .split(|byte| *byte == 0)
        .map(|field| {
            String::from_utf8(field.to_vec()).context("git printed a path that isn't UTF-8")
        })
        .collect()
}

/// Parses `git diff -z --name-status` into `(old path, new path)` pairs.
fn parse_name_status(output: &[u8]) -> anyhow::Result<Vec<(Option<String>, Option<String>)>> {
    let mut fields = nul_fields(output)?.into_iter();
    let mut files = Vec::new();
    while let Some(status) = fields.next() {
        let mut path = || {
            fields
                .next()
                .filter(|path| !path.is_empty())
                .with_context(|| format!("git's {status} entry is missing a path"))
        };
        let file = match status.chars().next() {
            Some('A') => (None, Some(path()?)),
            Some('D') => (Some(path()?), None),
            Some('M' | 'T') => {
                let path = path()?;
                (Some(path.clone()), Some(path))
            }
            Some('R' | 'C') => (Some(path()?), Some(path()?)),
            _ => bail!("git printed an unknown file status {status:?}"),
        };
        files.push(file);
    }
    Ok(files)
}

/// Parses `git diff -z --numstat` into `(path, binary)` pairs, where `path` is the new path.
fn parse_numstat(output: &[u8]) -> anyhow::Result<Vec<(String, bool)>> {
    let mut fields = nul_fields(output)?.into_iter();
    let mut files = Vec::new();
    while let Some(field) = fields.next() {
        let mut parts = field.splitn(3, '\t');
        let (Some(added), Some(deleted), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            bail!("git printed an unexpected --numstat line {field:?}");
        };
        let binary = added == "-" && deleted == "-";
        let path = if path.is_empty() {
            // A rename: the old and new paths follow as fields of their own.
            let _old = fields.next();
            fields
                .next()
                .filter(|path| !path.is_empty())
                .with_context(|| format!("git's --numstat rename {field:?} is missing a path"))?
        } else {
            path.to_string()
        };
        files.push((path, binary));
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;

    use tempfile::TempDir;

    /// A git command that ignores the user's config, as rozed's own git does.
    fn git(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "rozed test")
            .env("GIT_AUTHOR_EMAIL", "test@rozed.invalid")
            .env("GIT_COMMITTER_NAME", "rozed test")
            .env("GIT_COMMITTER_EMAIL", "test@rozed.invalid")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    fn write(dir: &Path, path: &str, contents: impl AsRef<[u8]>) {
        let path = dir.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    /// Lines long enough that a small edit still reads as a rename.
    fn rust_source(extra: &str) -> String {
        let mut source = String::new();
        for i in 0..20 {
            source.push_str(&format!("pub fn function_{i}() -> u32 {{ {i} }}\n"));
        }
        source.push_str(extra);
        source
    }

    /// A bare "GitHub" repository with `main` and a pull request #1 at `refs/pull/1/head`:
    ///
    /// ```text
    /// first ── base ──────── main_tip      (main)
    ///             └── head                 (refs/pull/1/head)
    /// ```
    ///
    /// The pull request modifies `keep.txt` and `data.bin` (binary), renames `old_name.rs` to
    /// `src/new_name.rs` with an edit, deletes `gone.txt` and adds `added.txt`.
    struct Remote {
        dir: TempDir,
        first: String,
        base: String,
        head: String,
        main_tip: String,
    }

    impl Remote {
        fn new() -> Self {
            let dir = TempDir::new().unwrap();
            let work = dir.path().join("work");
            fs::create_dir(&work).unwrap();
            git(&work, &["init", "-q", "-b", "main"]);

            write(&work, "keep.txt", "keep, first version\n");
            git(&work, &["add", "-A"]);
            git(&work, &["commit", "-q", "-m", "first"]);
            let first = git(&work, &["rev-parse", "HEAD"]);

            write(&work, "keep.txt", "keep\n");
            write(&work, "old_name.rs", rust_source(""));
            write(&work, "gone.txt", "gone\n");
            write(&work, "data.bin", b"\x00\x01\x02base");
            git(&work, &["add", "-A"]);
            git(&work, &["commit", "-q", "-m", "base"]);
            let base = git(&work, &["rev-parse", "HEAD"]);

            git(&work, &["checkout", "-q", "-b", "feature"]);
            write(&work, "keep.txt", "keep, changed\n");
            write(&work, "data.bin", b"\x00\x01\x02head");
            fs::remove_file(work.join("old_name.rs")).unwrap();
            write(&work, "src/new_name.rs", rust_source("pub fn added() {}\n"));
            git(&work, &["rm", "-q", "gone.txt"]);
            write(&work, "added.txt", "added\n");
            git(&work, &["add", "-A"]);
            git(&work, &["commit", "-q", "-m", "head"]);
            let head = git(&work, &["rev-parse", "HEAD"]);

            git(&work, &["checkout", "-q", "main"]);
            write(&work, "main_only.txt", "main moved on\n");
            git(&work, &["add", "-A"]);
            git(&work, &["commit", "-q", "-m", "main moves on"]);
            let main_tip = git(&work, &["rev-parse", "HEAD"]);

            let remote = dir.path().join("remote.git");
            git(dir.path(), &["init", "-q", "--bare", "remote.git"]);
            // Let clients filter and fetch single objects, as GitHub does.
            git(&remote, &["config", "uploadpack.allowFilter", "true"]);
            git(
                &remote,
                &["config", "uploadpack.allowAnySHA1InWant", "true"],
            );
            git(&work, &["push", "-q", remote.to_str().unwrap(), "main"]);
            git(
                &work,
                &[
                    "push",
                    "-q",
                    remote.to_str().unwrap(),
                    "feature:refs/pull/1/head",
                ],
            );

            Self {
                dir,
                first,
                base,
                head,
                main_tip,
            }
        }

        fn git_dir(&self) -> PathBuf {
            self.dir.path().join("remote.git")
        }

        fn url(&self) -> String {
            format!("file://{}", self.git_dir().display())
        }

        fn source(&self) -> PullRequestSource<'_> {
            // Leak the URL so the source can borrow it for the whole test.
            let url: &'static str = Box::leak(self.url().into_boxed_str());
            PullRequestSource {
                owner: "Owner",
                repo: "Repo.rs",
                number: 1,
                remote_url: url,
                base_branch: "main",
            }
        }

        /// Force-pushes a new head for pull request #1 and returns it.
        fn force_push_new_head(&self) -> String {
            let work = self.dir.path().join("work");
            git(&work, &["checkout", "-q", "feature"]);
            write(&work, "added.txt", "added, then amended\n");
            git(&work, &["add", "-A"]);
            git(&work, &["commit", "-q", "--amend", "-m", "head, amended"]);
            let remote = self.git_dir();
            git(
                &work,
                &[
                    "push",
                    "-q",
                    "--force",
                    remote.to_str().unwrap(),
                    "feature:refs/pull/1/head",
                ],
            );
            git(&work, &["rev-parse", "HEAD"])
        }
    }

    fn changed(old: Option<&str>, new: Option<&str>, binary: bool) -> ChangedFile {
        ChangedFile {
            old_path: old.map(Into::into),
            new_path: new.map(Into::into),
            binary,
        }
    }

    #[test]
    fn folders_are_per_repo_and_pull_request_and_lowercase() {
        let cache = RepoCache::new("/cache");
        assert_eq!(
            cache.repo_dir("SharkDP", "FD"),
            Path::new("/cache/repos/github.com/sharkdp/fd.git")
        );
        assert_eq!(
            cache.checkout_dir("SharkDP", "FD", 2212),
            Path::new("/cache/checkouts/github.com/sharkdp/fd/pull-2212")
        );
    }

    #[test]
    fn prepares_a_pull_request() {
        let remote = Remote::new();
        let cache_dir = TempDir::new().unwrap();
        let cache = RepoCache::new(cache_dir.path());

        let pr = cache.prepare(&remote.source()).unwrap();

        assert_eq!(pr.head, remote.head);
        // The merge base, not the tip of main, which has moved on.
        assert_eq!(pr.base, remote.base);
        assert_eq!(
            pr.files,
            vec![
                changed(None, Some("added.txt"), false),
                changed(Some("data.bin"), Some("data.bin"), true),
                changed(Some("gone.txt"), None, false),
                changed(Some("keep.txt"), Some("keep.txt"), false),
                changed(Some("old_name.rs"), Some("src/new_name.rs"), false),
            ]
        );
        assert_eq!(pr.files[2].path(), "gone.txt");
        assert_eq!(pr.files[4].path(), "src/new_name.rs");

        // A bare clone in the cache, with the fetched revisions under rozed's own refs.
        let git_dir = cache.repo_dir("Owner", "Repo.rs");
        assert_eq!(pr.repo.git_dir.as_path(), git_dir);
        assert_eq!(
            git(&git_dir, &["rev-parse", "--is-bare-repository"]),
            "true"
        );
        assert_eq!(
            git(&git_dir, &["rev-parse", "refs/rozed/pull/1/head"]),
            remote.head
        );
        assert_eq!(
            git(&git_dir, &["rev-parse", "refs/rozed/base/main"]),
            remote.main_tip
        );
        assert_eq!(git(&git_dir, &["for-each-ref", "refs/heads"]), "");

        // A detached checkout of the head.
        assert_eq!(pr.checkout, cache.checkout_dir("Owner", "Repo.rs", 1));
        assert_eq!(git(&pr.checkout, &["rev-parse", "HEAD"]), remote.head);
        assert_eq!(git(&pr.checkout, &["status", "--porcelain"]), "");
        assert_eq!(
            git(&pr.checkout, &["rev-parse", "--abbrev-ref", "HEAD"]),
            "HEAD"
        );
        assert!(pr.checkout.join("src/new_name.rs").is_file());
        assert!(pr.checkout.join("added.txt").is_file());
        assert!(!pr.checkout.join("gone.txt").exists());

        // The old side reads from git.
        assert_eq!(
            pr.repo.read_file(&pr.base, "old_name.rs").unwrap(),
            rust_source("").into_bytes()
        );
        assert_eq!(pr.repo.read_file(&pr.base, "gone.txt").unwrap(), b"gone\n");
        assert!(pr.repo.read_file(&pr.base, "added.txt").is_err());
    }

    #[test]
    fn the_clone_is_partial() {
        let remote = Remote::new();
        let cache_dir = TempDir::new().unwrap();
        let cache = RepoCache::new(cache_dir.path());
        let pr = cache.prepare(&remote.source()).unwrap();
        let git_dir = pr.repo.git_dir.as_path();

        assert_eq!(git(git_dir, &["config", "remote.origin.promisor"]), "true");
        assert_eq!(
            git(git_dir, &["config", "remote.origin.partialclonefilter"]),
            "blob:none"
        );
        // The first version of keep.txt is in no tree rozed needed, so it was never downloaded.
        let first_keep = git(
            git_dir,
            &["rev-parse", &format!("{}:keep.txt", remote.first)],
        );
        let missing = git(
            git_dir,
            &[
                "rev-list",
                "--objects",
                "--missing=print",
                "refs/rozed/pull/1/head",
            ],
        );
        assert!(missing.contains(&format!("?{first_keep}")), "{missing}");
        // Reading it downloads it.
        assert_eq!(
            pr.repo.read_file(&remote.first, "keep.txt").unwrap(),
            b"keep, first version\n"
        );
    }

    #[test]
    fn preparing_again_follows_a_force_push() {
        let remote = Remote::new();
        let cache_dir = TempDir::new().unwrap();
        let cache = RepoCache::new(cache_dir.path());
        cache.prepare(&remote.source()).unwrap();

        let new_head = remote.force_push_new_head();
        let pr = cache.prepare(&remote.source()).unwrap();

        assert_eq!(pr.head, new_head);
        assert_eq!(pr.base, remote.base);
        assert_eq!(git(&pr.checkout, &["rev-parse", "HEAD"]), new_head);
        assert_eq!(
            fs::read_to_string(pr.checkout.join("added.txt")).unwrap(),
            "added, then amended\n"
        );
    }

    #[test]
    fn a_checkout_left_without_its_clone_is_replaced() {
        let remote = Remote::new();
        let cache_dir = TempDir::new().unwrap();
        let cache = RepoCache::new(cache_dir.path());
        let first = cache.prepare(&remote.source()).unwrap();
        fs::remove_dir_all(&first.repo.git_dir).unwrap();

        let pr = cache.prepare(&remote.source()).unwrap();

        assert_eq!(git(&pr.checkout, &["rev-parse", "HEAD"]), remote.head);
        assert!(pr.checkout.join("src/new_name.rs").is_file());
    }

    #[test]
    fn refuses_a_base_branch_that_isnt_a_branch_name() {
        let remote = Remote::new();
        let cache_dir = TempDir::new().unwrap();
        let cache = RepoCache::new(cache_dir.path());
        for base_branch in ["main:refs/heads/x", "a..b", "", "with space"] {
            let source = PullRequestSource {
                base_branch,
                ..remote.source()
            };
            let error = cache.prepare(&source).unwrap_err();
            assert!(
                format!("{error:#}").contains("isn't a valid branch name"),
                "{base_branch}: {error:#}"
            );
        }
    }

    #[test]
    fn a_missing_pull_request_is_an_error() {
        let remote = Remote::new();
        let cache_dir = TempDir::new().unwrap();
        let cache = RepoCache::new(cache_dir.path());
        let source = PullRequestSource {
            number: 2,
            ..remote.source()
        };
        let error = cache.prepare(&source).unwrap_err();
        assert!(
            format!("{error:#}").contains("refs/pull/2/head"),
            "{error:#}"
        );
    }

    #[test]
    fn parses_name_status() {
        let output = b"M\0keep.txt\0A\0added.txt\0D\0gone.txt\0R087\0old.rs\0src/new.rs\0T\0link\0";
        assert_eq!(
            parse_name_status(output).unwrap(),
            vec![
                (Some("keep.txt".into()), Some("keep.txt".into())),
                (None, Some("added.txt".into())),
                (Some("gone.txt".into()), None),
                (Some("old.rs".into()), Some("src/new.rs".into())),
                (Some("link".into()), Some("link".into())),
            ]
        );
        assert_eq!(parse_name_status(b"").unwrap(), vec![]);
        assert!(parse_name_status(b"R100\0only-one-path\0").is_err());
        assert!(parse_name_status(b"X\0what\0").is_err());
    }

    #[test]
    fn parses_numstat() {
        let output = b"1\t1\tkeep.txt\0-\t-\tdata.bin\x003\t1\t\0old.rs\0src/new.rs\0";
        assert_eq!(
            parse_numstat(output).unwrap(),
            vec![
                ("keep.txt".into(), false),
                ("data.bin".into(), true),
                ("src/new.rs".into(), false),
            ]
        );
        assert_eq!(parse_numstat(b"").unwrap(), vec![]);
        assert!(parse_numstat(b"1\t1\t\0only-old\0").is_err());
    }
}
