//! A file's patch from the clone (`Repo::patch`), for the lines GitHub takes comments on when its
//! API leaves the patch out, or for a revision that isn't the latest.

use repo_cache::{Oid, Repo};
use review::threads::{CommentableLines, Hunk, Line, Lines, Screen};
use tempfile::TempDir;

use crate::support::{Server, git, packs, rust_source};

/// The commits a submodule points at. Neither is on the server, so fetching either fails.
const SUBMODULE_BEFORE: &str = "1111111111111111111111111111111111111111";
const SUBMODULE_AFTER: &str = "2222222222222222222222222222222222222222";

/// Twenty lines, `line 1` to `line 20`, with `edits` in place of some.
fn twenty_lines(edits: &[(usize, &str)]) -> String {
    (1..=20)
        .map(|number| {
            let edited = edits.iter().find(|(line, _)| *line == number);
            edited.map_or(format!("line {number}\n"), |(_, text)| format!("{text}\n"))
        })
        .collect()
}

/// Two versions of a file whose first hunk git's myers diff and its histogram diff draw
/// differently: myers' starts a line later.
const ALGORITHM_BEFORE: &str = "return;\nline 1\nreturn;\n{\n{\nline 5\nline 6\n\nreturn;\nline 9\n\
     line 10\nline 11\n{\nline 13\nline 14\n\nreturn;\nline 17\n}\nline 19\nline 20\nline 21\n\n\n\n\
     line 25\n}\n\n}\n\n";
const ALGORITHM_AFTER: &str = "return;\nreturn;\n{\nline 1\nchanged 4\n{\n{\nline 5\nline 6\n\n\
     return;\nline 9\nline 10\nline 11\n{\nline 13\nline 14\n\nreturn;\n}\nline 19\nline 20\n\
     line 21\n\n\n\nline 25\n}\n\n}\n\n";

/// A pull request's base and head, fetched into a clone whose own config asks for every diff
/// setting `Repo::patch` must not follow.
struct Fixture {
    _server: Server,
    _cache_dir: TempDir,
    repo: Repo,
    base: Oid,
    head: Oid,
}

fn fixture() -> Fixture {
    let server = Server::new();
    let remote = server.repo("owner", "repo");
    remote
        .write("lines.txt", twenty_lines(&[]))
        .write("algorithm.txt", ALGORITHM_BEFORE)
        .write("blank.txt", "a\n\nb\nc\n")
        .write("old_name.rs", rust_source(""))
        .write("gone.txt", "gone\n")
        .write("same.txt", "same\n")
        .write("data.bin", b"\x00\x01\x02base")
        .gitlink("vendor/lib", SUBMODULE_BEFORE);
    let base = remote.commit("base");
    remote.publish("refs/heads/trunk");

    remote
        .switch("feature")
        .write(
            "lines.txt",
            twenty_lines(&[(5, "line five"), (15, "line fifteen")]),
        )
        .write("algorithm.txt", ALGORITHM_AFTER)
        .write("blank.txt", "a\n\nb\nC\n")
        .remove("old_name.rs")
        .write("src/new_name.rs", rust_source("pub fn added() {}\n"))
        .remove("gone.txt")
        .write("added.txt", "one\ntwo\n")
        .write("data.bin", b"\x00\x01\x02head")
        .gitlink("vendor/lib", SUBMODULE_AFTER);
    let head = remote.commit("head");
    remote.publish("refs/pull/1/head");

    let (cache_dir, cache) = server.cache();
    let repo = cache.open("owner", "repo", None).unwrap();
    repo.fetch_pull(&repo_cache::PullRequestRefs {
        number: 1,
        head: Some(head.clone()),
        base_branch: "trunk".into(),
        base: None,
    })
    .unwrap();
    // What a clone's config could ask for, none of which GitHub's patches do. An external diff
    // that fails would fail every patch.
    for (name, value) in [
        ("diff.algorithm", "histogram"),
        ("diff.context", "10"),
        ("diff.interHunkContext", "10"),
        ("diff.indentHeuristic", "false"),
        ("diff.noprefix", "true"),
        ("diff.mnemonicPrefix", "true"),
        ("diff.suppressBlankEmpty", "true"),
        ("diff.submodule", "log"),
        ("diff.renames", "copies"),
        ("diff.external", "false"),
        ("color.ui", "always"),
        ("color.diff", "always"),
    ] {
        git(repo.git_dir(), &["config", name, value]);
    }
    Fixture {
        _server: server,
        _cache_dir: cache_dir,
        repo,
        base,
        head,
    }
}

impl Fixture {
    fn patch(&self, path: &str, old_path: Option<&str>) -> String {
        let patch = self
            .repo
            .patch(
                &self.base,
                &self.head,
                path.as_bytes(),
                old_path.map(str::as_bytes),
            )
            .unwrap();
        String::from_utf8(patch).unwrap()
    }
}

/// The patch from its first hunk on, without git's header.
fn hunks(patch: &str) -> &str {
    patch.find("@@").map_or("", |start| &patch[start..])
}

fn headers(patch: &str) -> Vec<&str> {
    (patch.lines())
        .filter(|line| line.starts_with("@@"))
        .collect()
}

#[test]
fn a_patch_has_three_lines_of_context_and_a_hunk_for_each_change() {
    let f = fixture();
    let patch = f.patch("lines.txt", None);

    assert!(
        patch.contains("--- a/lines.txt\n+++ b/lines.txt\n"),
        "{patch}"
    );
    let context = |lines: std::ops::RangeInclusive<usize>| -> String {
        lines.map(|number| format!(" line {number}\n")).collect()
    };
    let expected = format!(
        "@@ -2,7 +2,7 @@ line 1\n{}-line 5\n+line five\n{}\
         @@ -12,7 +12,7 @@ line 11\n{}-line 15\n+line fifteen\n{}",
        context(2..=4),
        context(6..=8),
        context(12..=14),
        context(16..=18),
    );
    assert_eq!(hunks(&patch), expected);

    // Its hunks say which lines take comments, as GitHub's patch would.
    let hunks = Hunk::parse(&patch).unwrap();
    let commentable = CommentableLines::new(&Screen::full(f.head.as_str()), hunks);
    for line in [
        Line::right(2),
        Line::right(5),
        Line::left(5),
        Line::right(18),
    ] {
        assert_eq!(commentable.line(line), Some(Lines::one(line)), "{line:?}");
    }
    for line in [Line::right(1), Line::right(10), Line::right(19)] {
        assert_eq!(commentable.line(line), None, "{line:?}");
    }
}

#[test]
fn a_patch_is_myers_whatever_the_clone_says() {
    let f = fixture();
    let patch = f.patch("algorithm.txt", None);

    assert_eq!(
        headers(&patch),
        ["@@ -1,7 +1,9 @@", "@@ -15,7 +17,6 @@ line 13"]
    );
    // Histogram's, for the same two files, which the clone's config asks for.
    let histogram = git(
        f.repo.git_dir(),
        &[
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--diff-algorithm=histogram",
            "--inter-hunk-context=0",
            "-U3",
            f.base.as_str(),
            f.head.as_str(),
            "--",
            "algorithm.txt",
        ],
    );
    assert_eq!(
        headers(&histogram),
        ["@@ -1,6 +1,8 @@", "@@ -15,7 +17,6 @@ line 13"]
    );
}

#[test]
fn a_blank_unchanged_line_keeps_its_space() {
    let f = fixture();
    assert_eq!(
        hunks(&f.patch("blank.txt", None)),
        "@@ -1,4 +1,4 @@\n a\n \n b\n-c\n+C\n"
    );
}

#[test]
fn an_added_or_deleted_file_is_diffed_against_nothing() {
    let f = fixture();
    assert_eq!(
        hunks(&f.patch("added.txt", None)),
        "@@ -0,0 +1,2 @@\n+one\n+two\n"
    );
    assert_eq!(hunks(&f.patch("gone.txt", None)), "@@ -1 +0,0 @@\n-gone\n");
}

#[test]
fn a_renamed_file_is_diffed_against_its_old_path() {
    let f = fixture();
    let renamed = f.patch("src/new_name.rs", Some("old_name.rs"));
    assert_eq!(
        hunks(&renamed),
        "@@ -18,3 +18,4 @@ pub fn function_16() -> u32 { 16 }\n \
         pub fn function_17() -> u32 { 17 }\n \
         pub fn function_18() -> u32 { 18 }\n \
         pub fn function_19() -> u32 { 19 }\n\
         +pub fn added() {}\n"
    );
    // Without its old path, it's a file added; with an old path the base doesn't have, too.
    let added = "@@ -0,0 +1,21 @@\n";
    assert!(hunks(&f.patch("src/new_name.rs", None)).starts_with(added));
    assert!(hunks(&f.patch("src/new_name.rs", Some("not_there.rs"))).starts_with(added));
}

#[test]
fn binary_files_and_submodules_have_no_lines_but_their_own() {
    let f = fixture();
    let binary = f.patch("data.bin", None);
    assert!(binary.contains("Binary files"), "{binary}");
    assert_eq!(headers(&binary), Vec::<&str>::new());
    // A submodule's commits, as GitHub shows them, without fetching either.
    assert_eq!(
        hunks(&f.patch("vendor/lib", None)),
        format!(
            "@@ -1 +1 @@\n-Subproject commit {SUBMODULE_BEFORE}\n+Subproject commit {SUBMODULE_AFTER}\n"
        )
    );
}

#[test]
fn an_unchanged_or_missing_file_has_an_empty_patch() {
    let f = fixture();
    assert_eq!(f.patch("same.txt", None), "");
    assert_eq!(f.patch("nowhere.txt", None), "");
    assert!(f.repo.patch(&f.base, &f.head, b"", None).is_err());
}

#[test]
fn a_patch_downloads_only_its_files_two_versions() {
    let f = fixture();
    let before = packs(f.repo.git_dir());
    f.patch("lines.txt", None);
    // One download, for the two blobs.
    assert_eq!(packs(f.repo.git_dir()), before + 1);
    f.patch("lines.txt", None);
    assert_eq!(packs(f.repo.git_dir()), before + 1);
}
