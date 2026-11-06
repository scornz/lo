//! Reading diffs, files and history from a clone.

use std::fs;
use std::os::unix::fs::{PermissionsExt as _, symlink};

use repo_cache::{
    Change, ChangedFile, Error, FetchFailure, FileMode, FileVersion, LineCounts, Oid, Repo,
};
use tempfile::TempDir;

use crate::support::{Remote, Server, blob_id, missing_objects, rust_source};

/// A pull request that changes a file of every kind, and the clone it's fetched into:
///
/// - `keep.txt` modified, `run.sh` made executable, `data.bin` (binary) modified;
/// - `old_name.rs` renamed to `src/new_name.rs` and edited;
/// - `gone.txt` deleted, `added.txt` added;
/// - `link`, a symlink, replaced by a file.
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
    remote.write("keep.txt", "keep, first version\n");
    let first = remote.commit("first");

    remote
        .write("keep.txt", "keep\n")
        .write("run.sh", "echo run\n")
        .write("old_name.rs", rust_source(""))
        .write("gone.txt", "gone\n")
        .write("data.bin", b"\x00\x01\x02base")
        .write("odd\nname.txt", "a line break in the name\n");
    symlink("keep.txt", remote.work.join("link")).unwrap();
    let base = remote.commit("base");

    remote
        .switch("feature")
        .write("keep.txt", "keep, changed\n")
        .write("data.bin", b"\x00\x01\x02head")
        .remove("old_name.rs")
        .write("src/new_name.rs", rust_source("pub fn added() {}\n"))
        .remove("gone.txt")
        .write("added.txt", "added\n")
        .remove("link")
        .write("link", "a file now\n");
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
    assert_eq!(repo.fetch_pull(1).unwrap(), head);
    assert_eq!(repo.fetch_branch("trunk").unwrap(), trunk_tip);
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
        Some(FileVersion {
            path: path.to_string(),
            oid: blob_id(&self.remote.work, rev, path).parse().unwrap(),
            mode,
        })
    }
}

#[test]
fn lists_what_changed_with_renames_binary_files_and_modes() {
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
    ];
    assert_eq!(files, expected);
    assert_eq!(files[2].path(), "gone.txt");
    assert_eq!(files[6].path(), "src/new_name.rs");
    assert_eq!(files[6].old_path(), Some("old_name.rs"));
}

#[test]
fn listing_changes_downloads_both_sides_at_once() {
    let f = fixture();
    let git_dir = f.repo.git_dir();
    let changed_blob = blob_id(&f.remote.work, &f.base, "keep.txt");
    assert!(missing_objects(git_dir, f.base.as_str()).contains(&changed_blob));

    f.repo.changed_files(&f.base, &f.head).unwrap();

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
fn reads_files_at_any_revision() {
    let f = fixture();
    let read = |rev: &Oid, path: &str| f.repo.read_blob(rev, path).unwrap();

    assert_eq!(
        read(&f.base, "old_name.rs").unwrap(),
        rust_source("").into_bytes()
    );
    assert_eq!(
        read(&f.head, "src/new_name.rs").unwrap(),
        rust_source("pub fn added() {}\n").into_bytes()
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
    // A symlink's contents are its target.
    assert_eq!(read(&f.base, "link").unwrap(), b"keep.txt");
    assert_eq!(
        read(&f.base, "odd\nname.txt").unwrap(),
        b"a line break in the name\n"
    );

    assert_eq!(read(&f.base, "added.txt"), None);
    assert_eq!(read(&f.head, "gone.txt"), None);
    assert_eq!(read(&f.head, "odd\nmissing.txt"), None);
    assert!(
        f.repo.read_blob(&f.head, "src").is_err(),
        "a folder isn't a file"
    );
    assert!(matches!(
        f.repo.read_blob(&f.head, ""),
        Err(Error::Invalid { .. })
    ));
}

#[test]
fn reading_a_file_downloads_it_once() {
    let f = fixture();
    let git_dir = f.repo.git_dir();
    let blob = blob_id(&f.remote.work, &f.first, "keep.txt");
    assert!(missing_objects(git_dir, f.first.as_str()).contains(&blob));

    f.repo.read_blob(&f.first, "keep.txt").unwrap();

    assert!(!missing_objects(git_dir, f.first.as_str()).contains(&blob));
}

#[test]
fn a_failed_download_is_a_fetch_error_and_the_next_read_recovers() {
    let f = fixture();
    // Start the long-lived reader, then take the server away.
    f.repo.read_blob(&f.head, "keep.txt").unwrap();
    let moved = f.remote.git_dir.with_extension("moved");
    fs::rename(&f.remote.git_dir, &moved).unwrap();

    let error = f.repo.read_blob(&f.first, "keep.txt").unwrap_err();
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
        f.repo.read_blob(&f.first, "keep.txt").unwrap().unwrap(),
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

    // A branch with no history in common.
    f.remote.orphan("unrelated").write("other.txt", "other\n");
    f.remote.commit("unrelated");
    f.remote.publish("refs/heads/unrelated");
    let unrelated = repo.fetch_branch("unrelated").unwrap();
    assert_eq!(repo.merge_base(&f.head, &unrelated).unwrap(), None);
    assert!(!repo.is_ancestor(&unrelated, &f.head).unwrap());
}
