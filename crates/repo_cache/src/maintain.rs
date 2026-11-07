//! Keeping clones fast: repacking the packs every fetch and download adds.
//!
//! Lo turns git's own background maintenance off (`gc.auto=0`, `maintenance.auto=false`), so
//! that nothing runs behind its back. Every fetch, every batch of files a diff downloads and every
//! file read on its own adds a pack, and every pin a loose ref, so a repository reviewed every day
//! would end up with thousands of packs, each of which every object lookup searches. At launch,
//! [`RepoCache::maintain`] repacks the clones that passed a limit, under each clone's lock.

use std::fs;
use std::path::Path;
use std::time::Instant;

use crate::lock::RepoLock;
use crate::{Error, GitVersion, RepoCache};

/// The first git whose geometric repack keeps a partial clone's promisor packs as promisor packs.
/// Older ones get a full repack, which always has.
const GEOMETRIC_WITH_PROMISORS: GitVersion = GitVersion {
    major: 2,
    minor: 52,
    patch: 0,
};

impl RepoCache {
    /// How many packs a clone can have before [`RepoCache::maintain`] repacks it. git's own
    /// `gc.autoPackLimit` is the same.
    pub const PACK_LIMIT: usize = 50;

    /// Repacks every clone with more than `pack_limit` packs, and packs its refs. A clone being
    /// written to is skipped, and one that fails is logged and skipped. Returns the `owner/repo`
    /// of each clone repacked. Needs no network: what was downloaded stays, and what wasn't stays
    /// downloadable.
    pub fn maintain(&self, pack_limit: usize) -> Result<Vec<String>, Error> {
        let mut repacked = Vec::new();
        for (owner, repo) in self.clones()? {
            let git_dir = self.repo_dir(&owner, &repo);
            let packs = pack_count(&git_dir);
            if packs <= pack_limit {
                continue;
            }
            let Some(_lock) = RepoLock::try_acquire(&self.lock_path(&owner, &repo))? else {
                continue;
            };
            let started = Instant::now();
            let result = self
                .git(&git_dir)
                .args(repack_args(self.git_version()))
                .run()
                .and_then(|_| self.git(&git_dir).args(["pack-refs", "--all"]).run());
            match result {
                Ok(_) => {
                    log::info!(
                        "repacked {owner}/{repo} from {packs} packs to {} in {:?}",
                        pack_count(&git_dir),
                        started.elapsed()
                    );
                    repacked.push(format!("{owner}/{repo}"));
                }
                Err(error) => log::warn!("can't repack {owner}/{repo}: {error}"),
            }
        }
        Ok(repacked)
    }
}

/// How `git repack` runs with `version`: geometrically, rolling small packs into bigger ones and
/// leaving big ones be, where that keeps promisor packs; otherwise everything into one pack.
/// Bitmaps need every object, which a partial clone doesn't have.
fn repack_args(version: GitVersion) -> &'static [&'static str] {
    if version >= GEOMETRIC_WITH_PROMISORS {
        &[
            "repack",
            "-d",
            "-q",
            "--geometric=2",
            "--no-write-bitmap-index",
        ]
    } else {
        &["repack", "-a", "-d", "-q", "--no-write-bitmap-index"]
    }
}

/// How many packs the clone at `git_dir` has.
fn pack_count(git_dir: &Path) -> usize {
    fs::read_dir(git_dir.join("objects/pack")).map_or(0, |entries| {
        entries
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "pack"))
            .count()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs::File;
    use std::path::PathBuf;
    use std::process::Command;

    use tempfile::TempDir;

    use crate::{Git, Oid, PullRequestRefs, Repo};

    fn version(major: u32, minor: u32) -> GitVersion {
        GitVersion {
            major,
            minor,
            patch: 0,
        }
    }

    #[test]
    fn repacks_geometrically_only_where_promisor_packs_survive_it() {
        assert!(repack_args(version(2, 54)).contains(&"--geometric=2"));
        assert!(repack_args(version(2, 52)).contains(&"--geometric=2"));
        assert!(repack_args(version(2, 51)).contains(&"-a"));
        assert!(repack_args(version(2, 41)).contains(&"-a"));
    }

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

    /// Files the fixture's pull request adds. All but the last are read, one download each.
    const FILES: [&str; 9] = [
        "a.txt",
        "b.txt",
        "c.txt",
        "d.txt",
        "e.txt",
        "f.txt",
        "g.txt",
        "h.txt",
        "unread.txt",
    ];

    /// A clone with a pack for its fetch and one for each file read on its own, made by a git
    /// that says it's `version`.
    struct Fixture {
        dir: TempDir,
        cache: RepoCache,
        head: Oid,
    }

    impl Fixture {
        fn new(version: GitVersion) -> Self {
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
            plain_git(&work, &["commit", "-q", "--allow-empty", "-m", "base"]);
            let server_path = server.to_str().unwrap();
            plain_git(&work, &["push", "-q", server_path, "HEAD:refs/heads/trunk"]);
            for file in FILES {
                fs::write(work.join(file), format!("{file}\n")).unwrap();
            }
            plain_git(&work, &["add", "-A"]);
            plain_git(&work, &["commit", "-q", "-m", "files"]);
            plain_git(&work, &["push", "-q", server_path, "HEAD:refs/pull/1/head"]);

            let git = Git::find().unwrap().with_version(version);
            let cache = RepoCache::new(dir.path().join("cache"), git)
                .unwrap()
                .with_remote_base(format!("file://{}", dir.path().join("remotes").display()));
            let repo = open(&cache);
            let head = repo
                .fetch_pull(&PullRequestRefs {
                    number: 1,
                    head: None,
                    base_branch: "trunk".into(),
                    base: None,
                })
                .unwrap()
                .head;
            for file in &FILES[..FILES.len() - 1] {
                repo.read_file(&head, file.as_bytes()).unwrap().unwrap();
            }
            Self { dir, cache, head }
        }

        fn git_dir(&self) -> PathBuf {
            self.cache.repo_dir("owner", "repo")
        }

        fn server(&self) -> PathBuf {
            self.dir.path().join("remotes/owner/repo.git")
        }

        /// Every pack, and whether each has its `.promisor` marker.
        fn packs(&self) -> Vec<(PathBuf, bool)> {
            fs::read_dir(self.git_dir().join("objects/pack"))
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "pack"))
                .map(|path| {
                    let promisor = path.with_extension("promisor").is_file();
                    (path, promisor)
                })
                .collect()
        }
    }

    fn open(cache: &RepoCache) -> Repo {
        cache.open("owner", "repo", None).unwrap()
    }

    #[test]
    fn repacks_a_clone_with_too_many_packs() {
        for version in [Git::find().unwrap().version(), version(2, 41)] {
            let f = Fixture::new(version);
            let before = f.packs().len();
            assert!(before >= FILES.len(), "{version}: {before} packs");

            // Without the server, so repacking needs no network.
            let moved = f.server().with_extension("moved");
            fs::rename(f.server(), &moved).unwrap();
            assert_eq!(
                f.cache.maintain(FILES.len() / 2).unwrap(),
                vec!["owner/repo".to_string()],
                "{version}"
            );

            let after = f.packs();
            assert!(after.len() <= 2, "{version}: {after:?}");
            // Every pack is still a promisor pack, so what isn't downloaded stays downloadable.
            assert!(after.iter().all(|(_, promisor)| *promisor), "{after:?}");
            // The refs are packed.
            let loose_refs = f.git_dir().join("refs/lo");
            assert!(
                !loose_refs.exists() || fs::read_dir(&loose_refs).unwrap().count() == 0,
                "{version}"
            );
            // What was downloaded reads without the server.
            let repo = open(&f.cache);
            for file in &FILES[..FILES.len() - 1] {
                assert_eq!(
                    repo.read_file(&f.head, file.as_bytes()).unwrap().unwrap(),
                    format!("{file}\n").into_bytes(),
                    "{version}: {file}"
                );
            }
            assert!(repo.pull_revisions(1).unwrap().contains(&f.head));
            // And what wasn't still downloads once the server is back.
            fs::rename(&moved, f.server()).unwrap();
            assert_eq!(
                repo.read_file(&f.head, b"unread.txt").unwrap().unwrap(),
                b"unread.txt\n",
                "{version}"
            );
        }
    }

    #[test]
    fn leaves_clones_under_the_limit_or_being_written_to_alone() {
        let f = Fixture::new(Git::find().unwrap().version());
        let before = f.packs().len();

        assert_eq!(f.cache.maintain(before).unwrap(), Vec::<String>::new());
        assert_eq!(f.packs().len(), before);

        // Another Lo holds the clone's lock.
        let lock = File::create(f.cache.lock_path("owner", "repo")).unwrap();
        lock.lock().unwrap();
        assert_eq!(f.cache.maintain(1).unwrap(), Vec::<String>::new());
        assert_eq!(f.packs().len(), before);
    }
}
