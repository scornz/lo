//! One repository's clone: fetching into it and reading from it.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::Error;
use crate::cache::GitEnv;
use crate::cat_file::{CatFile, ReadError};
use crate::diff::{self, ChangedFile};
use crate::git::{GitCommand, Token};
use crate::oid::{self, Oid};
use crate::progress::{Progress, ProgressFn};

/// A bare partial clone in the cache, from [`crate::RepoCache::open`]. Cheap to clone; clones
/// share one `git cat-file` process.
///
/// Everything rozed fetches goes under its own refs, so nothing collides with GitHub's names and
/// nothing fetched is ever lost to a force-push or to `git gc`:
///
/// - `refs/rozed/pull/<n>/head`: pull request `n`'s head as last fetched.
/// - `refs/rozed/pull/<n>/revs/<oid>`: every head of pull request `n` ever fetched.
/// - `refs/rozed/branches/<name>`: branch `name` as last fetched.
/// - `refs/rozed/commits/<oid>`: commits fetched by ID.
///
/// Every method blocks while git runs, so call them off the UI thread.
#[derive(Clone)]
pub struct Repo {
    shared: Arc<Shared>,
    progress: Option<ProgressFn>,
}

struct Shared {
    env: Arc<GitEnv>,
    /// `owner/repo`, for logs.
    name: String,
    git_dir: PathBuf,
    token: Option<Token>,
    cat_file: Mutex<Option<CatFile>>,
    /// Held while fetching, so two fetches into one clone don't race for its refs.
    fetching: Mutex<()>,
}

impl fmt::Debug for Repo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Repo")
            .field("name", &self.shared.name)
            .field("git_dir", &self.shared.git_dir)
            .field("signed_in", &self.shared.token.is_some())
            .finish()
    }
}

impl Repo {
    pub(crate) fn new(
        env: Arc<GitEnv>,
        name: String,
        git_dir: PathBuf,
        token: Option<Token>,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                env,
                name,
                git_dir,
                token,
                cat_file: Mutex::new(None),
                fetching: Mutex::new(()),
            }),
            progress: None,
        }
    }

    /// The clone's folder.
    pub fn git_dir(&self) -> &Path {
        &self.shared.git_dir
    }

    /// This repository, reporting the progress of its fetches to `progress`. The first fetch
    /// into a new clone downloads the history it needs, which takes a while for a big
    /// repository.
    pub fn with_progress(&self, progress: impl Fn(Progress) + Send + Sync + 'static) -> Self {
        Self {
            shared: self.shared.clone(),
            progress: Some(Arc::new(progress)),
        }
    }

    /// Fetches pull request `number`'s head from `refs/pull/<n>/head`, which holds the head of a
    /// pull request from a fork too. Keeps it at `refs/rozed/pull/<n>/head`, pins it at
    /// `refs/rozed/pull/<n>/revs/<oid>`, and returns it.
    pub fn fetch_pull(&self, number: u64) -> Result<Oid, Error> {
        let head = format!("refs/rozed/pull/{number}/head");
        self.fetch(
            &format!("pull request #{number}"),
            &format!("+refs/pull/{number}/head:{head}"),
        )?;
        let oid = self.resolve(&head)?;
        self.pin(&format!("refs/rozed/pull/{number}/revs/{oid}"), &oid)?;
        Ok(oid)
    }

    /// Every head of pull request `number` fetched so far, in no particular order.
    pub fn pull_revisions(&self, number: u64) -> Result<Vec<Oid>, Error> {
        let output = self.run(self.git().args([
            "for-each-ref",
            "--format=%(objectname)",
            &format!("refs/rozed/pull/{number}/revs/"),
        ]))?;
        String::from_utf8_lossy(&output)
            .lines()
            .map(str::parse)
            .collect()
    }

    /// Fetches branch `name` into `refs/rozed/branches/<name>` and returns its tip.
    pub fn fetch_branch(&self, name: &str) -> Result<Oid, Error> {
        let valid = self
            .git()
            .args(["check-ref-format", &format!("refs/heads/{name}")])
            .finish(None)?
            .status
            .success();
        if !valid || name.starts_with('-') {
            return Err(Error::Invalid {
                what: "branch name",
                value: name.to_string(),
            });
        }
        let branch = format!("refs/rozed/branches/{name}");
        self.fetch(
            &format!("branch {name}"),
            &format!("+refs/heads/{name}:{branch}"),
        )?;
        self.resolve(&branch)
    }

    /// Makes sure commit `oid` is in the clone, fetching it by ID if it isn't, and pins it at
    /// `refs/rozed/commits/<oid>`. For a revision rozed knows from GitHub's API but never fetched.
    /// Fails with [`Error::Unavailable`] when GitHub no longer has it.
    pub fn fetch_commit(&self, oid: &Oid) -> Result<(), Error> {
        let pin = format!("refs/rozed/commits/{oid}");
        if !self.has_commit(oid)? {
            match self.fetch(&format!("commit {}", oid.short(7)), &format!("{oid}:{pin}")) {
                Err(Error::Fetch { message, .. })
                    if message.contains("not our ref")
                        || message.contains("unadvertised object") =>
                {
                    return Err(Error::Unavailable { oid: oid.clone() });
                }
                result => result?,
            }
        }
        // Checks it's a commit, too.
        self.resolve(oid.as_str())?;
        self.pin(&pin, oid)
    }

    /// Whether commit `oid` is in the clone. Never downloads anything.
    fn has_commit(&self, oid: &Oid) -> Result<bool, Error> {
        // `--missing` stops rev-list from fetching what's missing.
        let finished = self
            .git()
            .args([
                "rev-list",
                "--missing=allow-any",
                "--no-walk",
                oid.as_str(),
                "--",
            ])
            .finish(None)?;
        Ok(finished.status.success()
            && String::from_utf8_lossy(&finished.stdout).trim() == oid.as_str())
    }

    /// The best common ancestor of `a` and `b`, or `None` when they share no history.
    pub fn merge_base(&self, a: &Oid, b: &Oid) -> Result<Option<Oid>, Error> {
        let command = self.git().args(["merge-base", a.as_str(), b.as_str()]);
        let display = command.display();
        let finished = command.finish(None)?;
        match finished.status.code() {
            Some(0) => oid::parse_output(&finished.stdout).map(Some),
            Some(1) if finished.stderr.trim().is_empty() => Ok(None),
            _ => Err(Error::from_git(
                display,
                crate::git::failure_message(&finished),
                None,
            )),
        }
    }

    /// Whether `ancestor` is `descendant` or one of its ancestors.
    pub fn is_ancestor(&self, ancestor: &Oid, descendant: &Oid) -> Result<bool, Error> {
        let command = self.git().args([
            "merge-base",
            "--is-ancestor",
            ancestor.as_str(),
            descendant.as_str(),
        ]);
        let display = command.display();
        let finished = command.finish(None)?;
        match finished.status.code() {
            Some(0) => Ok(true),
            Some(1) if finished.stderr.trim().is_empty() => Ok(false),
            _ => Err(Error::from_git(
                display,
                crate::git::failure_message(&finished),
                None,
            )),
        }
    }

    /// What changed from `from` to `to`, in git's order (by path), with renames detected and
    /// binary files marked. Downloads both sides of every changed file in one batch, so reading
    /// them afterwards needs no network.
    pub fn changed_files(&self, from: &Oid, to: &Oid) -> Result<Vec<ChangedFile>, Error> {
        let output = self.run(self.git().args([
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "-z",
            "-M",
            "--raw",
            "--numstat",
            "--no-abbrev",
            from.as_str(),
            to.as_str(),
            "--",
        ]))?;
        diff::parse(&output)
    }

    /// The contents of the file at `path` in `rev` (a commit or a tree), downloading it if
    /// needed. `None` when there's no such file.
    pub fn read_blob(&self, rev: &Oid, path: &str) -> Result<Option<Vec<u8>>, Error> {
        if path.is_empty() || path.contains('\0') {
            return Err(Error::Invalid {
                what: "path",
                value: path.to_string(),
            });
        }
        // `cat-file --batch` reads one name a line, so a path with a line break goes by its ID.
        let object = if path.contains(['\n', '\r']) {
            match self.resolve_path(rev, path)? {
                Some(oid) => oid.to_string(),
                None => return Ok(None),
            }
        } else {
            format!("{rev}:{path}")
        };
        let mut cat_file = self
            .shared
            .cat_file
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        // A second try, for a process that had already exited without saying why.
        for attempt in 0..2 {
            if cat_file.is_none() {
                *cat_file = Some(CatFile::spawn(self.git().args(["cat-file", "--batch"]))?);
            }
            let process = cat_file.as_mut().expect("started above");
            let display = process.display().to_string();
            match process.read(&object) {
                Ok(Some(object)) if object.kind == "blob" => return Ok(Some(object.data)),
                Ok(Some(object)) => {
                    return Err(Error::Git {
                        command: display,
                        message: format!("{rev}:{path} is a {}, not a file", object.kind),
                    });
                }
                Ok(None) => return Ok(None),
                Err(ReadError::Died { stderr }) => {
                    *cat_file = None;
                    if attempt == 0 && stderr.trim().is_empty() {
                        continue;
                    }
                    log::warn!("{display} for {}: {}", self.shared.name, stderr.trim());
                    return Err(Error::from_git(display, stderr, None));
                }
                Err(ReadError::Io(error)) => {
                    *cat_file = None;
                    return Err(Error::io(format!("reading {rev}:{path}"))(error));
                }
            }
        }
        unreachable!("the loop returns on its second try")
    }

    /// The ID of the object at `path` in `rev`, without reading it.
    fn resolve_path(&self, rev: &Oid, path: &str) -> Result<Option<Oid>, Error> {
        let finished = self
            .git()
            .args(["rev-parse", "--verify", "--quiet", "--end-of-options"])
            .arg(format!("{rev}:{path}"))
            .finish(None)?;
        if !finished.status.success() {
            return Ok(None);
        }
        oid::parse_output(&finished.stdout).map(Some)
    }

    /// Fetches `refspec` from GitHub. `what` names it in errors.
    fn fetch(&self, what: &str, refspec: &str) -> Result<(), Error> {
        let _fetching = self
            .shared
            .fetching
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let command = self.git().args([
            "fetch",
            "--no-tags",
            "--no-write-fetch-head",
            "--recurse-submodules=no",
            "--filter=blob:none",
        ]);
        let command = match &self.progress {
            Some(_) => command.arg("--progress"),
            None => command.arg("--quiet"),
        };
        let command = command.args(["origin", refspec]);
        let display = command.display();
        log::info!("{}: fetching {what}", self.shared.name);
        let finished = match &self.progress {
            Some(progress) => command.finish_with_progress(progress)?,
            None => command.finish(None)?,
        };
        if finished.status.success() {
            return Ok(());
        }
        let error = Error::from_git(display, crate::git::failure_message(&finished), Some(what));
        log::warn!("{}: {error}", self.shared.name);
        Err(error)
    }

    /// The commit `rev` names.
    fn resolve(&self, rev: &str) -> Result<Oid, Error> {
        let output = self.run(self.git().args([
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{rev}^{{commit}}"),
        ]))?;
        oid::parse_output(&output)
    }

    /// Points `reference` at `oid`.
    fn pin(&self, reference: &str, oid: &Oid) -> Result<(), Error> {
        self.run(self.git().args(["update-ref", reference, oid.as_str()]))?;
        Ok(())
    }

    /// A git command in the clone, signed in when the repository has a token.
    pub(crate) fn git(&self) -> GitCommand {
        self.shared
            .env
            .command(&self.shared.git_dir, self.shared.token.as_ref())
    }

    /// Runs `command`. Its errors say when a download failed, since any command can download
    /// in a partial clone.
    pub(crate) fn run(&self, command: GitCommand) -> Result<Vec<u8>, Error> {
        let display = command.display();
        let finished = command.finish(None)?;
        if finished.status.success() {
            Ok(finished.stdout)
        } else {
            Err(Error::from_git(
                display,
                crate::git::failure_message(&finished),
                None,
            ))
        }
    }
}
