//! Cloning, and fetching pull requests, branches and commits.

use std::fs;
use std::sync::{Arc, Mutex};

use repo_cache::{Error, FetchFailure, Git, Oid, Progress, RepoCache};
use tempfile::TempDir;

use crate::support::{Remote, Server, git, missing_objects};

/// A repository with `trunk` and pull request #1 on it:
///
/// ```text
/// base ── trunk_tip      (trunk)
///   └── head             (refs/pull/1/head)
/// ```
struct PullRequest {
    remote: Remote,
    base: Oid,
    head: Oid,
    trunk_tip: Oid,
}

fn pull_request(server: &Server) -> PullRequest {
    let remote = server.repo("owner", "repo");
    remote
        .write("README.md", "# repo\n")
        .write("src/lib.rs", "pub fn a() {}\n");
    let base = remote.commit("base");
    remote
        .switch("feature")
        .write("feature.txt", "first version\n");
    let head = remote.commit("feature");
    remote.publish("refs/pull/1/head");
    remote
        .switch("trunk")
        .write("trunk.txt", "trunk moved on\n");
    let trunk_tip = remote.commit("trunk moves on");
    remote.publish("refs/heads/trunk");
    PullRequest {
        remote,
        base,
        head,
        trunk_tip,
    }
}

#[test]
fn opens_a_bare_partial_clone_on_first_use() {
    let server = Server::new();
    let (_dir, cache) = server.cache();

    // Nothing is fetched, so the repository needn't even exist yet.
    let repo = cache.open("Owner", "Repo.rs", None).unwrap();

    let git_dir = repo.git_dir().to_path_buf();
    assert_eq!(
        git_dir,
        cache.root().join("repos/github.com/owner/repo.rs.git")
    );
    assert_eq!(cache.repo_dir("OWNER", "REPO.RS"), git_dir);
    assert_eq!(
        git(&git_dir, &["rev-parse", "--is-bare-repository"]),
        "true"
    );
    assert_eq!(
        git(&git_dir, &["config", "remote.origin.url"]),
        format!("{}/Owner/Repo.rs.git", server.base_url())
    );
    assert_eq!(git(&git_dir, &["config", "remote.origin.promisor"]), "true");
    assert_eq!(
        git(&git_dir, &["config", "remote.origin.partialclonefilter"]),
        "blob:none"
    );
    assert!(git_dir.join("rozed-last-used").is_file());

    // Opening it again reuses it.
    fs::write(git_dir.join("marker"), "").unwrap();
    let again = cache.open("owner", "repo.rs", None).unwrap();
    assert_eq!(again.git_dir(), git_dir);
    assert!(git_dir.join("marker").is_file());
}

#[test]
fn refuses_names_that_could_leave_the_cache() {
    let server = Server::new();
    let (_dir, cache) = server.cache();
    for (owner, repo) in [
        ("..", "x"),
        ("a/b", "c"),
        ("o", "../../x"),
        ("", "x"),
        ("o", "."),
    ] {
        let error = cache.open(owner, repo, None).unwrap_err();
        assert!(
            matches!(error, Error::Invalid { .. }),
            "{owner}/{repo}: {error:?}"
        );
    }
}

#[test]
fn fetches_a_pull_requests_head_and_pins_it() {
    let server = Server::new();
    let pr = pull_request(&server);
    let (_dir, cache) = server.cache();
    let repo = cache.open("owner", "repo", None).unwrap();

    let head = repo.fetch_pull(1).unwrap();

    assert_eq!(head, pr.head);
    let git_dir = repo.git_dir();
    assert_eq!(
        git(git_dir, &["rev-parse", "refs/rozed/pull/1/head"]),
        head.as_str()
    );
    assert_eq!(
        git(
            git_dir,
            &["rev-parse", &format!("refs/rozed/pull/1/revs/{head}")]
        ),
        head.as_str()
    );
    assert_eq!(repo.pull_revisions(1).unwrap(), vec![head.clone()]);
    // Only rozed's refs: no branches, tags or FETCH_HEAD.
    let refs = git(git_dir, &["for-each-ref", "--format=%(refname)"]);
    assert!(
        refs.lines().all(|name| name.starts_with("refs/rozed/")),
        "{refs}"
    );
    assert!(!git_dir.join("FETCH_HEAD").exists());
    // Partial: commits and trees, but no file contents yet.
    assert!(!missing_objects(git_dir, head.as_str()).is_empty());
    assert_eq!(
        git(git_dir, &["rev-list", "--count", head.as_str()]),
        "2",
        "the whole history up to the head"
    );
    let _ = pr.base;
}

#[test]
fn fetches_a_pull_request_from_a_fork() {
    let server = Server::new();
    let pr = pull_request(&server);
    // The fork has the base's history and a commit of its own.
    let fork = server.repo("contributor", "repo");
    fork.take_from(&pr.remote, "refs/heads/trunk", "refs/heads/trunk");
    git(
        &fork.work,
        &["fetch", "-q", fork.git_dir.to_str().unwrap(), "trunk"],
    );
    git(&fork.work, &["reset", "-q", "--hard", "FETCH_HEAD"]);
    fork.write("from-fork.txt", "a contributor's change\n");
    let fork_head = fork.commit("from a fork");
    fork.publish("refs/heads/patch-1");
    // GitHub copies the fork's head into the base repository's refs/pull/<n>/head.
    pr.remote
        .take_from(&fork, "refs/heads/patch-1", "refs/pull/2/head");
    let (_dir, cache) = server.cache();
    let repo = cache.open("owner", "repo", None).unwrap();

    assert_eq!(repo.fetch_pull(2).unwrap(), fork_head);

    // From the base repository alone.
    assert!(!git(repo.git_dir(), &["config", "remote.origin.url"]).contains("contributor"));
    assert_eq!(
        repo.merge_base(&fork_head, &repo.fetch_branch("trunk").unwrap())
            .unwrap(),
        Some(pr.trunk_tip)
    );
}

#[test]
fn a_force_pushed_revision_stays_readable() {
    let server = Server::new();
    let pr = pull_request(&server);
    let (_dir, cache) = server.cache();
    let repo = cache.open("owner", "repo", None).unwrap();
    let r1 = repo.fetch_pull(1).unwrap();

    pr.remote
        .switch("feature")
        .write("feature.txt", "second version\n");
    let r2 = pr.remote.amend("feature, amended");
    pr.remote.publish("refs/pull/1/head");
    assert_eq!(repo.fetch_pull(1).unwrap(), r2);

    // refs/pull/1/head moved on; rozed kept both.
    let git_dir = repo.git_dir();
    assert_eq!(
        git(git_dir, &["rev-parse", "refs/rozed/pull/1/head"]),
        r2.as_str()
    );
    let mut revisions = repo.pull_revisions(1).unwrap();
    revisions.sort();
    let mut expected = vec![r1.clone(), r2.clone()];
    expected.sort();
    assert_eq!(revisions, expected);

    // A garbage collection keeps every pinned revision.
    git(git_dir, &["gc", "-q", "--prune=now"]);
    assert_eq!(
        repo.read_blob(&r1, "feature.txt").unwrap().unwrap(),
        b"first version\n"
    );
    assert_eq!(
        repo.read_blob(&r2, "feature.txt").unwrap().unwrap(),
        b"second version\n"
    );
    let changes = repo.changed_files(&r1, &r2).unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].path(), "feature.txt");
    assert!(repo.is_ancestor(&pr.base, &r1).unwrap());
}

#[test]
fn a_revision_the_server_lost_stays_readable_once_read() {
    let server = Server::new();
    let pr = pull_request(&server);
    let (_dir, cache) = server.cache();
    let repo = cache.open("owner", "repo", None).unwrap();
    let r1 = repo.fetch_pull(1).unwrap();
    // Showing the revision's diff downloads both sides of each changed file.
    repo.changed_files(&pr.base, &r1).unwrap();

    pr.remote
        .switch("feature")
        .write("feature.txt", "second version\n");
    let r2 = pr.remote.amend("feature, amended");
    pr.remote.publish("refs/pull/1/head");
    repo.fetch_pull(1).unwrap();
    pr.remote.forget_unreachable();
    assert!(!pr.remote.has(&r1));
    git(repo.git_dir(), &["gc", "-q", "--prune=now"]);

    assert_eq!(
        repo.read_blob(&r1, "feature.txt").unwrap().unwrap(),
        b"first version\n"
    );
    assert_eq!(repo.changed_files(&pr.base, &r1).unwrap().len(), 1);
    // Fetching it by ID needs nothing from the server.
    repo.fetch_commit(&r1).unwrap();
    assert!(repo.is_ancestor(&pr.base, &r2).unwrap());

    // A cache that never fetched it can't get it any more.
    let (_other_dir, other_cache) = server.cache();
    let other = other_cache.open("owner", "repo", None).unwrap();
    let error = other.fetch_commit(&r1).unwrap_err();
    assert!(
        matches!(&error, Error::Unavailable { oid } if *oid == r1),
        "{error:?}"
    );
    assert_eq!(
        error.to_string(),
        format!("commit {} is no longer available", r1.short(7))
    );
}

#[test]
fn fetches_a_branch() {
    let server = Server::new();
    let pr = pull_request(&server);
    pr.remote
        .switch("release/nested")
        .publish("refs/heads/release/nested");
    let (_dir, cache) = server.cache();
    let repo = cache.open("owner", "repo", None).unwrap();

    assert_eq!(repo.fetch_branch("trunk").unwrap(), pr.trunk_tip);
    assert_eq!(
        git(repo.git_dir(), &["rev-parse", "refs/rozed/branches/trunk"]),
        pr.trunk_tip.as_str()
    );
    assert_eq!(repo.fetch_branch("release/nested").unwrap(), pr.trunk_tip);

    // It follows the branch.
    pr.remote.switch("trunk").write("later.txt", "later\n");
    let later = pr.remote.commit("later");
    pr.remote.publish("refs/heads/trunk");
    assert_eq!(repo.fetch_branch("trunk").unwrap(), later);
}

#[test]
fn refuses_names_that_arent_branch_names() {
    let server = Server::new();
    let (_dir, cache) = server.cache();
    let repo = cache.open("owner", "repo", None).unwrap();
    for name in [
        "",
        "a..b",
        "with space",
        "trunk:refs/heads/x",
        "x.lock",
        "-x",
        "a~1",
    ] {
        let error = repo.fetch_branch(name).unwrap_err();
        assert!(
            error.to_string().contains("isn't a valid branch name"),
            "{name}: {error}"
        );
    }
}

#[test]
fn fetches_a_commit_by_id_and_pins_it() {
    let server = Server::new();
    let pr = pull_request(&server);
    // A commit on no ref rozed fetches.
    pr.remote
        .switch("elsewhere")
        .write("elsewhere.txt", "elsewhere\n");
    let elsewhere = pr.remote.commit("elsewhere");
    pr.remote.publish("refs/heads/elsewhere");
    let (_dir, cache) = server.cache();
    let repo = cache.open("owner", "repo", None).unwrap();

    repo.fetch_commit(&elsewhere).unwrap();

    assert_eq!(
        git(
            repo.git_dir(),
            &["rev-parse", &format!("refs/rozed/commits/{elsewhere}")]
        ),
        elsewhere.as_str()
    );
    assert_eq!(
        repo.read_blob(&elsewhere, "elsewhere.txt")
            .unwrap()
            .unwrap(),
        b"elsewhere\n"
    );
    // Asking again is fine.
    repo.fetch_commit(&elsewhere).unwrap();
}

#[test]
fn a_commit_the_server_never_had_is_unavailable() {
    let server = Server::new();
    pull_request(&server);
    let (_dir, cache) = server.cache();
    let repo = cache.open("owner", "repo", None).unwrap();
    let unknown: Oid = "1234567890123456789012345678901234567890".parse().unwrap();

    let error = repo.fetch_commit(&unknown).unwrap_err();

    assert!(matches!(error, Error::Unavailable { .. }), "{error:?}");
}

#[test]
fn a_missing_pull_request_is_a_fetch_error() {
    let server = Server::new();
    pull_request(&server);
    let (_dir, cache) = server.cache();
    let repo = cache.open("owner", "repo", None).unwrap();

    let error = repo.fetch_pull(9).unwrap_err();

    let Error::Fetch { what, failure, .. } = &error else {
        panic!("{error:?}");
    };
    assert_eq!(what, "pull request #9");
    assert_eq!(*failure, FetchFailure::RefNotFound);
    assert!(!error.is_retryable());
    assert!(
        error
            .to_string()
            .starts_with("can't fetch pull request #9: GitHub doesn't have it"),
        "{error}"
    );
}

#[test]
fn a_missing_repository_is_a_fetch_error() {
    let server = Server::new();
    let (_dir, cache) = server.cache();
    let repo = cache.open("owner", "nothing-here", None).unwrap();

    let error = repo.fetch_pull(1).unwrap_err();

    assert!(
        matches!(
            error,
            Error::Fetch {
                failure: FetchFailure::RepoNotFound,
                ..
            }
        ),
        "{error:?}"
    );
}

#[test]
fn an_unreachable_server_is_a_retryable_network_error() {
    let dir = TempDir::new().unwrap();
    // Nothing listens on port 1.
    let cache = RepoCache::new(dir.path(), Git::find().unwrap())
        .unwrap()
        .with_remote_base("http://127.0.0.1:1");
    let repo = cache.open("owner", "repo", None).unwrap();

    let error = repo.fetch_pull(1).unwrap_err();

    assert!(
        matches!(
            error,
            Error::Fetch {
                failure: FetchFailure::Network,
                ..
            }
        ),
        "{error:?}"
    );
    assert!(error.is_retryable());
}

#[test]
fn reports_fetch_progress() {
    let server = Server::new();
    pull_request(&server);
    let (_dir, cache) = server.cache();
    let updates = Arc::new(Mutex::new(Vec::<Progress>::new()));

    let repo = cache.open("owner", "repo", None).unwrap().with_progress({
        let updates = updates.clone();
        move |progress| updates.lock().unwrap().push(progress)
    });
    repo.fetch_pull(1).unwrap();

    let updates = updates.lock().unwrap();
    assert!(
        updates
            .iter()
            .any(|update| update.stage == "Counting objects" && update.percent() == Some(100)),
        "{updates:#?}"
    );
    assert!(updates.iter().all(|update| !update.stage.is_empty()));
}
