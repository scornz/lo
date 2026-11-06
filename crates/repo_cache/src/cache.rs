//! The cache folder and the bare clones in it.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::Error;
use crate::askpass;
use crate::git::{Auth, Git, GitCommand, Token};
use crate::repo::Repo;

/// rozed's local copies of repositories, in one folder (`~/Library/Caches/rozed`):
///
/// - `repos/github.com/<owner>/<repo>.git`: a bare partial clone (`--filter=blob:none`) of each
///   repository, so file contents download only when something reads them. Everything fetched
///   goes under rozed's own refs, `refs/rozed/...`.
/// - `checkouts/github.com/<owner>/<repo>/pull-<n>`: a checkout of a pull request's head, for
///   language servers.
/// - `git-env/`: the askpass helper and the empty `HOME` git runs with.
///
/// Folder names are lowercase, since GitHub's owner and repository names aren't case-sensitive.
pub struct RepoCache {
    root: PathBuf,
    env: Arc<GitEnv>,
    /// Where repositories are fetched from: `<remote_base>/<owner>/<repo>.git`.
    remote_base: String,
}

/// Everything a git command needs besides its arguments.
pub(crate) struct GitEnv {
    pub git: Git,
    pub home: PathBuf,
    pub askpass: PathBuf,
}

impl GitEnv {
    /// A git command in `dir`, signing in with `token` if there is one.
    pub fn command(&self, dir: &Path, token: Option<&Token>) -> GitCommand {
        let auth = token.map(|token| Auth {
            askpass: &self.askpass,
            token,
        });
        self.git.command(dir, &self.home, auth)
    }
}

/// The only host rozed fetches from, for now.
const HOST: &str = "github.com";

/// The file in a clone that records when rozed last opened it.
pub(crate) const LAST_USED_FILE: &str = "rozed-last-used";

impl RepoCache {
    /// The cache in `root`, using `git`. Creates the folder and writes the askpass helper if
    /// they're missing.
    pub fn new(root: impl Into<PathBuf>, git: Git) -> Result<Self, Error> {
        let root = root.into();
        let env_dir = root.join("git-env");
        let home = env_dir.join("home");
        fs::create_dir_all(&home).map_err(Error::io(format!("creating {}", home.display())))?;
        let askpass = askpass::install(&env_dir)?;
        Ok(Self {
            root,
            env: Arc::new(GitEnv { git, home, askpass }),
            remote_base: format!("https://{HOST}"),
        })
    }

    /// Fetches from `<url>/<owner>/<repo>.git` instead of GitHub, e.g. `file:///tmp/remotes` in
    /// tests.
    pub fn with_remote_base(mut self, url: impl Into<String>) -> Self {
        self.remote_base = url.into().trim_end_matches('/').to_string();
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

    /// The clone of `owner/repo`, created on first use. Nothing is fetched yet; the first fetch
    /// downloads the history it needs. With `token`, git signs in with it whenever GitHub asks,
    /// including when reading a file downloads it.
    pub fn open(&self, owner: &str, repo: &str, token: Option<Token>) -> Result<Repo, Error> {
        check_name("repository owner", owner)?;
        check_name("repository name", repo)?;
        let git_dir = self.repo_dir(owner, repo);
        let url = format!("{}/{owner}/{repo}.git", self.remote_base);
        if git_dir.join("HEAD").is_file() {
            self.update_remote(&git_dir, &url)?;
        } else {
            self.create(&git_dir, &url)?;
        }
        stamp_last_used(&git_dir, SystemTime::now())?;
        Ok(Repo::new(
            self.env.clone(),
            format!("{owner}/{repo}"),
            git_dir,
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
            self.git(&temporary_root)
                .args(["init", "--quiet", "--bare"])
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

    /// Points the clone at `url` if it points somewhere else.
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
