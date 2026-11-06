//! Comparing two revisions of a pull request, across a rebase.

use std::fs;
use std::sync::Arc;
use std::time::Duration;

use repo_cache::{
    Change, ChangedFile, ComparedFile, Comparison, Error, FetchFailure, FileMode, Oid,
    PullRequestRefs, Replay, Repo, RepoCache, Revision,
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
        changed_paths(&self.repo.changed_files(old, new).unwrap())
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

fn paths(files: &[ComparedFile]) -> Vec<String> {
    files
        .iter()
        .map(|file| file.display_path().into_owned())
        .collect()
}

/// How each file changed between the comparison's sides. Every file must have changed.
fn changes(files: &[ComparedFile]) -> Vec<ChangedFile> {
    files
        .iter()
        .map(|file| file.change.clone().expect("a change"))
        .collect()
}

/// The files marked as conflicted in the replay.
fn conflicted(files: &[ComparedFile]) -> Vec<String> {
    files
        .iter()
        .filter(|file| file.conflicted())
        .map(|file| file.display_path().into_owned())
        .collect()
}

fn changed_paths(files: &[ChangedFile]) -> Vec<String> {
    files
        .iter()
        .map(|file| file.display_path().into_owned())
        .collect()
}

/// The trees pinned in `repo`.
fn pinned_trees(repo: &Repo) -> Vec<String> {
    replay_pins(repo)
        .into_iter()
        .map(|(tree, _)| tree)
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
    assert_eq!(conflicted(&comparison.files), Vec::<String>::new());
    let c = comparison.files[0].change.as_ref().unwrap();
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
fn a_conflicting_rebase_falls_back_to_the_plain_comparison_with_each_conflicted_file_marked() {
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

    let Replay::Conflicted { tree } = &comparison.replay else {
        panic!("{:?}", comparison.replay);
    };
    // The plain comparison of the heads, trunk's e.txt included, with a.txt marked.
    assert_eq!(paths(&comparison.files), ["a.txt", "e.txt"]);
    assert_eq!(
        changes(&comparison.files),
        f.repo.changed_files(&r1, &r2).unwrap()
    );
    assert_eq!(conflicted(&comparison.files), ["a.txt"]);
    assert_eq!((&comparison.old, &comparison.new), (&r1, &r2));
    // How the author resolved it: from git's conflict markers to the line they wrote.
    let resolution = comparison.files[0].resolution.as_ref().unwrap();
    assert_eq!(changed_paths(resolution), ["a.txt"]);
    let markers = f.read(tree, "a.txt").unwrap();
    for line in [
        "<<<<<<<",
        "a line 3, changed on trunk",
        "a line 3, changed by the PR",
        ">>>>>>>",
    ] {
        assert!(markers.contains(line), "{markers}");
    }
    let (old, new) = (
        resolution[0].old.as_ref().unwrap(),
        resolution[0].new.as_ref().unwrap(),
    );
    assert_eq!(
        f.repo.read_blob(&old.oid).unwrap(),
        Some(markers.into_bytes())
    );
    assert_eq!(new.oid.as_str(), blob_id(&f.remote.work, &r2, "a.txt"));
    // The conflicted replay is pinned, as a clean one is.
    assert_eq!(pinned_trees(&f.repo), [tree.to_string()]);
}

/// The base and the pull request change the same line, and the author resolves it by keeping
/// their own, which drops the base's change. Comparing the heads shows nothing for that file.
#[test]
fn a_conflict_resolved_by_dropping_the_bases_change_shows() {
    let f = fixture();
    let remote = &f.remote;
    remote
        .write("a.txt", text("a", &[]))
        .write("u.txt", text("u", &[]));
    let m1 = remote.commit("m1");
    remote.publish("refs/heads/trunk");
    remote
        .switch("feature")
        .write("a.txt", text("a", &[(3, "a line 3, the PR's")]));
    remote.commit("r1");
    let r1 = f.push(1, "trunk");
    remote
        .switch("trunk")
        .write("a.txt", text("a", &[(3, "a line 3, trunk's")]))
        .write("u.txt", text("u", &[(9, "u line 9, trunk's")]));
    let m2 = remote.commit("m2");
    remote.publish("refs/heads/trunk");
    remote
        .switch("feature-2")
        .write("a.txt", text("a", &[(3, "a line 3, the PR's")]));
    remote.commit("r2");
    let r2 = f.push(1, "trunk");
    assert_eq!(f.plain_paths(&r1, &r2), ["u.txt"]);

    let comparison = f
        .repo
        .compare_revisions(
            &Revision { base: m1, head: r1 },
            &Revision {
                base: m2,
                head: r2.clone(),
            },
        )
        .unwrap();

    assert!(matches!(comparison.replay, Replay::Conflicted { .. }));
    assert_eq!(paths(&comparison.files), ["a.txt", "u.txt"]);
    assert_eq!(conflicted(&comparison.files), ["a.txt"]);
    let a = &comparison.files[0];
    // The same in both heads.
    assert_eq!(a.change, None);
    let resolution = a.resolution.as_ref().unwrap();
    assert_eq!(changed_paths(resolution), ["a.txt"]);
    assert_eq!(resolution[0].change, Change::Modified);
    let new = resolution[0].new.as_ref().unwrap();
    assert_eq!(new.oid.as_str(), blob_id(&f.remote.work, &r2, "a.txt"));
}

/// The pull request turns a file into a symlink, which the base changes. git sets the base's file
/// aside as `l.txt~<base>`, which is in neither revision: the conflict is l.txt's.
#[test]
fn a_file_set_aside_in_a_conflict_is_marked_as_the_file_it_came_from() {
    let f = fixture();
    let remote = &f.remote;
    remote.write("l.txt", text("l", &[]));
    let m1 = remote.commit("m1");
    remote.publish("refs/heads/trunk");
    remote.switch("feature").symlink("l.txt", "target");
    remote.commit("r1");
    let r1 = f.push(1, "trunk");
    remote
        .switch("trunk")
        .write("l.txt", text("l", &[(5, "l line 5, trunk's")]));
    let m2 = remote.commit("m2");
    remote.publish("refs/heads/trunk");
    remote.switch("feature-2").symlink("l.txt", "target");
    remote.commit("r2");
    let r2 = f.push(1, "trunk");

    let comparison = f
        .repo
        .compare_revisions(
            &Revision { base: m1, head: r1 },
            &Revision {
                base: m2.clone(),
                head: r2,
            },
        )
        .unwrap();

    assert!(matches!(comparison.replay, Replay::Conflicted { .. }));
    assert_eq!(paths(&comparison.files), ["l.txt"]);
    assert_eq!(conflicted(&comparison.files), ["l.txt"]);
    assert_eq!(comparison.files[0].change, None);
    // The author dropped trunk's change, set aside under a name of its own.
    let resolution = comparison.files[0].resolution.as_ref().unwrap();
    assert_eq!(changed_paths(resolution), [format!("l.txt~{m2}")]);
    assert_eq!(resolution[0].change, Change::Deleted);
    let set_aside = resolution[0].old.as_ref().unwrap();
    assert_eq!(
        set_aside.oid.as_str(),
        blob_id(&f.remote.work, &m2, "l.txt")
    );
}

#[test]
fn a_binary_file_changed_on_both_sides_is_marked() {
    let f = fixture();
    let remote = &f.remote;
    remote.write("b.bin", b"a\0b");
    let m1 = remote.commit("m1");
    remote.publish("refs/heads/trunk");
    remote.switch("feature").write("b.bin", b"a\0the PR's");
    remote.commit("r1");
    let r1 = f.push(1, "trunk");
    remote.switch("trunk").write("b.bin", b"a\0trunk's");
    let m2 = remote.commit("m2");
    remote.publish("refs/heads/trunk");
    remote.switch("feature-2").write("b.bin", b"a\0both");
    remote.commit("r2");
    let r2 = f.push(1, "trunk");

    let comparison = f
        .repo
        .compare_revisions(
            &Revision { base: m1, head: r1 },
            &Revision {
                base: m2.clone(),
                head: r2.clone(),
            },
        )
        .unwrap();

    assert!(matches!(comparison.replay, Replay::Conflicted { .. }));
    assert_eq!(conflicted(&comparison.files), ["b.bin"]);
    let b = &comparison.files[0];
    assert!(b.change.as_ref().unwrap().binary);
    // git keeps the new base's version of a binary file it can't merge.
    let resolution = b.resolution.as_ref().unwrap();
    assert_eq!(changed_paths(resolution), ["b.bin"]);
    assert!(resolution[0].binary);
    let (old, new) = (
        resolution[0].old.as_ref().unwrap(),
        resolution[0].new.as_ref().unwrap(),
    );
    assert_eq!(old.oid.as_str(), blob_id(&f.remote.work, &m2, "b.bin"));
    assert_eq!(new.oid.as_str(), blob_id(&f.remote.work, &r2, "b.bin"));
}

/// Both sides move a submodule, and the author keeps the pull request's commit, which moves it
/// back from the base's. Its commits, which are in another repository, are never fetched.
#[test]
fn a_submodule_moved_on_both_sides_is_marked() {
    let (s1, s2, s3) = (
        "1111111111111111111111111111111111111111",
        "2222222222222222222222222222222222222222",
        "4444444444444444444444444444444444444444",
    );
    let f = fixture();
    let remote = &f.remote;
    remote
        .write(
            ".gitmodules",
            "[submodule \"sub\"]\n\tpath = sub\n\turl = https://example.com/sub.git\n",
        )
        .gitlink("sub", s1);
    let m1 = remote.commit("m1");
    remote.publish("refs/heads/trunk");
    remote.switch("feature").gitlink("sub", s2);
    remote.commit("r1");
    let r1 = f.push(1, "trunk");
    remote.switch("trunk").gitlink("sub", s3);
    let m2 = remote.commit("m2");
    remote.publish("refs/heads/trunk");
    remote.switch("feature-2").gitlink("sub", s2);
    remote.commit("r2");
    let r2 = f.push(1, "trunk");

    let comparison = f
        .repo
        .compare_revisions(
            &Revision { base: m1, head: r1 },
            &Revision { base: m2, head: r2 },
        )
        .unwrap();

    assert!(matches!(comparison.replay, Replay::Conflicted { .. }));
    assert_eq!(paths(&comparison.files), ["sub"]);
    assert_eq!(comparison.files[0].change, None);
    let resolution = comparison.files[0].resolution.as_ref().unwrap();
    assert_eq!(changed_paths(resolution), ["sub"]);
    assert!(resolution[0].is_submodule());
    let (old, new) = (
        resolution[0].old.as_ref().unwrap(),
        resolution[0].new.as_ref().unwrap(),
    );
    assert_eq!((old.oid.as_str(), new.oid.as_str()), (s3, s2));
    assert_eq!(
        (old.mode, new.mode),
        (FileMode::Submodule, FileMode::Submodule)
    );
}

/// The pull request deletes a file the base changes, and the author keeps it deleted, which drops
/// the base's change.
#[test]
fn a_file_deleted_on_one_side_and_changed_on_the_other_is_marked() {
    let f = fixture();
    let remote = &f.remote;
    remote
        .write("f.txt", text("f", &[]))
        .write("g.txt", text("g", &[]));
    let m1 = remote.commit("m1");
    remote.publish("refs/heads/trunk");
    remote.switch("feature").remove("f.txt");
    remote.commit("r1");
    let r1 = f.push(1, "trunk");
    remote
        .switch("trunk")
        .write("f.txt", text("f", &[(10, "f line 10, trunk's")]));
    let m2 = remote.commit("m2");
    remote.publish("refs/heads/trunk");
    remote.switch("feature-2").remove("f.txt");
    remote.commit("r2");
    let r2 = f.push(1, "trunk");
    assert_eq!(f.plain_paths(&r1, &r2), Vec::<String>::new());

    let comparison = f
        .repo
        .compare_revisions(
            &Revision { base: m1, head: r1 },
            &Revision {
                base: m2.clone(),
                head: r2,
            },
        )
        .unwrap();

    assert!(matches!(comparison.replay, Replay::Conflicted { .. }));
    assert_eq!(paths(&comparison.files), ["f.txt"]);
    assert_eq!(comparison.files[0].change, None);
    let resolution = comparison.files[0].resolution.as_ref().unwrap();
    assert_eq!(changed_paths(resolution), ["f.txt"]);
    assert_eq!(resolution[0].change, Change::Deleted);
    let dropped = resolution[0].old.as_ref().unwrap();
    assert_eq!(dropped.oid.as_str(), blob_id(&f.remote.work, &m2, "f.txt"));
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
    assert_eq!(
        changes(&comparison.files),
        f.repo.changed_files(&r1, &r2).unwrap()
    );
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
    assert!(matches!(from_trunk.replay, Replay::Conflicted { .. }));
    assert_eq!(conflicted(&from_trunk.files), ["lower.txt"]);

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

/// A stack whose upper layer (#2) gets a fix after its lower layer (#1) was amended and
/// force-pushed, and is restacked onto the amended lower layer after that:
///
/// ```text
/// m1 ── l1 ── u1 ── u1b     (lower, upper, then upper's fix, still on l1)
///  └─── l1' ── u2           (lower amended; upper restacked onto it)
/// ```
struct Restacked {
    f: Fixture,
    l1: Oid,
    l1_amended: Oid,
    u1: Oid,
    u1b: Oid,
    u2: Oid,
}

fn restacked() -> Restacked {
    let f = fixture();
    let remote = &f.remote;
    remote.write("a.txt", text("a", &[]));
    remote.commit("m1");
    remote.publish("refs/heads/trunk");
    remote
        .switch("lower")
        .write("lower.txt", text("lower", &[]));
    let l1 = remote.commit("lower layer");
    remote.publish("refs/heads/lower");
    remote
        .switch("upper")
        .write("upper.txt", text("upper", &[]));
    remote.commit("upper layer");
    let u1 = f.push(2, "lower");
    remote
        .switch("lower")
        .write("lower.txt", text("lower", &[(2, "lower line 2, amended")]));
    let l1_amended = remote.amend("lower layer, amended");
    remote.publish("refs/heads/lower");
    let fixed = text("upper", &[(9, "upper line 9, fixed")]);
    remote.switch("upper").write("upper.txt", &fixed);
    remote.commit("fix upper layer");
    let u1b = f.push(2, "lower");
    remote
        .switch("lower")
        .switch("upper-2")
        .write("upper.txt", &fixed);
    remote.commit("upper layer, fixed");
    let u2 = f.push(2, "lower");
    Restacked {
        f,
        l1,
        l1_amended,
        u1,
        u1b,
        u2,
    }
}

/// A base the head doesn't contain would show the changes between the two as the author's, with
/// nothing to warn about.
#[test]
fn a_base_that_isnt_an_ancestor_of_its_head_is_an_error() {
    let Restacked {
        f,
        l1,
        l1_amended,
        u1,
        u1b,
        u2,
    } = restacked();
    let not_ancestor = |error: Error, base: &Oid, head: &Oid| {
        assert!(
            matches!(&error, Error::BaseNotAncestor { base: b, head: h } if b == base && h == head),
            "{error:?}"
        );
    };
    // The lower layer's head when u1b was pushed, which u1b doesn't contain.
    let error = f
        .repo
        .compare_revisions(
            &Revision {
                base: l1_amended.clone(),
                head: u1b.clone(),
            },
            &Revision {
                base: l1_amended.clone(),
                head: u2.clone(),
            },
        )
        .unwrap_err();
    not_ancestor(error, &l1_amended, &u1b);
    let error = f
        .repo
        .compare_revisions(
            &Revision { base: l1, head: u1 },
            &Revision {
                base: l1_amended.clone(),
                head: u1b.clone(),
            },
        )
        .unwrap_err();
    not_ancestor(error, &l1_amended, &u1b);

    // The base branch's tip, as GitHub's `base.sha` gives it, after the base branch moved on.
    let Rebased { f, m2, r1, r2, .. } = rebased();
    let error = f
        .repo
        .compare_revisions(
            &Revision {
                base: m2.clone(),
                head: r1.clone(),
            },
            &Revision {
                base: m2.clone(),
                head: r2,
            },
        )
        .unwrap_err();
    not_ancestor(error, &m2, &r1);
    assert_eq!(f.replay_pins(), []);
}

/// A layer's base is the newest head of the layer below that its head contains: u1b's is still
/// l1, since it was pushed before the restack.
#[test]
fn a_stacked_layer_starts_at_the_newest_lower_head_it_contains() {
    let Restacked {
        f,
        l1,
        l1_amended,
        u1,
        u1b,
        u2,
    } = restacked();
    assert!(f.repo.is_ancestor(&l1, &u1b).unwrap());
    assert!(!f.repo.is_ancestor(&l1_amended, &u1b).unwrap());
    let u1b = Revision {
        base: l1.clone(),
        head: u1b,
    };

    // The fix, and only the fix.
    let fix = f
        .repo
        .compare_revisions(&Revision { base: l1, head: u1 }, &u1b)
        .unwrap();
    assert_eq!(fix.replay, Replay::SameBase);
    assert_eq!(paths(&fix.files), ["upper.txt"]);

    // The restack changed nothing of the author's.
    let restack = f
        .repo
        .compare_revisions(
            &u1b,
            &Revision {
                base: l1_amended,
                head: u2,
            },
        )
        .unwrap();
    assert_eq!(restack.replay, Replay::Clean);
    assert_eq!(restack.files, []);
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
    let files = changes(&comparison.files);
    assert_eq!(files[0].change, Change::Added);
    assert_eq!(files[1].change, Change::Renamed { similarity: 100 });
    assert_eq!(files[1].old_path(), Some(&b"c.rs"[..]));
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

/// The base renames a folder the old revision added a file to, and the rebased revision has the
/// file in the renamed folder, where a rebase puts it.
#[test]
fn a_folder_the_base_renamed_takes_the_authors_new_file_along() {
    let f = fixture();
    let remote = &f.remote;
    for name in ["x", "y", "z"] {
        remote.write(&format!("lib/{name}.rs"), source(name, &[]));
    }
    let m1 = remote.commit("m1");
    remote.publish("refs/heads/trunk");
    remote
        .switch("feature")
        .write("lib/new.rs", source("new", &[]));
    remote.commit("add lib/new.rs");
    let r1 = f.push(1, "trunk");
    remote.switch("trunk");
    for name in ["x", "y", "z"] {
        remote
            .remove(&format!("lib/{name}.rs"))
            .write(&format!("src/{name}.rs"), source(name, &[]));
    }
    let m2 = remote.commit("move lib to src");
    remote.publish("refs/heads/trunk");
    remote
        .switch("feature-2")
        .write("src/new.rs", source("new", &[]));
    remote.commit("add src/new.rs");
    let r2 = f.push(1, "trunk");
    assert_eq!(
        f.plain_paths(&r1, &r2),
        ["src/new.rs", "src/x.rs", "src/y.rs", "src/z.rs"]
    );

    let comparison = f
        .repo
        .compare_revisions(
            &Revision { base: m1, head: r1 },
            &Revision { base: m2, head: r2 },
        )
        .unwrap();

    assert_eq!(comparison.replay, Replay::Clean);
    assert_eq!(comparison.files, []);
    assert_eq!(f.read(&comparison.old, "lib/new.rs"), None);
    assert_eq!(
        f.read(&comparison.old, "src/new.rs"),
        Some(source("new", &[]))
    );
}

/// Each replayed tree is pinned once per process, at a ref named for it, and stays pinned while
/// the process has the clone open. Nothing runs git when a comparison goes.
#[test]
fn a_replayed_tree_is_pinned_once_and_stays_pinned_while_the_clone_is_open() {
    let Rebased { f, m1, m2, r1, r2 } = rebased();
    let old = Revision { base: m1, head: r1 };
    let new = Revision { base: m2, head: r2 };

    let comparison = f.repo.compare_revisions(&old, &new).unwrap();

    let pin = (
        comparison.old.to_string(),
        format!(
            "refs/rozed/replays/{}/{}",
            comparison.old,
            std::process::id()
        ),
    );
    assert_eq!(f.replay_pins(), std::slice::from_ref(&pin));
    // Comparing the same revisions again, here or through another `Repo` of the clone, makes
    // the same tree, which is pinned already.
    let again = f.repo.compare_revisions(&old, &new).unwrap();
    let other = f.cache.open("owner", "repo", None).unwrap();
    let through_other = other.compare_revisions(&old, &new).unwrap();
    assert_eq!(again.old, comparison.old);
    assert_eq!(through_other.old, comparison.old);
    assert_eq!(f.replay_pins(), std::slice::from_ref(&pin));

    // Dropping comparisons, or taking their files and dropping the rest, leaves the pin.
    let files = f.repo.compare_revisions(&old, &new).unwrap().files;
    let Comparison { old: tree, .. } = f.repo.compare_revisions(&old, &new).unwrap();
    drop((comparison, again, through_other, other));
    assert_eq!(f.replay_pins(), [pin]);
    assert_eq!(paths(&files), ["c.txt"]);
    assert_eq!(f.read(&tree, "c.txt"), Some(text("c", &[])));

    // Nothing else of the replay is in a ref, and fetching still works.
    let refs = git(f.repo.git_dir(), &["for-each-ref", "--format=%(refname)"]);
    assert!(
        refs.lines()
            .all(|name| name.starts_with("refs/rozed/pull/")
                || name.starts_with("refs/rozed/replays/")),
        "{refs}"
    );
    f.push(1, "trunk");
}

/// A comparison holds no `Repo`, so a view that keeps one keeps neither the clone open nor the
/// `Repo`'s progress callback, which may hold the view.
#[test]
fn a_comparison_keeps_no_repo_or_progress_callback() {
    let Rebased { f, m1, m2, r1, r2 } = rebased();
    let view = Arc::new(());
    let repo = f.repo.with_progress({
        let view = view.clone();
        move |_| {
            let _ = &view;
        }
    });

    let comparison = repo
        .compare_revisions(
            &Revision { base: m1, head: r1 },
            &Revision { base: m2, head: r2 },
        )
        .unwrap();
    drop(repo);

    assert_eq!(Arc::strong_count(&view), 1);
    assert_eq!(comparison.replay, Replay::Clean);
}

/// What `git` in `git_dir` does to drop what no ref reaches, with the server gone: repack at
/// launch, as rozed's maintenance does, then fully, as it does with git older than 2.52, then
/// prune, as `git gc` would.
fn repack_and_prune(f: &Fixture) {
    // A child process another test thread starts as a fetch lets go of the clone's lock can
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
    let git_dir = f.repo.git_dir();
    git(
        git_dir,
        &["repack", "-a", "-d", "-q", "--no-write-bitmap-index"],
    );
    git(git_dir, &["prune", "--expire=now"]);
}

/// Whether the clone at `git_dir` has `oid`, which must not be downloadable.
fn has_object(git_dir: &std::path::Path, oid: &str) -> bool {
    std::process::Command::new("git")
        .args(["cat-file", "-e", oid])
        .current_dir(git_dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_NO_LAZY_FETCH", "1")
        .status()
        .unwrap()
        .success()
}

/// A pin is a ref to a tree whose files are partly on the server, which repacking a partial clone
/// and fetching into it must take in stride. Without it, the files merged into the tree, which
/// exist only in the clone, go.
#[test]
fn a_pinned_replay_survives_repacking_and_pruning_and_an_unpinned_one_doesnt() {
    let merged = [(6, "line 6, the PR's"), (19, "line 19, trunk's")];
    for pinned in [true, false] {
        let (f, old, new) = content_merges();
        let comparison = f.repo.compare_revisions(&old, &new).unwrap();
        // The merged c.txt, without r2's change to line 12, was made here and is nowhere else.
        let replayed_c = git(
            f.repo.git_dir(),
            &["rev-parse", &format!("{}:c.txt", comparison.old)],
        );
        assert!(!f.remote.has(&replayed_c.parse().unwrap()));
        if !pinned {
            for (_, name) in f.replay_pins() {
                git(f.repo.git_dir(), &["update-ref", "-d", &name]);
            }
        }

        // A fetch, with a tree pinned among the refs.
        f.remote.switch("trunk").write("f.txt", "f\n");
        f.remote.commit("m3");
        f.remote.publish("refs/heads/trunk");
        f.remote.switch("feature-2");
        f.push(1, "trunk");
        let moved = f.remote.git_dir.with_extension("moved");
        fs::rename(&f.remote.git_dir, &moved).unwrap();
        repack_and_prune(&f);

        assert_eq!(has_object(f.repo.git_dir(), &replayed_c), pinned);
        assert_eq!(
            has_object(f.repo.git_dir(), comparison.old.as_str()),
            pinned
        );
        if pinned {
            assert_eq!(f.read(&comparison.old, "a.txt"), Some(text("a", &merged)));
            assert_eq!(f.read(&comparison.old, "c.txt"), Some(text("c", &merged)));
            // What was never downloaded still downloads.
            fs::rename(&moved, &f.remote.git_dir).unwrap();
            assert_eq!(
                f.read(&comparison.old, "untouched.txt"),
                Some(text("untouched", &[]))
            );
        }
    }
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

/// A commit GitHub has but the clone never fetched, as when rozed learns of a revision from the
/// timeline. git would download it, with its history, behind the caller's back.
#[test]
fn a_commit_the_clone_lacks_is_an_error_and_never_downloaded() {
    let Rebased { f, m1, m2, r1, r2 } = rebased();
    f.remote.switch("elsewhere").write("x.txt", "x\n");
    let unfetched = f.remote.commit("on the server only");
    f.remote.publish("refs/heads/elsewhere");
    let unknown: Oid = "3333333333333333333333333333333333333333".parse().unwrap();
    let old = Revision { base: m1, head: r1 };
    let new = Revision { base: m2, head: r2 };
    let git_dir = f.repo.git_dir();
    let before = packs(git_dir);

    for missing in [&unfetched, &unknown] {
        for (old, new) in [
            (
                Revision {
                    base: missing.clone(),
                    ..old.clone()
                },
                new.clone(),
            ),
            (
                Revision {
                    head: missing.clone(),
                    ..old.clone()
                },
                new.clone(),
            ),
            (
                old.clone(),
                Revision {
                    base: missing.clone(),
                    ..new.clone()
                },
            ),
            (
                old.clone(),
                Revision {
                    head: missing.clone(),
                    ..new.clone()
                },
            ),
        ] {
            let error = f.repo.compare_revisions(&old, &new).unwrap_err();
            assert!(
                matches!(&error, Error::NotFetched { oid } if oid == missing),
                "{old:?} {new:?}: {error:?}"
            );
        }
    }

    assert_eq!(packs(git_dir), before);
    assert_eq!(f.replay_pins(), []);
}

/// Pins outlive the process that made them: a rozed that quits runs no destructors, and one that
/// crashes can't. Opening the clone with no rozed having it open removes them all.
#[test]
fn pins_go_once_the_clone_is_opened_with_no_rozed_having_it_open() {
    let Rebased { f, m1, m2, r1, r2 } = rebased();
    let comparison = f
        .repo
        .compare_revisions(
            &Revision { base: m1, head: r1 },
            &Revision { base: m2, head: r2 },
        )
        .unwrap();
    let tree = comparison.old.to_string();
    let own = format!("refs/rozed/replays/{tree}/{}", std::process::id());
    // What another rozed that showed the comparison left when it quit.
    let left = format!("refs/rozed/replays/{tree}/99999");
    git(f.repo.git_dir(), &["update-ref", &left, &tree]);
    let mut pins = vec![(tree.clone(), own), (tree.clone(), left)];
    pins.sort();

    // Another window opening the clone leaves them, since this one might be showing them.
    let other = f.cache.open("owner", "repo", None).unwrap();
    assert_eq!(f.replay_pins(), pins);
    drop(other);

    // Once no `Repo` has the clone open, even while a comparison is kept, opening it removes
    // them. A child process another test thread starts at that moment can hold the in-use lock
    // for an instant, so this allows a moment.
    let Fixture { repo, cache, .. } = f;
    drop(repo);
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
    drop(comparison);
}
