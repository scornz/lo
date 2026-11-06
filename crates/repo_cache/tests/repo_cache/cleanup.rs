//! The cache's size, clearing it, and removing what nobody used for a month.

use std::fs::{self, File};
use std::os::unix::fs::symlink;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use repo_cache::{PullRequestRefs, Repo, RepoCache};
use tempfile::TempDir;

use crate::support::Server;

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

/// Opens `owner/name`, fetches pull request #1 and checks it out.
fn use_repo(cache: &RepoCache, name: &str) -> Repo {
    let repo = cache.open("owner", name, None).unwrap();
    let head = repo
        .fetch_pull(&PullRequestRefs {
            number: 1,
            head: None,
            base_branch: "trunk".into(),
            base: None,
        })
        .unwrap()
        .head;
    repo.checkout(1, &head).unwrap();
    repo
}

/// Makes `repo` look last opened `days` ago.
fn last_opened_days_ago(repo: &Repo, days: u64) {
    stamp_days_ago(repo.git_dir(), days);
}

/// Writes a last-used stamp `days` old into `dir`.
fn stamp_days_ago(dir: &Path, days: u64) {
    let then = SystemTime::now() - Duration::from_secs(days * 24 * 60 * 60);
    let seconds = then.duration_since(UNIX_EPOCH).unwrap().as_secs();
    fs::write(dir.join("rozed-last-used"), format!("{seconds}\n")).unwrap();
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
    assert!(repo.git_dir().is_dir());
}

#[test]
fn clear_leaves_a_clone_another_rozed_is_making_alone() {
    let server = server();
    let (dir, cache) = server.cache();
    use_repo(&cache, "old");
    // Another rozed is making a clone, in a temporary folder of its own.
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

    let removed = cache.remove_unused(RepoCache::UNUSED_FOR).unwrap();

    assert_eq!(removed, vec!["owner/old".to_string()]);
    let checkouts = dir.path().join("checkouts/github.com");
    assert!(!old.git_dir().exists());
    assert!(!checkouts.join("owner/old").exists());
    assert!(new.git_dir().is_dir());
    assert!(checkouts.join("owner/new/pull-1/README.md").is_file());
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
    // Another rozed holds the clone's lock.
    let lock = File::create(dir.path().join("locks/github.com/owner/old.lock")).unwrap();
    lock.lock().unwrap();

    assert_eq!(
        cache.remove_unused(RepoCache::UNUSED_FOR).unwrap(),
        Vec::<String>::new()
    );
    assert!(old.git_dir().is_dir());

    // Once it lets go, the clone goes. A child process another test thread starts at that moment
    // can hold a copy of the lock's file descriptor until it runs git, so allow a moment.
    drop(lock);
    let mut removed = Vec::new();
    for _ in 0..100 {
        removed = cache.remove_unused(RepoCache::UNUSED_FOR).unwrap();
        if !removed.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(removed, vec!["owner/old".to_string()]);
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

    cache.remove_unused(RepoCache::UNUSED_FOR).unwrap();

    assert!(untouched(&outside));
    // The real unused clone still goes.
    assert!(!old.git_dir().exists());
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

    cache.remove_unused(RepoCache::UNUSED_FOR).unwrap();

    assert!(untouched(&outside));
    assert!(!old.git_dir().exists());
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

    cache.remove_unused(RepoCache::UNUSED_FOR).unwrap();

    assert!(untouched(&outside));
    assert!(!old.git_dir().exists());
}
