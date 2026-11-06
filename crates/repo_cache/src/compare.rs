//! Comparing two revisions of a pull request so that only what its author changed shows, even
//! across a rebase (design §6, "Rebase-aware comparison").

use std::borrow::Cow;
use std::path::Path;

use crate::diff::ChangedFile;
use crate::git::{Finished, failure_message};
use crate::{Error, Oid, Repo, RepoCache};

/// Where replayed trees are pinned: `refs/lo/replays/<tree>/<process ID>`.
const REPLAYS: &str = "refs/lo/replays/";

/// One revision of a pull request, as git sees it: its head, and the commit its changes start
/// from.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Revision {
    /// Where the revision's changes start, which must be `head` or one of its ancestors: the
    /// merge base of `head` with the base branch as it was when the revision was pushed. For a
    /// layer of a stack, the newest head of the layer below that `head` contains. That's not
    /// always the lower layer's head when the revision was pushed: after the lower layer is
    /// force-pushed, the upper layer keeps the old one until it's restacked. Nor is it GitHub's
    /// `base.sha`, the base branch's tip.
    ///
    /// Ancestry can't catch one wrong base: `head` itself, as for a pull request that landed by
    /// a fast-forward or a merge commit, whose base branch then contains its head.
    pub base: Oid,
    pub head: Oid,
}

/// What changed between two revisions, from [`Repo::compare_revisions`].
///
/// A replayed tree ([`Self::old`] after a clean replay, or [`Replay::Conflicted`]'s) and the
/// files merged into it exist only in the clone. Each is pinned at
/// `refs/lo/replays/<tree>/<process ID>` the first time this process makes it, and stays
/// pinned while the process has the clone open: the next [`RepoCache::open`] of the clone with no
/// rozed having it open removes the pins. So keep a [`Repo`] of the clone open while showing a
/// comparison. A comparison holds none, and dropping one runs nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Comparison {
    /// The files that changed, as [`Repo::changed_files`] lists them, from [`Self::old`] to
    /// [`Self::new`]; and after a conflicted replay, every file it conflicted on.
    pub files: Vec<ComparedFile>,
    /// What the old side's files are read from with [`Repo::read_file`]: after a clean replay
    /// ([`Replay::Clean`] or [`Replay::Retargeted`]), the tree of the old revision's changes
    /// replayed onto the new base; otherwise the old head.
    pub old: Oid,
    /// What the new side's files are read from: the new head.
    pub new: Oid,
    /// Whether the old revision was replayed onto the new base, and how that went.
    pub replay: Replay,
}

/// Whether [`Repo::compare_revisions`] replayed the old revision onto the new base.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Replay {
    /// Both revisions start from the same base, so the comparison is of their heads.
    SameBase,
    /// The old revision's changes, replayed onto the new base, are compared with the new head,
    /// so only what the author changed shows. Nothing showing means the author only rebased.
    Clean,
    /// Replayed as for [`Replay::Clean`], but the base changed in a way a rebase onto a newer
    /// base doesn't explain, so what shows can have come with the base rather than from the
    /// author, which the UI says: the new base isn't the old one or a descendant of it, as when
    /// the pull request was retargeted to another branch, its base branch was rewound, or the
    /// lower layer of a stack was amended; or the head didn't change at all.
    Retargeted,
    /// Replaying the old revision onto the new base conflicted, so the comparison is of the
    /// heads and also shows what changed between the bases, which the UI says (design §6). Every
    /// file the replay conflicted on is listed too, with how the author resolved it (see
    /// [`ComparedFile::resolution`]). Some conflicts leave no file conflicted.
    Conflicted {
        /// The replay as git wrote it, conflicts and all, with git's markers in the files whose
        /// contents conflicted: what each resolution starts from. Pinned like a clean replay's
        /// tree. Comparing it with the new head ([`Repo::changed_files`]) shows everything the
        /// author did, as `git log --remerge-diff` does, base changes they dropped from files
        /// that didn't conflict included.
        tree: Oid,
        /// Whether the base changed as for [`Replay::Retargeted`], so the resolutions can hold
        /// what came with the base, not only what the author did.
        retargeted: bool,
    },
}

/// A file a [`Comparison`] lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComparedFile {
    /// The path, as bytes (see [`crate::FileVersion::path`]): [`ChangedFile::path`] for a file
    /// that changed, and otherwise the path the replay conflicted on.
    pub path: Vec<u8>,
    /// How the file changed from [`Comparison::old`] to [`Comparison::new`]. `None` only for a
    /// file the replay conflicted on that's the same in both, as when the author resolved the
    /// conflict by keeping their version over the base's.
    pub change: Option<ChangedFile>,
    /// For a file the replay conflicted on ([`Replay::Conflicted`]), which the UI marks "resolved
    /// a conflict", how the author resolved it: the files that changed from the conflicted replay
    /// to the new head, at this path and at any copy of it git set aside under a name of its own
    /// (`<path>~<commit>`), as when a file became a symlink on one side and changed on the other.
    /// Empty when the new head kept the conflicted replay's version, as when the base changed a
    /// file the old revision deleted, and the author kept the base's. `None` for a file the
    /// replay didn't conflict on.
    pub resolution: Option<Vec<ChangedFile>>,
}

impl ComparedFile {
    /// Whether the replay conflicted on this file, so the author resolved a conflict in it.
    pub fn conflicted(&self) -> bool {
        self.resolution.is_some()
    }

    /// [`Self::path`] as text, with `U+FFFD` for bytes that aren't UTF-8.
    pub fn display_path(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.path)
    }

    fn changed(change: ChangedFile) -> Self {
        Self {
            path: change.path().to_vec(),
            change: Some(change),
            resolution: None,
        }
    }
}

impl Repo {
    /// Compares revision `old` of a pull request with `new`, showing only what the author
    /// changed between them, even when the base moved.
    ///
    /// When the bases differ, as after a rebase, the old revision's changes are replayed onto
    /// the new base in the clone (`git merge-tree --write-tree --merge-base=<old base> <new
    /// base> <old head>`, following folders the base renamed), and the tree that makes is
    /// compared with the new head. Changes that came with the new base then cancel out. When
    /// the bases match, it compares the heads. When the replay conflicts, it compares the heads
    /// too, and lists every file it conflicted on with how the author resolved it
    /// ([`Replay::Conflicted`]); any other failure is an error. Whether the heads match doesn't
    /// matter, only the bases: an unchanged head whose base moved still gets the replay, which
    /// shows how its patch changed. That, and a base that moved anywhere but forward, as when a
    /// pull request is retargeted or a stack's lower layer amended, is flagged
    /// ([`Replay::Retargeted`]), since what shows can have come with the base.
    ///
    /// For a layer of a stack whose lower layer got a new push, the bases are the lower layer's
    /// heads that each revision contains (see [`Revision::base`]). A base that isn't its head or
    /// one of its ancestors is [`Error::BaseNotAncestor`].
    ///
    /// Every commit must be in the clone already: fetch them first with [`Repo::fetch_pull`] or
    /// [`Repo::fetch_commit`]. A missing one is [`Error::NotFetched`], and is never downloaded.
    /// The file contents the replay merges download as git needs them, a batch at a time; a
    /// replay of changes each made on one side alone needs none. Then, like
    /// [`Repo::changed_files`], both sides of every changed file download in one batch.
    ///
    /// The replay's tree is pinned (see [`Comparison`]). The replay writes no other ref, and
    /// comparing the same revisions again makes the same tree.
    pub fn compare_revisions(&self, old: &Revision, new: &Revision) -> Result<Comparison, Error> {
        // Checked without downloading: git would download a missing commit with all its history,
        // with no progress shown and no ref keeping it.
        for oid in [&old.base, &old.head, &new.base, &new.head] {
            if !self.has_commit(oid)? {
                return Err(Error::NotFetched { oid: oid.clone() });
            }
        }
        // A base the head doesn't contain would show the changes between them as the author's.
        for revision in [old, new] {
            if !self.is_ancestor(&revision.base, &revision.head)? {
                return Err(Error::BaseNotAncestor {
                    base: revision.base.clone(),
                    head: revision.head.clone(),
                });
            }
        }
        let plain = || self.changed_files(&old.head, &new.head);
        if old.base == new.base {
            return Ok(Comparison {
                files: plain()?.into_iter().map(ComparedFile::changed).collect(),
                old: old.head.clone(),
                new: new.head.clone(),
                replay: Replay::SameBase,
            });
        }
        // A rebase moves the base forward and the head with it. Otherwise what shows can have come
        // with the base.
        let retargeted = old.head == new.head || !self.is_ancestor(&old.base, &new.base)?;
        // Pinned before it's compared, so nothing can remove the tree meanwhile.
        match self.replay_pinned(|| self.replay(old, &new.base))? {
            Replayed::Clean(tree) => Ok(Comparison {
                files: self
                    .changed_files(&tree, &new.head)?
                    .into_iter()
                    .map(ComparedFile::changed)
                    .collect(),
                old: tree,
                new: new.head.clone(),
                replay: if retargeted {
                    Replay::Retargeted
                } else {
                    Replay::Clean
                },
            }),
            Replayed::Conflicted { tree, paths } => {
                let labels = [&new.base, &old.head];
                let conflicted = conflicted_paths(paths, &labels);
                log::info!(
                    "{}: replaying {} onto {} conflicted ({}), so comparing the heads",
                    self.name(),
                    old.head.short(7),
                    new.base.short(7),
                    match conflicted.len() {
                        0 => "no file left conflicted".to_string(),
                        1 => "1 file".to_string(),
                        n => format!("{n} files"),
                    }
                );
                let resolutions = self.changed_files(&tree, &new.head)?;
                Ok(Comparison {
                    files: with_conflicts(plain()?, &conflicted, &resolutions, &labels),
                    old: old.head.clone(),
                    new: new.head.clone(),
                    replay: Replay::Conflicted { tree, retargeted },
                })
            }
        }
    }

    /// Replays `old`'s changes onto `onto`, writing the tree but no ref.
    fn replay(&self, old: &Revision, onto: &Oid) -> Result<Replayed, Error> {
        let command = self.git().args([
            // A file the old revision added to a folder the base renamed goes where a rebase puts
            // it, into the renamed folder, rather than conflicting. If the author put it somewhere
            // else, the comparison shows that as a rename.
            "-c",
            "merge.directoryRenames=true",
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
        read_replay(display, &finished)
    }

    /// Runs `replay` and pins the tree it writes, the first time this process makes it.
    ///
    /// The tree is checked once pinned, since it can be gone by then: merge-tree only refreshes a
    /// tree it finds packed, and with git older than 2.52 another rozed's full repack, which
    /// listed what's reachable before the pin, can delete the pack. Replaying again writes it
    /// anew, so a tree that's gone is replayed once more.
    fn replay_pinned(
        &self,
        mut replay: impl FnMut() -> Result<Replayed, Error>,
    ) -> Result<Replayed, Error> {
        let mut failure = None;
        for attempt in 0..2 {
            let replayed = replay()?;
            let tree = replayed.tree();
            // Held while pinning, so this process writes each pin once.
            let mut pinned = self
                .pinned_replays()
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if pinned.contains(tree) {
                return Ok(replayed);
            }
            let error = match self.pin_replay(tree) {
                Ok(true) => {
                    pinned.insert(tree.clone());
                    return Ok(replayed);
                }
                Ok(false) => Error::Git {
                    command: "git merge-tree".into(),
                    message: format!("the tree it wrote, {tree}, was gone once pinned"),
                },
                Err(error) => error,
            };
            let again = if attempt == 0 {
                ", so replaying again"
            } else {
                ""
            };
            log::warn!(
                "{}: can't pin replayed tree {tree}{again}: {error}",
                self.name()
            );
            failure = Some(error);
        }
        Err(failure.expect("each attempt that doesn't return fails"))
    }

    /// Pins `tree` at `refs/lo/replays/<tree>/<process ID>`, and says whether the tree is
    /// there once pinned.
    fn pin_replay(&self, tree: &Oid) -> Result<bool, Error> {
        let reference = format!("{REPLAYS}{tree}/{}", std::process::id());
        // No lock: a ref only this process writes, always to the same tree, which git writes
        // atomically.
        self.run(self.git().args(["update-ref", &reference, tree.as_str()]))?;
        self.has_tree(tree)
    }

    /// Whether tree `oid` is in the clone. Never downloads anything.
    fn has_tree(&self, oid: &Oid) -> Result<bool, Error> {
        // `--missing` stops rev-list from downloading what's missing, and the filter from
        // listing anything in the tree.
        let finished = self
            .git()
            .args([
                "rev-list",
                "--objects",
                "--no-object-names",
                "--missing=allow-any",
                "--filter=tree:0",
                oid.as_str(),
                "--",
            ])
            .finish(None)?;
        Ok(finished.status.success()
            && String::from_utf8_lossy(&finished.stdout).trim() == oid.as_str())
    }
}

/// Reads how `git merge-tree`, run as `display`, went.
fn read_replay(display: String, finished: &Finished) -> Result<Replayed, Error> {
    // The exit status says whether it's clean: 0 is, 1 conflicted, and anything else failed.
    // A conflict can leave no path conflicted, so the paths don't say.
    let clean = match finished.status.code() {
        Some(0) => true,
        Some(1) => false,
        _ => return Err(Error::from_git(display, failure_message(finished), None)),
    };
    let Some((tree, paths)) = parse_merge_tree(&finished.stdout) else {
        // git's message, which can say a download failed: git 2.41 exits 1, as for a conflict,
        // when it can't read a commit.
        let message = match finished.stderr.trim() {
            "" => format!(
                "printed unexpected output {:?}",
                String::from_utf8_lossy(&finished.stdout)
            ),
            stderr => stderr.to_string(),
        };
        return Err(Error::from_git(display, message, None));
    };
    Ok(if clean {
        Replayed::Clean(tree)
    } else {
        Replayed::Conflicted { tree, paths }
    })
}

impl RepoCache {
    /// Removes every replay pin from the clone at `git_dir`. Only for a clone no rozed has open,
    /// where every pin is one a rozed left when it quit or crashed: a rozed pins a tree for as
    /// long as it has the clone open, and quitting runs no destructors anyway.
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
#[derive(Debug)]
enum Replayed {
    Clean(Oid),
    /// The tree, conflicts and all, and the paths git listed as conflicted.
    Conflicted {
        tree: Oid,
        paths: Vec<Vec<u8>>,
    },
}

impl Replayed {
    fn tree(&self) -> &Oid {
        match self {
            Replayed::Clean(tree) | Replayed::Conflicted { tree, .. } => tree,
        }
    }
}

/// Reads `git merge-tree --write-tree -z --name-only --no-messages`: the tree, then each
/// conflicted path, each ending in NUL.
fn parse_merge_tree(output: &[u8]) -> Option<(Oid, Vec<Vec<u8>>)> {
    let output = output.strip_suffix(b"\0")?;
    let mut fields = output.split(|byte| *byte == 0);
    let tree = std::str::from_utf8(fields.next()?).ok()?.parse().ok()?;
    let paths: Vec<Vec<u8>> = fields.map(<[u8]>::to_vec).collect();
    if paths.iter().any(Vec::is_empty) {
        return None;
    }
    Some((tree, paths))
}

/// The path `path` was set aside from, when it's a copy git set aside in a conflict under a name
/// of its own: `<path>~<label>`, or `<path>~<label>_<n>` when that was taken, where `label` is
/// one of `labels`, the commits merge-tree was given, which name its sides.
fn set_aside_from<'a>(path: &'a [u8], labels: &[&Oid]) -> Option<&'a [u8]> {
    let tilde = path.iter().rposition(|byte| *byte == b'~')?;
    let (from, suffix) = (&path[..tilde], &path[tilde + 1..]);
    let named = labels.iter().any(|label| {
        suffix
            .strip_prefix(label.as_str().as_bytes())
            .is_some_and(|rest| match rest.strip_prefix(b"_") {
                None => rest.is_empty(),
                Some(number) => !number.is_empty() && number.iter().all(u8::is_ascii_digit),
            })
    });
    (named && !from.is_empty()).then_some(from)
}

/// The paths a replay conflicted on, from the paths git `listed`, each once, in order. A copy git
/// set aside, which is in neither revision, stands for the path it came from.
fn conflicted_paths(listed: Vec<Vec<u8>>, labels: &[&Oid]) -> Vec<Vec<u8>> {
    let mut paths: Vec<Vec<u8>> = listed
        .into_iter()
        .map(|path| match set_aside_from(&path, labels) {
            Some(from) => from.to_vec(),
            None => path,
        })
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

/// The files that changed between the heads, `plain`, with each path in `conflicted` marked, and
/// added in path order where it didn't change, each with its part of `resolutions`, what changed
/// from the conflicted replay to the new head.
fn with_conflicts(
    plain: Vec<ChangedFile>,
    conflicted: &[Vec<u8>],
    resolutions: &[ChangedFile],
    labels: &[&Oid],
) -> Vec<ComparedFile> {
    // Whether `file` is at `path`, or at a copy of it git set aside.
    let at = |file: &ChangedFile, path: &[u8]| {
        [file.old_path(), file.new_path()]
            .into_iter()
            .flatten()
            .any(|at| set_aside_from(at, labels).unwrap_or(at) == path)
    };
    let mut files: Vec<ComparedFile> = plain.into_iter().map(ComparedFile::changed).collect();
    for path in conflicted {
        let resolution: Vec<ChangedFile> = resolutions
            .iter()
            .filter(|file| at(file, path))
            .cloned()
            .collect();
        let mut listed = false;
        for file in &mut files {
            if file.change.as_ref().is_some_and(|change| at(change, path)) {
                listed = true;
                let into = file.resolution.get_or_insert_with(Vec::new);
                for part in &resolution {
                    if !into.contains(part) {
                        into.push(part.clone());
                    }
                }
            }
        }
        if !listed {
            let index = files
                .iter()
                .position(|file| file.path > *path)
                .unwrap_or(files.len());
            files.insert(
                index,
                ComparedFile {
                    path: path.clone(),
                    change: None,
                    resolution: Some(resolution),
                },
            );
        }
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::Cell;
    use std::fs;
    use std::os::unix::process::ExitStatusExt as _;
    use std::process::{Command, ExitStatus};

    use tempfile::TempDir;

    use crate::Git;

    /// The test's own git in `dir`, without the user's config.
    fn plain_git(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "rozed test")
            .env("GIT_AUTHOR_EMAIL", "test@lo.invalid")
            .env("GIT_COMMITTER_NAME", "rozed test")
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

    /// A clone with one commit fetched, served over `file://`.
    struct Fixture {
        _dir: TempDir,
        repo: Repo,
        /// The fetched commit's tree.
        tree: Oid,
        /// A tree only the server has.
        unfetched: Oid,
    }

    impl Fixture {
        fn new() -> Self {
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
            let commit = |file: &str| {
                fs::write(work.join(file), format!("{file}\n")).unwrap();
                plain_git(&work, &["add", "-A"]);
                plain_git(&work, &["commit", "-q", "-m", file]);
                let server = server.to_str().unwrap();
                let branch = format!("HEAD:refs/heads/{}", file.replace('.', "-"));
                plain_git(&work, &["push", "-q", server, &branch]);
                let commit: Oid = plain_git(&work, &["rev-parse", "HEAD"]).parse().unwrap();
                let tree = plain_git(&work, &["rev-parse", "HEAD^{tree}"])
                    .parse()
                    .unwrap();
                (commit, tree)
            };
            let (fetched, tree) = commit("a.txt");
            let (_, unfetched) = commit("b.txt");
            let remotes = dir.path().join("remotes");
            let cache = RepoCache::new(dir.path().join("cache"), Git::find().unwrap())
                .unwrap()
                .with_remote_base(format!("file://{}", remotes.display()));
            let repo = cache.open("owner", "repo", None).unwrap();
            repo.fetch_commit(&fetched).unwrap();
            Self {
                _dir: dir,
                repo,
                tree,
                unfetched,
            }
        }

        fn pins(&self) -> String {
            plain_git(
                self.repo.git_dir(),
                &["for-each-ref", "--format=%(objectname)", REPLAYS],
            )
        }

        fn packs(&self) -> usize {
            fs::read_dir(self.repo.git_dir().join("objects/pack"))
                .unwrap()
                .count()
        }
    }

    #[test]
    fn tells_whether_a_tree_is_here_without_downloading_it() {
        let clone = Fixture::new();
        let packs = clone.packs();

        assert!(clone.repo.has_tree(&clone.tree).unwrap());
        assert!(!clone.repo.has_tree(&clone.unfetched).unwrap());
        assert!(!clone.repo.has_tree(&UNKNOWN.parse().unwrap()).unwrap());
        assert_eq!(clone.packs(), packs);
    }

    /// A tree another rozed's full repack deleted between the replay and its pin is replayed
    /// again, which writes it anew. Here the first replay names a tree that's nowhere, as one
    /// made in the clone and deleted from it is.
    #[test]
    fn a_replayed_tree_gone_by_the_time_its_pinned_is_replayed_again() {
        let clone = Fixture::new();
        let replays = Cell::new(0);

        let replayed = clone
            .repo
            .replay_pinned(|| {
                replays.set(replays.get() + 1);
                let tree = match replays.get() {
                    1 => UNKNOWN.parse().unwrap(),
                    _ => clone.tree.clone(),
                };
                Ok(Replayed::Clean(tree))
            })
            .unwrap();

        assert!(matches!(replayed, Replayed::Clean(tree) if tree == clone.tree));
        assert_eq!(replays.get(), 2);
        assert_eq!(clone.pins(), clone.tree.as_str());

        // Pinned once: replaying it again pins nothing more.
        clone
            .repo
            .replay_pinned(|| Ok(Replayed::Clean(clone.tree.clone())))
            .unwrap();
        assert_eq!(clone.pins(), clone.tree.as_str());
    }

    #[test]
    fn a_replayed_tree_still_gone_after_a_second_replay_is_an_error() {
        let clone = Fixture::new();
        let replays = Cell::new(0);

        let error = clone
            .repo
            .replay_pinned(|| {
                replays.set(replays.get() + 1);
                Ok(Replayed::Clean(UNKNOWN.parse().unwrap()))
            })
            .unwrap_err();

        assert!(matches!(error, Error::Git { .. }), "{error:?}");
        assert_eq!(replays.get(), 2);
        assert_eq!(clone.pins(), "");
    }

    const TREE: &str = "f74d6ab317132cd8ec8c2165016f89a7d9e03caa";
    /// An object that's nowhere.
    const UNKNOWN: &str = "3333333333333333333333333333333333333333";

    fn finished(code: i32, stdout: &[u8], stderr: &str) -> Finished {
        Finished {
            status: ExitStatus::from_raw(code << 8),
            stdout: stdout.to_vec(),
            stderr: stderr.to_string(),
        }
    }

    #[test]
    fn output_that_doesnt_parse_is_reported_with_gits_message() {
        // What git 2.41 does when it can't read a commit, as when downloading it failed: exit 1,
        // as for a conflict, with nothing on stdout.
        let error = read_replay(
            "git merge-tree".into(),
            &finished(
                1,
                b"",
                "fatal: unable to access 'https://github.com/a/b.git/': Could not resolve host: \
                 github.com\nfatal: could not fetch 6178079 from promisor remote\nmerge-tree: \
                 6178079 - not something we can merge",
            ),
        )
        .unwrap_err();
        assert!(
            matches!(
                error,
                Error::Fetch {
                    failure: crate::FetchFailure::Network,
                    ..
                }
            ),
            "{error:?}"
        );
        assert!(error.is_retryable());

        let error = read_replay(
            "git merge-tree".into(),
            &finished(1, b"", "merge-tree: 6178079 - not something we can merge"),
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "`git merge-tree` failed: merge-tree: 6178079 - not something we can merge"
        );

        // With nothing on stderr, what it printed.
        let error =
            read_replay("git merge-tree".into(), &finished(0, b"not a tree\0", "")).unwrap_err();
        assert!(
            matches!(&error, Error::Git { message, .. } if message.contains("not a tree")),
            "{error:?}"
        );
    }

    #[test]
    fn reads_whether_a_replay_was_clean_from_its_exit_status() {
        let output = format!("{TREE}\0");
        let clean = read_replay("git merge-tree".into(), &finished(0, output.as_bytes(), ""));
        assert!(matches!(clean, Ok(Replayed::Clean(tree)) if tree.as_str() == TREE));
        // A conflict can leave no path conflicted, as over where a renamed folder's files go.
        let conflicted = read_replay("git merge-tree".into(), &finished(1, output.as_bytes(), ""));
        assert!(
            matches!(conflicted, Ok(Replayed::Conflicted { tree, paths }) if tree.as_str() == TREE && paths.is_empty())
        );
        let failed = read_replay(
            "git merge-tree".into(),
            &finished(128, b"", "fatal: bad object 3333"),
        );
        assert!(
            matches!(&failed, Err(Error::Git { message, .. }) if message == "fatal: bad object 3333"),
            "{failed:?}"
        );
    }

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

    /// git lists each conflicted path once.
    #[test]
    fn reads_conflicted_paths_as_bytes() {
        let mut output = format!("{TREE}\0src/a.txt\0").into_bytes();
        output.extend_from_slice(b"caf\xe9\tname\n.txt\0");

        let (tree, paths) = parse_merge_tree(&output).unwrap();

        assert_eq!(tree.as_str(), TREE);
        assert_eq!(paths, [&b"src/a.txt"[..], b"caf\xe9\tname\n.txt"]);
    }

    /// The commits merge-tree was given, which name the copies it sets aside.
    const ONTO: &str = "856e106c454a82f1827674a185f94a2456c8883d";
    const OLD_HEAD: &str = "b3ab5414d310233f6e349cbb75e151cb86349f8b";

    #[test]
    fn copies_git_set_aside_stand_for_the_paths_they_came_from() {
        let (onto, old_head): (Oid, Oid) = (ONTO.parse().unwrap(), OLD_HEAD.parse().unwrap());
        let labels = [&onto, &old_head];
        for (path, from) in [
            (format!("l.txt~{ONTO}"), Some("l.txt")),
            (format!("src/a.rs~{OLD_HEAD}"), Some("src/a.rs")),
            // With a number when the name was taken.
            (format!("a~b.txt~{ONTO}_0"), Some("a~b.txt")),
            (format!("dir~{OLD_HEAD}_12"), Some("dir")),
            ("a~b.txt".into(), None),
            (format!("l.txt~{TREE}"), None),
            (format!("l.txt~{ONTO}x"), None),
            (format!("l.txt~{ONTO}_"), None),
            (format!("l.txt~{ONTO}_1a"), None),
            (format!("~{ONTO}"), None),
        ] {
            assert_eq!(
                set_aside_from(path.as_bytes(), &labels),
                from.map(str::as_bytes),
                "{path}"
            );
        }

        // Each path once, in order, with copies standing for theirs.
        let listed = vec![
            b"dir/f".to_vec(),
            format!("dir~{ONTO}").into_bytes(),
            b"l.txt".to_vec(),
            format!("l.txt~{OLD_HEAD}").into_bytes(),
        ];
        assert_eq!(
            conflicted_paths(listed, &labels),
            [&b"dir"[..], b"dir/f", b"l.txt"]
        );
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
