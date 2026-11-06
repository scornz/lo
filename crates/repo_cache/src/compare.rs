//! Comparing two revisions of a pull request so that only what its author changed shows, even
//! across a rebase (design §6, "Rebase-aware comparison").

use std::fmt;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::diff::ChangedFile;
use crate::git::failure_message;
use crate::{Error, Oid, Repo, RepoCache};

/// Where replayed trees are pinned: `refs/rozed/replays/<tree>/<pin>`.
const REPLAYS: &str = "refs/rozed/replays/";

/// One revision of a pull request, as git sees it: its head, and the commit its changes start
/// from.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Revision {
    /// Where the revision's changes start: the merge base of `head` with the base branch as it
    /// was when the revision was pushed. For a layer of a stack, the head of the layer below it,
    /// as it was then.
    pub base: Oid,
    pub head: Oid,
}

/// What changed between two revisions, from [`Repo::compare_revisions`].
///
/// A clean replay's tree is pinned in the clone while the comparison, or any copy of it, is
/// kept, so its files stay readable. Dropping the last copy removes the pin, which runs git once.
#[derive(Clone, Debug)]
pub struct Comparison {
    /// The files that changed, as [`Repo::changed_files`] lists them, from [`Self::old`] to
    /// [`Self::new`].
    pub files: Vec<ChangedFile>,
    /// What the old side's files are read from with [`Repo::read_file`]: after a clean replay,
    /// the tree of the old revision's changes replayed onto the new base; otherwise the old head.
    pub old: Oid,
    /// What the new side's files are read from: the new head.
    pub new: Oid,
    /// Whether the old revision was replayed onto the new base, and how that went.
    pub replay: Replay,
    _pin: Option<Arc<ReplayPin>>,
}

/// Whether [`Repo::compare_revisions`] replayed the old revision onto the new base.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Replay {
    /// Both revisions start from the same base, so the comparison is of their heads.
    SameBase,
    /// The old revision's changes, replayed onto the new base, are compared with the new head,
    /// so only what the author changed shows. Nothing showing means the author only rebased.
    Clean,
    /// Replaying the old revision onto the new base conflicted, so the comparison is of the
    /// heads and also shows what changed between the bases, which the UI says. `paths` are the
    /// paths git left conflicted, as bytes (see [`crate::FileVersion::path`]). Some conflicts,
    /// such as over where a renamed folder's files go, leave none.
    Conflicted { paths: Vec<Vec<u8>> },
}

impl Repo {
    /// Compares revision `old` of a pull request with `new`, showing only what the author
    /// changed between them, even when the base moved.
    ///
    /// When the bases differ, as after a rebase, the old revision's changes are replayed onto
    /// the new base in the clone (`git merge-tree --write-tree --merge-base=<old base> <new
    /// base> <old head>`), and the tree that makes is compared with the new head. Changes that
    /// came with the new base then cancel out. When the bases match, it compares the heads. When
    /// the replay conflicts, it compares the heads too, and says which paths conflicted
    /// ([`Replay::Conflicted`]); any other failure is an error. Whether the heads match doesn't
    /// matter, only the bases: an unchanged head whose base moved still gets the replay.
    ///
    /// For a layer of a stack whose lower layer got a new push, pass the lower layer's old and
    /// new heads as the bases.
    ///
    /// Every commit must be in the clone already: fetch them first with [`Repo::fetch_pull`] or
    /// [`Repo::fetch_commit`]. A missing one is [`Error::NotFetched`], and is never downloaded.
    /// The file contents the replay merges download as git needs them, a batch at a time; a
    /// replay of changes each made on one side alone needs none. Then, like
    /// [`Repo::changed_files`], both sides of every changed file download in one batch.
    ///
    /// A clean replay's tree is pinned at `refs/rozed/replays/<tree>/<pin>` while the comparison
    /// is kept (see [`Comparison`]). The replay writes no other ref. Its objects stay in the clone
    /// unreferenced, and comparing the same revisions again makes the same tree.
    pub fn compare_revisions(&self, old: &Revision, new: &Revision) -> Result<Comparison, Error> {
        // Checked without downloading: git would download a missing commit with all its history,
        // with no progress shown and no ref keeping it.
        for oid in [&old.base, &old.head, &new.base, &new.head] {
            if !self.has_commit(oid)? {
                return Err(Error::NotFetched { oid: oid.clone() });
            }
        }
        let plain = |replay| -> Result<Comparison, Error> {
            Ok(Comparison {
                files: self.changed_files(&old.head, &new.head)?,
                old: old.head.clone(),
                new: new.head.clone(),
                replay,
                _pin: None,
            })
        };
        if old.base == new.base {
            return plain(Replay::SameBase);
        }
        match self.replay(old, &new.base)? {
            Replayed::Clean(tree) => {
                // Pinned first, so nothing can remove the tree while it's being compared.
                let pin = ReplayPin::new(self, &tree)?;
                Ok(Comparison {
                    files: self.changed_files(&tree, &new.head)?,
                    old: tree,
                    new: new.head.clone(),
                    replay: Replay::Clean,
                    _pin: Some(Arc::new(pin)),
                })
            }
            Replayed::Conflicted(paths) => {
                log::info!(
                    "{}: replaying {} onto {} conflicted in {} files, so comparing the heads",
                    self.name(),
                    old.head.short(7),
                    new.base.short(7),
                    paths.len()
                );
                plain(Replay::Conflicted { paths })
            }
        }
    }

    /// Replays `old`'s changes onto `onto`, writing the tree but no ref.
    fn replay(&self, old: &Revision, onto: &Oid) -> Result<Replayed, Error> {
        let command = self.git().args([
            "merge-tree",
            "--write-tree",
            "-z",
            "--name-only",
            "--no-messages",
            &format!("--merge-base={}", old.base),
            onto.as_str(),
            old.head.as_str(),
        ]);
        let display = command.display();
        let finished = command.finish(None)?;
        // The exit status says whether it's clean: 0 is, 1 conflicted, and anything else failed.
        // A conflict can leave no path conflicted, so the paths don't say.
        let clean = match finished.status.code() {
            Some(0) => true,
            Some(1) => false,
            _ => return Err(Error::from_git(display, failure_message(&finished), None)),
        };
        let Some((tree, paths)) = parse_merge_tree(&finished.stdout) else {
            return Err(Error::Git {
                command: display,
                message: format!(
                    "printed unexpected output {:?}",
                    String::from_utf8_lossy(&finished.stdout)
                ),
            });
        };
        Ok(if clean {
            Replayed::Clean(tree)
        } else {
            Replayed::Conflicted(paths)
        })
    }
}

impl RepoCache {
    /// Removes every replay pin from the clone at `git_dir`. Only for a clone no rozed has open,
    /// where a pin is one a rozed that crashed left behind.
    pub(crate) fn remove_left_replay_pins(&self, git_dir: &Path) -> Result<(), Error> {
        let deletes = self
            .git(git_dir)
            .args(["for-each-ref", "--format=delete %(refname)", REPLAYS])
            .run()?;
        if deletes.is_empty() {
            return Ok(());
        }
        self.git(git_dir)
            .args(["update-ref", "--stdin"])
            .run_with_input(Some(&deletes))?;
        log::info!(
            "removed {} replay pins left in {}",
            deletes.iter().filter(|byte| **byte == b'\n').count(),
            git_dir.display()
        );
        Ok(())
    }
}

/// How a replay went.
enum Replayed {
    Clean(Oid),
    Conflicted(Vec<Vec<u8>>),
}

/// Reads `git merge-tree --write-tree -z --name-only --no-messages`: the tree, then each
/// conflicted path, each ending in NUL.
fn parse_merge_tree(output: &[u8]) -> Option<(Oid, Vec<Vec<u8>>)> {
    let output = output.strip_suffix(b"\0")?;
    let mut fields = output.split(|byte| *byte == 0);
    let tree = std::str::from_utf8(fields.next()?).ok()?.parse().ok()?;
    let mut paths: Vec<Vec<u8>> = Vec::new();
    for path in fields {
        if path.is_empty() {
            return None;
        }
        if !paths.iter().any(|seen| seen == path) {
            paths.push(path.to_vec());
        }
    }
    Some((tree, paths))
}

/// A ref keeping a replayed tree in the clone, removed when dropped. Each comparison has its own,
/// named for this process, so one dropped never unpins a tree another comparison, or another
/// rozed, still shows. The clone stays open while it's kept, so cleanup leaves it alone.
struct ReplayPin {
    repo: Repo,
    reference: String,
    tree: Oid,
}

impl ReplayPin {
    fn new(repo: &Repo, tree: &Oid) -> Result<Self, Error> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let reference = format!(
            "{REPLAYS}{tree}/{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        // No lock: one ref, which nothing else writes, and git writes it atomically.
        repo.run(repo.git().args(["update-ref", &reference, tree.as_str()]))?;
        Ok(Self {
            repo: repo.clone(),
            reference,
            tree: tree.clone(),
        })
    }
}

impl Drop for ReplayPin {
    fn drop(&mut self) {
        let command =
            self.repo
                .git()
                .args(["update-ref", "-d", &self.reference, self.tree.as_str()]);
        if let Err(error) = self.repo.run(command) {
            log::warn!(
                "{}: can't unpin {}: {error}",
                self.repo.name(),
                self.reference
            );
        }
    }
}

impl fmt::Debug for ReplayPin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.reference)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TREE: &str = "f74d6ab317132cd8ec8c2165016f89a7d9e03caa";

    #[test]
    fn a_comparison_can_go_to_another_thread() {
        fn send_and_sync<T: Send + Sync>() {}
        send_and_sync::<Comparison>();
    }

    #[test]
    fn reads_a_clean_replays_tree() {
        let (tree, paths) = parse_merge_tree(format!("{TREE}\0").as_bytes()).unwrap();
        assert_eq!(tree.as_str(), TREE);
        assert_eq!(paths, Vec::<Vec<u8>>::new());
    }

    #[test]
    fn reads_conflicted_paths_as_bytes_once_each() {
        let mut output = format!("{TREE}\0src/a.txt\0").into_bytes();
        output.extend_from_slice(b"caf\xe9\tname\n.txt\0src/a.txt\0");

        let (tree, paths) = parse_merge_tree(&output).unwrap();

        assert_eq!(tree.as_str(), TREE);
        assert_eq!(paths, [&b"src/a.txt"[..], b"caf\xe9\tname\n.txt"]);
    }

    #[test]
    fn refuses_anything_else() {
        for output in [
            String::new(),
            "\0".into(),
            TREE.into(),
            "not a tree\0".into(),
            format!("{TREE}\0a.txt"),
            format!("{TREE}\0\0a.txt\0"),
            // What `merge-tree` prints with its messages, which rozed turns off.
            format!("{TREE}\0a.txt\0\x001\0a.txt\0Auto-merging\0Auto-merging a.txt\n\0"),
        ] {
            assert_eq!(parse_merge_tree(output.as_bytes()), None, "{output:?}");
        }
    }
}
