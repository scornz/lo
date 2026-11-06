//! Sorting files into the groups of review order: source, test, config and generated.

use super::{ChangedFile, file_name, is_lockfile};

/// Which group of review order a file belongs to.
///
/// A file that fits several groups takes the first of generated, config, test and source. So a
/// generated test is generated, and a test under `.github/` is config.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    /// Code, and anything that isn't a test, config or generated, such as docs and images.
    Source,
    /// A test, recognized by its name:
    ///
    /// - JavaScript and TypeScript: `foo.test.ts` and `foo.spec.ts`, and the same for `.tsx`,
    ///   `.js`, `.jsx`, `.mjs` and `.cjs`.
    /// - Go: `foo_test.go`.
    /// - Python: `test_foo.py` and `foo_test.py`.
    /// - Rust: any `.rs` file in a `tests` directory, such as `tests/foo.rs` or
    ///   `tests/common/mod.rs`.
    ///
    /// Other files in test directories, such as `tests/conftest.py` or `__tests__/foo.ts`, keep
    /// the group their name gives them.
    Test,
    /// A JSON, YAML or TOML file, a lockfile that isn't generated, a dotfile such as
    /// `.gitignore`, or anything in a dot-directory such as `.github/` or `.husky/`.
    Config,
    /// A generated file ([`generated()`](super::generated())).
    Generated,
}

/// The group of review order the file belongs to.
pub fn kind(file: &ChangedFile) -> FileKind {
    if super::generated(file).is_some() {
        FileKind::Generated
    } else if is_config(&file.path) {
        FileKind::Config
    } else if test(&file.path).is_some() {
        FileKind::Test
    } else {
        FileKind::Source
    }
}

/// Extensions of config files, matched ignoring case.
const CONFIG_EXTENSIONS: &[&str] = &["json", "jsonc", "json5", "yaml", "yml", "toml"];

/// JavaScript's and TypeScript's extensions. A test with one matches a source file with any of
/// them.
pub(super) const SCRIPT_EXTENSIONS: &[&str] = &["ts", "tsx", "js", "jsx", "mjs", "cjs"];

fn is_config(path: &str) -> bool {
    path.split('/').any(|component| component.starts_with('.'))
        || is_lockfile(path)
        || Name::of(file_name(path)).is_some_and(|name| {
            CONFIG_EXTENSIONS
                .iter()
                .any(|extension| name.extension.eq_ignore_ascii_case(extension))
        })
}

/// A test file ([`FileKind::Test`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Test<'a> {
    /// The name of the source file it tests: `foo.ts` for `foo.test.ts`, `foo.rs` for
    /// `tests/foo.rs`. `None` for a Rust file deeper in `tests`, such as `tests/common/mod.rs`.
    pub subject: Option<Name<'a>>,
}

/// A code file's name, split for matching tests to sources.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Name<'a> {
    /// The name without its extension: `foo` for `foo.ts`, `foo.d` for `foo.d.ts`.
    pub stem: &'a str,
    /// The extension, without the dot.
    pub extension: &'a str,
}

impl<'a> Name<'a> {
    /// Splits a file name at its last dot. `None` when it has no extension, or nothing before it.
    pub fn of(file_name: &'a str) -> Option<Self> {
        let (stem, extension) = file_name.rsplit_once('.')?;
        (!stem.is_empty() && !extension.is_empty()).then_some(Self { stem, extension })
    }
}

/// The file at `path` as a test, or `None` when it isn't one.
pub(super) fn test<'a>(path: &'a str) -> Option<Test<'a>> {
    let (directory, file_name) = match path.rsplit_once('/') {
        Some((directory, file_name)) => (directory, file_name),
        None => ("", path),
    };
    let name = Name::of(file_name)?;
    // The test's stem with its marker taken off, which must leave something.
    let tests = |stem: Option<&'a str>| {
        let stem = stem.filter(|stem| !stem.is_empty())?;
        Some(Test {
            subject: Some(Name {
                stem,
                extension: name.extension,
            }),
        })
    };
    match name.extension {
        extension if SCRIPT_EXTENSIONS.contains(&extension) => tests(
            name.stem
                .strip_suffix(".test")
                .or_else(|| name.stem.strip_suffix(".spec")),
        ),
        "go" => tests(name.stem.strip_suffix("_test")),
        "py" => tests(
            name.stem
                .strip_prefix("test_")
                .or_else(|| name.stem.strip_suffix("_test")),
        ),
        "rs" => {
            let mut directories = directory.rsplit('/');
            if directories.next() == Some("tests") {
                Some(Test {
                    subject: Some(name),
                })
            } else if directories.any(|directory| directory == "tests") {
                Some(Test { subject: None })
            } else {
                None
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind_of(path: &str) -> FileKind {
        kind(&ChangedFile {
            path: path.to_string(),
            ..Default::default()
        })
    }

    fn subject(path: &str) -> Option<(&str, &str)> {
        test(path)
            .expect("a test")
            .subject
            .map(|name| (name.stem, name.extension))
    }

    #[test]
    fn code_docs_and_assets_are_source() {
        for path in [
            "src/main.rs",
            "web/src/app.tsx",
            "pkg/server/server.go",
            "srcs/api/models.py",
            "README.md",
            "docs/guide.md",
            "Makefile",
            "Dockerfile",
            "assets/logo.png",
            "src/foo.d.ts",
        ] {
            assert_eq!(kind_of(path), FileKind::Source, "{path}");
        }
    }

    #[test]
    fn javascript_and_typescript_tests_are_named_test_or_spec() {
        for extension in ["ts", "tsx", "js", "jsx", "mjs", "cjs"] {
            let test_path = format!("src/button.test.{extension}");
            let spec_path = format!("src/button.spec.{extension}");

            assert_eq!(kind_of(&test_path), FileKind::Test, "{test_path}");
            assert_eq!(kind_of(&spec_path), FileKind::Test, "{spec_path}");
            assert_eq!(subject(&test_path), Some(("button", extension)));
            assert_eq!(subject(&spec_path), Some(("button", extension)));
        }
    }

    #[test]
    fn a_test_of_a_dotted_name_keeps_the_dots() {
        assert_eq!(
            subject("src/user.service.spec.ts"),
            Some(("user.service", "ts"))
        );
    }

    #[test]
    fn go_tests_end_in_underscore_test() {
        assert_eq!(kind_of("pkg/server/server_test.go"), FileKind::Test);
        assert_eq!(subject("pkg/server/server_test.go"), Some(("server", "go")));
    }

    #[test]
    fn python_tests_start_with_test_or_end_with_test() {
        assert_eq!(kind_of("tests/test_models.py"), FileKind::Test);
        assert_eq!(kind_of("srcs/api/models_test.py"), FileKind::Test);
        assert_eq!(subject("tests/test_models.py"), Some(("models", "py")));
        assert_eq!(subject("srcs/api/models_test.py"), Some(("models", "py")));
    }

    #[test]
    fn rust_tests_are_files_in_a_tests_directory() {
        assert_eq!(kind_of("crates/review/tests/order.rs"), FileKind::Test);
        assert_eq!(kind_of("tests/cli.rs"), FileKind::Test);
        assert_eq!(
            subject("crates/review/tests/order.rs"),
            Some(("order", "rs"))
        );
    }

    #[test]
    fn rust_files_deeper_in_a_tests_directory_test_nothing_in_particular() {
        assert_eq!(kind_of("tests/common/mod.rs"), FileKind::Test);
        assert_eq!(subject("tests/common/mod.rs"), None);
    }

    #[test]
    fn names_that_only_look_like_tests_are_source() {
        for path in [
            "src/test.ts",
            "src/testing.ts",
            "src/button.tests.ts",
            "src/latest.go",
            "src/_test.go",
            "src/contest.py",
            "src/test_.py",
            "src/_test.py",
            "src/tests.rs",
            "src/attest.rs",
            "tests/helpers.py",
            "test/button.ts",
            "src/button.test.rb",
        ] {
            assert_eq!(kind_of(path), FileKind::Source, "{path}");
        }
    }

    #[test]
    fn json_yaml_and_toml_are_config() {
        for path in [
            "package.json",
            "tsconfig.json",
            ".vscode/settings.jsonc",
            "renovate.json5",
            "docker-compose.yaml",
            "config/app.yml",
            "Cargo.toml",
            "crates/review/Cargo.toml",
            "deploy/VALUES.YAML",
        ] {
            assert_eq!(kind_of(path), FileKind::Config, "{path}");
        }
    }

    #[test]
    fn dotfiles_are_config() {
        for path in [".gitignore", ".env.example", "apps/web/.eslintrc"] {
            assert_eq!(kind_of(path), FileKind::Config, "{path}");
        }
    }

    #[test]
    fn everything_in_a_dot_directory_is_config() {
        for path in [
            ".github/CODEOWNERS",
            ".github/workflows/ci.yml",
            ".github/pull_request_template.md",
            ".husky/pre-commit",
            ".storybook/main.ts",
        ] {
            assert_eq!(kind_of(path), FileKind::Config, "{path}");
        }
    }

    #[test]
    fn a_test_in_a_dot_directory_is_config() {
        assert_eq!(kind_of(".github/scripts/label.test.js"), FileKind::Config);
    }

    #[test]
    fn a_lockfile_that_is_not_generated_is_config() {
        let file = ChangedFile {
            path: "Cargo.lock".to_string(),
            linguist_generated: Some(false),
            ..Default::default()
        };

        assert_eq!(kind(&file), FileKind::Config);
    }

    #[test]
    fn generated_beats_every_other_group() {
        assert_eq!(kind_of("pnpm-lock.yaml"), FileKind::Generated);
        assert_eq!(kind_of("Cargo.lock"), FileKind::Generated);
        for path in [
            "src/schema.ts",
            "src/schema.test.ts",
            "openapi.json",
            ".github/labels.yml",
        ] {
            let file = ChangedFile {
                path: path.to_string(),
                linguist_generated: Some(true),
                ..Default::default()
            };

            assert_eq!(kind(&file), FileKind::Generated, "{path}");
        }
    }
}
