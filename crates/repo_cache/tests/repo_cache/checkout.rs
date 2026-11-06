//! Checkouts of pull request heads.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::sync::Arc;

use repo_cache::{Oid, PullRequestRefs, Repo, RepoCache};
use tempfile::TempDir;

use crate::support::{Remote, Server, git};

/// Pull request #1, with a second revision `r2` that changes, adds and removes files.
struct Fixture {
    server: Server,
    remote: Remote,
    r1: Oid,
    r2: Oid,
    cache_dir: TempDir,
    cache: RepoCache,
    repo: Repo,
}

fn fixture() -> Fixture {
    let server = Server::new();
    let remote = server.repo("owner", "repo");
    remote
        .write("README.md", "# repo\n")
        .write("src/lib.rs", "pub fn a() {}\n");
    remote.commit("base");
    remote.publish("refs/heads/trunk");
    remote
        .switch("feature")
        .write("src/lib.rs", "pub fn a() { 1 }\n")
        .write("removed_later.txt", "here in r1\n")
        .gitlink("vendor/lib", "1111111111111111111111111111111111111111");
    let r1 = remote.commit("r1");
    remote.publish("refs/pull/1/head");
    remote
        .write("src/lib.rs", "pub fn a() { 2 }\n")
        .write("added_later.txt", "new in r2\n")
        .remove("removed_later.txt");
    let r2 = remote.commit("r2");

    let (cache_dir, cache) = server.cache();
    let repo = cache.open("owner", "repo", None).unwrap();
    fetch(&repo);
    Fixture {
        server,
        remote,
        r1,
        r2,
        cache_dir,
        cache,
        repo,
    }
}

fn fetch(repo: &Repo) -> Oid {
    repo.fetch_pull(&PullRequestRefs {
        number: 1,
        head: None,
        base_branch: "trunk".into(),
        base: None,
    })
    .unwrap()
    .head
}

impl Fixture {
    /// Pushes r2 as pull request #1's new head and fetches it.
    fn push_r2(&self) {
        self.remote.publish("refs/pull/1/head");
        assert_eq!(fetch(&self.repo), self.r2);
    }
}

/// Every symlink under `dir`, which must have none.
fn symlinks(dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let metadata = fs::symlink_metadata(&path).unwrap();
        if metadata.file_type().is_symlink() {
            found.push(path.display().to_string());
        } else if metadata.is_dir() {
            found.extend(symlinks(&path));
        }
    }
    found
}

#[test]
fn checks_out_a_pull_request_detached() {
    let f = fixture();

    let dir = f.repo.checkout(1, &f.r1).unwrap();

    assert_eq!(
        dir,
        f.cache_dir
            .path()
            .join("checkouts/github.com/owner/repo/pull-1")
    );
    assert_eq!(git(&dir, &["rev-parse", "HEAD"]), f.r1.as_str());
    assert_eq!(git(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]), "HEAD");
    assert_eq!(git(&dir, &["status", "--porcelain"]), "");
    assert_eq!(
        fs::read_to_string(dir.join("src/lib.rs")).unwrap(),
        "pub fn a() { 1 }\n"
    );
    // The submodule is an empty folder; its commit isn't fetched.
    assert!(dir.join("vendor/lib").is_dir());
    assert_eq!(fs::read_dir(dir.join("vendor/lib")).unwrap().count(), 0);
    // The checkout belongs to the clone.
    let dot_git = fs::read_to_string(dir.join(".git")).unwrap();
    assert!(
        dot_git.contains(f.repo.git_dir().join("worktrees").to_str().unwrap()),
        "{dot_git}"
    );
}

#[test]
fn the_checkout_follows_a_new_head_and_keeps_only_what_git_put_there() {
    let f = fixture();
    let dir = f.repo.checkout(1, &f.r1).unwrap();
    // A language server leaves things behind, and a file gets changed.
    fs::write(dir.join("untracked.txt"), "left behind\n").unwrap();
    fs::create_dir_all(dir.join("target/debug")).unwrap();
    fs::write(dir.join("target/debug/x"), "build output\n").unwrap();
    fs::write(dir.join("README.md"), "changed on disk\n").unwrap();

    f.push_r2();
    let moved = f.repo.checkout(1, &f.r2).unwrap();

    assert_eq!(moved, dir);
    assert_eq!(git(&dir, &["rev-parse", "HEAD"]), f.r2.as_str());
    assert_eq!(
        fs::read_to_string(dir.join("src/lib.rs")).unwrap(),
        "pub fn a() { 2 }\n"
    );
    assert!(dir.join("added_later.txt").is_file());
    assert!(!dir.join("removed_later.txt").exists());
    assert!(!dir.join("untracked.txt").exists());
    assert!(!dir.join("target").exists());
    assert_eq!(
        fs::read_to_string(dir.join("README.md")).unwrap(),
        "# repo\n"
    );
    assert_eq!(git(&dir, &["status", "--porcelain", "--ignored"]), "");
}

#[test]
fn nothing_in_a_checkout_points_outside_it() {
    let f = fixture();
    f.remote.switch("feature");
    symlink("../../../../../../etc", f.remote.work.join("escape")).unwrap();
    symlink("/etc/passwd", f.remote.work.join("absolute")).unwrap();
    fs::create_dir_all(f.remote.work.join("nested")).unwrap();
    symlink("..", f.remote.work.join("nested/up")).unwrap();
    let with_links = f.remote.commit("symlinks");
    f.remote.publish("refs/pull/1/head");
    assert_eq!(fetch(&f.repo), with_links);

    let dir = f.repo.checkout(1, &with_links).unwrap();

    assert_eq!(symlinks(&dir), Vec::<String>::new());
    // Each is a plain file holding its target.
    assert_eq!(
        fs::read_to_string(dir.join("escape")).unwrap(),
        "../../../../../../etc"
    );
    assert_eq!(
        fs::read_to_string(dir.join("absolute")).unwrap(),
        "/etc/passwd"
    );
    assert_eq!(fs::read_to_string(dir.join("nested/up")).unwrap(), "..");
}

#[test]
fn a_checkout_left_without_its_clone_is_rebuilt() {
    let f = fixture();
    let dir = f.repo.checkout(1, &f.r1).unwrap();
    fs::remove_dir_all(f.repo.git_dir()).unwrap();

    let repo = f.cache.open("owner", "repo", None).unwrap();
    let head = fetch(&repo);
    let rebuilt = repo.checkout(1, &head).unwrap();

    assert_eq!(rebuilt, dir);
    assert_eq!(git(&dir, &["rev-parse", "HEAD"]), f.r1.as_str());
    assert_eq!(git(&dir, &["status", "--porcelain"]), "");
}

#[test]
fn checkouts_at_once_all_succeed() {
    let f = fixture();
    f.push_r2();
    let cache = Arc::new(f.cache);

    // Two rozed windows on one pull request, one still on r1, check it out at once.
    let threads: Vec<_> = (0..8)
        .map(|i| {
            let cache = cache.clone();
            let head = if i % 2 == 0 {
                f.r1.clone()
            } else {
                f.r2.clone()
            };
            std::thread::spawn(move || {
                let repo = cache.open("owner", "repo", None).unwrap();
                let dir = repo.checkout(1, &head).unwrap();
                (dir, head)
            })
        })
        .collect();

    let mut dirs = Vec::new();
    for thread in threads {
        dirs.push(thread.join().unwrap().0);
    }
    let dir = &dirs[0];
    assert!(dirs.iter().all(|other| other == dir));
    let head = git(dir, &["rev-parse", "HEAD"]);
    assert!(head == f.r1.as_str() || head == f.r2.as_str(), "{head}");
    assert_eq!(git(dir, &["status", "--porcelain"]), "");
    let worktrees = git(f.repo.git_dir(), &["worktree", "list"]);
    assert_eq!(worktrees.lines().count(), 2, "{worktrees}");
    let _ = &f.server;
}

#[test]
fn hashes_a_checkouts_files_as_git_would() {
    let f = fixture();
    let dir = f.repo.checkout(1, &f.r1).unwrap();
    let blob = |path: &str| -> Oid {
        git(&f.remote.work, &["rev-parse", &format!("{}:{path}", f.r1)])
            .parse()
            .unwrap()
    };

    for path in ["README.md", "src/lib.rs", "removed_later.txt"] {
        assert_eq!(
            f.repo.hash_checkout_file(&dir, path).unwrap(),
            Some(blob(path)),
            "{path}"
        );
    }
    // Not regular files: missing, a folder, a submodule's empty folder.
    for path in ["missing.txt", "src", "vendor/lib"] {
        assert_eq!(
            f.repo.hash_checkout_file(&dir, path).unwrap(),
            None,
            "{path}"
        );
    }
    // A file changed on disk no longer hashes to its blob.
    fs::write(dir.join("README.md"), "changed on disk\n").unwrap();
    assert_ne!(
        f.repo.hash_checkout_file(&dir, "README.md").unwrap(),
        Some(blob("README.md"))
    );
    // Nothing outside the checkout.
    for path in ["../repo.git/HEAD", "/etc/passwd", ""] {
        assert!(f.repo.hash_checkout_file(&dir, path).is_err(), "{path}");
    }
}
