//! Checkouts of pull request heads.

use std::fs;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};
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
            .join(format!("checkouts/github.com/owner/repo/pull-1/{}", f.r1))
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
fn each_head_gets_a_checkout_of_its_own() {
    let f = fixture();
    let r1_dir = f.repo.checkout(1, &f.r1).unwrap();

    f.push_r2();
    let r2_dir = f.repo.checkout(1, &f.r2).unwrap();

    // A window still showing r1 keeps its checkout as it was.
    assert_ne!(r1_dir, r2_dir);
    assert_eq!(git(&r1_dir, &["rev-parse", "HEAD"]), f.r1.as_str());
    assert_eq!(
        fs::read_to_string(r1_dir.join("src/lib.rs")).unwrap(),
        "pub fn a() { 1 }\n"
    );
    assert!(r1_dir.join("removed_later.txt").is_file());
    assert_eq!(git(&r2_dir, &["rev-parse", "HEAD"]), f.r2.as_str());
    assert_eq!(
        fs::read_to_string(r2_dir.join("src/lib.rs")).unwrap(),
        "pub fn a() { 2 }\n"
    );
    assert!(r2_dir.join("added_later.txt").is_file());
    assert!(!r2_dir.join("removed_later.txt").exists());
}

#[test]
fn checking_a_head_out_again_keeps_only_what_git_put_there() {
    let f = fixture();
    let dir = f.repo.checkout(1, &f.r1).unwrap();
    // A language server leaves things behind, and a file gets changed.
    fs::write(dir.join("untracked.txt"), "left behind\n").unwrap();
    fs::create_dir_all(dir.join("target/debug")).unwrap();
    fs::write(dir.join("target/debug/x"), "build output\n").unwrap();
    fs::write(dir.join("README.md"), "changed on disk\n").unwrap();

    let again = f.repo.checkout(1, &f.r1).unwrap();

    assert_eq!(again, dir);
    assert!(!dir.join("untracked.txt").exists());
    assert!(!dir.join("target").exists());
    assert_eq!(
        fs::read_to_string(dir.join("README.md")).unwrap(),
        "# repo\n"
    );
    assert_eq!(git(&dir, &["status", "--porcelain", "--ignored"]), "");
}

#[test]
fn a_checkout_from_before_heads_had_their_own_is_replaced() {
    let f = fixture();
    // Earlier, a pull request had one checkout, `pull-<n>`, moved to each new head.
    let old = f
        .cache_dir
        .path()
        .join("checkouts/github.com/owner/repo/pull-1");
    fs::create_dir_all(old.parent().unwrap()).unwrap();
    git(
        f.repo.git_dir(),
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            old.to_str().unwrap(),
            f.r1.as_str(),
        ],
    );

    let dir = f.repo.checkout(1, &f.r1).unwrap();

    assert_eq!(dir, old.join(f.r1.as_str()));
    assert!(!old.join(".git").exists());
    assert!(!old.join("README.md").exists());
    assert_eq!(git(&dir, &["rev-parse", "HEAD"]), f.r1.as_str());
    let worktrees = git(f.repo.git_dir(), &["worktree", "list"]);
    assert_eq!(worktrees.lines().count(), 2, "{worktrees}");
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

    for thread in threads {
        let (dir, head) = thread.join().unwrap();
        assert_eq!(git(&dir, &["rev-parse", "HEAD"]), head.as_str());
        assert_eq!(git(&dir, &["status", "--porcelain"]), "");
    }
    // The clone, and a checkout for each head.
    let worktrees = git(f.repo.git_dir(), &["worktree", "list"]);
    assert_eq!(worktrees.lines().count(), 3, "{worktrees}");
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
            f.repo.hash_checkout_file(&dir, path.as_bytes()).unwrap(),
            Some(blob(path)),
            "{path}"
        );
    }
    // Not regular files: missing, a folder, a submodule's empty folder.
    for path in ["missing.txt", "src", "vendor/lib"] {
        assert_eq!(
            f.repo.hash_checkout_file(&dir, path.as_bytes()).unwrap(),
            None,
            "{path}"
        );
    }
    // A file changed on disk no longer hashes to its blob.
    fs::write(dir.join("README.md"), "changed on disk\n").unwrap();
    assert_ne!(
        f.repo.hash_checkout_file(&dir, b"README.md").unwrap(),
        Some(blob("README.md"))
    );
    // Nothing outside the checkout.
    for path in ["../repo.git/HEAD", "/etc/passwd", ""] {
        assert!(
            f.repo.hash_checkout_file(&dir, path.as_bytes()).is_err(),
            "{path}"
        );
    }
    // Nor in a folder that isn't one of the clone's checkouts, such as another repository.
    let error = f
        .repo
        .hash_checkout_file(&f.remote.work, b"README.md")
        .unwrap_err();
    assert!(
        error.to_string().contains("isn't a valid checkout"),
        "{error}"
    );
}

/// A program that leaves a marker named `name` in `markers` when it runs, so a test can see it did.
fn marker_program(markers: &Path, name: &str) -> PathBuf {
    fs::create_dir_all(markers).unwrap();
    let path = markers.with_file_name(format!("{name}-program"));
    let script = format!("#!/bin/sh\ntouch '{}/{name}'\nexit 0\n", markers.display());
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[test]
fn hostile_config_in_the_clone_has_no_effect() {
    let f = fixture();
    let scratch = TempDir::new().unwrap();
    let markers = scratch.path().join("markers");
    // What anything able to write the clone's config could set: hooks, a file-system monitor, and
    // attributes for every file.
    let hooks = scratch.path().join("hooks");
    let own_hooks = f.repo.git_dir().join("hooks");
    for (dir, prefix) in [(&hooks, ""), (&own_hooks, "own-")] {
        fs::create_dir_all(dir).unwrap();
        for hook in ["post-checkout", "reference-transaction"] {
            let program = marker_program(&markers, &format!("{prefix}{hook}"));
            fs::copy(program, dir.join(hook)).unwrap();
        }
    }
    let attributes = scratch.path().join("attributes");
    fs::write(&attributes, "* ident eol=crlf\n").unwrap();
    let fsmonitor = marker_program(&markers, "fsmonitor");
    for (key, value) in [
        ("core.hooksPath", hooks.to_str().unwrap()),
        ("core.fsmonitor", fsmonitor.to_str().unwrap()),
        ("core.attributesFile", attributes.to_str().unwrap()),
    ] {
        git(f.repo.git_dir(), &["config", key, value]);
    }

    // A checkout made, moved to a new head, and its files hashed.
    let dir = f.repo.checkout(1, &f.r1).unwrap();
    f.push_r2();
    let dir_r2 = f.repo.checkout(1, &f.r2).unwrap();
    for dir in [&dir, &dir_r2] {
        if dir.join("src/lib.rs").is_file() {
            f.repo.hash_checkout_file(dir, b"src/lib.rs").unwrap();
        }
    }

    let ran: Vec<_> = fs::read_dir(&markers)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(ran, Vec::<std::ffi::OsString>::new());
    assert_eq!(
        fs::read_to_string(dir_r2.join("src/lib.rs")).unwrap(),
        "pub fn a() { 2 }\n"
    );
}

#[test]
fn a_pull_requests_own_attributes_never_apply_in_its_checkout() {
    let f = fixture();
    f.remote.switch("feature");
    // Attributes that would change the checkout's bytes: keyword expansion, line endings and a
    // re-encoding. git applies the last when committing, so the blob is UTF-8.
    f.remote
        .write(
            ".gitattributes",
            "*.txt ident eol=crlf\n*.enc working-tree-encoding=UTF-16LE\n",
        )
        .write("id.txt", "$Id$\nline\n")
        .write("x.enc", b"h\0i\0\n\0");
    let head = f.remote.commit("attributes");
    f.remote.publish("refs/pull/1/head");
    assert_eq!(fetch(&f.repo), head);

    let dir = f.repo.checkout(1, &head).unwrap();

    for path in ["id.txt", "x.enc", ".gitattributes"] {
        let blob = f.repo.read_file(&head, path.as_bytes()).unwrap().unwrap();
        assert_eq!(
            fs::read(dir.join(path)).unwrap(),
            blob,
            "{path} differs from its blob"
        );
        let id = git(&f.remote.work, &["rev-parse", &format!("{head}:{path}")]);
        assert_eq!(
            f.repo.hash_checkout_file(&dir, path.as_bytes()).unwrap(),
            Some(id.parse().unwrap()),
            "{path}"
        );
    }
}

/// Whether the disk under `dir` treats `a` and `b` as one name.
fn same_name_on_disk(dir: &Path, a: &str, b: &str) -> bool {
    let probe = TempDir::new_in(dir).unwrap();
    fs::write(probe.path().join(a), "").unwrap();
    probe.path().join(b).exists()
}

#[test]
fn names_a_disk_folds_together_never_both_hash_to_their_blobs() {
    let f = fixture();
    f.remote.switch("feature");
    let nfc = "caf\u{e9}.txt";
    let nfd = "cafe\u{301}.txt";
    let head = f.remote.commit_with_names(
        &[
            (b"Lib.rs", b"pub fn evil() {}\n"),
            (b"lib.rs", b"pub fn benign() {}\n"),
            (nfc.as_bytes(), b"composed\n"),
            (nfd.as_bytes(), b"decomposed\n"),
        ],
        "names that differ only in case or normalization",
    );
    f.remote.publish("refs/pull/1/head");
    assert_eq!(fetch(&f.repo), head);

    // Each name reads as its own file from git.
    for (path, contents) in [
        ("Lib.rs", "pub fn evil() {}\n"),
        ("lib.rs", "pub fn benign() {}\n"),
        (nfc, "composed\n"),
        (nfd, "decomposed\n"),
    ] {
        assert_eq!(
            f.repo.read_file(&head, path.as_bytes()).unwrap().unwrap(),
            contents.as_bytes(),
            "{path:?}"
        );
    }
    let base = git(f.repo.git_dir(), &["rev-parse", "refs/lo/pull/1/base"]);
    let files = f.repo.changed_files(&base.parse().unwrap(), &head).unwrap();

    let dir = f.repo.checkout(1, &head).unwrap();

    for (a, b) in [("Lib.rs", "lib.rs"), (nfc, nfd)] {
        let matches = |path: &str| {
            let file = files
                .iter()
                .find(|file| file.path() == path.as_bytes())
                .unwrap_or_else(|| panic!("{path:?} not listed"));
            let blob = &file.new.as_ref().unwrap().oid;
            f.repo.hash_checkout_file(&dir, path.as_bytes()).unwrap() == Some(blob.clone())
        };
        let (a_matches, b_matches) = (matches(a), matches(b));
        if same_name_on_disk(&dir, a, b) {
            // One file holds one of the two, so the other is shown from git.
            assert!(!(a_matches && b_matches), "{a} and {b}");
        } else {
            assert!(a_matches && b_matches, "{a} and {b}");
        }
    }
}
