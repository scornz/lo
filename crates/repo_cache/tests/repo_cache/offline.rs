//! Reading a clone offline: only what's there, without the server, without downloading anything,
//! and without waiting for another rozed's fetch.

use std::fs::File;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use repo_cache::{Error, Git, GitVersion};

use crate::fetch::{fetch, pull_request, refs};
use crate::support::{Server, missing_objects};

#[test]
fn opening_offline_never_makes_a_clone() {
    let server = Server::new();
    pull_request(&server);
    let (dir, cache) = server.cache();

    assert!(cache.open_offline("owner", "repo").unwrap().is_none());

    assert!(!cache.repo_dir("owner", "repo").exists());
    assert!(!dir.path().join("repos").exists());
}

#[test]
fn what_was_fetched_and_diffed_reads_without_the_server() {
    let server = Server::new();
    let pr = pull_request(&server);
    let (_dir, cache) = server.cache();
    let repo = cache.open("owner", "repo", None).unwrap();
    fetch(&repo, 1);
    let files = repo.changed_files(&pr.base, &pr.head).unwrap();
    drop(repo);

    // Offline.
    drop(pr.remote);
    drop(server);

    let repo = cache.open_offline("owner", "repo").unwrap().unwrap();
    assert_eq!(repo.pull_revisions(1).unwrap(), vec![pr.head.clone()]);
    assert!(repo.has_base(1, &pr.trunk_tip).unwrap());
    assert!(!repo.has_base(1, &pr.base).unwrap());
    assert!(!repo.has_base(2, &pr.trunk_tip).unwrap());
    assert_eq!(
        repo.branch_base(1, "trunk").unwrap(),
        Some(pr.trunk_tip.clone())
    );
    assert_eq!(
        repo.merge_base(&pr.head, &pr.trunk_tip).unwrap(),
        Some(pr.base.clone())
    );
    assert!(repo.has_diff_contents(&pr.base, &pr.head).unwrap());
    assert_eq!(repo.changed_files(&pr.base, &pr.head).unwrap(), files);

    // It never writes.
    assert!(matches!(
        repo.fetch_pull(&refs(1, "trunk")),
        Err(Error::ReadOnly { .. })
    ));
}

#[test]
fn offline_what_was_never_diffed_is_missing_and_isnt_downloaded() {
    let server = Server::new();
    let pr = pull_request(&server);
    let (_dir, cache) = server.cache();
    let repo = cache.open("owner", "repo", None).unwrap();
    // Fetched, but never diffed, so no file's contents were downloaded.
    fetch(&repo, 1);
    let git_dir = repo.git_dir().to_path_buf();
    drop(repo);
    let missing = missing_objects(&git_dir, pr.head.as_str());
    assert!(!missing.is_empty());

    // The server is still there, so a read that could download would.
    let offline = cache.open_offline("owner", "repo").unwrap().unwrap();
    assert!(!offline.has_diff_contents(&pr.base, &pr.head).unwrap());
    assert_eq!(missing_objects(&git_dir, pr.head.as_str()), missing);
    if Git::find().unwrap().version() >= GitVersion::NO_LAZY_FETCH {
        // Reading the diff anyway fails rather than downloading.
        assert!(offline.changed_files(&pr.base, &pr.head).is_err());
        assert_eq!(missing_objects(&git_dir, pr.head.as_str()), missing);
    }
}

#[test]
fn each_base_branch_keeps_the_tip_it_was_last_fetched_at() {
    let server = Server::new();
    let pr = pull_request(&server);
    let (_dir, cache) = server.cache();
    let repo = cache.open("owner", "repo", None).unwrap();
    fetch(&repo, 1);
    // The pull request moves onto another branch, as a stack's upper layer does when the one
    // below merges.
    let other = pr
        .remote
        .switch("stack/lower")
        .write("lower.txt", "lower\n")
        .commit("lower layer");
    pr.remote.publish("refs/heads/stack/lower");
    repo.fetch_pull(&refs(1, "stack/lower")).unwrap();

    assert_eq!(
        repo.branch_base(1, "trunk").unwrap(),
        Some(pr.trunk_tip.clone())
    );
    assert_eq!(
        repo.branch_base(1, "stack/lower").unwrap(),
        Some(other.clone())
    );
    assert_eq!(repo.branch_base(1, "stack").unwrap(), None);
    assert!(repo.has_base(1, &pr.trunk_tip).unwrap());
    assert!(repo.has_base(1, &other).unwrap());
}

#[test]
fn opening_offline_doesnt_wait_for_another_los_fetch() {
    let server = Server::new();
    pull_request(&server);
    let (_dir, cache) = server.cache();
    let repo = cache.open("owner", "repo", None).unwrap();
    fetch(&repo, 1);
    drop(repo);
    // Another rozed holds the clone's lock, as it does while fetching.
    let lock = File::options()
        .write(true)
        .open(cache.root().join("locks/github.com/owner/repo.lock"))
        .unwrap();
    lock.lock().unwrap();

    let cache = Arc::new(cache);
    let (opened, open) = mpsc::channel();
    std::thread::spawn({
        let cache = cache.clone();
        move || {
            let repo = cache
                .open_offline("owner", "repo")
                .map(|repo| repo.is_some());
            opened.send(repo).ok();
        }
    });

    let opened = open.recv_timeout(Duration::from_secs(20));
    assert!(matches!(opened, Ok(Ok(true))), "{opened:?}");
}
