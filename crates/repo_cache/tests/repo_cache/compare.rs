//! Comparing two revisions of a pull request, across a rebase.

use std::fs;
use std::time::Duration;

use repo_cache::{
    Change, ChangedFile, Comparison, Error, FetchFailure, Oid, PullRequestRefs, Replay, Repo,
    RepoCache, Revision,
};
use tempfile::TempDir;

use crate::support::{Remote, Server, blob_id, git, missing_objects, packs};

/// Twenty lines, `<name> line <n>`, with `changes` replacing lines by number. Changes to lines
/// far enough apart merge cleanly.
fn text(name: &str, changes: &[(usize, &str)]) -> String {
    (1..=20)
        .map(|n| match changes.iter().find(|(line, _)| *line == n) {
            Some((_, changed)) => format!("{changed}\n"),
            None => format!("{name} line {n}\n"),
        })
        .collect()
}

/// Twenty lines of Rust named after `name`, long enough that a small edit still reads as a
/// rename, with `changes` replacing lines by number.
fn source(name: &str, changes: &[(usize, &str)]) -> String {
    (1..=20)
        .map(|n| match changes.iter().find(|(line, _)| *line == n) {
            Some((_, changed)) => format!("{changed}\n"),
            None => format!("pub fn {name}_{n}() -> u32 {{ {n} }}\n"),
        })
        .collect()
}

/// A repository whose `trunk` and pull request #1 the tests move, and the clone they fetch into.
struct Fixture {
    _server: Server,
    remote: Remote,
    repo: Repo,
    cache: RepoCache,
    _cache_dir: TempDir,
}

fn fixture() -> Fixture {
    let server = Server::new();
    let remote = server.repo("owner", "repo");
    let (cache_dir, cache) = server.cache();
    let repo = cache.open("owner", "repo", None).unwrap();
    Fixture {
        _server: server,
        remote,
        repo,
        cache,
        _cache_dir: cache_dir,
    }
}

impl Fixture {
    /// Publishes the working copy's commit as pull request `number`'s head, with `base_branch` as
    /// it is, and fetches both, as rozed does after each push. Returns the head.
    fn push(&self, number: u64, base_branch: &str) -> Oid {
        self.remote.publish(&format!("refs/pull/{number}/head"));
        let fetched = self
            .repo
            .fetch_pull(&PullRequestRefs {
                number,
                head: Some(self.remote.head()),
                base_branch: base_branch.into(),
                base: None,
            })
            .unwrap();
        fetched.head
    }

    /// What a plain comparison of the two heads shows: every path, base changes included.
    fn plain_paths(&self, old: &Oid, new: &Oid) -> Vec<String> {
        paths(&self.repo.changed_files(old, new).unwrap())
    }

    fn read(&self, rev: &Oid, path: &str) -> Option<String> {
        self.repo
            .read_file(rev, path.as_bytes())
            .unwrap()
            .map(|bytes| String::from_utf8(bytes).unwrap())
    }

    /// The replayed trees pinned in the clone: `(tree, ref)` pairs.
    fn replay_pins(&self) -> Vec<(String, String)> {
        replay_pins(&self.repo)
    }
}

fn replay_pins(repo: &Repo) -> Vec<(String, String)> {
    git(
        repo.git_dir(),
        &[
            "for-each-ref",
            "--format=%(objectname) %(refname)",
            "refs/rozed/replays/",
        ],
    )
    .lines()
    .map(|line| {
        let (oid, name) = line.split_once(' ').unwrap();
        (oid.to_string(), name.to_string())
    })
    .collect()
}

fn paths(files: &[ChangedFile]) -> Vec<String> {
    files
        .iter()
        .map(|file| file.display_path().into_owned())
        .collect()
}

/// The object type of `rev` in the clone.
fn kind(repo: &Repo, rev: &Oid) -> String {
    git(repo.git_dir(), &["cat-file", "-t", rev.as_str()])
}

/// Pull request #1 rebased onto a trunk that moved, with one more change:
///
/// ```text
/// m1 ── m2                   (trunk: m2 changes b.txt and adds e.txt)
///  │     └── r2              (rebased: r1's changes, then c.txt changed too)
///  └── r1                    (a.txt changed, d.txt added)
/// ```
struct Rebased {
    f: Fixture,
    m1: Oid,
    m2: Oid,
    r1: Oid,
    r2: Oid,
}

fn rebased() -> Rebased {
    let f = fixture();
    let remote = &f.remote;
    remote
        .write("a.txt", text("a", &[]))
        .write("b.txt", text("b", &[]))
        .write("c.txt", text("c", &[]))
        .write("untouched.txt", text("untouched", &[]));
    let m1 = remote.commit("m1");
    remote.publish("refs/heads/trunk");

    remote
        .switch("feature")
        .write("a.txt", text("a", &[(3, "a line 3, changed by the PR")]))
        .write("d.txt", text("d", &[]));
    remote.commit("change a.txt and add d.txt");
    let r1 = f.push(1, "trunk");

    remote
        .switch("trunk")
        .write("b.txt", text("b", &[(10, "b line 10, changed on trunk")]))
        .write("e.txt", text("e", &[]));
    let m2 = remote.commit("m2");
    remote.publish("refs/heads/trunk");

    remote
        .switch("feature-2")
        .write("a.txt", text("a", &[(3, "a line 3, changed by the PR")]))
        .write("d.txt", text("d", &[]));
    remote.commit("change a.txt and add d.txt");
    remote.write("c.txt", text("c", &[(15, "c line 15, the change r2 adds")]));
    remote.commit("change c.txt");
    let r2 = f.push(1, "trunk");

    Rebased { f, m1, m2, r1, r2 }
}

#[test]
fn a_rebase_onto_a_moved_base_shows_only_the_authors_new_change() {
    let Rebased { f, m1, m2, r1, r2 } = rebased();
    // A plain comparison shows trunk's changes as if the author had made them.
    assert_eq!(f.plain_paths(&r1, &r2), ["b.txt", "c.txt", "e.txt"]);
    assert_eq!(f.repo.merge_base(&r1, &m2).unwrap(), Some(m1.clone()));
    assert_eq!(f.repo.merge_base(&r2, &m2).unwrap(), Some(m2.clone()));

    let comparison = f
        .repo
        .compare_revisions(
            &Revision {
                base: m1,
                head: r1.clone(),
            },
            &Revision {
                base: m2.clone(),
                head: r2.clone(),
            },
        )
        .unwrap();

    assert_eq!(comparison.replay, Replay::Clean);
    assert_eq!(paths(&comparison.files), ["c.txt"]);
    let c = &comparison.files[0];
    assert_eq!(c.change, Change::Modified);
    // The old side is c.txt as r1 left it, which is trunk's, and the new side is r2's.
    let old = c.old.as_ref().unwrap();
    let new = c.new.as_ref().unwrap();
    assert_eq!(old.oid.as_str(), blob_id(&f.remote.work, &m2, "c.txt"));
    assert_eq!(new.oid.as_str(), blob_id(&f.remote.work, &r2, "c.txt"));
    assert_eq!(old.size, Some(text("c", &[]).len() as u64));
    assert_eq!(
        c.lines.map(|lines| (lines.added, lines.deleted)),
        Some((1, 1))
    );

    // The old side is r1's changes on r2's base: a tree, which reads like any revision.
    assert_ne!(comparison.old, r1);
    assert_eq!(kind(&f.repo, &comparison.old), "tree");
    assert_eq!(comparison.new, r2);
    let read_old = |path| f.read(&comparison.old, path);
    assert_eq!(read_old("c.txt"), Some(text("c", &[])));
    assert_eq!(
        read_old("a.txt"),
        Some(text("a", &[(3, "a line 3, changed by the PR")]))
    );
    assert_eq!(
        read_old("b.txt"),
        Some(text("b", &[(10, "b line 10, changed on trunk")]))
    );
    assert_eq!(read_old("e.txt"), Some(text("e", &[])));
    assert_eq!(
        f.read(&comparison.new, "c.txt"),
        Some(text("c", &[(15, "c line 15, the change r2 adds")]))
    );
}

#[test]
fn a_conflicting_rebase_falls_back_to_the_plain_comparison_with_the_conflicted_paths() {
    let f = fixture();
    let remote = &f.remote;
    remote
        .write("a.txt", text("a", &[]))
        .write("b.txt", text("b", &[]));
    let m1 = remote.commit("m1");
    remote.publish("refs/heads/trunk");
    remote
        .switch("feature")
        .write("a.txt", text("a", &[(3, "a line 3, changed by the PR")]))
        .write("b.txt", text("b", &[(5, "b line 5, changed by the PR")]));
    remote.commit("change a.txt and b.txt");
    let r1 = f.push(1, "trunk");
    // Trunk changes the line the pull request changed, so the author resolves it in r2.
    remote
        .switch("trunk")
        .write("a.txt", text("a", &[(3, "a line 3, changed on trunk")]))
        .write("e.txt", text("e", &[]));
    let m2 = remote.commit("m2");
    remote.publish("refs/heads/trunk");
    remote
        .switch("feature-2")
        .write("a.txt", text("a", &[(3, "a line 3, changed by both")]))
        .write("b.txt", text("b", &[(5, "b line 5, changed by the PR")]));
    remote.commit("change a.txt and b.txt, after trunk");
    let r2 = f.push(1, "trunk");

    let comparison = f
        .repo
        .compare_revisions(
            &Revision {
                base: m1,
                head: r1.clone(),
            },
            &Revision {
                base: m2,
                head: r2.clone(),
            },
        )
        .unwrap();

    assert_eq!(
        comparison.replay,
        Replay::Conflicted {
            paths: vec![b"a.txt".to_vec()]
        }
    );
    // The plain comparison of the heads, trunk's e.txt included.
    assert_eq!(paths(&comparison.files), ["a.txt", "e.txt"]);
    assert_eq!(comparison.files, f.repo.changed_files(&r1, &r2).unwrap());
    assert_eq!(comparison.old, r1);
    assert_eq!(comparison.new, r2);
    // Nothing replayed is shown, so nothing is pinned.
    assert_eq!(f.replay_pins(), []);
}

#[test]
fn revisions_on_the_same_base_get_the_plain_comparison() {
    let f = fixture();
    let remote = &f.remote;
    remote
        .write("a.txt", text("a", &[]))
        .write("c.txt", text("c", &[]));
    let m1 = remote.commit("m1");
    remote.publish("refs/heads/trunk");
    remote
        .switch("feature")
        .write("a.txt", text("a", &[(3, "a line 3, changed by the PR")]));
    remote.commit("change a.txt");
    let r1 = f.push(1, "trunk");
    // Trunk moves on, but the pull request only adds a commit, so both revisions start at m1.
    remote.switch("trunk").write("e.txt", text("e", &[]));
    remote.commit("m2");
    remote.publish("refs/heads/trunk");
    remote
        .switch("feature")
        .write("c.txt", text("c", &[(15, "c line 15, the change r2 adds")]));
    remote.commit("change c.txt");
    let r2 = f.push(1, "trunk");

    let comparison = f
        .repo
        .compare_revisions(
            &Revision {
                base: m1.clone(),
                head: r1.clone(),
            },
            &Revision {
                base: m1,
                head: r2.clone(),
            },
        )
        .unwrap();

    assert_eq!(comparison.replay, Replay::SameBase);
    assert_eq!(paths(&comparison.files), ["c.txt"]);
    assert_eq!(comparison.files, f.repo.changed_files(&r1, &r2).unwrap());
    assert_eq!((&comparison.old, &comparison.new), (&r1, &r2));
    assert_eq!(f.replay_pins(), []);
}

/// A stack: pull request #2 (`upper`) on #1 (`lower`), on `trunk`. The lower layer is amended and
/// force-pushed, and the upper layer rebased onto it, then given one more change:
///
/// ```text
/// m1 ── l1 ── u1            (lower, then upper, as first pushed)
///  └─── l1' ── u2 ── u3     (lower amended; upper rebased onto it, then changed)
/// ```
#[test]
fn a_stacked_layer_compares_across_its_lower_layers_force_push() {
    let f = fixture();
    let remote = &f.remote;
    remote.write("a.txt", text("a", &[]));
    let m1 = remote.commit("m1");
    remote.publish("refs/heads/trunk");
    remote
        .switch("lower")
        .write("lower.txt", text("lower", &[]));
    let l1 = remote.commit("lower layer");
    remote
        .publish("refs/heads/lower")
        .publish("refs/pull/1/head");
    remote
        .switch("upper")
        .write("upper.txt", text("upper", &[]));
    remote.commit("upper layer");
    let u1 = f.push(2, "lower");

    remote
        .switch("lower")
        .write("lower.txt", text("lower", &[(2, "lower line 2, amended")]));
    let l1_amended = remote.amend("lower layer, amended");
    remote
        .publish("refs/heads/lower")
        .publish("refs/pull/1/head");
    remote
        .switch("upper-2")
        .write("upper.txt", text("upper", &[]));
    remote.commit("upper layer");
    let u2 = f.push(2, "lower");
    // The lower layer's amendment shows in a plain comparison.
    assert_eq!(f.plain_paths(&u1, &u2), ["lower.txt"]);

    let old = Revision {
        base: l1.clone(),
        head: u1.clone(),
    };
    let rebased_only = f
        .repo
        .compare_revisions(
            &old,
            &Revision {
                base: l1_amended.clone(),
                head: u2.clone(),
            },
        )
        .unwrap();
    // The author changed nothing.
    assert_eq!(rebased_only.replay, Replay::Clean);
    assert_eq!(rebased_only.files, []);

    // The merge base of u1 with the lower branch as it is now is trunk's m1, since l1 is gone
    // from the branch. Replaying from there takes the lower layer for the author's change, and
    // conflicts with its amendment.
    assert_eq!(
        f.repo.merge_base(&u1, &l1_amended).unwrap(),
        Some(m1.clone())
    );
    let from_trunk = f
        .repo
        .compare_revisions(
            &Revision {
                base: m1,
                head: u1.clone(),
            },
            &Revision {
                base: l1_amended.clone(),
                head: u2.clone(),
            },
        )
        .unwrap();
    assert_eq!(
        from_trunk.replay,
        Replay::Conflicted {
            paths: vec![b"lower.txt".to_vec()]
        }
    );

    remote.write(
        "upper.txt",
        text("upper", &[(7, "upper line 7, the change u3 adds")]),
    );
    remote.commit("change upper.txt");
    let u3 = f.push(2, "lower");
    let changed = f
        .repo
        .compare_revisions(
            &old,
            &Revision {
                base: l1_amended,
                head: u3.clone(),
            },
        )
        .unwrap();
    assert_eq!(changed.replay, Replay::Clean);
    assert_eq!(paths(&changed.files), ["upper.txt"]);
    assert_eq!(f.read(&changed.old, "upper.txt"), Some(text("upper", &[])));
    assert_eq!(
        f.read(&changed.new, "upper.txt"),
        Some(text("upper", &[(7, "upper line 7, the change u3 adds")]))
    );
}

#[test]
fn renames_across_a_rebase_show_only_what_the_author_renamed() {
    let f = fixture();
    let remote = &f.remote;
    remote
        .write("b.rs", source("b", &[]))
        .write("c.rs", source("c", &[]));
    let m1 = remote.commit("m1");
    remote.publish("refs/heads/trunk");
    // r1 renames b.rs and edits it.
    remote.switch("feature").remove("b.rs").write(
        "src/bee.rs",
        source("b", &[(3, "pub fn b_3() -> u32 { 33 }")]),
    );
    remote.commit("rename b.rs to src/bee.rs");
    let r1 = f.push(1, "trunk");
    // Trunk edits b.rs elsewhere.
    remote.switch("trunk").write(
        "b.rs",
        source("b", &[(16, "pub fn b_16() -> u32 { 1616 }")]),
    );
    let m2 = remote.commit("m2");
    remote.publish("refs/heads/trunk");
    // r2 is r1 rebased, which carries trunk's edit into src/bee.rs, then renames c.rs and adds a
    // file.
    remote.switch("feature-2").remove("b.rs").write(
        "src/bee.rs",
        source(
            "b",
            &[
                (3, "pub fn b_3() -> u32 { 33 }"),
                (16, "pub fn b_16() -> u32 { 1616 }"),
            ],
        ),
    );
    remote.commit("rename b.rs to src/bee.rs");
    remote
        .remove("c.rs")
        .write("src/c.rs", source("c", &[]))
        .write("extra.txt", "extra\n");
    remote.commit("rename c.rs, add extra.txt");
    let r2 = f.push(1, "trunk");
    assert_eq!(
        f.plain_paths(&r1, &r2),
        ["extra.txt", "src/bee.rs", "src/c.rs"]
    );

    let comparison = f
        .repo
        .compare_revisions(
            &Revision { base: m1, head: r1 },
            &Revision { base: m2, head: r2 },
        )
        .unwrap();

    assert_eq!(comparison.replay, Replay::Clean);
    assert_eq!(paths(&comparison.files), ["extra.txt", "src/c.rs"]);
    assert_eq!(comparison.files[0].change, Change::Added);
    assert_eq!(
        comparison.files[1].change,
        Change::Renamed { similarity: 100 }
    );
    assert_eq!(comparison.files[1].old_path(), Some(&b"c.rs"[..]));
    // r1's rename, replayed: src/bee.rs with trunk's edit, and no b.rs.
    assert_eq!(f.read(&comparison.old, "b.rs"), None);
    assert_eq!(
        f.read(&comparison.old, "src/bee.rs"),
        Some(source(
            "b",
            &[
                (3, "pub fn b_3() -> u32 { 33 }"),
                (16, "pub fn b_16() -> u32 { 1616 }"),
            ]
        ))
    );
}

#[test]
fn a_replayed_tree_is_pinned_only_while_its_comparison_is_kept() {
    let Rebased { f, m1, m2, r1, r2 } = rebased();
    let old = Revision { base: m1, head: r1 };
    let new = Revision { base: m2, head: r2 };

    let comparison = f.repo.compare_revisions(&old, &new).unwrap();

    let pins = f.replay_pins();
    assert_eq!(pins.len(), 1, "{pins:?}");
    assert_eq!(pins[0].0, comparison.old.as_str());
    assert!(
        pins[0]
            .1
            .starts_with(&format!("refs/rozed/replays/{}/", comparison.old)),
        "{pins:?}"
    );
    // A copy shares the pin, and a second comparison of the same revisions pins its own.
    let copy = comparison.clone();
    let again = f.repo.compare_revisions(&old, &new).unwrap();
    assert_eq!(again.old, comparison.old);
    assert_eq!(f.replay_pins().len(), 2);

    drop(comparison);
    assert_eq!(f.replay_pins().len(), 2);
    drop(copy);
    assert_eq!(f.replay_pins().len(), 1);
    drop(again);
    assert_eq!(f.replay_pins(), []);
    // Nothing else of the replay is in a ref, and fetching still works with none left.
    let refs = git(f.repo.git_dir(), &["for-each-ref", "--format=%(refname)"]);
    assert!(
        refs.lines()
            .all(|name| name.starts_with("refs/rozed/pull/")),
        "{refs}"
    );
    f.push(1, "trunk");
}

/// A pin is a ref to a tree whose files are partly on the server, which repacking a partial clone
/// and fetching into it must take in stride.
#[test]
fn a_pinned_replay_survives_a_repack_and_fetches() {
    let (f, old, new) = content_merges();
    let comparison = f.repo.compare_revisions(&old, &new).unwrap();
    let merged = [(6, "line 6, the PR's"), (19, "line 19, trunk's")];
    // The merged c.txt, without r2's change to line 12, was made here and is nowhere else.
    let replayed_c = git(
        f.repo.git_dir(),
        &["rev-parse", &format!("{}:c.txt", comparison.old)],
    );
    assert!(!f.remote.has(&replayed_c.parse().unwrap()));

    // A fetch, with a tree pinned among the refs.
    f.remote.switch("trunk").write("f.txt", "f\n");
    f.remote.commit("m3");
    f.remote.publish("refs/heads/trunk");
    f.remote.switch("feature-2");
    f.push(1, "trunk");
    // Everything repacked with the server gone, as maintenance at launch does, then fully, as
    // it does with git older than 2.52.
    let moved = f.remote.git_dir.with_extension("moved");
    fs::rename(&f.remote.git_dir, &moved).unwrap();
    // A child process another test thread starts as the fetch lets go of the clone's lock can
    // hold it for an instant, and maintenance skips a locked clone, so this allows a moment.
    let mut repacked = Vec::new();
    for _ in 0..100 {
        repacked = f.cache.maintain(0).unwrap();
        if !repacked.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(repacked, ["owner/repo"]);
    git(
        f.repo.git_dir(),
        &["repack", "-a", "-d", "-q", "--no-write-bitmap-index"],
    );

    assert_eq!(f.read(&comparison.old, "a.txt"), Some(text("a", &merged)));
    assert_eq!(f.read(&comparison.old, "c.txt"), Some(text("c", &merged)));
    // What was never downloaded still downloads.
    fs::rename(&moved, &f.remote.git_dir).unwrap();
    assert_eq!(
        f.read(&comparison.old, "untouched.txt"),
        Some(text("untouched", &[]))
    );
}

/// Pull request #1 changes line 6 of a.txt, b.txt and c.txt; trunk then changes line 19 of each,
/// and the rebased r2 also changes line 12 of c.txt. Replaying r1 merges all three files.
fn content_merges() -> (Fixture, Revision, Revision) {
    let f = fixture();
    let remote = &f.remote;
    for name in ["a", "b", "c", "untouched"] {
        remote.write(&format!("{name}.txt"), text(name, &[]));
    }
    let m1 = remote.commit("m1");
    remote.publish("refs/heads/trunk");
    remote.switch("feature");
    for name in ["a", "b", "c"] {
        remote.write(
            &format!("{name}.txt"),
            text(name, &[(6, "line 6, the PR's")]),
        );
    }
    remote.commit("change line 6");
    let r1 = f.push(1, "trunk");
    remote.switch("trunk");
    for name in ["a", "b", "c"] {
        remote.write(
            &format!("{name}.txt"),
            text(name, &[(19, "line 19, trunk's")]),
        );
    }
    let m2 = remote.commit("m2");
    remote.publish("refs/heads/trunk");
    remote.switch("feature-2");
    for name in ["a", "b"] {
        let changes = [(6, "line 6, the PR's"), (19, "line 19, trunk's")];
        remote.write(&format!("{name}.txt"), text(name, &changes));
    }
    let changes = [
        (6, "line 6, the PR's"),
        (12, "line 12, the change r2 adds"),
        (19, "line 19, trunk's"),
    ];
    remote.write("c.txt", text("c", &changes));
    remote.commit("change line 6, after trunk, and line 12 of c.txt");
    let r2 = f.push(1, "trunk");
    let old = Revision { base: m1, head: r1 };
    let new = Revision { base: m2, head: r2 };
    (f, old, new)
}

#[test]
fn replaying_downloads_only_what_it_merges_and_shows_in_batches() {
    // Each path changed on one side only: the replay needs no file contents, and the comparison
    // downloads both sides of c.txt at once.
    let Rebased { f, m1, m2, r1, r2 } = rebased();
    let git_dir = f.repo.git_dir();
    let before = packs(git_dir);
    let comparison = f
        .repo
        .compare_revisions(
            &Revision {
                base: m1.clone(),
                head: r1.clone(),
            },
            &Revision {
                base: m2.clone(),
                head: r2.clone(),
            },
        )
        .unwrap();
    assert_eq!(paths(&comparison.files), ["c.txt"]);
    assert_eq!(packs(git_dir) - before, 1);
    let missing = missing_objects(git_dir, r2.as_str());
    for path in ["a.txt", "b.txt", "d.txt", "e.txt", "untouched.txt"] {
        assert!(
            missing.contains(&blob_id(&f.remote.work, &r2, path)),
            "{path}"
        );
    }

    // Three files changed on both sides: one download for the three merges, and one for the
    // comparison.
    let (f, old, new) = content_merges();
    let git_dir = f.repo.git_dir();
    let before = packs(git_dir);
    let comparison = f.repo.compare_revisions(&old, &new).unwrap();
    assert_eq!(paths(&comparison.files), ["c.txt"]);
    assert_eq!(packs(git_dir) - before, 2);
    assert!(
        missing_objects(git_dir, new.head.as_str()).contains(&blob_id(
            &f.remote.work,
            &new.head,
            "untouched.txt"
        ))
    );
    // Everything is here now, so comparing again downloads nothing.
    let again = f.repo.compare_revisions(&old, &new).unwrap();
    assert_eq!(again.old, comparison.old);
    assert_eq!(packs(git_dir) - before, 2);
}

#[test]
fn a_replay_that_cant_download_is_a_fetch_error() {
    let (f, old, new) = content_merges();
    let moved = f.remote.git_dir.with_extension("moved");
    fs::rename(&f.remote.git_dir, &moved).unwrap();

    let error = f.repo.compare_revisions(&old, &new).unwrap_err();

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
    assert_eq!(f.replay_pins(), []);
    fs::rename(&moved, &f.remote.git_dir).unwrap();
    let comparison: Comparison = f.repo.compare_revisions(&old, &new).unwrap();
    assert_eq!(paths(&comparison.files), ["c.txt"]);
}

#[test]
fn an_unknown_commit_is_an_error_not_a_conflict() {
    let Rebased { f, m1, m2, r2, .. } = rebased();
    let unknown: Oid = "3333333333333333333333333333333333333333".parse().unwrap();

    let result = f.repo.compare_revisions(
        &Revision {
            base: m1,
            head: unknown,
        },
        &Revision { base: m2, head: r2 },
    );

    assert!(result.is_err(), "{result:?}");
    assert_eq!(f.replay_pins(), []);
}

#[test]
fn pins_a_crash_left_behind_go_once_nobody_has_the_clone_open() {
    let Rebased { f, m1, m2, r1, r2 } = rebased();
    let comparison = f
        .repo
        .compare_revisions(
            &Revision { base: m1, head: r1 },
            &Revision { base: m2, head: r2 },
        )
        .unwrap();
    let tree = comparison.old.clone();
    drop(comparison);
    // What a rozed that crashed while showing the comparison leaves.
    let left = format!("refs/rozed/replays/{tree}/99999-0");
    git(f.repo.git_dir(), &["update-ref", &left, tree.as_str()]);

    // Another window opening the clone leaves it, since this one might be showing it.
    let other = f.cache.open("owner", "repo", None).unwrap();
    assert_eq!(f.replay_pins(), [(tree.to_string(), left.clone())]);
    drop(other);

    let Fixture { repo, cache, .. } = f;
    drop(repo);
    // Opening it with no other open removes it. A child process another test thread starts at
    // that moment can hold the in-use lock for an instant, so this allows a moment.
    let mut reopened = cache.open("owner", "repo", None).unwrap();
    for _ in 0..100 {
        if replay_pins(&reopened).is_empty() {
            break;
        }
        drop(reopened);
        std::thread::sleep(Duration::from_millis(20));
        reopened = cache.open("owner", "repo", None).unwrap();
    }
    assert_eq!(replay_pins(&reopened), []);
}
