//! Which files a file's imports name, so definitions there rank above the rest.
//!
//! A small heuristic per language, reading only the importing file's text: Rust `use` and `mod`,
//! JavaScript and TypeScript `import`, `export … from` and `require`, Python `import` and `from`,
//! and Go `import`. It knows nothing of the build: no `Cargo.toml`, `tsconfig.json` paths,
//! `PYTHONPATH` or `go.mod`, so it guesses where those would decide.

mod go;
mod javascript;
mod lexer;
mod python;
mod rust;

/// Where an import says a name may be defined.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    /// The file at this path.
    File(String),
    /// Any file whose path ends with this one: Python's absolute imports, which are relative to a
    /// root the heuristic doesn't know, and Rust's other crates.
    Suffix(String),
    /// A JavaScript or TypeScript module: this path with any script extension, or the `index`
    /// file in the directory at this path. With `exact` false, any path ending with it.
    Script { path: String, exact: bool },
    /// A Go package: any file directly in a directory its import path ends with.
    Package(String),
}

/// An import: the names it brings in, and where they may be defined.
#[derive(Debug, PartialEq, Eq)]
struct Import {
    names: Names,
    targets: Vec<Target>,
}

/// The names an import brings in.
#[derive(Debug, PartialEq, Eq)]
enum Names {
    /// Any name: a glob, or a module used through a qualifier (`ns.parse`, `os.path.join`).
    All,
    /// Go's exported names, the capitalized ones.
    Exported,
    /// These names, under their own names and any aliases.
    These(Vec<String>),
}

/// The places `from_text`'s imports say `name` may be defined, by the language its path's
/// extension names. None for other languages.
pub(crate) fn targets(from_path: &str, from_text: &str, name: &str) -> Vec<Target> {
    let extension = from_path
        .rsplit_once('.')
        .map_or("", |(_, extension)| extension);
    let imports = match extension {
        "rs" => rust::imports(from_path, from_text),
        "ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs" => {
            javascript::imports(from_path, from_text)
        }
        "py" | "pyi" => python::imports(from_path, from_text),
        "go" => go::imports(from_text),
        _ => Vec::new(),
    };
    imports
        .into_iter()
        .filter(|import| import.names.include(name))
        .flat_map(|import| import.targets)
        .collect()
}

impl Names {
    fn include(&self, name: &str) -> bool {
        match self {
            Self::All => true,
            Self::Exported => name.starts_with(|c: char| c.is_uppercase()),
            Self::These(names) => names.iter().any(|n| n == name),
        }
    }
}

/// Extensions of JavaScript and TypeScript files, longest first.
const SCRIPT_EXTENSIONS: &[&str] = &[
    ".d.ts", ".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs",
];

impl Target {
    pub(crate) fn matches(&self, path: &str) -> bool {
        match self {
            Self::File(file) => path == file,
            Self::Suffix(suffix) => ends_with_path(path, suffix),
            Self::Script {
                path: module,
                exact,
            } => {
                let Some(stem) = strip_script_extension(path) else {
                    return false;
                };
                let directory = stem.strip_suffix("/index");
                let is = |candidate: &str| {
                    if *exact {
                        candidate == module
                    } else {
                        ends_with_path(candidate, module)
                    }
                };
                is(stem) || directory.is_some_and(is)
            }
            Self::Package(import) => {
                let directory = directory(path);
                !directory.is_empty()
                    && (import == directory
                        || import
                            .strip_suffix(directory)
                            .is_some_and(|rest| rest.ends_with('/')))
            }
        }
    }
}

/// `path` without its JavaScript or TypeScript extension, if it has one.
fn strip_script_extension(path: &str) -> Option<&str> {
    SCRIPT_EXTENSIONS
        .iter()
        .find_map(|extension| path.strip_suffix(extension))
}

/// Whether `path` is `suffix` or ends with `/` and `suffix`.
fn ends_with_path(path: &str, suffix: &str) -> bool {
    path.strip_suffix(suffix)
        .is_some_and(|rest| rest.is_empty() || rest.ends_with('/'))
}

/// The directory part of a path, `""` at the root.
pub(crate) fn directory(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(directory, _)| directory)
}

/// `relative` resolved against `directory`, following `.` and `..`. `None` if it climbs above
/// the root.
fn join(directory: &str, relative: &str) -> Option<String> {
    let mut parts: Vec<&str> = directory.split('/').filter(|p| !p.is_empty()).collect();
    for part in relative.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

/// `directory/name`, or `name` at the root.
fn child(directory: &str, name: &str) -> String {
    if directory.is_empty() {
        name.to_string()
    } else {
        format!("{directory}/{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_suffix_matches_whole_path_parts() {
        let target = Target::Suffix("pkg/util.py".into());
        assert!(target.matches("pkg/util.py"));
        assert!(target.matches("src/pkg/util.py"));
        assert!(!target.matches("src/mypkg/util.py"));
    }

    #[test]
    fn a_script_matches_any_extension_or_an_index() {
        let target = Target::Script {
            path: "src/util".into(),
            exact: true,
        };
        for path in [
            "src/util.ts",
            "src/util.tsx",
            "src/util.d.ts",
            "src/util.js",
            "src/util/index.ts",
            "src/util/index.jsx",
        ] {
            assert!(target.matches(path), "{path}");
        }
        for path in ["src/util.rs", "src/util/other.ts", "lib/src/util.ts"] {
            assert!(!target.matches(path), "{path}");
        }

        let target = Target::Script {
            path: "components/button".into(),
            exact: false,
        };
        assert!(target.matches("web/src/components/button.tsx"));
        assert!(!target.matches("web/src/mycomponents/button.tsx"));
    }

    #[test]
    fn a_package_matches_files_in_a_directory_its_import_path_ends_with() {
        let target = Target::Package("github.com/acme/app/internal/store".into());
        assert!(target.matches("internal/store/store.go"));
        assert!(target.matches("store/store.go"));
        assert!(!target.matches("internal/store/sub/sub.go"));
        assert!(!target.matches("main.go"));
        assert!(!target.matches("internal/mystore/store.go"));
    }

    #[test]
    fn joining_follows_dots() {
        assert_eq!(join("src/a", "../b/c").as_deref(), Some("src/b/c"));
        assert_eq!(join("src", "./x").as_deref(), Some("src/x"));
        assert_eq!(join("", "x").as_deref(), Some("x"));
        assert_eq!(join("src", "../../x"), None);
    }

    #[test]
    fn other_languages_have_no_imports() {
        assert!(targets("README.md", "import x from './x'", "x").is_empty());
    }
}
