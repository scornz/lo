//! Checkouts of pull request heads, for language servers on trusted repositories.

use std::fs;
use std::path::{Path, PathBuf};

use crate::{Error, Oid, Repo};

impl Repo {
    /// Checks `head` out, detached, as pull request `number`'s checkout, and returns its folder:
    /// `<cache>/checkouts/github.com/<owner>/<repo>/pull-<n>`, a `git worktree` of the clone.
    /// Called again after a push, it moves the checkout to the new head, leaving only what git
    /// put there.
    ///
    /// Only for repositories the user trusts: a checkout is what language servers run in. The
    /// caller decides. Symlinks become plain files holding their target, so nothing in a checkout
    /// points outside it, and no hook or filter runs.
    pub fn checkout(&self, number: u64, head: &Oid) -> Result<PathBuf, Error> {
        let dir = self.checkouts_dir().join(format!("pull-{number}"));
        let _lock = self.lock()?;
        if self.is_worktree(&dir) {
            let moved = self
                .run(self.git_in(&dir).args([
                    "checkout",
                    "--quiet",
                    "--force",
                    "--detach",
                    head.as_str(),
                    "--",
                ]))
                .and_then(|_| self.run(self.git_in(&dir).args(["clean", "-ffdxq"])));
            match moved {
                Ok(_) => return Ok(dir),
                // A checkout in a bad state is rebuilt below.
                Err(error) => log::warn!("rebuilding {}: {error}", dir.display()),
            }
        }
        if dir.exists() {
            fs::remove_dir_all(&dir).map_err(Error::io(format!("removing {}", dir.display())))?;
        }
        let parent = dir.parent().expect("a checkout has a parent folder");
        fs::create_dir_all(parent).map_err(Error::io(format!("creating {}", parent.display())))?;
        // Forget worktrees whose folders are gone, such as this one.
        self.run(self.git().args(["worktree", "prune"]))?;
        self.run(
            self.git()
                .args(["worktree", "add", "--quiet", "--force", "--detach"])
                .arg(&dir)
                .arg(head.as_str()),
        )?;
        Ok(dir)
    }

    /// The blob ID git would give the file at `path` in `checkout`, one of this clone's
    /// checkouts, hashing its bytes as they are on disk, without the conversions `.gitattributes`
    /// asks for. `None` when `path` isn't a regular file there: missing, a folder or a symlink.
    ///
    /// A checkout's file can differ from the head's blob: changed on disk, or two names that
    /// differ only in case sharing one file on a case-insensitive disk. Compare before letting a
    /// checkout's file stand in for git's text.
    pub fn hash_checkout_file(&self, checkout: &Path, path: &str) -> Result<Option<Oid>, Error> {
        let relative = Path::new(path);
        let inside = !path.is_empty()
            && relative
                .components()
                .all(|component| matches!(component, std::path::Component::Normal(_)));
        if !inside {
            return Err(Error::Invalid {
                what: "path in a checkout",
                value: path.to_string(),
            });
        }
        let is_file = fs::symlink_metadata(checkout.join(relative))
            .is_ok_and(|metadata| metadata.file_type().is_file());
        if !is_file {
            return Ok(None);
        }
        let output = self.run(
            self.git_in(checkout)
                .args(["hash-object", "--no-filters", "--"])
                .arg(path),
        )?;
        crate::oid::parse_output(&output).map(Some)
    }

    /// Whether `dir` is a worktree of this clone, judged from its `.git` file rather than by
    /// running git there, which could find some other repository further up.
    fn is_worktree(&self, dir: &Path) -> bool {
        let Ok(dot_git) = fs::read_to_string(dir.join(".git")) else {
            return false;
        };
        let Some(worktree_git_dir) = dot_git.strip_prefix("gitdir:") else {
            return false;
        };
        let (Ok(worktree_git_dir), Ok(worktrees)) = (
            fs::canonicalize(worktree_git_dir.trim()),
            fs::canonicalize(self.git_dir().join("worktrees")),
        ) else {
            return false;
        };
        worktree_git_dir.starts_with(worktrees)
    }
}
