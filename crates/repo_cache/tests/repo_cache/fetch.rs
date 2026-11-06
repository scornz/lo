//! Cloning, and fetching pull requests and commits.

use std::fs;
use std::sync::{Arc, Mutex};

use repo_cache::{
    BaseSource, Error, FetchFailure, FetchedPull, Git, HeadSource, Oid, Progress, PullRequestRefs,
    Repo, RepoCache,
};
use tempfile::TempDir;

use crate::support::{Remote, Server, git, missing_objects, packs};

/// A repository with `trunk` and pull request #1 on it:
///
/// ```text
/// base ── trunk_tip      (trunk)
///   └── head             (refs/pull/1/head, branch feature)
/// ```
pub(crate) struct PullRequest {
    pub remote: Remote,
    pub base: Oid,
    pub head: Oid,
    pub trunk_tip: Oid,
}

pub(crate) fn pull_request(server: &Server) -> PullRequest {
    let remote = server.repo("owner", "repo");
    remote
        .write("README.md", "# repo\n")
        .write("src/lib.rs", "pub fn a() {}\n");
    let base = remote.commit("base");
    remote
        .switch("feature")
        .write("feature.txt", "first version\n");
    let head = remote.commit("feature");
    remote
        .publish("refs/heads/feature")
        .publish("refs/pull/1/head");
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

/// Pull request `number` into `base_branch`, as an API that knows nothing else describes it.
pub(crate) fn refs(number: u64, base_branch: &str) -> PullRequestRefs {
    PullRequestRefs {
        number,
        head: None,
        base_branch: base_branch.into(),
        base: None,
    }
}

pub(crate) fn fetch(repo: &Repo, number: u64) -> FetchedPull {
    repo.fetch_pull(&refs(number, "trunk")).unwrap()
}

fn rev_parse(repo: &Repo, rev: &str) -> String {
    git(repo.git_dir(), &["rev-parse", rev])
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
    assert!(git_dir.join("lo-last-used").is_file());
    // No template, so no sample hooks.
    assert!(!git_dir.join("hooks").exists());

    // Opening it again reuses it, and writes no config when nothing changed.
    fs::write(git_dir.join("marker"), "").unwrap();
    let config_before = fs::metadata(git_dir.join("config"))
        .unwrap()
        .modified()
        .unwrap();
    let again = cache.open("Owner", "Repo.rs", None).unwrap();
    assert_eq!(again.git_dir(), git_dir);
    assert!(git_dir.join("marker").is_file());
    let config_after = fs::metadata(git_dir.join("config"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(config_before, config_after);
    // A new canonical name (different case) for the same repository updates the URL.
    cache.open("owner", "repo.rs", None).unwrap();
    assert_eq!(
        git(&git_dir, &["config", "remote.origin.url"]),
        format!("{}/owner/repo.rs.git", server.base_url())
    );
}

#[test]
fn git_never_uses_a_repository_above_the_cache() {
    let server = Server::new();
    let pr = pull_request(&server);
    // The user's own repository around the cache, such as dotfiles kept in the home folder. Its
    // origin has pull request #1; the clone's own remote doesn't exist.
    let home = TempDir::new().unwrap();
    git(home.path(), &["init", "-q"]);
    let origin = format!("{}/owner/repo.git", server.base_url());
    git(home.path(), &["config", "remote.origin.url", &origin]);
    let cache = RepoCache::new(home.path().join("Library/Caches/Lo"), Git::find().unwrap())
        .unwrap()
        .with_remote_base(server.base_url());
    let repo = cache.open("owner", "not-on-the-server", None).unwrap();
    // Half deleted, as by a cleanup that was interrupted.
    fs::remove_file(repo.git_dir().join("HEAD")).unwrap();

    let error = repo.fetch_pull(&refs(1, "trunk")).unwrap_err();

    assert!(matches!(error, Error::Git { .. }), "{error:?}");
    assert!(
        error.to_string().contains("not a git repository"),
        "{error}"
    );
    assert!(!error.is_retryable());
    assert_eq!(
        git(home.path(), &["for-each-ref", "--format=%(refname)"]),
        ""
    );
    assert!(repo.read_file(&pr.head, b"README.md").is_err());
}

#[test]
fn a_clone_left_without_head_is_made_again() {
    let server = Server::new();
    let pr = pull_request(&server);
    let (_dir, cache) = server.cache();
    let repo = cache.open("owner", "repo", None).unwrap();
    fetch(&repo, 1);
    fs::remove_file(repo.git_dir().join("HEAD")).unwrap();

    let repo = cache.open("owner", "repo", None).unwrap();

    assert_eq!(fetch(&repo, 1).head, pr.head);
    assert!(repo.git_dir().join("HEAD").is_file());
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
fn fetches_a_pull_requests_head_and_base_and_pins_them() {
    let server = Server::new();
    let pr = pull_request(&server);
    let (_dir, cache) = server.cache();
    let repo = cache.open("owner", "repo", None).unwrap();

    let fetched = fetch(&repo, 1);

    assert_eq!(
        fetched,
        FetchedPull {
            head: pr.head.clone(),
            head_source: HeadSource::PullRef,
            base_tip: pr.trunk_tip.clone(),
            base_source: BaseSource::Branch,
        }
    );
    assert_eq!(rev_parse(&repo, "refs/lo/pull/1/head"), pr.head.as_str());
    assert_eq!(
        rev_parse(&repo, "refs/lo/pull/1/base"),
        pr.trunk_tip.as_str()
    );
    let revs = format!("refs/lo/pull/1/revs/{}", pr.head);
    assert_eq!(rev_parse(&repo, &revs), pr.head.as_str());
    let bases = format!("refs/lo/pull/1/bases/{}", pr.trunk_tip);
    assert_eq!(rev_parse(&repo, &bases), pr.trunk_tip.as_str());
    assert_eq!(repo.pull_revisions(1).unwrap(), vec![pr.head.clone()]);
    assert_eq!(
        repo.merge_base(&fetched.head, &fetched.base_tip).unwrap(),
        Some(pr.base.clone())
    );

    // Only rozed's refs: no branches, tags or FETCH_HEAD.
    let git_dir = repo.git_dir();
    let refs = git(git_dir, &["for-each-ref", "--format=%(refname)"]);
    assert!(
        refs.lines().all(|name| name.starts_with("refs/lo/pull/1/")),
        "{refs}"
    );
    assert!(!git_dir.join("FETCH_HEAD").exists());
    // Partial: commits and trees, but no file contents yet.
    assert!(!missing_objects(git_dir, pr.head.as_str()).is_empty());
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

    let fetched = fetch(&repo, 2);

    assert_eq!(fetched.head, fork_head);
    assert_eq!(
        repo.merge_base(&fetched.head, &fetched.base_tip).unwrap(),
        Some(pr.trunk_tip)
    );
    // From the base repository alone.
    assert!(!git(repo.git_dir(), &["config", "remote.origin.url"]).contains("contributor"));
}

#[test]
fn a_force_pushed_revision_stays_readable() {
    let server = Server::new();
    let pr = pull_request(&server);
    let (_dir, cache) = server.cache();
    let repo = cache.open("owner", "repo", None).unwrap();
    let r1 = fetch(&repo, 1).head;

    pr.remote
        .switch("feature")
        .write("feature.txt", "second version\n");
    let r2 = pr.remote.amend("feature, amended");
    pr.remote.publish("refs/pull/1/head");
    assert_eq!(fetch(&repo, 1).head, r2);

    // refs/pull/1/head moved on; rozed kept both.
    assert_eq!(rev_parse(&repo, "refs/lo/pull/1/head"), r2.as_str());
    let mut revisions = repo.pull_revisions(1).unwrap();
    revisions.sort();
    let mut expected = vec![r1.clone(), r2.clone()];
    expected.sort();
    assert_eq!(revisions, expected);

    // A garbage collection keeps every revision.
    git(repo.git_dir(), &["gc", "-q", "--prune=now"]);
    assert_eq!(
        repo.read_file(&r1, b"feature.txt").unwrap().unwrap(),
        b"first version\n"
    );
    assert_eq!(
        repo.read_file(&r2, b"feature.txt").unwrap().unwrap(),
        b"second version\n"
    );
    let changes = repo.changed_files(&r1, &r2).unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].path(), b"feature.txt");
    assert!(repo.is_ancestor(&pr.base, &r1).unwrap());
}

#[test]
fn a_revision_the_server_lost_stays_readable_once_read() {
    let server = Server::new();
    let pr = pull_request(&server);
    let (_dir, cache) = server.cache();
    let repo = cache.open("owner", "repo", None).unwrap();
    let r1 = fetch(&repo, 1).head;
    // Showing the revision's diff downloads both sides of each changed file.
    repo.changed_files(&pr.base, &r1).unwrap();

    pr.remote
        .switch("feature")
        .write("feature.txt", "second version\n");
    let r2 = pr.remote.amend("feature, amended");
    pr.remote
        .publish("refs/heads/feature")
        .publish("refs/pull/1/head");
    fetch(&repo, 1);
    pr.remote.forget_unreachable();
    assert!(!pr.remote.has(&r1));
    git(repo.git_dir(), &["gc", "-q", "--prune=now"]);

    assert_eq!(
        repo.read_file(&r1, b"feature.txt").unwrap().unwrap(),
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
fn uses_the_head_githubs_api_reported() {
    let server = Server::new();
    let pr = pull_request(&server);
    // A push GitHub's API already shows, before refs/pull/1/head catches up.
    pr.remote
        .switch("feature")
        .write("feature.txt", "pushed a moment ago\n");
    let pushed = pr.remote.commit("a new push");
    pr.remote.publish("refs/heads/feature");
    let (_dir, cache) = server.cache();
    let repo = cache.open("owner", "repo", None).unwrap();

    let fetched = repo
        .fetch_pull(&PullRequestRefs {
            head: Some(pushed.clone()),
            ..refs(1, "trunk")
        })
        .unwrap();

    assert_eq!(fetched.head, pushed);
    assert_eq!(
        fetched.head_source,
        HeadSource::Api {
            pull_ref: pr.head.clone()
        }
    );
    // Both are pinned.
    let mut revisions = repo.pull_revisions(1).unwrap();
    revisions.sort();
    let mut expected = vec![pr.head.clone(), pushed.clone()];
    expected.sort();
    assert_eq!(revisions, expected);

    // The API's head, already fetched, isn't fetched again.
    let before = packs(repo.git_dir());
    let fetched = repo
        .fetch_pull(&PullRequestRefs {
            head: Some(pushed.clone()),
            ..refs(1, "trunk")
        })
        .unwrap();
    assert_eq!(fetched.head, pushed);
    assert_eq!(packs(repo.git_dir()), before);
    // Nor is anything when the API's head is refs/pull/1/head's.
    let fetched = repo
        .fetch_pull(&PullRequestRefs {
            head: Some(pr.head.clone()),
            ..refs(1, "trunk")
        })
        .unwrap();
    assert_eq!(fetched.head_source, HeadSource::PullRef);
    assert_eq!(packs(repo.git_dir()), before);
}

#[test]
fn says_when_githubs_head_is_gone() {
    let server = Server::new();
    let pr = pull_request(&server);
    let (_dir, cache) = server.cache();
    let repo = cache.open("owner", "repo", None).unwrap();
    let unknown: Oid = "1234567890123456789012345678901234567890".parse().unwrap();

    let fetched = repo
        .fetch_pull(&PullRequestRefs {
            head: Some(unknown.clone()),
            ..refs(1, "trunk")
        })
        .unwrap();

    assert_eq!(fetched.head, pr.head);
    assert_eq!(
        fetched.head_source,
        HeadSource::PullRefNotApi { api: unknown }
    );
}

#[test]
fn a_deleted_base_branch_falls_back_to_the_apis_base() {
    let server = Server::new();
    let pr = pull_request(&server);
    // Pull request #2 sits on `feature`, which was merged and deleted.
    pr.remote.switch("feature").write("upper.txt", "upper\n");
    let upper = pr.remote.commit("upper layer");
    pr.remote.publish("refs/pull/2/head");
    let (_dir, cache) = server.cache();
    let repo = cache.open("owner", "repo", None).unwrap();

    // Without the API's base, the missing branch is an error.
    let error = repo.fetch_pull(&refs(2, "gone")).unwrap_err();
    assert!(
        matches!(
            error,
            Error::Fetch {
                failure: FetchFailure::RefNotFound,
                ..
            }
        ),
        "{error:?}"
    );

    let fetched = repo
        .fetch_pull(&PullRequestRefs {
            base: Some(pr.head.clone()),
            ..refs(2, "gone")
        })
        .unwrap();
    assert_eq!(fetched.head, upper);
    assert_eq!(fetched.base_tip, pr.head);
    assert_eq!(fetched.base_source, BaseSource::Api);
    assert_eq!(rev_parse(&repo, "refs/lo/pull/2/base"), pr.head.as_str());
}

#[test]
fn a_deleted_base_branch_whose_commit_github_lost_is_unavailable() {
    let server = Server::new();
    pull_request(&server);
    let (_dir, cache) = server.cache();
    let repo = cache.open("owner", "repo", None).unwrap();
    let lost: Oid = "1234567890123456789012345678901234567890".parse().unwrap();

    let error = repo
        .fetch_pull(&PullRequestRefs {
            base: Some(lost.clone()),
            ..refs(1, "gone")
        })
        .unwrap_err();

    assert!(
        matches!(&error, Error::Unavailable { oid } if *oid == lost),
        "{error:?}"
    );
    assert!(!error.is_retryable());
}

#[test]
fn base_branches_whose_names_nest_dont_collide() {
    let server = Server::new();
    let pr = pull_request(&server);
    // Pull request #1 goes into `rel`. Then `rel` is deleted and `rel/1.0` created, and pull
    // request #2 goes into that.
    pr.remote.switch("rel").publish("refs/heads/rel");
    let (_dir, cache) = server.cache();
    let repo = cache.open("owner", "repo", None).unwrap();
    repo.fetch_pull(&refs(1, "rel")).unwrap();
    git(&pr.remote.git_dir, &["update-ref", "-d", "refs/heads/rel"]);
    git(&pr.remote.work, &["branch", "-q", "-m", "rel", "rel-old"]);
    pr.remote.switch("rel/1.0").publish("refs/heads/rel/1.0");
    pr.remote.publish("refs/pull/2/head");

    let fetched = repo.fetch_pull(&refs(2, "rel/1.0")).unwrap();

    assert_eq!(fetched.base_tip, pr.trunk_tip);
    // And #1 still fetches, with the API's base now that its branch is gone.
    let fetched = repo
        .fetch_pull(&PullRequestRefs {
            base: Some(pr.trunk_tip.clone()),
            ..refs(1, "rel")
        })
        .unwrap();
    assert_eq!(fetched.base_source, BaseSource::Api);
}

#[test]
fn refuses_base_branches_that_arent_branch_names() {
    let server = Server::new();
    pull_request(&server);
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
        let error = repo.fetch_pull(&refs(1, name)).unwrap_err();
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
        rev_parse(&repo, &format!("refs/lo/commits/{elsewhere}")),
        elsewhere.as_str()
    );
    assert_eq!(
        repo.read_file(&elsewhere, b"elsewhere.txt")
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

    let error = repo.fetch_pull(&refs(9, "trunk")).unwrap_err();

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

    let error = repo.fetch_pull(&refs(1, "trunk")).unwrap_err();

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
        .with_remote_base("https://127.0.0.1:1");
    let repo = cache.open("owner", "repo", None).unwrap();

    let error = repo.fetch_pull(&refs(1, "trunk")).unwrap_err();

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
fn without_opting_in_only_https_is_allowed() {
    let server = Server::new();
    pull_request(&server);
    let dir = TempDir::new().unwrap();
    // A cache that fetches from GitHub, pointed at a file:// remote behind its back.
    let cache = RepoCache::new(dir.path(), Git::find().unwrap()).unwrap();
    let repo = cache.open("owner", "repo", None).unwrap();
    let url = format!("{}/owner/repo.git", server.base_url());
    git(repo.git_dir(), &["config", "remote.origin.url", &url]);

    let error = repo.fetch_pull(&refs(1, "trunk")).unwrap_err();

    assert!(
        error.to_string().contains("transport 'file' not allowed"),
        "{error}"
    );
}

#[test]
fn many_fetches_at_once_all_succeed() {
    let server = Server::new();
    let pr = pull_request(&server);
    pr.remote.switch("feature").publish("refs/pull/2/head");
    let (_dir, cache) = server.cache();
    let cache = Arc::new(cache);

    let threads: Vec<_> = (0..8)
        .map(|i| {
            let cache = cache.clone();
            std::thread::spawn(move || {
                let repo = cache.open("owner", "repo", None).unwrap();
                repo.fetch_pull(&refs(1 + i % 2, "trunk")).unwrap()
            })
        })
        .collect();

    for thread in threads {
        assert_eq!(thread.join().unwrap().head, pr.head);
    }
}

#[test]
fn fetches_of_one_pull_request_at_once_each_get_what_they_asked_for() {
    let server = Server::new();
    let pr = pull_request(&server);
    // Two windows disagree about pull request #1's base: one still has `trunk`, the other knows
    // it was retargeted to `rel`, whose tip is another commit.
    pr.remote.switch("rel").write("rel.txt", "release\n");
    let rel_tip = pr.remote.commit("release");
    pr.remote.publish("refs/heads/rel");
    let (_dir, cache) = server.cache();
    let cache = Arc::new(cache);

    for _ in 0..5 {
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let (cache, barrier) = (cache.clone(), barrier.clone());
                let (branch, tip) = if i % 2 == 0 {
                    ("trunk", pr.trunk_tip.clone())
                } else {
                    ("rel", rel_tip.clone())
                };
                std::thread::spawn(move || {
                    let repo = cache.open("owner", "repo", None).unwrap();
                    barrier.wait();
                    let fetched = repo.fetch_pull(&refs(1, branch)).unwrap();
                    (fetched.base_tip, tip)
                })
            })
            .collect();
        for thread in threads {
            let (base_tip, expected) = thread.join().unwrap();
            assert_eq!(base_tip, expected);
        }
    }
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
    fetch(&repo, 1);

    let updates = updates.lock().unwrap();
    assert!(
        updates
            .iter()
            .any(|update| update.stage == "Counting objects" && update.percent() == Some(100)),
        "{updates:#?}"
    );
    assert!(updates.iter().all(|update| !update.stage.is_empty()));
}
