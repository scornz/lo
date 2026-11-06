//! JavaScript and TypeScript: `import`, `export … from`, `require` and `import()`.

use super::{
    Import, Names, Target, directory, join,
    lexer::{Syntax, Token, tokens},
    strip_script_extension,
};

const JAVASCRIPT: Syntax = Syntax {
    line_comment: "//",
    block_comments: true,
    quotes: &['"', '\'', '`'],
    multiline_quotes: &['`'],
    triple_quotes: false,
    lifetimes: false,
};

/// Prefixes projects commonly alias to their source root, like `@/components/button`.
const ALIASES: &[&str] = &["@/", "~/", "#"];

pub(super) fn imports(from_path: &str, text: &str) -> Vec<Import> {
    let tokens = tokens(text, &JAVASCRIPT);
    let mut imports = Vec::new();
    for i in 0..tokens.len() {
        // `x.import` or `x.require` is a property, not a keyword.
        if i > 0 && tokens[i - 1] == Token::Punct('.') {
            continue;
        }
        let found = match tokens[i] {
            Token::Ident("import") => import(&tokens[i + 1..]),
            Token::Ident("export") => export(&tokens[i + 1..]),
            Token::Ident("require") => require(&tokens[..i], &tokens[i + 1..]),
            _ => None,
        };
        if let Some((specifier, names)) = found {
            imports.push(Import {
                names,
                targets: targets(from_path, specifier),
            });
        }
    }
    imports
}

/// What follows `import`: its module specifier and the names it brings in.
fn import<'a>(tokens: &[Token<'a>]) -> Option<(&'a str, Names)> {
    match tokens {
        // `import "./polyfill"` runs a module for its effects.
        [Token::Str(specifier), ..] => Some((specifier, Names::These(Vec::new()))),
        // `import("./lazy")`
        [Token::Punct('('), Token::Str(specifier), ..] => Some((specifier, Names::All)),
        // TypeScript's `import x = require("./x")`
        [
            Token::Ident(name),
            Token::Punct('='),
            Token::Ident("require"),
            Token::Punct('('),
            Token::Str(specifier),
            ..,
        ] => Some((specifier, Names::These(vec![name.to_string()]))),
        _ => from_clause(tokens),
    }
}

/// What follows `export`, if it re-exports from another module: `export { a } from "./a"`,
/// `export * from "./b"`.
fn export<'a>(tokens: &[Token<'a>]) -> Option<(&'a str, Names)> {
    match tokens {
        [Token::Punct('{' | '*'), ..] | [Token::Ident("type"), Token::Punct('{'), ..] => {
            from_clause(tokens)
        }
        _ => None,
    }
}

/// A clause like `a, { b as c, type d }` or `* as ns` and then `from "specifier"`.
fn from_clause<'a>(tokens: &[Token<'a>]) -> Option<(&'a str, Names)> {
    let from = tokens
        .iter()
        // A clause is short; past a `;` or another statement's keyword, this isn't one.
        .take_while(|token| !matches!(token, Token::Punct(';') | Token::Ident("import")))
        .take(500)
        .position(|token| *token == Token::Ident("from"))?;
    let Some(Token::Str(specifier)) = tokens.get(from + 1) else {
        return None;
    };

    let clause = &tokens[..from];
    if clause.contains(&Token::Punct('*')) && !clause.contains(&Token::Punct('{')) {
        return Some((specifier, Names::All));
    }
    let names = clause
        .iter()
        .filter_map(|token| match token {
            Token::Ident("type" | "typeof" | "as") => None,
            Token::Ident(name) | Token::Str(name) => Some(name.to_string()),
            _ => None,
        })
        .collect();
    Some((specifier, Names::These(names)))
}

/// What follows `require`, and what's before it if it's assigned:
/// `const { a, b: c } = require("./x")`, `const x = require("./x")`.
fn require<'a>(before: &[Token<'a>], after: &[Token<'a>]) -> Option<(&'a str, Names)> {
    let [Token::Punct('('), Token::Str(specifier), ..] = after else {
        return None;
    };
    let names = match before {
        // TypeScript's `import x = require("./x")`, which `import` reads.
        [
            ..,
            Token::Ident("import"),
            Token::Ident(_),
            Token::Punct('='),
        ] => return None,
        [
            ..,
            Token::Ident("const" | "let" | "var"),
            Token::Ident(name),
            Token::Punct('='),
        ] => Names::These(vec![name.to_string()]),
        [.., Token::Punct('}'), Token::Punct('=')] => {
            let open = before
                .iter()
                .rposition(|token| *token == Token::Punct('{'))?;
            Names::These(
                before[open..]
                    .iter()
                    .filter_map(|token| match token {
                        Token::Ident(name) => Some(name.to_string()),
                        _ => None,
                    })
                    .collect(),
            )
        }
        _ => Names::All,
    };
    Some((specifier, names))
}

/// Where a module specifier may point. A relative one resolves against the importing file's
/// directory; any other may be a path alias or a package in the repository, so it matches any
/// path ending with it.
fn targets(from_path: &str, specifier: &str) -> Vec<Target> {
    let path = strip_script_extension(specifier).unwrap_or(specifier);
    if specifier.starts_with("./") || specifier.starts_with("../") || specifier == "." {
        return join(directory(from_path), path)
            .map(|path| Target::Script { path, exact: true })
            .into_iter()
            .collect();
    }
    let path = ALIASES
        .iter()
        .find_map(|alias| path.strip_prefix(alias))
        .unwrap_or(path);
    // A scope names who publishes a package, not where it lives: `@acme/ui` is in `…/ui`.
    let path = match path.strip_prefix('@') {
        Some(scoped) => scoped.split_once('/').map_or(scoped, |(_, path)| path),
        None => path,
    };
    if path.is_empty() {
        return Vec::new();
    }
    vec![Target::Script {
        path: path.to_string(),
        exact: false,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(names: &[&str]) -> Names {
        Names::These(names.iter().map(|name| name.to_string()).collect())
    }

    fn exact(path: &str) -> Vec<Target> {
        vec![Target::Script {
            path: path.into(),
            exact: true,
        }]
    }

    #[test]
    fn named_default_and_namespace_imports() {
        let text = r#"
import React, { useState as state, type FC } from "react";
import * as shapes from "./shapes";
import type { Circle } from '../geometry/circle';
"#;
        assert_eq!(
            imports("web/src/app.tsx", text),
            [
                Import {
                    names: names(&["React", "useState", "state", "FC"]),
                    targets: vec![Target::Script {
                        path: "react".into(),
                        exact: false
                    }],
                },
                Import {
                    names: Names::All,
                    targets: exact("web/src/shapes"),
                },
                Import {
                    names: names(&["Circle"]),
                    targets: exact("web/geometry/circle"),
                },
            ]
        );
    }

    #[test]
    fn re_exports() {
        let text = "export { a, b as c } from './a';\nexport * from './b';\nexport const d = 1;";
        let imports = imports("index.ts", text);
        assert_eq!(imports.len(), 2);
        assert_eq!(imports[0].names, names(&["a", "b", "c"]));
        assert_eq!(imports[1].names, Names::All);
    }

    #[test]
    fn require_and_dynamic_import() {
        let text = r#"
const { helper, other: renamed } = require("./helpers");
const lib = require('./lib.js');
register(require("./plugin"));
const lazy = import("./lazy");
import "./polyfill";
"#;
        let imports = imports("src/index.js", text);
        assert_eq!(
            imports.iter().map(|i| &i.names).collect::<Vec<_>>(),
            [
                &names(&["helper", "other", "renamed"]),
                &names(&["lib"]),
                &Names::All,
                &Names::All,
                &names(&[]),
            ]
        );
        // An extension in the specifier still matches a TypeScript source.
        assert_eq!(imports[1].targets, exact("src/lib"));
    }

    #[test]
    fn typescript_import_equals_require() {
        let imports = imports("a.ts", "import fs = require('./fs');");
        assert_eq!(
            imports,
            [Import {
                names: names(&["fs"]),
                targets: exact("fs"),
            }]
        );
    }

    #[test]
    fn aliases_and_scoped_packages_match_by_suffix() {
        assert_eq!(
            targets("src/a.ts", "@/components/button"),
            [Target::Script {
                path: "components/button".into(),
                exact: false
            }]
        );
        assert_eq!(
            targets("src/a.ts", "@acme/ui/button"),
            [Target::Script {
                path: "ui/button".into(),
                exact: false
            }]
        );
    }

    #[test]
    fn properties_named_like_keywords_arent_imports() {
        let text = "const meta = import.meta;\nmodule.require('./x');\nconst from = 1;";
        assert!(imports("a.js", text).is_empty());
    }
}
