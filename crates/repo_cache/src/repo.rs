//! One repository's clone: fetching into it and reading from it.

use std::ffi::OsStr;
use std::fmt;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::cache::GitEnv;
use crate::cat_file::{CatFile, ReadError};
use crate::diff::{self, ChangedFile};
use crate::git::{GitCommand, Location, Token, failure_message};
use crate::lock::RepoLock;
use crate::oid::{self, Oid};
use crate::progress::{Progress, ProgressFn};
use crate::{Error, FetchFailure};

/// A bare partial clone in the cache, from [`crate::RepoCache::open`]. Cheap to clone; clones
/// share one `git cat-file` process. `Send` and `Sync`, and every method blocks while git runs, so
/// call them from a background task, one step at a time: fetch, list the changed files, read
/// them, and check out only for a trusted repository.
///
/// Everything rozed fetches goes under its own refs, keyed by pull request so that no two branch
/// names can collide, and nothing fetched is ever lost to a force-push:
///
/// - `refs/rozed/pull/<n>/head` and `refs/rozed/pull/<n>/base`: pull request `n`'s head and its
///   base branch's tip, as last fetched.
/// - `refs/rozed/pull/<n>/revs/<oid>`: every head of pull request `n` ever fetched.
/// - `refs/rozed/pull/<n>/bases/<oid>`: every tip of its base branch ever fetched.
/// - `refs/rozed/commits/<oid>`: commits fetched by ID.
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
    /// Where this repository's checkouts go.
    checkouts_dir: PathBuf,
    /// Held while writing to the clone or its checkouts.
    lock_path: PathBuf,
    token: Option<Token>,
    cat_file: Mutex<Option<CatFile>>,
}

/// A pull request to fetch, as GitHub's API describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequestRefs {
    pub number: u64,
    /// `head.sha`: the head GitHub showed, which rozed checks the fetched head against.
    pub head: Option<Oid>,
    /// `base.ref`: the branch it merges into.
    pub base_branch: String,
    /// `base.sha`: the base commit GitHub showed, used when the base branch no longer exists.
    pub base: Option<Oid>,
}

/// What [`Repo::fetch_pull`] fetched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchedPull {
    /// The head to show.
    pub head: Oid,
    /// Where `head` came from.
    pub head_source: HeadSource,
    /// The base branch's tip. The diff starts at its merge base with `head`.
    pub base_tip: Oid,
    pub base_source: BaseSource,
}

/// Which head [`Repo::fetch_pull`] chose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HeadSource {
    /// `refs/pull/<n>/head`, which held the head GitHub's API reported, or there was none to
    /// check.
    PullRef,
    /// The API's head, fetched by ID, because `refs/pull/<n>/head` held `pull_ref`. GitHub moves
    /// the ref a moment after a push, so the two can briefly differ.
    Api { pull_ref: Oid },
    /// `refs/pull/<n>/head`, because GitHub no longer has the API's head `api`.
    PullRefNotApi { api: Oid },
}

/// Where [`FetchedPull::base_tip`] came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BaseSource {
    /// The base branch.
    Branch,
    /// The API's base commit, because the base branch is gone, as when a lower layer of a stack
    /// was merged and its branch deleted.
    Api,
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
        checkouts_dir: PathBuf,
        lock_path: PathBuf,
        token: Option<Token>,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                env,
                name,
                git_dir,
                checkouts_dir,
                lock_path,
                token,
                cat_file: Mutex::new(None),
            }),
            progress: None,
        }
    }

    /// The clone's folder.
    pub fn git_dir(&self) -> &Path {
        &self.shared.git_dir
    }

    pub(crate) fn checkouts_dir(&self) -> &Path {
        &self.shared.checkouts_dir
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

    /// Fetches a pull request's head and its base branch's tip in one fetch, and pins both.
    ///
    /// The head comes from `refs/pull/<n>/head`, which holds the head of a pull request from a
    /// fork too. When GitHub's API reported another head, rozed fetches that one by ID and uses it,
    /// since it's what GitHub shows; [`FetchedPull::head_source`] says which it used. When the
    /// base branch is gone, the API's base commit stands in for its tip.
    pub fn fetch_pull(&self, pull: &PullRequestRefs) -> Result<FetchedPull, Error> {
        self.check_branch_name(&pull.base_branch)?;
        let number = pull.number;
        let what = format!("pull request #{number}");
        let head_ref = format!("refs/rozed/pull/{number}/head");
        let base_ref = format!("refs/rozed/pull/{number}/base");
        let head_spec = format!("+refs/pull/{number}/head:{head_ref}");
        let branch = format!("refs/heads/{}", pull.base_branch);

        let _lock = self.lock()?;
        let mut base_source = BaseSource::Branch;
        match self.fetch_locked(&what, &[&head_spec, &format!("+{branch}:{base_ref}")]) {
            Ok(()) => {}
            Err(Error::Fetch {
                failure: FetchFailure::RefNotFound,
                message,
                ..
            }) if message.contains(&branch) && pull.base.is_some() => {
                let base = pull.base.as_ref().expect("checked above");
                log::info!(
                    "{}: {what}'s base branch {} is gone, so using its base {base}",
                    self.shared.name,
                    pull.base_branch
                );
                match self.fetch_locked(&what, &[&head_spec, &format!("+{base}:{base_ref}")]) {
                    Err(Error::Fetch { message, .. }) if is_lost_commit(&message, base) => {
                        return Err(Error::Unavailable { oid: base.clone() });
                    }
                    result => result?,
                }
                base_source = BaseSource::Api;
            }
            Err(error) => return Err(error),
        }
        let pull_ref = self.resolve(&head_ref)?;
        let base_tip = self.resolve(&base_ref)?;
        self.pin(&revision_ref(number, &pull_ref), &pull_ref)?;
        self.pin(
            &format!("refs/rozed/pull/{number}/bases/{base_tip}"),
            &base_tip,
        )?;

        let (head, head_source) = match &pull.head {
            Some(api) if *api != pull_ref => {
                match self.fetch_by_id_locked(&what, api, &revision_ref(number, api)) {
                    Ok(()) => {
                        log::warn!(
                            "{}: GitHub's API says {what}'s head is {api}, but \
                             refs/pull/{number}/head is {pull_ref}; using the API's",
                            self.shared.name
                        );
                        (api.clone(), HeadSource::Api { pull_ref })
                    }
                    Err(Error::Unavailable { .. }) => {
                        log::warn!(
                            "{}: GitHub's API says {what}'s head is {api}, which GitHub no longer \
                             has; using refs/pull/{number}/head, {pull_ref}",
                            self.shared.name
                        );
                        let api = api.clone();
                        (pull_ref, HeadSource::PullRefNotApi { api })
                    }
                    Err(error) => return Err(error),
                }
            }
            _ => (pull_ref, HeadSource::PullRef),
        };
        Ok(FetchedPull {
            head,
            head_source,
            base_tip,
            base_source,
        })
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

    /// Makes sure commit `oid` is in the clone, fetching it by ID if it isn't, and pins it at
    /// `refs/rozed/commits/<oid>`. For a revision rozed knows from GitHub's API but never fetched.
    /// Fails with [`Error::Unavailable`] when GitHub no longer has it.
    pub fn fetch_commit(&self, oid: &Oid) -> Result<(), Error> {
        let _lock = self.lock()?;
        self.fetch_by_id_locked(
            &format!("commit {}", oid.short(7)),
            oid,
            &format!("refs/rozed/commits/{oid}"),
        )
    }

    /// Fetches commit `oid` by ID unless the clone has it, and pins it at `pin`. The caller holds
    /// the lock.
    fn fetch_by_id_locked(&self, what: &str, oid: &Oid, pin: &str) -> Result<(), Error> {
        if !self.has_commit(oid)? {
            match self.fetch_locked(what, &[&format!("+{oid}:{pin}")]) {
                Err(Error::Fetch { message, .. }) if is_lost_commit(&message, oid) => {
                    return Err(Error::Unavailable { oid: oid.clone() });
                }
                result => result?,
            }
        }
        // Also checks it's a commit.
        self.resolve(oid.as_str())?;
        self.pin(pin, oid)
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
            _ => Err(Error::from_git(display, failure_message(&finished), None)),
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
            _ => Err(Error::from_git(display, failure_message(&finished), None)),
        }
    }

    /// What changed from `from` to `to`, in git's order (by path), with renames detected, binary
    /// files marked, and each side's mode and blob. Downloads both sides of every changed file in
    /// one batch, so reading them afterwards needs no network. Submodules are never fetched.
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

    /// The contents of blob `oid`, such as a [`crate::FileVersion::oid`], downloading it if
    /// needed. `None` when there's no such object. Never pass a submodule's commit, which isn't in
    /// this repository.
    pub fn read_blob(&self, oid: &Oid) -> Result<Option<Vec<u8>>, Error> {
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
            match process.read(oid.as_str()) {
                Ok(Some(object)) if object.kind == "blob" => return Ok(Some(object.data)),
                Ok(Some(object)) => {
                    return Err(Error::Git {
                        command: display,
                        message: format!("{oid} is a {}, not a file", object.kind),
                    });
                }
                Ok(None) => return Ok(None),
                Err(ReadError::Died { stderr }) => {
                    *cat_file = None;
                    if attempt == 0 && stderr.trim().is_empty() {
                        continue;
                    }
                    log::warn!("{display} in {}: {}", self.shared.name, stderr.trim());
                    return Err(Error::from_git(display, stderr, None));
                }
                Err(ReadError::Io(error)) => {
                    *cat_file = None;
                    return Err(Error::io(format!("reading {oid}"))(error));
                }
            }
        }
        unreachable!("the loop returns on its second try")
    }

    /// The contents of the file at `path` in `rev`, a commit or a tree, downloading it if needed.
    /// `path` is bytes, as git stores it (see [`crate::FileVersion::path`]). `None` when there's
    /// no file there. A folder or a submodule there is an error.
    pub fn read_file(&self, rev: &Oid, path: &[u8]) -> Result<Option<Vec<u8>>, Error> {
        let shown = String::from_utf8_lossy(path);
        if path.is_empty() || path.contains(&0) {
            return Err(Error::Invalid {
                what: "path",
                value: shown.into_owned(),
            });
        }
        // ls-tree reads only trees, which a partial clone has, so it tells a submodule (a commit
        // in another repository, which must never be fetched from this one) from a file.
        let output = self.run(
            self.git()
                .args(["ls-tree", "-z", rev.as_str(), "--"])
                .arg(OsStr::from_bytes(path)),
        )?;
        let Some(entry) = output.strip_suffix(b"\0") else {
            return Ok(None);
        };
        let Some(tab) = entry.iter().position(|byte| *byte == b'\t') else {
            return Err(unexpected_ls_tree(entry));
        };
        let (info, found) = (&entry[..tab], &entry[tab + 1..]);
        if found != path {
            return Ok(None);
        }
        let mut info = std::str::from_utf8(info)
            .map_err(|_| unexpected_ls_tree(entry))?
            .split(' ');
        let (Some(mode), Some(kind), Some(oid)) = (info.next(), info.next(), info.next()) else {
            return Err(unexpected_ls_tree(entry));
        };
        if mode == "160000" || kind != "blob" {
            let what = if mode == "160000" { "submodule" } else { kind };
            return Err(Error::Git {
                command: format!("git ls-tree {rev} -- {shown}"),
                message: format!("{shown} is a {what}, not a file"),
            });
        }
        self.read_blob(&oid.parse()?)
    }

    /// Fails unless `name` is a branch name git accepts, which also keeps it from changing what
    /// a refspec means.
    fn check_branch_name(&self, name: &str) -> Result<(), Error> {
        let valid = !name.starts_with('-')
            && self
                .git_at(Location::Nowhere)
                .args(["check-ref-format", &format!("refs/heads/{name}")])
                .finish(None)?
                .status
                .success();
        if !valid {
            return Err(Error::Invalid {
                what: "branch name",
                value: name.to_string(),
            });
        }
        Ok(())
    }

    /// Fetches `refspecs` from GitHub. `what` names them in errors. The caller holds the lock.
    fn fetch_locked(&self, what: &str, refspecs: &[&str]) -> Result<(), Error> {
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
        let command = command.arg("origin").args(refspecs);
        let display = command.display();
        log::info!("{}: fetching {what}", self.shared.name);
        let finished = match &self.progress {
            Some(progress) => command.finish_with_progress(progress)?,
            None => command.finish(None)?,
        };
        if finished.status.success() {
            return Ok(());
        }
        let error = Error::from_git(display, failure_message(&finished), Some(what));
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

    /// Points `reference` at `oid`. The caller holds the lock.
    fn pin(&self, reference: &str, oid: &Oid) -> Result<(), Error> {
        self.run(self.git().args(["update-ref", reference, oid.as_str()]))?;
        Ok(())
    }

    /// Waits for the lock on this repository's clone and checkouts.
    pub(crate) fn lock(&self) -> Result<RepoLock, Error> {
        RepoLock::acquire(&self.shared.lock_path)
    }

    /// A git command on the clone, signed in when the repository has a token.
    pub(crate) fn git(&self) -> GitCommand {
        self.git_at(Location::Clone(&self.shared.git_dir))
    }

    /// A git command on the repository at `location`, such as a checkout of the clone, signed in
    /// like [`Self::git`].
    pub(crate) fn git_at(&self, location: Location<'_>) -> GitCommand {
        self.shared
            .env
            .command(location, self.shared.token.as_ref())
    }

    /// Moves `path`, in the cache, out of the way and deletes it.
    pub(crate) fn trash(&self, path: &Path) -> Result<(), Error> {
        self.shared.env.trash(path)
    }

    /// Runs `command`. Its errors say when a download failed, since any command can download
    /// in a partial clone.
    pub(crate) fn run(&self, command: GitCommand) -> Result<Vec<u8>, Error> {
        let display = command.display();
        let finished = command.finish(None)?;
        if finished.status.success() {
            Ok(finished.stdout)
        } else {
            Err(Error::from_git(display, failure_message(&finished), None))
        }
    }
}

/// Whether a fetch failed with `message` because the server doesn't have commit `oid`, which was
/// asked for by ID.
fn is_lost_commit(message: &str, oid: &Oid) -> bool {
    (message.contains("not our ref") || message.contains("unadvertised object"))
        && message.contains(oid.as_str())
}

/// Where pull request `number`'s head `oid` is pinned.
fn revision_ref(number: u64, oid: &Oid) -> String {
    format!("refs/rozed/pull/{number}/revs/{oid}")
}

fn unexpected_ls_tree(entry: &[u8]) -> Error {
    Error::Git {
        command: "git ls-tree".into(),
        message: format!(
            "printed an unexpected entry {:?}",
            String::from_utf8_lossy(entry)
        ),
    }
}
