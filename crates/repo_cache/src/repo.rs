//! One repository's clone: fetching into it and reading from it.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fmt;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use sha2::{Digest as _, Sha256};

use crate::cache::{GitEnv, stamp_last_used};
use crate::cat_file::{CatFile, ReadError};
use crate::diff::{self, ChangedFile, FileVersion};
use crate::git::{GitCommand, GitVersion, Location, Token, failure_message};
use crate::lock::RepoLock;
use crate::oid::{self, Oid};
use crate::progress::{Progress, ProgressFn};
use crate::{Error, FetchFailure};

/// A bare partial clone in the cache, from [`crate::RepoCache::open`]. Cheap to clone; clones
/// share one `git cat-file` process. `Send` and `Sync`, and every method blocks while git runs, so
/// call them from a background task, one step at a time: fetch, list the changed files, read
/// them, and check out only for a trusted repository.
///
/// Everything Lo fetches goes under its own refs, keyed by pull request so that no two branch
/// names can collide, and nothing fetched is ever lost to a force-push:
///
/// - `refs/lo/pull/<n>/head` and `refs/lo/pull/<n>/base`: pull request `n`'s head and its
///   base branch's tip, as last fetched.
/// - `refs/lo/pull/<n>/revs/<oid>`: every head of pull request `n` ever fetched.
/// - `refs/lo/pull/<n>/bases/<oid>`: every tip of its base branch ever fetched.
/// - `refs/lo/pull/<n>/branch-bases/<hash>`: the tip of each branch pull request `n` was
///   fetched against, as last fetched, by the SHA-256 of the branch's name (any branch name fits
///   in one ref then, and none is another's folder).
/// - `refs/lo/commits/<oid>`: commits fetched by ID.
/// - `refs/lo/replays/<tree>/<process ID>`: a tree [`Repo::compare_revisions`] replayed,
///   while that process has the clone open (see [`crate::Comparison`]).
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
    /// Held shared while this repository is open, so cleanup leaves it alone.
    _in_use: RepoLock,
    token: Option<Token>,
    access: Access,
    cat_file: Mutex<Option<CatFile>>,
    /// The replayed trees this open clone has pinned, each once, never unpinned while it's open.
    replays: Mutex<HashSet<Oid>>,
}

/// What a [`Repo`] may do with its clone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Access {
    /// Fetch into it, check out from it, and download what reading needs.
    Fetch,
    /// Read what's there, offline: no writes, no lock, and no downloads where git can be told so
    /// ([`GitVersion::NO_LAZY_FETCH`]).
    ReadOffline,
}

/// A pull request to fetch, as GitHub's API describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequestRefs {
    pub number: u64,
    /// `head.sha`: the head GitHub showed, which Lo checks the fetched head against.
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
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        env: Arc<GitEnv>,
        name: String,
        git_dir: PathBuf,
        checkouts_dir: PathBuf,
        lock_path: PathBuf,
        in_use: RepoLock,
        token: Option<Token>,
        access: Access,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                env,
                name,
                git_dir,
                checkouts_dir,
                lock_path,
                _in_use: in_use,
                token,
                access,
                cat_file: Mutex::new(None),
                replays: Mutex::new(HashSet::new()),
            }),
            progress: None,
        }
    }

    /// The clone's folder.
    pub fn git_dir(&self) -> &Path {
        &self.shared.git_dir
    }

    /// `owner/repo`, for logs.
    pub(crate) fn name(&self) -> &str {
        &self.shared.name
    }

    pub(crate) fn checkouts_dir(&self) -> &Path {
        &self.shared.checkouts_dir
    }

    /// The replayed trees this open clone has pinned (see [`crate::Comparison`]).
    pub(crate) fn pinned_replays(&self) -> &Mutex<HashSet<Oid>> {
        &self.shared.replays
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
    /// fork too. When GitHub's API reported another head, Lo fetches that one by ID and uses it,
    /// since it's what GitHub shows; [`FetchedPull::head_source`] says which it used. When the
    /// base branch is gone, the API's base commit stands in for its tip.
    pub fn fetch_pull(&self, pull: &PullRequestRefs) -> Result<FetchedPull, Error> {
        self.check_branch_name(&pull.base_branch)?;
        let number = pull.number;
        let what = format!("pull request #{number}");
        let head_ref = format!("refs/lo/pull/{number}/head");
        let base_ref = format!("refs/lo/pull/{number}/base");
        let head_spec = format!("+refs/pull/{number}/head:{head_ref}");
        let branch = format!("refs/heads/{}", pull.base_branch);

        let _lock = self.lock()?;
        self.stamp_used()?;
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
        self.pin(&base_ref_for(number, &base_tip), &base_tip)?;
        self.pin(&branch_base_ref(number, &pull.base_branch), &base_tip)?;

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
            &format!("refs/lo/pull/{number}/revs/"),
        ]))?;
        String::from_utf8_lossy(&output)
            .lines()
            .map(str::parse)
            .collect()
    }

    /// Whether `base` was ever fetched as the tip of pull request `number`'s base branch, so
    /// the clone has it pinned. Reads only refs, so it works offline.
    pub fn has_base(&self, number: u64, base: &Oid) -> Result<bool, Error> {
        Ok(self.pinned(&base_ref_for(number, base))?.as_ref() == Some(base))
    }

    /// The tip of `branch` as last fetched as pull request `number`'s base, or `None` when the
    /// pull request was never fetched against that branch. Reads only refs, so it works
    /// offline.
    pub fn branch_base(&self, number: u64, branch: &str) -> Result<Option<Oid>, Error> {
        self.pinned(&branch_base_ref(number, branch))
    }

    /// The commit pinned at exactly `reference`, if any.
    fn pinned(&self, reference: &str) -> Result<Option<Oid>, Error> {
        let output =
            self.run(
                self.git()
                    .args(["for-each-ref", "--format=%(objectname)", reference]),
            )?;
        String::from_utf8_lossy(&output)
            .lines()
            .next()
            .map(str::parse)
            .transpose()
    }

    /// Whether the clone has both sides of every file that changed from `from` to `to`, so their
    /// diff and contents read without downloading anything. Never downloads anything itself.
    /// Submodules aren't in this repository and don't count.
    pub fn has_diff_contents(&self, from: &Oid, to: &Oid) -> Result<bool, Error> {
        // Without rename detection or line counts, git compares trees alone, which a partial
        // clone always has, and reads no file.
        let output = self.run(self.git().args([
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--no-renames",
            "-z",
            "--raw",
            "--no-abbrev",
            from.as_str(),
            to.as_str(),
            "--",
        ]))?;
        let blobs = raw_diff_blobs(&output)?;
        if blobs.is_empty() {
            return Ok(true);
        }
        let missing = if self.shared.env.git.version() >= GitVersion::NO_LAZY_FETCH {
            // Told not to download, cat-file says "missing" for what isn't there.
            let input: String = blobs.iter().map(|oid| format!("{oid}\n")).collect();
            let output = self
                .git()
                .env(NO_LAZY_FETCH_VAR, "1")
                .args(["cat-file", "--batch-check=%(objectname)"])
                .run_with_input(Some(input.as_bytes()))?;
            String::from_utf8_lossy(&output)
                .lines()
                .filter_map(|line| line.strip_suffix(" missing"))
                .map(str::parse)
                .collect::<Result<Vec<Oid>, _>>()?
        } else {
            // Older git can't be told so, but rev-list's `--missing` never downloads: it lists
            // both trees' files, marking those the clone lacks with `?`.
            let output = self.run(self.git().args([
                "rev-list",
                "--objects",
                "--missing=print",
                "--no-walk",
                from.as_str(),
                to.as_str(),
                "--",
            ]))?;
            String::from_utf8_lossy(&output)
                .lines()
                .filter_map(|line| line.strip_prefix('?'))
                .map(str::parse)
                .collect::<Result<Vec<Oid>, _>>()?
        };
        Ok(!blobs.iter().any(|blob| missing.contains(blob)))
    }

    /// Makes sure commit `oid` is in the clone, fetching it by ID if it isn't, and pins it at
    /// `refs/lo/commits/<oid>`. For a revision Lo knows from GitHub's API but never fetched.
    /// Fails with [`Error::Unavailable`] when GitHub no longer has it.
    pub fn fetch_commit(&self, oid: &Oid) -> Result<(), Error> {
        let _lock = self.lock()?;
        self.stamp_used()?;
        self.fetch_by_id_locked(
            &format!("commit {}", oid.short(7)),
            oid,
            &format!("refs/lo/commits/{oid}"),
        )
    }

    /// Makes sure every head in `heads` is in the clone and pins each at
    /// `refs/lo/pull/<number>/revs/<oid>`, as one of pull request `number`'s revisions. Those
    /// the clone lacks are fetched by ID in one fetch. Returns the ones GitHub no longer has
    /// (design §11, "Missing revisions"), which are left out; any other failure fails the whole
    /// fetch.
    ///
    /// GitHub stops a fetch at the first commit it doesn't have, naming it, so each lost one costs
    /// a fetch of the rest again. The clone remembers them per pull request
    /// ([`Self::unavailable_revisions`]), so they're never asked for again.
    pub fn fetch_commits(&self, number: u64, heads: &[Oid]) -> Result<Vec<Oid>, Error> {
        let _lock = self.lock()?;
        self.stamp_used()?;
        let mut wanted: Vec<&Oid> = Vec::new();
        for oid in heads {
            if !wanted.contains(&oid) {
                wanted.push(oid);
            }
        }
        let lost_before = self.remembered_unavailable(number)?;
        let present = self.commits_present(&wanted)?;
        let mut unavailable: Vec<Oid> = Vec::new();
        let mut missing: Vec<Oid> = Vec::new();
        for (oid, present) in wanted.iter().zip(&present) {
            match (present, lost_before.contains(oid)) {
                (true, _) => {}
                (false, true) => unavailable.push((*oid).clone()),
                (false, false) => missing.push((*oid).clone()),
            }
        }
        let what = match missing.as_slice() {
            [oid] => format!("pull request #{number}'s revision {}", oid.short(7)),
            _ => format!("{} of pull request #{number}'s revisions", missing.len()),
        };
        let mut newly_lost = Vec::new();
        while !missing.is_empty() {
            let refspecs: Vec<String> = (missing.iter())
                .map(|oid| format!("+{oid}:{}", revision_ref(number, oid)))
                .collect();
            let refspecs: Vec<&str> = refspecs.iter().map(String::as_str).collect();
            match self.fetch_locked(&what, &refspecs) {
                Ok(()) => break,
                Err(Error::Fetch { message, .. })
                    if let Some(lost) =
                        (missing.iter()).position(|oid| is_lost_commit(&message, oid)) =>
                {
                    newly_lost.push(missing.remove(lost));
                }
                Err(error) => return Err(error),
            }
        }
        // Heads the clone has now, from another pull request's fetch say, aren't lost any more.
        let still_lost: Vec<&Oid> = (lost_before.iter())
            .filter(|oid| !wanted.contains(oid) || unavailable.contains(oid))
            .collect();
        let still_lost = (still_lost.iter().zip(self.commits_present(&still_lost)?))
            .filter(|(_, present)| !present)
            .map(|(oid, _)| (*oid).clone());
        let lost: Vec<Oid> = still_lost.chain(newly_lost.iter().cloned()).collect();
        if lost != lost_before {
            self.remember_unavailable(number, &lost)?;
        }
        unavailable.extend(newly_lost);
        let pins: Vec<&Oid> = (wanted.into_iter())
            .filter(|oid| !unavailable.contains(oid))
            .collect();
        // Also checks each is a commit.
        if let Some(position) = (self.commits_present(&pins)?.iter()).position(|present| !present) {
            return Err(Error::Git {
                command: "git fetch".into(),
                message: format!("{} isn't a commit", pins[position]),
            });
        }
        let input: String = (pins.iter())
            .map(|oid| format!("update {} {oid}\n", revision_ref(number, oid)))
            .collect();
        if !input.is_empty() {
            (self.git().args(["update-ref", "--stdin"])).run_with_input(Some(input.as_bytes()))?;
        }
        Ok(unavailable)
    }

    /// The heads of pull request `number` that GitHub no longer had when
    /// [`Self::fetch_commits`] asked for them, and that the clone still lacks. Reads only the
    /// clone, so it works offline.
    pub fn unavailable_revisions(&self, number: u64) -> Result<Vec<Oid>, Error> {
        let remembered = self.remembered_unavailable(number)?;
        let remembered_refs: Vec<&Oid> = remembered.iter().collect();
        let present = self.commits_present(&remembered_refs)?;
        Ok((remembered.iter().zip(present))
            .filter(|(_, present)| !present)
            .map(|(oid, _)| oid.clone())
            .collect())
    }

    /// The heads of pull request `number` noted as lost. A line that isn't an ID, as a write cut
    /// short could leave, is skipped.
    fn remembered_unavailable(&self, number: u64) -> Result<Vec<Oid>, Error> {
        let path = self.unavailable_file(number);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(Error::io(format!("reading {}", path.display()))(error)),
        };
        Ok(text
            .lines()
            .filter_map(|line| match line.parse() {
                Ok(oid) => Some(oid),
                Err(_) => {
                    log::warn!(
                        "{}: skipping {line:?} in {}",
                        self.shared.name,
                        path.display()
                    );
                    None
                }
            })
            .collect())
    }

    /// Notes `lost` as pull request `number`'s heads GitHub no longer has, replacing the file
    /// whole, so a reader never sees half of it. The caller holds the lock.
    fn remember_unavailable(&self, number: u64, lost: &[Oid]) -> Result<(), Error> {
        let path = self.unavailable_file(number);
        let dir = path.parent().expect("the file is in a folder");
        std::fs::create_dir_all(dir).map_err(Error::io(format!("creating {}", dir.display())))?;
        let text: String = lost.iter().map(|oid| format!("{oid}\n")).collect();
        let temporary = dir.join(format!(".pull-{number}.{}", std::process::id()));
        std::fs::write(&temporary, text)
            .map_err(Error::io(format!("writing {}", temporary.display())))?;
        std::fs::rename(&temporary, &path)
            .map_err(Error::io(format!("replacing {}", path.display())))
    }

    fn unavailable_file(&self, number: u64) -> PathBuf {
        (self.shared.git_dir)
            .join(UNAVAILABLE_DIR)
            .join(format!("pull-{number}"))
    }

    /// Whether each of `oids` is a commit in the clone, in one `git cat-file`, never downloading
    /// anything. Older git can't be told not to download, so it asks one by one with `rev-list`.
    fn commits_present(&self, oids: &[&Oid]) -> Result<Vec<bool>, Error> {
        if oids.is_empty() {
            return Ok(Vec::new());
        }
        if self.shared.env.git.version() < GitVersion::NO_LAZY_FETCH {
            return oids.iter().map(|oid| self.has_commit(oid)).collect();
        }
        let input: String = oids.iter().map(|oid| format!("{oid}\n")).collect();
        let output = self
            .git()
            .env(NO_LAZY_FETCH_VAR, "1")
            .args(["cat-file", "--batch-check=%(objectname) %(objecttype)"])
            .run_with_input(Some(input.as_bytes()))?;
        let output = String::from_utf8_lossy(&output);
        let mut lines = output.lines();
        Ok(oids
            .iter()
            .map(|oid| {
                let line = lines.next().unwrap_or_default();
                line.strip_prefix(oid.as_str()) == Some(" commit")
            })
            .collect())
    }

    /// The commits on `head` that `base` doesn't have, oldest first and each after its parents
    /// (`git rev-list --topo-order --reverse <base>..<head>`): with its base as `base`, a pull
    /// request's current commits, as `review::revisions::build_revisions` takes them. For a
    /// merged pull request, that's its base as it was (`review::revisions::base_tip_at`), since
    /// today's base branch holds every commit a merge commit or a fast-forward merged. Reads only
    /// commits, which the clone keeps whole, so it works offline. Both must be in the clone
    /// already: a missing one is [`Error::NotFetched`], and is never downloaded.
    pub fn pull_commits(&self, base: &Oid, head: &Oid) -> Result<Vec<Oid>, Error> {
        for oid in [base, head] {
            if !self.has_commit(oid)? {
                return Err(Error::NotFetched { oid: oid.clone() });
            }
        }
        let output = self.run(self.git().args([
            "rev-list",
            "--topo-order",
            "--reverse",
            &format!("{base}..{head}"),
            "--",
        ]))?;
        String::from_utf8_lossy(&output)
            .lines()
            .map(str::parse)
            .collect()
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

    /// Whether commit `oid` is in the clone. Never downloads anything: check before asking git
    /// about a commit that may be missing, which it would download with all its history.
    pub fn has_commit(&self, oid: &Oid) -> Result<bool, Error> {
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

    /// What changed from `from` to `to`, commits or trees, in git's order (by path), with renames
    /// detected, binary files marked, and each side's mode, blob and size. Downloads both sides of
    /// every changed file in one batch, so reading them afterwards needs no network. Submodules
    /// are never fetched.
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
        let mut files = diff::parse(&output)?;
        self.fill_in_sizes(&mut files)?;
        Ok(files)
    }

    /// Sets the size of each side of `files` but submodules', from one `git cat-file
    /// --batch-check`. The diff downloaded them already, so this reads only what's on disk.
    fn fill_in_sizes(&self, files: &mut [ChangedFile]) -> Result<(), Error> {
        let mut versions: Vec<_> = files
            .iter_mut()
            .flat_map(|file| [file.old.as_mut(), file.new.as_mut()])
            .flatten()
            .filter(|version| version.mode != diff::FileMode::Submodule)
            .collect();
        if versions.is_empty() {
            return Ok(());
        }
        let input: String = versions
            .iter()
            .map(|version| format!("{}\n", version.oid))
            .collect();
        let output = self
            .git()
            .args(["cat-file", "--batch-check=%(objectname) %(objectsize)"]);
        let output = output.run_with_input(Some(input.as_bytes()))?;
        let output = String::from_utf8_lossy(&output);
        let mut lines = output.lines();
        for version in &mut versions {
            let line = lines.next().unwrap_or_default();
            // "<oid> <size>", or "<oid> missing" for a blob the server no longer has.
            version.size = line
                .strip_prefix(version.oid.as_str())
                .and_then(|rest| rest.trim().parse().ok());
        }
        Ok(())
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
        let Some(entry) = self.tree_entry(rev, path)? else {
            return Ok(None);
        };
        if !entry.is_blob() {
            let what = if entry.mode == "160000" {
                "submodule"
            } else {
                entry.kind.as_str()
            };
            return Err(Error::Git {
                command: format!("git ls-tree {rev} -- {shown}"),
                message: format!("{shown} is a {what}, not a file"),
            });
        }
        self.read_blob(&entry.oid)
    }

    /// What's at `path` in `rev`, a commit or a tree, or `None` when nothing is. It reads only
    /// trees, which a partial clone has, so it downloads nothing, and it tells a submodule (a
    /// commit in another repository, which must never be fetched from this one) from a file.
    fn tree_entry(&self, rev: &Oid, path: &[u8]) -> Result<Option<TreeEntry>, Error> {
        let shown = String::from_utf8_lossy(path);
        if path.is_empty() || path.contains(&0) {
            return Err(Error::Invalid {
                what: "path",
                value: shown.into_owned(),
            });
        }
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
        Ok(Some(TreeEntry {
            mode: mode.to_string(),
            kind: kind.to_string(),
            oid: oid.parse()?,
        }))
    }

    /// The unified diff of the file at `path` from `base` to `head`, with three lines of context,
    /// as GitHub gives a pull request's changed file as its `patch`: the lines of its hunks are
    /// the ones GitHub takes comments on (`review::threads::Hunk::parse` reads it, through
    /// `String::from_utf8_lossy`). For a file whose patch GitHub leaves out, and for a revision
    /// other than the latest, whose patch GitHub doesn't give.
    ///
    /// - `base` is where the diff starts: for a revision's whole diff, its merge base with the
    ///   base branch, as GitHub diffs a pull request.
    /// - `old_path` is the file's path in `base` when it was renamed, as `changed_files` and
    ///   GitHub's `previous_filename` give it. Its two versions are diffed whatever their
    ///   similarity, and the diff's header names them by blob.
    /// - It's git's myers diff, with the indent heuristic, as git diffs by default, and with no
    ///   diff settings but these: not the user's (no git Lo runs reads them), and not any the
    ///   clone has. No external diff, no text conversion, no color.
    /// - Like GitHub's, it ignores the repository's attributes. GitHub gives a text patch for a
    ///   file `.gitattributes` marks `binary` (`-diff`), such as yarnpkg/berry's `.pnp.cjs`.
    ///
    /// It downloads the file's two versions, if they aren't here. It's empty when the file is
    /// the same on both sides, or in neither. A binary file has no hunks: git says only that it
    /// differs. A submodule's two commits show as `Subproject commit` lines, as GitHub's do.
    pub fn patch(
        &self,
        base: &Oid,
        head: &Oid,
        path: &[u8],
        old_path: Option<&[u8]>,
    ) -> Result<Vec<u8>, Error> {
        let command = self.git().args([
            // The one diff setting without an option to override it: a blank unchanged line
            // keeps its space.
            "-c",
            "diff.suppressBlankEmpty=false",
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--no-renames",
            "--ignore-submodules=none",
            "--submodule=short",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            "--unified=3",
            "--inter-hunk-context=0",
            "--diff-algorithm=myers",
            "--indent-heuristic",
        ]);
        if let Some(old_path) = old_path.filter(|old_path| *old_path != path) {
            let old = self.tree_entry(base, old_path)?;
            let new = self.tree_entry(head, path)?;
            if let (Some(old), Some(new)) = (&old, &new)
                && old.is_blob()
                && new.is_blob()
            {
                return self.run(command.args([old.oid.as_str(), new.oid.as_str(), "--"]));
            }
        }
        self.run(
            command
                .args([base.as_str(), head.as_str(), "--"])
                .arg(OsStr::from_bytes(path)),
        )
    }

    /// The file at `path` in `rev`, a commit or a tree, as a [`crate::diff::ChangedFile`]'s side
    /// has it: its blob, mode and size, or for a submodule the commit it points at. `path` is
    /// bytes, as git stores it. `None` when there's no file there; a folder there is an error.
    /// Reads only trees, which a partial clone has, and blobs' sizes, which it may download.
    pub fn file_version(&self, rev: &Oid, path: &[u8]) -> Result<Option<FileVersion>, Error> {
        let shown = String::from_utf8_lossy(path);
        if path.is_empty() || path.contains(&0) {
            return Err(Error::Invalid {
                what: "path",
                value: shown.into_owned(),
            });
        }
        let output = self.run(
            self.git()
                .args(["ls-tree", "-z", "-l", rev.as_str(), "--"])
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
        // "<mode> <kind> <oid> <size>", the size padded with spaces, or "-" for a submodule.
        let info = std::str::from_utf8(info).map_err(|_| unexpected_ls_tree(entry))?;
        let mut fields = info.split_whitespace();
        let (Some(mode), Some(kind), Some(oid), Some(size)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err(unexpected_ls_tree(entry));
        };
        if kind == "tree" {
            return Err(Error::Git {
                command: format!("git ls-tree {rev} -- {shown}"),
                message: format!("{shown} is a tree, not a file"),
            });
        }
        let mode = diff::parse_mode(mode)?.ok_or_else(|| unexpected_ls_tree(entry))?;
        Ok(Some(FileVersion {
            path: path.to_vec(),
            oid: oid.parse()?,
            mode,
            size: size.parse().ok(),
        }))
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

    /// Records that the clone was used just now, which keeps cleanup away for a while. The caller
    /// holds the lock.
    pub(crate) fn stamp_used(&self) -> Result<(), Error> {
        stamp_last_used(&self.shared.git_dir, SystemTime::now())
    }

    /// Waits for the lock on this repository's clone and checkouts, before writing to them. A
    /// clone opened to read offline refuses: it never writes.
    pub(crate) fn lock(&self) -> Result<RepoLock, Error> {
        if self.shared.access == Access::ReadOffline {
            return Err(Error::ReadOnly {
                name: self.shared.name.clone(),
                what: "fetch or check out",
            });
        }
        RepoLock::acquire(&self.shared.lock_path)
    }

    /// Whether the clone was opened to read offline: it never writes or downloads.
    pub(crate) fn is_offline(&self) -> bool {
        self.shared.access == Access::ReadOffline
    }

    /// The version of the git that runs the clone's commands.
    pub(crate) fn git_version(&self) -> crate::GitVersion {
        self.shared.env.git.version()
    }

    /// A git command on the clone, signed in when the repository has a token.
    pub(crate) fn git(&self) -> GitCommand {
        self.git_at(Location::Clone(&self.shared.git_dir))
    }

    /// A git command on the repository at `location`, such as a checkout of the clone, signed in
    /// like [`Self::git`]. Offline, it never downloads, where git can be told so.
    pub(crate) fn git_at(&self, location: Location<'_>) -> GitCommand {
        let command = self
            .shared
            .env
            .command(location, self.shared.token.as_ref());
        if self.shared.access == Access::ReadOffline
            && self.shared.env.git.version() >= GitVersion::NO_LAZY_FETCH
        {
            command.env(NO_LAZY_FETCH_VAR, "1")
        } else {
            command
        }
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
    format!("refs/lo/pull/{number}/revs/{oid}")
}

/// Where pull request `number`'s base tip `oid` is pinned.
fn base_ref_for(number: u64, oid: &Oid) -> String {
    format!("refs/lo/pull/{number}/bases/{oid}")
}

/// Where the tip of `branch`, as last fetched as pull request `number`'s base, is pinned.
fn branch_base_ref(number: u64, branch: &str) -> String {
    let hash: String = Sha256::digest(branch.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("refs/lo/pull/{number}/branch-bases/{hash}")
}

/// The folder in a clone where each pull request's heads GitHub no longer has are noted, one
/// file per pull request.
const UNAVAILABLE_DIR: &str = "lo-unavailable";

/// The variable that stops git downloading what a partial clone lacks.
pub(crate) const NO_LAZY_FETCH_VAR: &str = "GIT_NO_LAZY_FETCH";

/// The blobs on either side of `git diff --raw -z --no-renames` output, without submodules'
/// commits or the all-zero ID of a side that isn't there. Each entry is its metadata, then its
/// path, so a path that starts with `:` is never taken for an entry.
fn raw_diff_blobs(output: &[u8]) -> Result<Vec<Oid>, Error> {
    let mut blobs = Vec::new();
    let mut fields = output
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty());
    while let Some(field) = fields.next() {
        // ":<old mode> <new mode> <old oid> <new oid> <status>", then the path.
        let _path = fields.next();
        let Some(entry) = field.strip_prefix(b":") else {
            return Err(Error::Git {
                command: "git diff --raw".into(),
                message: format!(
                    "printed an unexpected entry {:?}",
                    String::from_utf8_lossy(field)
                ),
            });
        };
        let entry = String::from_utf8_lossy(entry);
        let parts: Vec<&str> = entry.split(' ').collect();
        let [old_mode, new_mode, old_oid, new_oid, ..] = parts.as_slice() else {
            continue;
        };
        for (mode, oid) in [(old_mode, old_oid), (new_mode, new_oid)] {
            if *mode == "160000" || oid.bytes().all(|byte| byte == b'0') {
                continue;
            }
            blobs.push(oid.parse()?);
        }
    }
    Ok(blobs)
}

/// An entry of a tree, as `git ls-tree` lists it.
struct TreeEntry {
    /// Such as `100644`, or `160000` for a submodule.
    mode: String,
    /// `blob`, `tree` or `commit`.
    kind: String,
    oid: Oid,
}

impl TreeEntry {
    /// Whether it's a file in this repository: a blob, and not a submodule.
    fn is_blob(&self) -> bool {
        self.kind == "blob" && self.mode != "160000"
    }
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use tempfile::TempDir;

    use super::*;
    use crate::{Git, PullRequestRefs, RepoCache};

    /// The test's own git in `dir`, without the user's config.
    fn plain_git(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Lo test")
            .env("GIT_AUTHOR_EMAIL", "test@lo.invalid")
            .env("GIT_COMMITTER_NAME", "Lo test")
            .env("GIT_COMMITTER_EMAIL", "test@lo.invalid")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    /// The blobs `rev` reaches that the clone at `git_dir` hasn't downloaded.
    fn missing(git_dir: &Path, rev: &Oid) -> Vec<String> {
        plain_git(
            git_dir,
            &[
                "rev-list",
                "--objects",
                "--missing=print",
                rev.as_str(),
                "--",
            ],
        )
        .lines()
        .filter_map(|line| line.strip_prefix('?'))
        .map(str::to_string)
        .collect()
    }

    #[test]
    fn older_git_checks_whats_missing_without_downloading_it() {
        let dir = TempDir::new().unwrap();
        let server = dir.path().join("remotes/owner/repo.git");
        fs::create_dir_all(&server).unwrap();
        plain_git(&server, &["init", "-q", "--bare"]);
        plain_git(&server, &["config", "uploadpack.allowFilter", "true"]);
        plain_git(
            &server,
            &["config", "uploadpack.allowAnySHA1InWant", "true"],
        );
        let work = dir.path().join("work");
        fs::create_dir_all(&work).unwrap();
        plain_git(&work, &["init", "-q", "-b", "trunk"]);
        fs::write(work.join("a.txt"), "a\n").unwrap();
        plain_git(&work, &["add", "-A"]);
        plain_git(&work, &["commit", "-q", "-m", "base"]);
        let server_path = server.to_str().unwrap();
        plain_git(&work, &["push", "-q", server_path, "HEAD:refs/heads/trunk"]);
        fs::write(work.join("a.txt"), "a2\n").unwrap();
        fs::write(work.join("b.txt"), "b\n").unwrap();
        plain_git(&work, &["add", "-A"]);
        plain_git(&work, &["commit", "-q", "-m", "change"]);
        plain_git(&work, &["push", "-q", server_path, "HEAD:refs/pull/1/head"]);

        // A git that says it's 2.43, which can't be told not to download.
        let old = GitVersion {
            major: 2,
            minor: 43,
            patch: 0,
        };
        let git = Git::find().unwrap().with_version(old);
        let cache = RepoCache::new(dir.path().join("cache"), git)
            .unwrap()
            .with_remote_base(format!("file://{}", dir.path().join("remotes").display()));
        let repo = cache.open("owner", "repo", None).unwrap();
        let fetched = repo
            .fetch_pull(&PullRequestRefs {
                number: 1,
                head: None,
                base_branch: "trunk".into(),
                base: None,
            })
            .unwrap();
        let base = repo
            .merge_base(&fetched.head, &fetched.base_tip)
            .unwrap()
            .unwrap();
        let git_dir = repo.git_dir().to_path_buf();
        let before = missing(&git_dir, &fetched.head);
        assert!(!before.is_empty());

        // The server is still there, so a check that could download would.
        let offline = cache.open_offline("owner", "repo").unwrap().unwrap();
        assert!(!offline.has_diff_contents(&base, &fetched.head).unwrap());
        assert_eq!(missing(&git_dir, &fetched.head), before);

        // Once the diff was read online, the clone has everything it needs.
        repo.changed_files(&base, &fetched.head).unwrap();
        assert!(offline.has_diff_contents(&base, &fetched.head).unwrap());
    }

    #[test]
    fn a_raw_diff_names_both_sides_blobs_but_not_submodules_or_missing_sides() {
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        let c = "c".repeat(40);
        let zero = "0".repeat(40);
        // The second path starts with a colon and has spaces, as a file's name can: read as an
        // entry, it would name blobs that aren't there.
        let output = format!(
            ":100644 100644 {a} {b} M\0x.rs\0:000000 100644 {zero} {c} A\0:x y z w\0\
             :160000 160000 {a} {b} M\0sub\0"
        );
        let blobs: Vec<String> = raw_diff_blobs(output.as_bytes())
            .unwrap()
            .iter()
            .map(|oid| oid.to_string())
            .collect();
        assert_eq!(blobs, [a, b, c]);
    }
}
