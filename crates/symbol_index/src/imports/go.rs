//! Go: `import "path"` and `import ( … )`. A package is a directory, and its exported names are
//! used through it, so an import brings in any capitalized name from the files of the directory
//! its path ends with.

use super::{
    Import, Names, Target,
    lexer::{Syntax, Token, tokens},
};

const GO: Syntax = Syntax {
    line_comment: "//",
    block_comments: true,
    quotes: &['"', '`', '\''],
    multiline_quotes: &['`'],
    triple_quotes: false,
    lifetimes: false,
};

pub(super) fn imports(text: &str) -> Vec<Import> {
    let tokens = tokens(text, &GO);
    let mut imports = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        if tokens[i] != Token::Ident("import") {
            i += 1;
            continue;
        }
        i += 1;
        let grouped = tokens.get(i) == Some(&Token::Punct('('));
        if grouped {
            i += 1;
        }
        while let Some(token) = tokens.get(i) {
            // An alias, `_` for side effects only, or `.` to use the names unqualified.
            let alias = match token {
                Token::Ident(alias) => Some(*alias),
                Token::Punct('.') => Some("."),
                _ => None,
            };
            if alias.is_some() {
                i += 1;
            }
            let Some(Token::Str(path)) = tokens.get(i) else {
                break;
            };
            i += 1;
            imports.push(Import {
                names: if alias == Some("_") {
                    Names::These(Vec::new())
                } else {
                    Names::Exported
                },
                targets: vec![Target::Package(path.to_string())],
            });
            if !grouped {
                break;
            }
            if tokens.get(i) == Some(&Token::Punct(';')) {
                i += 1;
            }
        }
    }
    imports
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(imports: &[Import]) -> Vec<&Target> {
        imports.iter().flat_map(|import| &import.targets).collect()
    }

    #[test]
    fn single_and_grouped_imports() {
        let text = r#"
package main

import "fmt"

import (
	"github.com/acme/app/internal/store"
	db "github.com/acme/app/internal/database"
	. "github.com/acme/app/internal/dot"
	_ "github.com/lib/pq"
)
"#;
        let imports = imports(text);
        assert_eq!(
            paths(&imports),
            [
                &Target::Package("fmt".into()),
                &Target::Package("github.com/acme/app/internal/store".into()),
                &Target::Package("github.com/acme/app/internal/database".into()),
                &Target::Package("github.com/acme/app/internal/dot".into()),
                &Target::Package("github.com/lib/pq".into()),
            ]
        );
        assert_eq!(imports[1].names, Names::Exported);
        assert_eq!(imports[4].names, Names::These(Vec::new()));
    }

    #[test]
    fn imports_bring_in_only_exported_names() {
        assert!(Names::Exported.include("Store"));
        assert!(!Names::Exported.include("store"));
    }
}
