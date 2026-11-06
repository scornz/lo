//! Rust: `use` declarations, resolved through `crate`, `self`, `super`, the file's own `mod`
//! declarations, or another crate in the repository.

use super::{
    Import, Names, Target, child, directory,
    lexer::{Syntax, Token, tokens},
};

const RUST: Syntax = Syntax {
    line_comment: "//",
    block_comments: true,
    quotes: &['"'],
    multiline_quotes: &['"'],
    triple_quotes: false,
    lifetimes: true,
};

/// Crates outside any repository.
const STANDARD_CRATES: &[&str] = &["std", "core", "alloc"];

/// One name a `use` tree brings in: `use a::b::{C, d::E as F, g::*}` has three.
#[derive(Debug, PartialEq, Eq)]
struct Leaf {
    /// The module path before the name: `a::b`, `a::b::d`, `a::b::g`.
    path: Vec<String>,
    /// The name and its alias, or `None` for a glob.
    name: Option<(String, Option<String>)>,
}

pub(super) fn imports(from_path: &str, text: &str) -> Vec<Import> {
    let tokens = tokens(text, &RUST);
    let mut leaves = Vec::new();
    let mut children = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        match (tokens[i], tokens.get(i + 1), tokens.get(i + 2)) {
            (Token::Ident("use"), ..) => {
                i = use_tree(&tokens, i + 1, &mut Vec::new(), &mut leaves);
            }
            (Token::Ident("mod"), Some(Token::Ident(name)), Some(Token::Punct(';'))) => {
                children.push(*name);
                i += 3;
            }
            _ => i += 1,
        }
    }

    let module = Module::of(from_path, children);
    let mut imports = Vec::new();
    for Leaf { mut path, name } in leaves {
        let Some((name, alias)) = name else {
            imports.push(Import {
                names: Names::All,
                targets: module.files(&path),
            });
            continue;
        };
        imports.push(Import {
            names: Names::These([name.clone()].into_iter().chain(alias).collect()),
            targets: module.files(&path),
        });
        // The name may be a module, used through it: `use crate::stacks;`, then `stacks::Stack`.
        path.push(name);
        imports.push(Import {
            names: Names::All,
            targets: module.files(&path),
        });
    }
    imports
}

/// Reads the `use` tree starting at `tokens[i]` into `leaves`, under `prefix`. Returns the index
/// after it.
fn use_tree(
    tokens: &[Token],
    mut i: usize,
    prefix: &mut Vec<String>,
    leaves: &mut Vec<Leaf>,
) -> usize {
    let depth = prefix.len();
    loop {
        match (tokens.get(i), tokens.get(i + 1), tokens.get(i + 2)) {
            (Some(Token::Punct(':')), Some(Token::Punct(':')), _) => i += 2,
            (Some(Token::Ident(segment)), Some(Token::Punct(':')), Some(Token::Punct(':'))) => {
                prefix.push(segment.to_string());
                i += 3;
            }
            (Some(Token::Ident(name)), ..) => {
                let alias = match (tokens.get(i + 1), tokens.get(i + 2)) {
                    (Some(Token::Ident("as")), Some(Token::Ident(alias))) => {
                        i += 2;
                        Some(alias.to_string())
                    }
                    _ => None,
                };
                i += 1;
                // `self` in a list names the module itself, which the file defining it holds.
                if *name != "self" {
                    leaves.push(Leaf {
                        path: prefix.clone(),
                        name: Some((name.to_string(), alias)),
                    });
                }
                break;
            }
            (Some(Token::Punct('*')), ..) => {
                leaves.push(Leaf {
                    path: prefix.clone(),
                    name: None,
                });
                i += 1;
                break;
            }
            (Some(Token::Punct('{')), ..) => {
                i += 1;
                while let Some(token) = tokens.get(i) {
                    match token {
                        Token::Punct('}') => {
                            i += 1;
                            break;
                        }
                        Token::Punct(',') => i += 1,
                        _ => {
                            let next = use_tree(tokens, i, prefix, leaves);
                            // Skip what a malformed tree couldn't read, so the loop ends.
                            i = next.max(i + 1);
                        }
                    }
                }
                break;
            }
            _ => break,
        }
    }
    prefix.truncate(depth);
    i
}

/// The module a file holds, for resolving paths from it.
struct Module<'a> {
    path: &'a str,
    /// Where its child modules' files are: `src/a/b` for `src/a/b.rs` or `src/a/b/mod.rs`.
    directory: String,
    /// The crate's `src` directory, or the file's directory if it isn't under one.
    crate_root: String,
    /// The modules it declares with `mod name;`.
    children: Vec<&'a str>,
}

impl<'a> Module<'a> {
    fn of(path: &'a str, children: Vec<&'a str>) -> Self {
        let parent = directory(path);
        let file_name = path.rsplit('/').next().unwrap_or(path);
        let directory = match file_name {
            "mod.rs" | "lib.rs" | "main.rs" => parent.to_string(),
            _ => child(parent, file_name.trim_end_matches(".rs")),
        };
        let crate_root = match path.rfind("/src/") {
            Some(index) => path[..index + 4].to_string(),
            None if path.starts_with("src/") => "src".to_string(),
            None => parent.to_string(),
        };
        Self {
            path,
            directory,
            crate_root,
            children,
        }
    }

    /// The files that may hold the module at `path`, resolved from this one.
    fn files(&self, path: &[String]) -> Vec<Target> {
        let Some(first) = path.first() else {
            return Vec::new();
        };
        let (base, rest) = match first.as_str() {
            "crate" => (self.crate_root.clone(), &path[1..]),
            "self" => (self.directory.clone(), &path[1..]),
            "super" => {
                let supers = path.iter().take_while(|part| *part == "super").count();
                let mut base = self.directory.as_str();
                for _ in 0..supers {
                    base = directory(base);
                }
                (base.to_string(), &path[supers..])
            }
            child if self.children.contains(&child) => (self.directory.clone(), path),
            other if STANDARD_CRATES.contains(&other) => return Vec::new(),
            other => return other_crate(other, &path[1..]),
        };

        if rest.is_empty() {
            if base == self.crate_root {
                return ["lib.rs", "main.rs"]
                    .iter()
                    .map(|root| Target::File(child(&base, root)))
                    .collect();
            }
            if base == self.directory {
                return vec![Target::File(self.path.to_string())];
            }
            return vec![
                Target::File(format!("{base}.rs")),
                Target::File(child(&base, "mod.rs")),
            ];
        }
        let module = child(&base, &rest.join("/"));
        vec![
            Target::File(format!("{module}.rs")),
            Target::File(child(&module, "mod.rs")),
        ]
    }
}

/// Another crate in the repository, in a directory named like it: `my_crate::a::b` is in
/// `…/my_crate/src/a/b.rs`, or `…/my-crate/src/…`.
fn other_crate(name: &str, rest: &[String]) -> Vec<Target> {
    let mut directories = vec![name.to_string()];
    if name.contains('_') {
        directories.push(name.replace('_', "-"));
    }
    directories
        .iter()
        .flat_map(|directory| {
            let source = format!("{directory}/src");
            if rest.is_empty() {
                vec![Target::Suffix(format!("{source}/lib.rs"))]
            } else {
                let module = format!("{source}/{}", rest.join("/"));
                vec![
                    Target::Suffix(format!("{module}.rs")),
                    Target::Suffix(format!("{module}/mod.rs")),
                ]
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(paths: &[&str]) -> Vec<Target> {
        paths
            .iter()
            .map(|path| Target::File(path.to_string()))
            .collect()
    }

    fn names(names: &[&str]) -> Names {
        Names::These(names.iter().map(|name| name.to_string()).collect())
    }

    /// The imports of named items, leaving out those of a module by the item's name.
    fn named(imports: Vec<Import>) -> Vec<Import> {
        imports
            .into_iter()
            .filter(|import| import.names != Names::All)
            .collect()
    }

    #[test]
    fn a_crate_path() {
        assert_eq!(
            imports(
                "crates/app/src/main.rs",
                "use crate::stacks::detect::Stack;"
            ),
            [
                Import {
                    names: names(&["Stack"]),
                    targets: files(&[
                        "crates/app/src/stacks/detect.rs",
                        "crates/app/src/stacks/detect/mod.rs"
                    ]),
                },
                Import {
                    names: Names::All,
                    targets: files(&[
                        "crates/app/src/stacks/detect/Stack.rs",
                        "crates/app/src/stacks/detect/Stack/mod.rs"
                    ]),
                },
            ]
        );
    }

    #[test]
    fn a_module_brings_in_any_name_used_through_it() {
        let imports = imports(
            "src/main.rs",
            "use crate::stacks;
fn f() { stacks::detect(); }",
        );
        assert!(imports[1].names.include("detect"));
        assert_eq!(
            imports[1].targets,
            files(&["src/stacks.rs", "src/stacks/mod.rs"])
        );
    }

    #[test]
    fn a_tree_with_aliases_globs_and_self() {
        let imports = imports(
            "src/lib.rs",
            "pub(crate) use crate::{\n    a::{self, B as C},\n    // a comment\n    d::*,\n    E,\n};",
        );
        assert_eq!(
            imports,
            [
                Import {
                    names: names(&["B", "C"]),
                    targets: files(&["src/a.rs", "src/a/mod.rs"]),
                },
                Import {
                    names: Names::All,
                    targets: files(&["src/a/B.rs", "src/a/B/mod.rs"]),
                },
                Import {
                    names: Names::All,
                    targets: files(&["src/d.rs", "src/d/mod.rs"]),
                },
                Import {
                    names: names(&["E"]),
                    targets: files(&["src/lib.rs", "src/main.rs"]),
                },
                Import {
                    names: Names::All,
                    targets: files(&["src/E.rs", "src/E/mod.rs"]),
                },
            ]
        );
    }

    #[test]
    fn self_and_super_from_a_module_file() {
        let imports = named(imports(
            "src/stacks/detect.rs",
            "use self::rules::Rule;\nuse super::Stack;\nuse super::super::Config;",
        ));
        let targets: Vec<Vec<Target>> = imports.into_iter().map(|i| i.targets).collect();
        assert_eq!(
            targets,
            [
                files(&[
                    "src/stacks/detect/rules.rs",
                    "src/stacks/detect/rules/mod.rs"
                ]),
                files(&["src/stacks.rs", "src/stacks/mod.rs"]),
                files(&["src/lib.rs", "src/main.rs"]),
            ]
        );
    }

    #[test]
    fn super_from_a_mod_rs() {
        let imports = imports("src/stacks/mod.rs", "use super::Config;");
        assert_eq!(imports[0].targets, files(&["src/lib.rs", "src/main.rs"]));
    }

    #[test]
    fn a_child_module_declared_in_the_file() {
        let imports = imports("src/lib.rs", "mod store;\nuse store::Store;");
        assert_eq!(
            imports[0].targets,
            files(&["src/store.rs", "src/store/mod.rs"])
        );
    }

    #[test]
    fn another_crate_in_the_repository() {
        let imports = named(imports(
            "crates/app/src/main.rs",
            "use symbol_index::{Index, store::Store};",
        ));
        assert_eq!(
            imports[0].targets,
            [
                Target::Suffix("symbol_index/src/lib.rs".into()),
                Target::Suffix("symbol-index/src/lib.rs".into()),
            ]
        );
        assert_eq!(
            imports[1].targets,
            [
                Target::Suffix("symbol_index/src/store.rs".into()),
                Target::Suffix("symbol_index/src/store/mod.rs".into()),
                Target::Suffix("symbol-index/src/store.rs".into()),
                Target::Suffix("symbol-index/src/store/mod.rs".into()),
            ]
        );
    }

    #[test]
    fn the_standard_library_names_no_files() {
        let imports = imports("src/lib.rs", "use std::collections::HashMap;");
        assert!(imports[0].targets.is_empty());
    }

    #[test]
    fn use_in_strings_and_comments_isnt_an_import() {
        let text =
            "// use crate::a::B;\nconst C: &str = \"use crate::c::D;\";\nfn f<'a>(x: &'a str) {}\n";
        assert!(imports("src/lib.rs", text).is_empty());
    }

    #[test]
    fn a_file_outside_src() {
        let imports = imports("tests/index/main.rs", "mod support;\nuse support::blobs;");
        assert_eq!(
            imports[0].targets,
            files(&["tests/index/support.rs", "tests/index/support/mod.rs"])
        );
    }
}
