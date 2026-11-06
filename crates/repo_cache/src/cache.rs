//! The cache folder and the bare clones in it.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::Error;
use crate::git::{Auth, Git, GitCommand, Token};
use crate::lock::RepoLock;
use crate::repo::Repo;

/// rozed's local copies of repositories, in one folder (`~/Library/Caches/rozed`):
///
/// - `repos/github.com/<owner>/<repo>.git`: a bare partial clone (`--filter=blob:none`) of each
///   repository, so file contents download only when something reads them. Everything fetched
///   goes under rozed's own refs, `refs/rozed/...`.
/// - `checkouts/github.com/<owner>/<repo>/pull-<n>`: a checkout of a pull request's head, for
///   language servers.
/// - `locks/github.com/<owner>/<repo>.lock`: the lock held while writing to a clone (see
///   [`crate::lock`]).
/// - `git-env/home`: the empty `HOME` git runs with.
///
/// Folder names are lowercase, since GitHub's owner and repository names aren't case-sensitive.
pub struct RepoCache {
    root: PathBuf,
    env: Arc<GitEnv>,
}

/// Everything a git command needs besides its arguments.
#[derive(Clone)]
pub(crate) struct GitEnv {
    pub git: Git,
    pub home: PathBuf,
    /// Where repositories are fetched from: `<remote_base>/<owner>/<repo>.git`. The token goes
    /// to this URL and nowhere else.
    pub remote_base: String,
    /// Extra `name=value` config for every git, from [`RepoCache::with_remote_base`].
    pub config: Vec<String>,
}

impl GitEnv {
    /// A git command in `dir`, signing in to the remote with `token` if there is one.
    pub fn command(&self, dir: &Path, token: Option<&Token>) -> GitCommand {
        let auth = token.map(|token| Auth {
            remote: &self.remote_base,
            token,
        });
        self.git.command(dir, &self.home, auth, &self.config)
    }
}

/// The only host rozed fetches from, for now.
const HOST: &str = "github.com";

/// The file in a clone that records when rozed last opened it.
pub(crate) const LAST_USED_FILE: &str = "rozed-last-used";

impl RepoCache {
    /// The cache in `root`, using `git`. Creates the folder if it's missing.
    pub fn new(root: impl Into<PathBuf>, git: Git) -> Result<Self, Error> {
        let root = root.into();
        let env_dir = root.join("git-env");
        let home = env_dir.join("home");
        fs::create_dir_all(&home).map_err(Error::io(format!("creating {}", home.display())))?;
        // The askpass helper earlier versions wrote, which nothing uses now.
        let _ = fs::remove_file(env_dir.join("askpass.sh"));
        Ok(Self {
            root,
            env: Arc::new(GitEnv {
                git,
                home,
                remote_base: format!("https://{HOST}"),
                config: Vec::new(),
            }),
        })
    }

    /// Fetches from `<url>/<owner>/<repo>.git` instead of GitHub, and allows git that URL's
    /// protocol: for tests, which serve repositories from `file://` folders.
    pub fn with_remote_base(mut self, url: impl Into<String>) -> Self {
        let env = Arc::make_mut(&mut self.env);
        env.remote_base = url.into().trim_end_matches('/').to_string();
        if let Some((scheme, _)) = env.remote_base.split_once("://")
            && scheme != "https"
        {
            env.config.push(format!("protocol.{scheme}.allow=always"));
        }
        self
    }

    /// Adds `name=value` config to every git, for tests.
    #[cfg(test)]
    pub(crate) fn with_config(mut self, config: &str) -> Self {
        Arc::make_mut(&mut self.env).config.push(config.to_string());
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn repos_dir(&self) -> PathBuf {
        self.root.join("repos").join(HOST)
    }

    /// Where the clone of `owner/repo` lives.
    pub fn repo_dir(&self, owner: &str, repo: &str) -> PathBuf {
        self.repos_dir()
            .join(owner.to_ascii_lowercase())
            .join(format!("{}.git", repo.to_ascii_lowercase()))
    }

    /// Where the checkouts of `owner/repo`'s pull requests live.
    fn repo_checkouts_dir(&self, owner: &str, repo: &str) -> PathBuf {
        self.checkouts_root()
            .join(owner.to_ascii_lowercase())
            .join(repo.to_ascii_lowercase())
    }

    /// The lock held while writing to `owner/repo`'s clone or its checkouts.
    fn lock_path(&self, owner: &str, repo: &str) -> PathBuf {
        self.root
            .join("locks")
            .join(HOST)
            .join(owner.to_ascii_lowercase())
            .join(format!("{}.lock", repo.to_ascii_lowercase()))
    }

    /// The clone of `owner/repo`, created on first use. Nothing is fetched yet; the first fetch
    /// downloads the history it needs. With `token`, git signs in with it whenever GitHub asks,
    /// including when reading a file downloads it.
    ///
    /// Pass the repository's canonical name, GitHub's `full_name` for it, not a name from a link:
    /// a renamed or transferred repository then keeps one clone.
    pub fn open(&self, owner: &str, repo: &str, token: Option<Token>) -> Result<Repo, Error> {
        check_name("repository owner", owner)?;
        check_name("repository name", repo)?;
        let git_dir = self.repo_dir(owner, repo);
        let lock_path = self.lock_path(owner, repo);
        let url = format!("{}/{owner}/{repo}.git", self.env.remote_base);
        {
            let _lock = RepoLock::acquire(&lock_path)?;
            if git_dir.join("HEAD").is_file() {
                self.update_remote(&git_dir, &url)?;
            } else {
                self.create(&git_dir, &url)?;
            }
            stamp_last_used(&git_dir, SystemTime::now())?;
        }
        Ok(Repo::new(
            self.env.clone(),
            format!("{owner}/{repo}"),
            git_dir,
            self.repo_checkouts_dir(owner, repo),
            lock_path,
            token,
        ))
    }

    /// Creates a bare clone at `git_dir`, set up to fetch from `url`. It's made in a temporary
    /// folder and moved into place, so a clone in the cache is always complete.
    fn create(&self, git_dir: &Path, url: &str) -> Result<(), Error> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let temporary_root = self.root.join("tmp");
        fs::create_dir_all(&temporary_root)
            .map_err(Error::io(format!("creating {}", temporary_root.display())))?;
        let temporary = temporary_root.join(format!(
            "{}-{}.git",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| {
            // No template: nothing but what git needs, so no sample hooks.
            self.git(&temporary_root)
                .args(["init", "--quiet", "--bare", "--template="])
                .arg(&temporary)
                .run()?;
            for (key, value) in [
                ("remote.origin.url", url),
                // A promisor remote downloads file contents when something reads them.
                ("remote.origin.promisor", "true"),
                ("remote.origin.partialclonefilter", "blob:none"),
                ("core.symlinks", "false"),
            ] {
                self.git(&temporary).args(["config", key, value]).run()?;
            }
            stamp_last_used(&temporary, SystemTime::now())?;
            let parent = git_dir.parent().expect("a clone's folder has a parent");
            fs::create_dir_all(parent)
                .map_err(Error::io(format!("creating {}", parent.display())))?;
            match fs::rename(&temporary, git_dir) {
                Ok(()) => Ok(()),
                // Another rozed created it meanwhile.
                Err(_) if git_dir.join("HEAD").is_file() => Ok(()),
                Err(error) => Err(Error::io(format!("creating {}", git_dir.display()))(error)),
            }
        })();
        if temporary.exists() {
            let _ = fs::remove_dir_all(&temporary);
        }
        if result.is_ok() {
            log::info!("created {}", git_dir.display());
        }
        result
    }

    /// Points the clone at `url` if it points somewhere else. Config is written only when it
    /// changes, so opening a clone usually writes nothing.
    fn update_remote(&self, git_dir: &Path, url: &str) -> Result<(), Error> {
        let current = self
            .git(git_dir)
            .args(["config", "--get", "remote.origin.url"])
            .finish(None)?;
        if String::from_utf8_lossy(&current.stdout).trim() != url {
            self.git(git_dir)
                .args(["config", "remote.origin.url", url])
                .run()?;
        }
        Ok(())
    }

    fn git(&self, dir: &Path) -> GitCommand {
        self.env.command(dir, None)
    }

    /// How much disk space the cache takes, in bytes.
    pub fn size(&self) -> Result<u64, Error> {
        disk_usage(&self.root)
    }

    /// Removes every clone and checkout, for "clear cache" in settings. Each clone's lock is held
    /// while it goes, so a fetch in progress finishes first. Close the repositories first:
    /// a [`Repo`] whose clone is gone fails until it's opened again.
    pub fn clear(&self) -> Result<(), Error> {
        for (owner, repo) in self.clones()? {
            let _lock = RepoLock::acquire(&self.lock_path(&owner, &repo))?;
            self.remove_clone(&owner, &repo)?;
        }
        for dir in [
            self.root.join("repos"),
            self.root.join("checkouts"),
            self.root.join("tmp"),
        ] {
            remove_dir_all_if_exists(&dir)?;
        }
        log::info!("cleared {}", self.root.display());
        Ok(())
    }

    /// Removes the clones nobody opened for `max_age`, [`RepoCache::UNUSED_FOR`] at launch, with
    /// their checkouts, and checkouts left without a clone. A clone being written to is skipped.
    /// Returns the `owner/repo` of each clone removed.
    pub fn remove_unused(&self, max_age: Duration) -> Result<Vec<String>, Error> {
        let now = SystemTime::now();
        let is_stale = |git_dir: &Path| {
            last_used(git_dir).is_none_or(|used| {
                now.duration_since(used)
                    .is_ok_and(|unused_for| unused_for > max_age)
            })
        };
        let mut removed = Vec::new();
        for (owner, repo) in self.clones()? {
            let git_dir = self.repo_dir(&owner, &repo);
            if !is_stale(&git_dir) {
                continue;
            }
            let Some(_lock) = RepoLock::try_acquire(&self.lock_path(&owner, &repo))? else {
                continue;
            };
            // Opened while we waited? `open` stamps it under the lock.
            if !is_stale(&git_dir) {
                continue;
            }
            self.remove_clone(&owner, &repo)?;
            removed.push(format!("{owner}/{repo}"));
        }
        self.remove_orphaned_checkouts()?;
        // Temporary clones a crash left behind. A day is far longer than making one takes.
        for entry in read_dir_if_exists(&self.root.join("tmp"))? {
            let old = fs::symlink_metadata(&entry)
                .and_then(|metadata| metadata.modified())
                .is_ok_and(|modified| {
                    now.duration_since(modified)
                        .is_ok_and(|age| age > Duration::from_secs(24 * 60 * 60))
                });
            if old {
                remove_dir_all_if_exists(&entry)?;
            }
        }
        if !removed.is_empty() {
            log::info!("removed unused clones: {}", removed.join(", "));
        }
        Ok(removed)
    }

    /// How long a clone can go unopened before [`RepoCache::remove_unused`] removes it at launch.
    pub const UNUSED_FOR: Duration = Duration::from_secs(30 * 24 * 60 * 60);

    /// The `(owner, repo)` of every clone, as their folders name them (lowercase).
    fn clones(&self) -> Result<Vec<(String, String)>, Error> {
        let mut clones = Vec::new();
        for owner in read_dir_if_exists(&self.repos_dir())? {
            for clone in read_dir_if_exists(&owner)? {
                let (Some(owner), Some(repo)) = (
                    file_name(&owner),
                    file_name(&clone)
                        .and_then(|name| name.strip_suffix(".git").map(str::to_string)),
                ) else {
                    continue;
                };
                if check_name("owner", &owner).is_ok() && check_name("repo", &repo).is_ok() {
                    clones.push((owner, repo));
                }
            }
        }
        clones.sort();
        Ok(clones)
    }

    /// Removes a clone and its checkouts. The caller holds its lock.
    fn remove_clone(&self, owner: &str, repo: &str) -> Result<(), Error> {
        remove_dir_all_if_exists(&self.repo_checkouts_dir(owner, repo))?;
        remove_dir_all_if_exists(&self.repo_dir(owner, repo))?;
        remove_dir_if_empty(&self.repos_dir().join(owner));
        remove_dir_if_empty(&self.checkouts_root().join(owner));
        Ok(())
    }

    /// Removes the checkouts of clones that no longer exist.
    fn remove_orphaned_checkouts(&self) -> Result<(), Error> {
        for owner in read_dir_if_exists(&self.checkouts_root())? {
            for checkouts in read_dir_if_exists(&owner)? {
                let (Some(owner_name), Some(repo)) = (file_name(&owner), file_name(&checkouts))
                else {
                    continue;
                };
                if check_name("owner", &owner_name).is_err() || check_name("repo", &repo).is_err() {
                    continue;
                }
                if self.repo_dir(&owner_name, &repo).exists() {
                    continue;
                }
                if let Some(_lock) = RepoLock::try_acquire(&self.lock_path(&owner_name, &repo))?
                    && !self.repo_dir(&owner_name, &repo).exists()
                {
                    remove_dir_all_if_exists(&checkouts)?;
                }
            }
            remove_dir_if_empty(&owner);
        }
        Ok(())
    }

    fn checkouts_root(&self) -> PathBuf {
        self.root.join("checkouts").join(HOST)
    }
}

/// When the clone at `git_dir` was last opened, from its stamp, or the folder's own time if the
/// stamp is missing.
fn last_used(git_dir: &Path) -> Option<SystemTime> {
    let stamp = fs::read_to_string(git_dir.join(LAST_USED_FILE))
        .ok()
        .and_then(|stamp| stamp.trim().parse::<u64>().ok())
        .map(|seconds| UNIX_EPOCH + Duration::from_secs(seconds));
    stamp.or_else(|| fs::metadata(git_dir).and_then(|m| m.modified()).ok())
}

/// The bytes `path` takes on disk, counting each file's allocated blocks and following no
/// symlinks.
fn disk_usage(path: &Path) -> Result<u64, Error> {
    use std::os::unix::fs::MetadataExt as _;
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(Error::io(format!("reading {}", path.display()))(error)),
    };
    let mut total = metadata.blocks() * 512;
    if metadata.is_dir() {
        for entry in read_dir_if_exists(path)? {
            total += disk_usage(&entry)?;
        }
    }
    Ok(total)
}

/// The entries in `dir`, or none if it doesn't exist.
fn read_dir_if_exists(dir: &Path) -> Result<Vec<PathBuf>, Error> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(Error::io(format!("reading {}", dir.display()))(error)),
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(Error::io(format!("reading {}", dir.display())))?;
        paths.push(entry.path());
    }
    Ok(paths)
}

fn remove_dir_all_if_exists(dir: &Path) -> Result<(), Error> {
    match fs::remove_dir_all(dir) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            Err(Error::io(format!("removing {}", dir.display()))(error))
        }
        _ => Ok(()),
    }
}

/// Removes `dir` if it's empty, so no empty owner folders pile up.
fn remove_dir_if_empty(dir: &Path) {
    let _ = fs::remove_dir(dir);
}

fn file_name(path: &Path) -> Option<String> {
    path.file_name()?.to_str().map(str::to_string)
}

/// Fails unless `value` is safe as an owner or repository name and as a folder name: GitHub's
/// letters, digits, `-`, `_` and `.`, and not `.` or `..`.
fn check_name(what: &'static str, value: &str) -> Result<(), Error> {
    let valid = !value.is_empty()
        && value.len() <= 100
        && value != "."
        && value != ".."
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if valid {
        Ok(())
    } else {
        Err(Error::Invalid {
            what,
            value: value.to_string(),
        })
    }
}

/// Records that the clone at `git_dir` was used at `time`.
pub(crate) fn stamp_last_used(git_dir: &Path, time: SystemTime) -> Result<(), Error> {
    let seconds = time
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    let path = git_dir.join(LAST_USED_FILE);
    fs::write(&path, format!("{seconds}\n"))
        .map_err(Error::io(format!("writing {}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_must_be_safe_as_folder_names() {
        for name in ["sharkdp", "fd", "rozed-dev", ".github", "my_repo.rs", "A1"] {
            assert!(check_name("name", name).is_ok(), "{name}");
        }
        for name in [
            "",
            ".",
            "..",
            "a/b",
            "../x",
            "a b",
            "a\nb",
            "ä",
            &"a".repeat(101),
        ] {
            let error = check_name("repository name", name).unwrap_err();
            assert_eq!(
                error.to_string(),
                format!("`{name}` isn't a valid repository name")
            );
        }
    }
}
