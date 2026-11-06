//! Python: `import a.b` and `from a.b import c`, absolute or relative.

use super::{
    Import, Names, Target, child, directory,
    lexer::{Syntax, Token, tokens},
};

const PYTHON: Syntax = Syntax {
    line_comment: "#",
    block_comments: false,
    quotes: &['"', '\''],
    multiline_quotes: &[],
    triple_quotes: true,
    lifetimes: false,
};

pub(super) fn imports(from_path: &str, text: &str) -> Vec<Import> {
    let tokens = tokens(text, &PYTHON);
    let mut imports = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        i = match tokens[i] {
            Token::Ident("from") => from_import(from_path, &tokens, i + 1, &mut imports),
            Token::Ident("import") => import(&tokens, i + 1, &mut imports),
            _ => i + 1,
        };
    }
    imports
}

/// `import a.b, c as d`: each module is used through its name, so it brings in any name.
fn import(tokens: &[Token], mut i: usize, imports: &mut Vec<Import>) -> usize {
    loop {
        let (module, next) = dotted_name(tokens, i);
        if module.is_empty() {
            return next.max(i + 1);
        }
        imports.push(Import {
            names: Names::All,
            targets: Module::absolute(&module).files(),
        });
        i = next;
        if let (Some(Token::Ident("as")), Some(Token::Ident(_))) =
            (tokens.get(i), tokens.get(i + 1))
        {
            i += 2;
        }
        if tokens.get(i) != Some(&Token::Punct(',')) {
            return i;
        }
        i += 1;
    }
}

/// `from .a.b import (c, d as e)` or `from a import *`. Returns the index after it, or after
/// `from` if it isn't an import (`yield from`, `raise … from`).
fn from_import(
    from_path: &str,
    tokens: &[Token],
    start: usize,
    imports: &mut Vec<Import>,
) -> usize {
    let mut i = start;
    let mut dots = 0;
    while tokens.get(i) == Some(&Token::Punct('.')) {
        dots += 1;
        i += 1;
    }
    let (module, next) = dotted_name(tokens, i);
    i = next;
    if tokens.get(i) != Some(&Token::Ident("import")) || (dots == 0 && module.is_empty()) {
        return start;
    }
    i += 1;
    if tokens.get(i) == Some(&Token::Punct('(')) {
        i += 1;
    }

    let module = if dots == 0 {
        Module::absolute(&module)
    } else {
        Module::relative(from_path, dots, &module)
    };
    let mut names = Vec::new();
    if tokens.get(i) == Some(&Token::Punct('*')) {
        i += 1;
    } else {
        while let Some(Token::Ident(name)) = tokens.get(i) {
            names.push(name.to_string());
            // The name may be a submodule, used through it: `from app import models`, then
            // `models.User`.
            imports.push(Import {
                names: Names::All,
                targets: module.child(name).files(),
            });
            i += 1;
            if let (Some(Token::Ident("as")), Some(Token::Ident(alias))) =
                (tokens.get(i), tokens.get(i + 1))
            {
                names.push(alias.to_string());
                i += 2;
            }
            if tokens.get(i) != Some(&Token::Punct(',')) {
                break;
            }
            i += 1;
        }
    }

    imports.push(Import {
        names: if names.is_empty() {
            Names::All
        } else {
            Names::These(names)
        },
        targets: module.files(),
    });
    i
}

/// A dotted name like `a.b.c` starting at `tokens[i]`, as `["a", "b", "c"]`, and the index after it.
fn dotted_name<'a>(tokens: &[Token<'a>], mut i: usize) -> (Vec<&'a str>, usize) {
    let mut parts = Vec::new();
    while let Some(Token::Ident(part)) = tokens.get(i) {
        if *part == "import" {
            break;
        }
        parts.push(*part);
        i += 1;
        if tokens.get(i) != Some(&Token::Punct('.')) {
            break;
        }
        i += 1;
    }
    (parts, i)
}

/// A module, by its path in the repository.
struct Module {
    path: String,
    /// Whether the path is from the repository's root. An absolute import's isn't: it's from a
    /// source root the heuristic doesn't know, so any path ending with it matches.
    exact: bool,
}

impl Module {
    fn absolute(module: &[&str]) -> Self {
        Self {
            path: module.join("/"),
            exact: false,
        }
    }

    /// One dot is the importing file's package, each more its parent's.
    fn relative(from_path: &str, dots: usize, module: &[&str]) -> Self {
        let mut package = directory(from_path);
        for _ in 1..dots {
            package = directory(package);
        }
        Self {
            path: child(package, &module.join("/"))
                .trim_end_matches('/')
                .to_string(),
            exact: true,
        }
    }

    fn child(&self, name: &str) -> Self {
        Self {
            path: child(&self.path, name),
            exact: self.exact,
        }
    }

    /// The files that may hold it: a module file or a package's `__init__`.
    fn files(&self) -> Vec<Target> {
        let files = if self.path.is_empty() {
            vec!["__init__.py".to_string(), "__init__.pyi".to_string()]
        } else {
            vec![
                format!("{}.py", self.path),
                format!("{}.pyi", self.path),
                format!("{}/__init__.py", self.path),
                format!("{}/__init__.pyi", self.path),
            ]
        };
        files
            .into_iter()
            .map(|file| {
                if self.exact {
                    Target::File(file)
                } else {
                    Target::Suffix(file)
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(names: &[&str]) -> Names {
        Names::These(names.iter().map(|name| name.to_string()).collect())
    }

    /// Each import's names, and the first file it names.
    fn summary(imports: &[Import]) -> Vec<(&Names, &Target)> {
        imports
            .iter()
            .map(|import| (&import.names, &import.targets[0]))
            .collect()
    }

    #[test]
    fn plain_imports_bring_in_any_name() {
        let imports = imports("app/main.py", "import os.path, app.util as u\n");
        assert_eq!(
            summary(&imports),
            [
                (&Names::All, &Target::Suffix("os/path.py".into())),
                (&Names::All, &Target::Suffix("app/util.py".into())),
            ]
        );
        assert!(
            imports[1]
                .targets
                .contains(&Target::Suffix("app/util/__init__.py".into()))
        );
    }

    #[test]
    fn from_imports_bring_in_their_names_and_any_submodule() {
        let imports = imports(
            "app/main.py",
            "from app.shapes import (\n    Circle,\n    Square as Box,\n)\nfrom app.all import *\n",
        );
        assert_eq!(
            summary(&imports),
            [
                (&Names::All, &Target::Suffix("app/shapes/Circle.py".into())),
                (&Names::All, &Target::Suffix("app/shapes/Square.py".into())),
                (
                    &names(&["Circle", "Square", "Box"]),
                    &Target::Suffix("app/shapes.py".into())
                ),
                (&Names::All, &Target::Suffix("app/all.py".into())),
            ]
        );
        assert!(
            imports[2]
                .targets
                .contains(&Target::Suffix("app/shapes/__init__.py".into()))
        );
    }

    #[test]
    fn relative_imports() {
        let imports = imports(
            "pkg/sub/mod.py",
            "from . import sibling\nfrom .helpers import help\nfrom ..base import Base\n",
        );
        assert_eq!(
            summary(&imports),
            [
                (&Names::All, &Target::File("pkg/sub/sibling.py".into())),
                (&names(&["sibling"]), &Target::File("pkg/sub.py".into())),
                (&Names::All, &Target::File("pkg/sub/helpers/help.py".into())),
                (
                    &names(&["help"]),
                    &Target::File("pkg/sub/helpers.py".into())
                ),
                (&Names::All, &Target::File("pkg/base/Base.py".into())),
                (&names(&["Base"]), &Target::File("pkg/base.py".into())),
            ]
        );
        assert!(
            imports[1]
                .targets
                .contains(&Target::File("pkg/sub/__init__.py".into()))
        );
        assert!(
            imports[5]
                .targets
                .contains(&Target::File("pkg/base/__init__.py".into()))
        );
    }

    #[test]
    fn from_without_import_isnt_an_import() {
        let text = "def f():\n    yield from g()\n    raise E from e\n";
        assert!(imports("a.py", text).is_empty());
    }

    #[test]
    fn imports_in_docstrings_arent_imports() {
        let text = "\"\"\"\nimport fake\n\"\"\"\n# from fake import x\nimport real\n";
        let imports = imports("a.py", text);
        assert_eq!(
            summary(&imports),
            [(&Names::All, &Target::Suffix("real.py".into()))]
        );
    }
}
