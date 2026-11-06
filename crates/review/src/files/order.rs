//! The order the file list shows a pull request's files in.

use std::cmp::Reverse;
use std::collections::HashMap;

use super::kind::{Name, SCRIPT_EXTENSIONS, test};
use super::{ChangedFile, FileKind, file_name, kind};

/// The files in review order, as indices into `files`.
///
/// Source files come first, each followed by its tests, then tests whose source file the pull
/// request doesn't change, then config, then generated files ([`FileKind`] says which is which).
/// Within each group, files keep [`path_order`], and so do several tests of one source file.
///
/// A test's name says which source file it tests: `foo.test.ts` tests `foo.ts`, `test_foo.py`
/// tests `foo.py`. That source file can be in any directory, and a JavaScript or TypeScript test
/// can test a source file with any of their extensions: `foo.test.ts` tests `foo.tsx` when the
/// pull request has no `foo.ts`. When several source files have the name, the test follows the
/// nearest: the one sharing the most leading directories with it, then the one with its own
/// extension, then the first in path order.
pub fn review_order(files: &[ChangedFile]) -> Vec<usize> {
    let mut sources = Vec::new();
    let mut tests = Vec::new();
    let mut config = Vec::new();
    let mut generated = Vec::new();
    for index in path_order(files) {
        match kind(&files[index]) {
            FileKind::Source => sources.push(index),
            FileKind::Test => tests.push(index),
            FileKind::Config => config.push(index),
            FileKind::Generated => generated.push(index),
        }
    }

    // The source files with each stem, in path order.
    let mut sources_by_stem: HashMap<&str, Vec<usize>> = HashMap::new();
    for &index in &sources {
        if let Some(name) = Name::of(file_name(&files[index].path)) {
            sources_by_stem.entry(name.stem).or_default().push(index);
        }
    }

    // The tests that follow each source file, by the source file's index.
    let mut tests_of: Vec<Vec<usize>> = vec![Vec::new(); files.len()];
    let mut unmatched_tests = Vec::new();
    for index in tests {
        match source_of(&files[index].path, files, &sources_by_stem) {
            Some(source) => tests_of[source].push(index),
            None => unmatched_tests.push(index),
        }
    }

    let mut order = Vec::with_capacity(files.len());
    for source in sources {
        order.push(source);
        order.append(&mut tests_of[source]);
    }
    order.extend(unmatched_tests);
    order.extend(config);
    order.extend(generated);
    order
}

/// The index of the source file the test at `path` follows, if the pull request changes it.
fn source_of(
    path: &str,
    files: &[ChangedFile],
    sources_by_stem: &HashMap<&str, Vec<usize>>,
) -> Option<usize> {
    let subject = test(path)?.subject?;
    sources_by_stem
        .get(subject.stem)?
        .iter()
        .filter_map(|&index| {
            let source = files[index].path.as_str();
            let extension = Name::of(file_name(source))?.extension;
            let nearness = (
                Reverse(shared_directories(path, source)),
                Reverse(extension == subject.extension),
            );
            same_language(subject.extension, extension).then_some((index, nearness))
        })
        // `min_by_key` keeps the first of equals, so ties go to path order.
        .min_by_key(|&(_, nearness)| nearness)
        .map(|(index, _)| index)
}

/// Whether a test with the extension `test` can test a source file with the extension `source`.
fn same_language(test: &str, source: &str) -> bool {
    test == source || (SCRIPT_EXTENSIONS.contains(&test) && SCRIPT_EXTENSIONS.contains(&source))
}

/// How many leading directories two paths share.
fn shared_directories(a: &str, b: &str) -> usize {
    directories(a)
        .zip(directories(b))
        .take_while(|(a, b)| a == b)
        .count()
}

/// A path's directories, outermost first.
fn directories(path: &str) -> impl Iterator<Item = &str> {
    path.rsplit_once('/')
        .map(|(directory, _)| directory.split('/'))
        .into_iter()
        .flatten()
}

/// The files in path order, as indices into `files`.
///
/// Paths compare byte by byte, which is git's order and the order of GitHub's changed-file list:
/// uppercase before lowercase, and `a.rs` before `a/b.rs`. Every directory's files stay together.
pub fn path_order(files: &[ChangedFile]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..files.len()).collect();
    // Stable, so two entries with the same path keep the caller's order.
    order.sort_by(|&a, &b| files[a].path.cmp(&files[b].path));
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(paths: &[&str]) -> Vec<ChangedFile> {
        paths
            .iter()
            .map(|path| ChangedFile {
                path: path.to_string(),
                ..Default::default()
            })
            .collect()
    }

    fn paths(files: &[ChangedFile], order: Vec<usize>) -> Vec<&str> {
        order.into_iter().map(|i| files[i].path.as_str()).collect()
    }

    fn review_paths(files: &[ChangedFile]) -> Vec<&str> {
        paths(files, review_order(files))
    }

    #[test]
    fn path_order_sorts_by_path() {
        let files = files(&["src/main.rs", "Cargo.toml", "README.md", "build.rs"]);

        assert_eq!(
            paths(&files, path_order(&files)),
            ["Cargo.toml", "README.md", "build.rs", "src/main.rs"]
        );
    }

    #[test]
    fn path_order_compares_bytes_like_git() {
        let files = files(&["src/a/b.rs", "src/a.rs", "src/a-b.rs", "src/B.rs"]);

        assert_eq!(
            paths(&files, path_order(&files)),
            ["src/B.rs", "src/a-b.rs", "src/a.rs", "src/a/b.rs"]
        );
    }

    #[test]
    fn path_order_of_nothing_is_empty() {
        assert!(path_order(&[]).is_empty());
    }

    #[test]
    fn review_order_of_nothing_is_empty() {
        assert!(review_order(&[]).is_empty());
    }

    #[test]
    fn each_source_file_is_followed_by_its_test() {
        let files = files(&[
            "src/button.test.tsx",
            "src/api.test.ts",
            "src/button.tsx",
            "src/api.ts",
        ]);

        assert_eq!(
            review_paths(&files),
            [
                "src/api.ts",
                "src/api.test.ts",
                "src/button.tsx",
                "src/button.test.tsx",
            ]
        );
    }

    #[test]
    fn spec_files_follow_their_source_too() {
        let files = files(&["lib/parse.spec.mjs", "lib/format.cjs", "lib/parse.mjs"]);

        assert_eq!(
            review_paths(&files),
            ["lib/format.cjs", "lib/parse.mjs", "lib/parse.spec.mjs"]
        );
    }

    #[test]
    fn go_tests_follow_their_source() {
        let files = files(&[
            "pkg/server/server_test.go",
            "pkg/server/routes.go",
            "pkg/server/server.go",
        ]);

        assert_eq!(
            review_paths(&files),
            [
                "pkg/server/routes.go",
                "pkg/server/server.go",
                "pkg/server/server_test.go",
            ]
        );
    }

    #[test]
    fn python_tests_follow_their_source_from_another_directory() {
        let files = files(&[
            "tests/test_models.py",
            "srcs/api/views_test.py",
            "srcs/api/models.py",
            "srcs/api/views.py",
        ]);

        assert_eq!(
            review_paths(&files),
            [
                "srcs/api/models.py",
                "tests/test_models.py",
                "srcs/api/views.py",
                "srcs/api/views_test.py",
            ]
        );
    }

    #[test]
    fn rust_integration_tests_follow_their_source() {
        let files = files(&[
            "crates/review/tests/order.rs",
            "crates/review/src/order.rs",
            "crates/review/src/lib.rs",
        ]);

        assert_eq!(
            review_paths(&files),
            [
                "crates/review/src/lib.rs",
                "crates/review/src/order.rs",
                "crates/review/tests/order.rs",
            ]
        );
    }

    #[test]
    fn several_tests_of_one_source_follow_it_in_path_order() {
        let files = files(&["src/user.test.ts", "src/user.ts", "src/user.spec.ts"]);

        assert_eq!(
            review_paths(&files),
            ["src/user.ts", "src/user.spec.ts", "src/user.test.ts"]
        );
    }

    #[test]
    fn a_typescript_test_follows_a_javascript_source_of_the_same_name() {
        let files = files(&["src/button.test.ts", "src/button.jsx", "src/card.ts"]);

        assert_eq!(
            review_paths(&files),
            ["src/button.jsx", "src/button.test.ts", "src/card.ts"]
        );
    }

    #[test]
    fn a_test_prefers_the_source_with_its_own_extension() {
        let files = files(&["src/foo.ts", "src/foo.test.js", "src/foo.js"]);

        assert_eq!(
            review_paths(&files),
            ["src/foo.js", "src/foo.test.js", "src/foo.ts"]
        );
    }

    #[test]
    fn tests_do_not_match_sources_in_other_languages() {
        let files = files(&["src/foo.py", "src/foo_test.go", "src/bar.rs"]);

        assert_eq!(
            review_paths(&files),
            ["src/bar.rs", "src/foo.py", "src/foo_test.go"]
        );
    }

    #[test]
    fn a_test_follows_the_nearest_source_of_its_name() {
        let files = files(&[
            "web/src/index.ts",
            "api/src/index.ts",
            "web/src/index.test.ts",
            "api/test/index.test.ts",
        ]);

        assert_eq!(
            review_paths(&files),
            [
                "api/src/index.ts",
                "api/test/index.test.ts",
                "web/src/index.ts",
                "web/src/index.test.ts",
            ]
        );
    }

    #[test]
    fn a_test_equally_near_several_sources_follows_the_first_in_path_order() {
        let files = files(&["b/util.go", "a/util.go", "c/util_test.go"]);

        assert_eq!(
            review_paths(&files),
            ["a/util.go", "c/util_test.go", "b/util.go"]
        );
    }

    #[test]
    fn unmatched_tests_follow_the_sources_and_precede_config() {
        let files = files(&[
            "package.json",
            "src/legacy.test.ts",
            "tests/common/mod.rs",
            "src/app.ts",
            "src/app.test.ts",
            "src/zoo.ts",
        ]);

        assert_eq!(
            review_paths(&files),
            [
                "src/app.ts",
                "src/app.test.ts",
                "src/zoo.ts",
                "src/legacy.test.ts",
                "tests/common/mod.rs",
                "package.json",
            ]
        );
    }

    #[test]
    fn a_test_of_a_generated_file_is_unmatched() {
        let mut files = files(&["src/schema.ts", "src/schema.test.ts", "src/main.ts"]);
        files[0].linguist_generated = Some(true);

        assert_eq!(
            review_paths(&files),
            ["src/main.ts", "src/schema.test.ts", "src/schema.ts"]
        );
    }

    #[test]
    fn groups_go_source_then_tests_then_config_then_generated() {
        let files = files(&[
            "yarn.lock",
            ".github/workflows/ci.yml",
            "Cargo.toml",
            "tests/cli.rs",
            "src/main.rs",
            "README.md",
        ]);

        assert_eq!(
            review_paths(&files),
            [
                "README.md",
                "src/main.rs",
                "tests/cli.rs",
                ".github/workflows/ci.yml",
                "Cargo.toml",
                "yarn.lock",
            ]
        );
    }

    #[test]
    fn each_group_keeps_path_order() {
        let files = files(&[
            "z.lock.json",
            "pnpm-lock.yaml",
            "Cargo.lock",
            "tsconfig.json",
            ".prettierrc",
            "b.ts",
            "a.ts",
        ]);

        assert_eq!(
            review_paths(&files),
            [
                "a.ts",
                "b.ts",
                ".prettierrc",
                "tsconfig.json",
                "z.lock.json",
                "Cargo.lock",
                "pnpm-lock.yaml",
            ]
        );
    }

    #[test]
    fn a_renamed_file_is_placed_by_its_new_path() {
        let files = vec![
            ChangedFile {
                path: "src/session.ts".to_string(),
                previous_path: Some("src/auth.ts".to_string()),
                ..Default::default()
            },
            ChangedFile {
                path: "src/session.test.ts".to_string(),
                previous_path: Some("src/auth.test.ts".to_string()),
                ..Default::default()
            },
            ChangedFile {
                path: "src/router.ts".to_string(),
                ..Default::default()
            },
        ];

        assert_eq!(
            review_paths(&files),
            ["src/router.ts", "src/session.ts", "src/session.test.ts"]
        );
    }
}
