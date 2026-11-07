//! Reading diffs, files and history from a clone.

use std::fs;
use std::os::unix::fs::{PermissionsExt as _, symlink};

use repo_cache::{
    Change, ChangedFile, Error, FetchFailure, FileMode, FileVersion, LineCounts, Oid,
    PullRequestRefs, Repo,
};
use tempfile::TempDir;

use crate::support::{Remote, Server, blob_id, git, missing_objects, packs, rust_source};

/// The commits the submodule points at. Neither is on the server, so fetching either fails.
const SUBMODULE_BEFORE: &str = "1111111111111111111111111111111111111111";
const SUBMODULE_AFTER: &str = "2222222222222222222222222222222222222222";

/// A pull request that changes a file of every kind, and the clone it's fetched into:
///
/// - `keep.txt` modified, `run.sh` made executable, `data.bin` (binary) modified;
/// - `crlf.txt`'s line endings changed to CRLF;
/// - `old_name.rs` renamed to `src/new_name.rs` and edited;
/// - `gone.txt` deleted, `added.txt` added;
/// - `link`, a symlink, replaced by a file;
/// - `vendor/lib`, a submodule, moved to another commit.
///
/// Every commit has a `.gitattributes` calling `*.txt` binary, which Lo ignores.
struct Fixture {
    server: Server,
    remote: Remote,
    first: Oid,
    base: Oid,
    head: Oid,
    trunk_tip: Oid,
    _cache_dir: TempDir,
    repo: Repo,
}

fn fixture() -> Fixture {
    let server = Server::new();
    let remote = server.repo("owner", "repo");
    remote
        .write(".gitattributes", "*.txt binary\n")
        .write("keep.txt", "keep, first version\n");
    let first = remote.commit("first");

    remote
        .write("keep.txt", "keep\n")
        .write("crlf.txt", "one\ntwo\n")
        .write("run.sh", "echo run\n")
        .write("old_name.rs", rust_source(""))
        .write("gone.txt", "gone\n")
        .write("data.bin", b"\x00\x01\x02base")
        .write("odd\nname.txt", "a line break in the name\n")
        .gitlink("vendor/lib", SUBMODULE_BEFORE);
    symlink("keep.txt", remote.work.join("link")).unwrap();
    let base = remote.commit("base");

    remote
        .switch("feature")
        .write("keep.txt", "keep, changed\n")
        .write("crlf.txt", "one\r\ntwo\r\n")
        .write("data.bin", b"\x00\x01\x02head")
        .remove("old_name.rs")
        .write("src/new_name.rs", rust_source("pub fn added() {}\n"))
        .remove("gone.txt")
        .write("added.txt", "added\n")
        .remove("link")
        .write("link", "a file now\n")
        .gitlink("vendor/lib", SUBMODULE_AFTER);
    let run = remote.work.join("run.sh");
    fs::set_permissions(&run, fs::Permissions::from_mode(0o755)).unwrap();
    let head = remote.commit("head");
    remote.publish("refs/pull/1/head");

    remote
        .switch("trunk")
        .write("trunk.txt", "trunk moved on\n");
    let trunk_tip = remote.commit("trunk moves on");
    remote.publish("refs/heads/trunk");

    let (cache_dir, cache) = server.cache();
    let repo = cache.open("owner", "repo", None).unwrap();
    let fetched = repo
        .fetch_pull(&PullRequestRefs {
            number: 1,
            head: Some(head.clone()),
            base_branch: "trunk".into(),
            base: None,
        })
        .unwrap();
    assert_eq!(fetched.head, head);
    assert_eq!(fetched.base_tip, trunk_tip);
    Fixture {
        server,
        remote,
        first,
        base,
        head,
        trunk_tip,
        _cache_dir: cache_dir,
        repo,
    }
}

impl Fixture {
    fn version(&self, rev: &Oid, path: &str, mode: FileMode) -> Option<FileVersion> {
        let oid = blob_id(&self.remote.work, rev, path);
        let size = git(&self.remote.work, &["cat-file", "-s", &oid]);
        Some(FileVersion {
            path: path.into(),
            oid: oid.parse().unwrap(),
            mode,
            size: Some(size.parse().unwrap()),
        })
    }
}

#[test]
fn lists_what_changed_with_renames_binary_files_modes_and_submodules() {
    let f = fixture();

    let files = f.repo.changed_files(&f.base, &f.head).unwrap();

    let regular = FileMode::Regular;
    let lines = |added, deleted| Some(LineCounts { added, deleted });
    let file = |change, old, new, binary, lines| ChangedFile {
        change,
        old,
        new,
        binary,
        lines,
    };
    let (base, head) = (&f.base, &f.head);
    let submodule = |oid: &str| {
        Some(FileVersion {
            path: "vendor/lib".into(),
            oid: oid.parse().unwrap(),
            mode: FileMode::Submodule,
            // A commit in another repository, which Lo never reads.
            size: None,
        })
    };
    let expected = vec![
        file(
            Change::Added,
            None,
            f.version(head, "added.txt", regular),
            false,
            lines(1, 0),
        ),
        file(
            Change::Modified,
            f.version(base, "crlf.txt", regular),
            f.version(head, "crlf.txt", regular),
            false,
            lines(2, 2),
        ),
        file(
            Change::Modified,
            f.version(base, "data.bin", regular),
            f.version(head, "data.bin", regular),
            true,
            None,
        ),
        file(
            Change::Deleted,
            f.version(base, "gone.txt", regular),
            None,
            false,
            lines(0, 1),
        ),
        file(
            Change::Modified,
            f.version(base, "keep.txt", regular),
            f.version(head, "keep.txt", regular),
            false,
            lines(1, 1),
        ),
        file(
            Change::TypeChanged,
            f.version(base, "link", FileMode::Symlink),
            f.version(head, "link", regular),
            false,
            lines(1, 1),
        ),
        file(
            Change::Modified,
            f.version(base, "run.sh", regular),
            f.version(head, "run.sh", FileMode::Executable),
            false,
            lines(0, 0),
        ),
        file(
            Change::Renamed { similarity: 97 },
            f.version(base, "old_name.rs", regular),
            f.version(head, "src/new_name.rs", regular),
            false,
            lines(1, 0),
        ),
        file(
            Change::Modified,
            submodule(SUBMODULE_BEFORE),
            submodule(SUBMODULE_AFTER),
            false,
            None,
        ),
    ];
    assert_eq!(files, expected);
    assert_eq!(files[3].path(), b"gone.txt");
    assert_eq!(files[7].path(), b"src/new_name.rs");
    assert_eq!(files[7].old_path(), Some(&b"old_name.rs"[..]));
    assert!(files[6].is_mode_change_only());
    // The larger side's size, in bytes.
    assert_eq!(files[4].size(), Some("keep, changed\n".len() as u64));
    assert_eq!(files[0].size(), Some("added\n".len() as u64));
    assert_eq!(files[8].size(), None);
    assert!(!files[4].is_mode_change_only());
    assert!(files[8].is_submodule());
}

#[test]
fn listing_changes_downloads_both_sides_at_once() {
    let f = fixture();
    let git_dir = f.repo.git_dir();
    let changed_blob = blob_id(&f.remote.work, &f.base, "keep.txt");
    assert!(missing_objects(git_dir, f.base.as_str()).contains(&changed_blob));

    let before = packs(git_dir);

    f.repo.changed_files(&f.base, &f.head).unwrap();

    // In one download.
    assert_eq!(packs(git_dir), before + 1);
    let missing = missing_objects(git_dir, f.head.as_str());
    assert!(!missing.contains(&changed_blob));
    for path in ["keep.txt", "added.txt", "data.bin", "src/new_name.rs"] {
        assert!(
            !missing.contains(&blob_id(&f.remote.work, &f.head, path)),
            "{path}"
        );
    }
    // A file the pull request doesn't touch stays on the server.
    assert!(missing.contains(&blob_id(&f.remote.work, &f.head, "odd\nname.txt")));
}

#[test]
fn reads_files_by_blob_id() {
    let f = fixture();
    let files = f.repo.changed_files(&f.base, &f.head).unwrap();
    let renamed = &files[7];

    let read = |version: &Option<FileVersion>| {
        f.repo
            .read_blob(&version.as_ref().unwrap().oid)
            .unwrap()
            .unwrap()
    };
    assert_eq!(read(&renamed.old), rust_source("").into_bytes());
    assert_eq!(
        read(&renamed.new),
        rust_source("pub fn added() {}\n").into_bytes()
    );
    assert_eq!(read(&files[2].new), b"\x00\x01\x02head");
    // A symlink's contents are its target.
    assert_eq!(read(&files[5].old), b"keep.txt");

    assert!(f.repo.read_blob(&f.head).is_err(), "a commit isn't a file");
    // An object neither the clone nor the server has.
    let unknown: Oid = "3333333333333333333333333333333333333333".parse().unwrap();
    assert_eq!(f.repo.read_blob(&unknown).unwrap(), None);
    // And the reader still works afterwards.
    assert_eq!(read(&files[2].new), b"\x00\x01\x02head");
}

#[test]
fn reads_files_by_path_at_any_revision() {
    let f = fixture();
    let read = |rev: &Oid, path: &str| f.repo.read_file(rev, path.as_bytes()).unwrap();

    assert_eq!(
        read(&f.base, "old_name.rs").unwrap(),
        rust_source("").into_bytes()
    );
    assert_eq!(read(&f.head, "data.bin").unwrap(), b"\x00\x01\x02head");
    assert_eq!(
        read(&f.first, "keep.txt").unwrap(),
        b"keep, first version\n"
    );
    assert_eq!(
        read(&f.trunk_tip, "trunk.txt").unwrap(),
        b"trunk moved on\n"
    );
    assert_eq!(
        read(&f.base, "odd\nname.txt").unwrap(),
        b"a line break in the name\n"
    );

    assert_eq!(read(&f.base, "added.txt"), None);
    assert_eq!(read(&f.head, "gone.txt"), None);
    assert_eq!(read(&f.head, "odd\nmissing.txt"), None);
    // Pathspec magic and globs are just names.
    assert_eq!(read(&f.head, "*.txt"), None);
    assert_eq!(read(&f.head, ":(glob)**/*.rs"), None);
    assert_eq!(read(&f.head, "src/"), None);

    let error = f.repo.read_file(&f.head, b"src").unwrap_err();
    assert!(error.to_string().contains("src is a tree"), "{error}");
    assert!(matches!(
        f.repo.read_file(&f.head, b""),
        Err(Error::Invalid { .. })
    ));
}

#[test]
fn describes_a_file_at_any_revision_as_a_diff_would() {
    let f = fixture();
    let version = |rev: &Oid, path: &str| f.repo.file_version(rev, path.as_bytes()).unwrap();
    let side = |rev: &Oid, path: &str, mode| f.version(rev, path, mode);

    assert_eq!(
        version(&f.head, "keep.txt"),
        side(&f.head, "keep.txt", FileMode::Regular)
    );
    assert_eq!(
        version(&f.head, "run.sh"),
        side(&f.head, "run.sh", FileMode::Executable)
    );
    assert_eq!(
        version(&f.base, "link"),
        side(&f.base, "link", FileMode::Symlink)
    );
    assert_eq!(
        version(&f.base, "odd\nname.txt"),
        side(&f.base, "odd\nname.txt", FileMode::Regular)
    );
    // A submodule is the commit it points at, which nothing reads.
    assert_eq!(
        version(&f.head, "vendor/lib"),
        Some(FileVersion {
            path: b"vendor/lib".to_vec(),
            oid: SUBMODULE_AFTER.parse().unwrap(),
            mode: FileMode::Submodule,
            size: None,
        })
    );
    // As the diff listed it.
    let listed = f.repo.changed_files(&f.base, &f.head).unwrap();
    let keep = listed
        .iter()
        .find(|file| file.path() == b"keep.txt")
        .unwrap();
    assert_eq!(version(&f.head, "keep.txt").as_ref(), keep.new.as_ref());
    assert_eq!(version(&f.base, "keep.txt").as_ref(), keep.old.as_ref());

    assert_eq!(version(&f.head, "gone.txt"), None);
    assert_eq!(version(&f.head, "src/"), None);
    let error = f.repo.file_version(&f.head, b"src").unwrap_err();
    assert!(error.to_string().contains("src is a tree"), "{error}");
}

#[test]
fn never_fetches_a_submodules_commit() {
    let f = fixture();

    // The submodule's commits aren't on the server, so fetching either would fail.
    let files = f.repo.changed_files(&f.base, &f.head).unwrap();
    assert!(files.iter().any(|file| file.is_submodule()));
    let error = f.repo.read_file(&f.head, b"vendor/lib").unwrap_err();
    assert!(
        error.to_string().contains("vendor/lib is a submodule"),
        "{error}"
    );

    // Everything else still reads.
    assert_eq!(
        f.repo.read_file(&f.head, b"keep.txt").unwrap().unwrap(),
        b"keep, changed\n"
    );
}

#[test]
fn reading_a_file_downloads_it_once() {
    let f = fixture();
    let git_dir = f.repo.git_dir();
    let blob = blob_id(&f.remote.work, &f.first, "keep.txt");
    assert!(missing_objects(git_dir, f.first.as_str()).contains(&blob));

    f.repo.read_file(&f.first, b"keep.txt").unwrap();

    assert!(!missing_objects(git_dir, f.first.as_str()).contains(&blob));
}

#[test]
fn a_failed_download_is_a_fetch_error_and_the_next_read_recovers() {
    let f = fixture();
    // Start the long-lived reader, then take the server away.
    f.repo.read_file(&f.head, b"keep.txt").unwrap();
    let moved = f.remote.git_dir.with_extension("moved");
    fs::rename(&f.remote.git_dir, &moved).unwrap();

    let error = f.repo.read_file(&f.first, b"keep.txt").unwrap_err();
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

    fs::rename(&moved, &f.remote.git_dir).unwrap();
    assert_eq!(
        f.repo.read_file(&f.first, b"keep.txt").unwrap().unwrap(),
        b"keep, first version\n"
    );
    let _ = &f.server;
}

#[test]
fn finds_merge_bases_and_ancestors() {
    let f = fixture();
    let repo = &f.repo;

    assert_eq!(
        repo.merge_base(&f.head, &f.trunk_tip).unwrap(),
        Some(f.base.clone())
    );
    assert!(repo.is_ancestor(&f.base, &f.head).unwrap());
    assert!(repo.is_ancestor(&f.head, &f.head).unwrap());
    assert!(!repo.is_ancestor(&f.head, &f.base).unwrap());
    assert!(!repo.is_ancestor(&f.trunk_tip, &f.head).unwrap());

    // A pull request with no history in common with its base.
    f.remote.orphan("unrelated").write("other.txt", "other\n");
    let unrelated = f.remote.commit("unrelated");
    f.remote.publish("refs/pull/2/head");
    let fetched = repo
        .fetch_pull(&PullRequestRefs {
            number: 2,
            head: None,
            base_branch: "trunk".into(),
            base: None,
        })
        .unwrap();
    assert_eq!(fetched.head, unrelated);
    assert_eq!(repo.merge_base(&unrelated, &f.head).unwrap(), None);
    assert!(!repo.is_ancestor(&unrelated, &f.head).unwrap());
    // A commit the clone doesn't have is an error, not "no history in common".
    let unknown: Oid = "3333333333333333333333333333333333333333".parse().unwrap();
    assert!(repo.merge_base(&unknown, &f.head).is_err());
}

#[test]
fn the_clones_own_config_cant_turn_attributes_on() {
    let f = fixture();
    // Every commit's `.gitattributes` calls `*.txt` binary. Config naming a tree to read
    // attributes from, as `attr.tree` does, would apply it.
    git(
        f.repo.git_dir(),
        &["config", "attr.tree", "refs/lo/pull/1/head"],
    );

    let files = f.repo.changed_files(&f.base, &f.head).unwrap();

    let keep = files
        .iter()
        .find(|file| file.path() == b"keep.txt")
        .unwrap();
    assert!(!keep.binary);
    assert_eq!(
        keep.lines,
        Some(LineCounts {
            added: 1,
            deleted: 1
        })
    );
}

#[test]
fn names_that_arent_utf8_or_hold_a_tab_are_listed_and_read() {
    let server = Server::new();
    let remote = server.repo("owner", "repo");
    remote.write("README.md", "# repo\n");
    let base = remote.commit("base");
    remote.publish("refs/heads/trunk");
    // Latin-1 for "café.txt", which GitHub accepts, and a tab, which a Mac's disk does.
    remote.switch("feature").write("tab\tname.txt", "a tab\n");
    let head = remote.commit_with_names(&[(b"caf\xe9.txt", b"latin-1\n")], "names");
    remote.publish("refs/pull/1/head");
    let (_dir, cache) = server.cache();
    let repo = cache.open("owner", "repo", None).unwrap();
    repo.fetch_pull(&PullRequestRefs {
        number: 1,
        head: None,
        base_branch: "trunk".into(),
        base: None,
    })
    .unwrap();

    let files = repo.changed_files(&base, &head).unwrap();

    let paths: Vec<&[u8]> = files.iter().map(|file| file.path()).collect();
    assert_eq!(paths, [&b"caf\xe9.txt"[..], b"tab\tname.txt"]);
    assert_eq!(files[0].display_path(), "caf\u{fffd}.txt");
    assert_eq!(
        files[0].lines,
        Some(LineCounts {
            added: 1,
            deleted: 0
        })
    );
    assert_eq!(
        repo.read_file(&head, b"caf\xe9.txt").unwrap().unwrap(),
        b"latin-1\n"
    );
    assert_eq!(
        repo.read_file(&head, b"tab\tname.txt").unwrap().unwrap(),
        b"a tab\n"
    );
    assert_eq!(repo.read_file(&head, b"caf\xe8.txt").unwrap(), None);
}
