//! The cache's size, clearing it, and removing what nobody used for a month.

use std::fs::{self, File};
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use repo_cache::{PullRequestRefs, Repo, RepoCache};
use tempfile::TempDir;

use crate::support::{Server, git};

/// A server with repositories `owner/old` and `owner/new`, each with pull request #1.
fn server() -> Server {
    let server = Server::new();
    for name in ["old", "new"] {
        let remote = server.repo("owner", name);
        remote.write("README.md", format!("# {name}\n"));
        remote.commit("base");
        remote.publish("refs/heads/trunk");
        remote.switch("feature").write("feature.txt", "feature\n");
        remote.commit("feature");
        remote.publish("refs/pull/1/head");
    }
    server
}

/// Opens `owner/name`, fetches pull request #1, checks it out and closes it again. Returns the
/// clone's folder.
fn use_repo(cache: &RepoCache, name: &str) -> PathBuf {
    let repo = open_and_check_out(cache, name);
    repo.git_dir().to_path_buf()
}

/// Opens `owner/name`, fetches pull request #1 and checks it out.
fn open_and_check_out(cache: &RepoCache, name: &str) -> Repo {
    let repo = cache.open("owner", name, None).unwrap();
    let head = fetch(&repo);
    repo.checkout(1, &head).unwrap();
    repo
}

fn fetch(repo: &Repo) -> repo_cache::Oid {
    repo.fetch_pull(&PullRequestRefs {
        number: 1,
        head: None,
        base_branch: "trunk".into(),
        base: None,
    })
    .unwrap()
    .head
}

/// Makes the clone at `git_dir` look last opened `days` ago.
fn last_opened_days_ago(git_dir: &Path, days: u64) {
    stamp_days_ago(git_dir, days);
}

/// [`RepoCache::remove_unused`], once a lock just let go is free. A child process another test
/// thread starts at that moment can hold a copy of the lock's file descriptor until it runs git, so
/// this allows a moment. The same goes for every test that lets go of a `Repo` and then cleans up.
fn remove_unused_once_free(cache: &RepoCache) -> Vec<String> {
    let mut removed = Vec::new();
    for _ in 0..100 {
        removed = cache.remove_unused(RepoCache::UNUSED_FOR).unwrap();
        if !removed.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    removed
}

/// Writes a last-used stamp `days` old into `dir`.
fn stamp_days_ago(dir: &Path, days: u64) {
    let then = SystemTime::now() - Duration::from_secs(days * 24 * 60 * 60);
    let seconds = then.duration_since(UNIX_EPOCH).unwrap().as_secs();
    fs::write(dir.join("lo-last-used"), format!("{seconds}\n")).unwrap();
}

/// A folder outside the cache, holding files cleanup must never touch, laid out like what it
/// removes: `old.git`, an unused clone; `ghost`, an orphaned checkout; `old/pull-1`, a checkout.
fn outside_folder() -> TempDir {
    let outside = TempDir::new().unwrap();
    for dir in ["old.git", "ghost", "old/pull-1"] {
        fs::create_dir_all(outside.path().join(dir)).unwrap();
        fs::write(outside.path().join(dir).join("keep.txt"), "important\n").unwrap();
    }
    stamp_days_ago(&outside.path().join("old.git"), 365);
    outside
}

/// Whether every file in [`outside_folder`] is still there.
fn untouched(outside: &TempDir) -> bool {
    ["old.git", "ghost", "old/pull-1"]
        .iter()
        .all(|dir| outside.path().join(dir).join("keep.txt").is_file())
}

#[test]
fn size_counts_clones_and_checkouts() {
    let server = server();
    let (_dir, cache) = server.cache();
    let empty = cache.size().unwrap();

    use_repo(&cache, "old");

    let used = cache.size().unwrap();
    assert!(used > empty, "{empty} then {used}");
    // At least the checkout's files and the clone's packs.
    assert!(used > 8 * 1024, "{used}");
}

#[test]
fn clear_removes_every_clone_and_checkout() {
    let server = server();
    let (dir, cache) = server.cache();
    use_repo(&cache, "old");
    use_repo(&cache, "new");

    cache.clear().unwrap();

    assert!(!dir.path().join("repos").exists());
    assert!(!dir.path().join("checkouts").exists());
    assert!(cache.size().unwrap() < 64 * 1024);
    // And the cache still works.
    let repo = use_repo(&cache, "old");
    assert!(repo.is_dir());
}

#[test]
fn clear_leaves_a_clone_another_lo_is_making_alone() {
    let server = server();
    let (dir, cache) = server.cache();
    use_repo(&cache, "old");
    // Another Lo is making a clone, in a temporary folder of its own.
    let making = dir.path().join("tmp/99999-0.git");
    fs::create_dir_all(&making).unwrap();
    fs::write(making.join("HEAD"), "ref: refs/heads/main\n").unwrap();

    cache.clear().unwrap();

    assert!(making.join("HEAD").is_file());
    assert!(!dir.path().join("repos").exists());
    assert!(!dir.path().join("checkouts").exists());
    assert_eq!(fs::read_dir(dir.path().join("tmp")).unwrap().count(), 1);
}

#[test]
fn removes_only_clones_unused_for_a_month_and_their_checkouts() {
    let server = server();
    let (dir, cache) = server.cache();
    let old = use_repo(&cache, "old");
    let new = use_repo(&cache, "new");
    last_opened_days_ago(&old, 31);
    last_opened_days_ago(&new, 29);
    // A checkout whose clone is gone.
    let orphan = dir.path().join("checkouts/github.com/someone/ghost/pull-3");
    fs::create_dir_all(&orphan).unwrap();
    fs::write(orphan.join("file.txt"), "orphaned\n").unwrap();

    let removed = remove_unused_once_free(&cache);

    assert_eq!(removed, vec!["owner/old".to_string()]);
    let checkouts = dir.path().join("checkouts/github.com");
    assert!(!old.exists());
    assert!(!checkouts.join("owner/old").exists());
    assert!(new.is_dir());
    let head = git(&new, &["rev-parse", "refs/lo/pull/1/head"]);
    assert!(
        checkouts
            .join(format!("owner/new/pull-1/{head}/README.md"))
            .is_file()
    );
    assert!(!dir.path().join("checkouts/github.com/someone").exists());
    // Each was moved aside, then deleted, leaving nothing behind.
    assert_eq!(fs::read_dir(dir.path().join("tmp")).unwrap().count(), 0);

    // Nothing else is stale now.
    assert_eq!(
        cache.remove_unused(RepoCache::UNUSED_FOR).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn a_clone_being_written_to_is_kept() {
    let server = server();
    let (dir, cache) = server.cache();
    let old = use_repo(&cache, "old");
    last_opened_days_ago(&old, 31);
    // Another Lo holds the clone's lock.
    let lock = File::create(dir.path().join("locks/github.com/owner/old.lock")).unwrap();
    lock.lock().unwrap();

    assert_eq!(
        cache.remove_unused(RepoCache::UNUSED_FOR).unwrap(),
        Vec::<String>::new()
    );
    assert!(old.is_dir());

    // Once it lets go, the clone goes.
    drop(lock);
    assert_eq!(
        remove_unused_once_free(&cache),
        vec!["owner/old".to_string()]
    );
}

#[test]
fn opening_a_clone_keeps_it() {
    let server = server();
    let (_dir, cache) = server.cache();
    let old = use_repo(&cache, "old");
    last_opened_days_ago(&old, 31);

    cache.open("owner", "old", None).unwrap();

    assert_eq!(
        cache.remove_unused(RepoCache::UNUSED_FOR).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn cleanup_never_follows_a_symlinked_owner_folder() {
    let server = server();
    let (dir, cache) = server.cache();
    let old = use_repo(&cache, "old");
    last_opened_days_ago(&old, 31);
    let outside = outside_folder();
    // Owner folders that are symlinks to a folder outside the cache: one with what looks like an
    // unused clone, one with what looks like an orphaned checkout.
    symlink(outside.path(), dir.path().join("repos/github.com/someone")).unwrap();
    symlink(
        outside.path(),
        dir.path().join("checkouts/github.com/someone"),
    )
    .unwrap();

    assert_eq!(
        remove_unused_once_free(&cache),
        vec!["owner/old".to_string()]
    );

    assert!(untouched(&outside));
    // The real unused clone still goes.
    assert!(!old.exists());
}

#[test]
fn cleanup_never_follows_a_symlinked_repository_or_checkouts_folder() {
    let server = server();
    let (dir, cache) = server.cache();
    let old = use_repo(&cache, "old");
    last_opened_days_ago(&old, 31);
    let outside = outside_folder();
    // The unused clone's checkouts, and an orphaned repository's, are symlinks out of the cache.
    let checkouts = dir.path().join("checkouts/github.com/owner");
    fs::remove_dir_all(checkouts.join("old")).unwrap();
    symlink(outside.path().join("old"), checkouts.join("old")).unwrap();
    symlink(outside.path().join("ghost"), checkouts.join("ghost")).unwrap();
    symlink(
        outside.path().join("old.git"),
        dir.path().join("repos/github.com/owner/stale.git"),
    )
    .unwrap();

    assert_eq!(
        remove_unused_once_free(&cache),
        vec!["owner/old".to_string()]
    );

    assert!(untouched(&outside));
    assert!(!old.exists());
}

#[test]
fn cleanup_never_follows_a_symlink_above_a_clones_checkouts() {
    let server = server();
    let (dir, cache) = server.cache();
    let old = use_repo(&cache, "old");
    last_opened_days_ago(&old, 31);
    let outside = outside_folder();
    // The owner folder above the unused clone's checkouts is a symlink out of the cache.
    let checkouts = dir.path().join("checkouts/github.com/owner");
    fs::remove_dir_all(&checkouts).unwrap();
    symlink(outside.path(), &checkouts).unwrap();

    assert_eq!(
        remove_unused_once_free(&cache),
        vec!["owner/old".to_string()]
    );

    assert!(untouched(&outside));
    assert!(!old.exists());
}

#[test]
fn a_clone_still_open_is_kept() {
    let server = server();
    let (_dir, cache) = server.cache();
    let open = open_and_check_out(&cache, "old");
    last_opened_days_ago(open.git_dir(), 31);

    // Another Lo's launch, while this one still shows a pull request from the clone.
    assert_eq!(
        cache.remove_unused(RepoCache::UNUSED_FOR).unwrap(),
        Vec::<String>::new()
    );
    assert!(open.git_dir().is_dir());

    let git_dir = open.git_dir().to_path_buf();
    drop(open);
    assert_eq!(
        remove_unused_once_free(&cache),
        vec!["owner/old".to_string()]
    );
    assert!(!git_dir.exists());
}

#[test]
fn fetching_or_checking_out_keeps_a_clone() {
    let server = server();
    let (_dir, cache) = server.cache();
    for name in ["old", "new"] {
        // Opened a month ago, and used since.
        let repo = cache.open("owner", name, None).unwrap();
        last_opened_days_ago(repo.git_dir(), 31);
        let head = fetch(&repo);
        if name == "new" {
            last_opened_days_ago(repo.git_dir(), 31);
            repo.checkout(1, &head).unwrap();
        }
    }

    assert_eq!(
        cache.remove_unused(RepoCache::UNUSED_FOR).unwrap(),
        Vec::<String>::new()
    );
}

/// Makes the checkout in `dir` look last used `days` ago. Its stamp is in its git dir, which its
/// `.git` file names, so nothing in the checkout itself changes.
fn checkout_used_days_ago(dir: &Path, days: u64) {
    let dot_git = fs::read_to_string(dir.join(".git")).unwrap();
    let git_dir = dot_git.strip_prefix("gitdir:").unwrap().trim();
    stamp_days_ago(Path::new(git_dir), days);
}

#[test]
fn removes_checkouts_unused_for_two_weeks() {
    let server = server();
    let (_dir, cache) = server.cache();
    let repo = cache.open("owner", "old", None).unwrap();
    let head = fetch(&repo);
    let base = git(repo.git_dir(), &["rev-parse", "refs/lo/pull/1/base"])
        .parse()
        .unwrap();
    let stale = repo.checkout(1, &head).unwrap();
    let fresh = repo.checkout(2, &base).unwrap();
    checkout_used_days_ago(&stale, 15);
    checkout_used_days_ago(&fresh, 13);
    let git_dir = repo.git_dir().to_path_buf();
    drop(repo);

    // No clone goes, but the stale checkout does, once the clone's in-use lock is free.
    for _ in 0..100 {
        assert_eq!(
            cache.remove_unused(RepoCache::UNUSED_FOR).unwrap(),
            Vec::<String>::new()
        );
        if !stale.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    assert!(!stale.exists());
    assert!(!stale.parent().unwrap().exists(), "its empty pull-1 folder");
    assert!(fresh.join("README.md").is_file());
    let worktrees = git(&git_dir, &["worktree", "list", "--porcelain"]);
    assert!(!worktrees.contains(stale.to_str().unwrap()), "{worktrees}");
    assert_eq!(worktrees.matches("worktree ").count(), 2, "{worktrees}");
}

#[test]
fn a_checkout_of_a_clone_still_open_is_kept() {
    let server = server();
    let (_dir, cache) = server.cache();
    let repo = cache.open("owner", "old", None).unwrap();
    let head = fetch(&repo);
    let checkout = repo.checkout(1, &head).unwrap();
    checkout_used_days_ago(&checkout, 15);

    cache.remove_unused(RepoCache::UNUSED_FOR).unwrap();

    assert!(checkout.join("README.md").is_file());
    drop(repo);
}

#[test]
fn checking_a_head_out_again_keeps_its_checkout() {
    let server = server();
    let (_dir, cache) = server.cache();
    let repo = cache.open("owner", "old", None).unwrap();
    let head = fetch(&repo);
    let checkout = repo.checkout(1, &head).unwrap();
    checkout_used_days_ago(&checkout, 15);
    repo.checkout(1, &head).unwrap();
    drop(repo);

    cache.remove_unused(RepoCache::UNUSED_FOR).unwrap();

    assert!(checkout.join("README.md").is_file());
}
