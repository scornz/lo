//! A pull request's revisions: fetching every head by ID at once, and listing its current commits.

use repo_cache::{Error, Oid};

use crate::fetch::{fetch, pull_request};
use crate::support::{Server, git, packs};

/// What `refs/rozed/pull/<number>/revs/` pins, sorted.
fn pinned_revisions(git_dir: &std::path::Path, number: u64) -> Vec<String> {
    let output = git(
        git_dir,
        &[
            "for-each-ref",
            "--format=%(refname:lstrip=5) %(objectname)",
            &format!("refs/rozed/pull/{number}/revs/"),
        ],
    );
    let mut pins: Vec<String> = output.lines().map(str::to_string).collect();
    pins.sort();
    pins
}

fn pins(heads: &[&Oid]) -> Vec<String> {
    let mut pins: Vec<String> = heads.iter().map(|oid| format!("{oid} {oid}")).collect();
    pins.sort();
    pins
}

/// Pull request #1 force-pushed twice after its first head: `r1`, then `r2` and `r3`, each
/// amending the last. Only `r3` is on a ref the server advertises now.
struct ForcePushed {
    r1: Oid,
    r2: Oid,
    r3: Oid,
}

fn force_pushed(pr: &crate::fetch::PullRequest) -> ForcePushed {
    let r1 = pr.head.clone();
    pr.remote
        .switch("feature")
        .write("feature.txt", "second version\n");
    let r2 = pr.remote.amend("feature, amended");
    pr.remote
        .publish("refs/heads/feature")
        .publish("refs/pull/1/head");
    pr.remote.write("feature.txt", "third version\n");
    let r3 = pr.remote.amend("feature, amended again");
    pr.remote
        .publish("refs/heads/feature")
        .publish("refs/pull/1/head");
    ForcePushed { r1, r2, r3 }
}

#[test]
fn fetches_every_head_it_lacks_in_one_fetch_and_pins_them() {
    let server = Server::new();
    let pr = pull_request(&server);
    let ForcePushed { r1, r2, r3 } = force_pushed(&pr);
    let (_dir, cache) = server.cache();
    let repo = cache.open("owner", "repo", None).unwrap();
    // The clone has the latest head already, from opening the pull request.
    assert_eq!(fetch(&repo, 1).head, r3);
    let before = packs(repo.git_dir());

    let unavailable = repo
        .fetch_commits(1, &[r1.clone(), r2.clone(), r3.clone(), r1.clone()])
        .unwrap();

    assert_eq!(unavailable, Vec::<Oid>::new());
    assert_eq!(packs(repo.git_dir()), before + 1, "one fetch");
    assert_eq!(pinned_revisions(repo.git_dir(), 1), pins(&[&r1, &r2, &r3]));
    for (head, contents) in [(&r1, "first version\n"), (&r2, "second version\n")] {
        assert_eq!(
            repo.read_file(head, b"feature.txt").unwrap().unwrap(),
            contents.as_bytes()
        );
    }
    // A garbage collection keeps them.
    git(repo.git_dir(), &["gc", "-q", "--prune=now"]);
    assert!(repo.is_ancestor(&pr.base, &r1).unwrap());

    // Asking again fetches nothing.
    let before = packs(repo.git_dir());
    repo.fetch_commits(1, &[r1.clone(), r2.clone()]).unwrap();
    assert_eq!(packs(repo.git_dir()), before);
}

#[test]
fn heads_github_no_longer_has_are_unavailable_and_the_rest_are_fetched() {
    let server = Server::new();
    let pr = pull_request(&server);
    let ForcePushed { r1, r2, r3 } = force_pushed(&pr);
    // The server lost r1 and r2, which nothing it advertises reaches any more.
    pr.remote.forget_unreachable();
    assert!(!pr.remote.has(&r1) && !pr.remote.has(&r2));
    let never: Oid = "1234567890123456789012345678901234567890".parse().unwrap();
    let (_dir, cache) = server.cache();
    let repo = cache.open("owner", "repo", None).unwrap();

    let unavailable = repo
        .fetch_commits(1, &[r1.clone(), never.clone(), r3.clone(), r2.clone()])
        .unwrap();

    let mut unavailable: Vec<String> = unavailable.iter().map(Oid::to_string).collect();
    unavailable.sort();
    let mut expected = vec![r1.to_string(), r2.to_string(), never.to_string()];
    expected.sort();
    assert_eq!(unavailable, expected);
    assert_eq!(pinned_revisions(repo.git_dir(), 1), pins(&[&r3]));
    assert_eq!(
        repo.read_file(&r3, b"feature.txt").unwrap().unwrap(),
        b"third version\n"
    );

    // The clone remembers them, so asking again asks GitHub for nothing: with the server gone,
    // it still answers.
    let mut remembered: Vec<String> = (repo.unavailable_revisions(1).unwrap().iter())
        .map(Oid::to_string)
        .collect();
    remembered.sort();
    assert_eq!(remembered, expected);
    assert_eq!(repo.unavailable_revisions(2).unwrap(), Vec::<Oid>::new());
    drop(pr);
    drop(server);
    let again = repo
        .fetch_commits(1, &[r1.clone(), never.clone(), r3.clone(), r2.clone()])
        .unwrap();
    assert_eq!(again.len(), 3);
    drop(repo);
    let offline = cache.open_offline("owner", "repo").unwrap().unwrap();
    assert_eq!(offline.unavailable_revisions(1).unwrap().len(), 3);
}

#[test]
fn heads_the_clone_kept_are_pinned_without_the_server() {
    let server = Server::new();
    let pr = pull_request(&server);
    let (_dir, cache) = server.cache();
    let repo = cache.open("owner", "repo", None).unwrap();
    let r1 = fetch(&repo, 1).head;
    let ForcePushed { r3, .. } = force_pushed(&pr);
    assert_eq!(fetch(&repo, 1).head, r3);
    // The clone's pins went, as an older rozed's would never have made them; its commits stay.
    git(
        repo.git_dir(),
        &["update-ref", "-d", &format!("refs/rozed/pull/1/revs/{r1}")],
    );
    pr.remote.forget_unreachable();
    let before = packs(repo.git_dir());

    assert_eq!(
        repo.fetch_commits(1, std::slice::from_ref(&r1)).unwrap(),
        vec![]
    );

    assert_eq!(packs(repo.git_dir()), before);
    assert!(pinned_revisions(repo.git_dir(), 1).contains(&format!("{r1} {r1}")));
}

#[test]
fn fetching_heads_is_refused_offline() {
    let server = Server::new();
    let pr = pull_request(&server);
    let (_dir, cache) = server.cache();
    fetch(&cache.open("owner", "repo", None).unwrap(), 1);
    let repo = cache.open_offline("owner", "repo").unwrap().unwrap();

    let error = repo
        .fetch_commits(1, std::slice::from_ref(&pr.head))
        .unwrap_err();

    assert!(matches!(error, Error::ReadOnly { .. }), "{error:?}");
}

/// Pull request #1 with a merge in it:
///
/// ```text
/// base ── c1 ── c2 ──── m (head)
///           └── s1 ──┘
/// ```
struct Merged {
    base: Oid,
    c1: Oid,
    c2: Oid,
    s1: Oid,
    m: Oid,
}

fn merged(server: &Server) -> Merged {
    let remote = server.repo("owner", "repo");
    remote.write("README.md", "# repo\n");
    let base = remote.commit("base");
    remote.publish("refs/heads/trunk");
    remote.switch("feature").write("a.txt", "a\n");
    let c1 = remote.commit("c1");
    remote.switch("side").write("s.txt", "s\n");
    let s1 = remote.commit("s1");
    remote.switch("feature").write("b.txt", "b\n");
    let c2 = remote.commit("c2");
    git(&remote.work, &["merge", "-q", "--no-ff", "-m", "m", "side"]);
    let m = remote.head();
    remote
        .publish("refs/heads/feature")
        .publish("refs/pull/1/head");
    Merged {
        base,
        c1,
        c2,
        s1,
        m,
    }
}

#[test]
fn lists_a_pull_requests_commits_each_after_its_parents() {
    let server = Server::new();
    let Merged {
        base,
        c1,
        c2,
        s1,
        m,
    } = merged(&server);
    let (_dir, cache) = server.cache();
    let repo = cache.open("owner", "repo", None).unwrap();
    let fetched = fetch(&repo, 1);
    assert_eq!(fetched.head, m);
    let before = packs(repo.git_dir());

    let commits = repo.pull_commits(&fetched.base_tip, &m).unwrap();

    assert_eq!(commits.len(), 4);
    assert_eq!(commits.first(), Some(&c1));
    assert_eq!(commits.last(), Some(&m));
    let position = |oid: &Oid| commits.iter().position(|commit| commit == oid).unwrap();
    assert!(position(&c1) < position(&c2) && position(&c1) < position(&s1));
    assert!(!commits.contains(&base));
    // Reading commits downloads nothing.
    assert_eq!(packs(repo.git_dir()), before);
    // From a commit of the pull request, only what came after it.
    assert_eq!(repo.pull_commits(&c2, &c2).unwrap(), Vec::<Oid>::new());
    assert_eq!(repo.pull_commits(&base, &c1).unwrap(), vec![c1.clone()]);
}

#[test]
fn lists_commits_offline_but_never_downloads_one() {
    let server = Server::new();
    let pr = pull_request(&server);
    let (_dir, cache) = server.cache();
    let fetched = fetch(&cache.open("owner", "repo", None).unwrap(), 1);
    // A commit the server has but the clone never fetched.
    pr.remote.switch("trunk").write("later.txt", "later\n");
    let later = pr.remote.commit("later");
    pr.remote.publish("refs/heads/trunk");
    let repo = cache.open_offline("owner", "repo").unwrap().unwrap();

    assert_eq!(
        repo.pull_commits(&fetched.base_tip, &fetched.head).unwrap(),
        vec![pr.head.clone()]
    );
    let error = repo.pull_commits(&later, &fetched.head).unwrap_err();
    assert!(
        matches!(&error, Error::NotFetched { oid } if *oid == later),
        "{error:?}"
    );
}

#[test]
fn heads_noted_as_lost_that_the_clone_has_now_or_that_dont_parse_are_left_out() {
    let server = Server::new();
    let pr = pull_request(&server);
    let ForcePushed { r1, r3, .. } = force_pushed(&pr);
    pr.remote.forget_unreachable();
    let (_dir, cache) = server.cache();
    let repo = cache.open("owner", "repo", None).unwrap();
    assert_eq!(fetch(&repo, 1).head, r3);
    let lost = vec![r1.clone()];
    assert_eq!(
        repo.fetch_commits(1, std::slice::from_ref(&r1)).unwrap(),
        lost
    );
    // A write cut short, and a head the clone got since, from another pull request's fetch.
    let file = repo.git_dir().join("rozed-unavailable/pull-1");
    let noted = std::fs::read_to_string(&file).unwrap();
    std::fs::write(&file, format!("{noted}{}\n12ab", pr.base)).unwrap();

    assert_eq!(repo.unavailable_revisions(1).unwrap(), lost);
    // The next fetch writes the file again with only what's still lost.
    assert_eq!(
        repo.fetch_commits(1, &[r1.clone(), r3.clone()]).unwrap(),
        lost
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), format!("{r1}\n"));
}
