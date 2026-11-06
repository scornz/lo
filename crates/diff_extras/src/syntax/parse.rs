// Ported from difftastic (https://github.com/Wilfred/difftastic), commit 87aad1a:
//   src/parse/tree_sitter_parser.rs
//
// MIT License
//
// Copyright (c) 2021-2025 Wilfred Hughes
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

//! Turns a tree-sitter tree into difftastic's syntax nodes.
//!
//! Tree-sitter keeps a list's delimiters as tokens among its children: `(x)` is `"(" "x" ")"`.
//! difftastic's lists hold them apart from their children, so a node whose children include a
//! pair of the language's delimiters becomes a list between them. Children before the open
//! delimiter or after the close one, as in C's `foo[0]`, go in an outer list around it.

use tree_sitter::{Node as TsNode, Tree, TreeCursor};

use super::language::Language;
use super::nodes::{AtomKind, NodeId, Shape, Syntax, Token};

/// One side's top-level nodes, and how many tree-sitter `ERROR` nodes it has.
pub(crate) struct Parsed {
    pub(crate) roots: Vec<NodeId>,
    pub(crate) errors: usize,
}

/// Adds the nodes of `tree`, a parse of `src`, to `syntax`.
pub(crate) fn parse<'s>(
    syntax: &mut Syntax<'s>,
    src: &'s str,
    tree: &Tree,
    language: &Language,
) -> Parsed {
    // Most parsers return a zero-width node for an empty file, which isn't useful to diff.
    if src.trim().is_empty() {
        return Parsed {
            roots: Vec::new(),
            errors: 0,
        };
    }
    let mut converter = Converter {
        syntax,
        src,
        language,
        errors: 0,
    };
    let mut cursor = tree.walk();
    if cursor.node().is_error() {
        converter.errors += 1;
    }
    // The tree has one root, but each top-level item should be a node.
    cursor.goto_first_child();
    let roots = converter.siblings(&mut cursor);
    Parsed {
        roots,
        errors: converter.errors,
    }
}

struct Converter<'a, 's> {
    syntax: &'a mut Syntax<'s>,
    src: &'s str,
    language: &'a Language,
    errors: usize,
}

impl<'s> Converter<'_, 's> {
    /// Converts the node at `cursor` and its siblings after it.
    fn siblings(&mut self, cursor: &mut TreeCursor) -> Vec<NodeId> {
        let mut nodes = Vec::new();
        loop {
            nodes.extend(self.node(cursor));
            if !cursor.goto_next_sibling() {
                return nodes;
            }
        }
    }

    fn node(&mut self, cursor: &mut TreeCursor) -> Option<NodeId> {
        let node = cursor.node();
        if node.is_error() {
            self.errors += 1;
        }
        if self.language.atoms.contains(&node.kind()) || is_comment(&node) {
            // A string literal or a comment is one atom, whatever its children.
            self.atom(&node)
        } else if node.child_count() > 0 {
            Some(self.list(cursor))
        } else {
            self.atom(&node)
        }
    }

    fn list(&mut self, cursor: &mut TreeCursor) -> NodeId {
        let node = cursor.node();
        let delimiters = self.delimiters(cursor);
        let (open_at, close_at) = delimiters.unwrap_or((usize::MAX, usize::MAX));

        // Without delimiters, the list's are empty, at its start and end.
        let outer_open = Token::empty(node.start_byte());
        let outer_close = Token::empty(node.end_byte());
        let (mut open, mut close) = (outer_open, outer_close);

        let mut before = Vec::new();
        let mut between = Vec::new();
        let mut after = Vec::new();
        cursor.goto_first_child();
        let mut i = 0;
        loop {
            let child = cursor.node();
            if delimiters.is_none() {
                between.extend(self.node(cursor));
            } else if i < open_at {
                before.extend(self.node(cursor));
            } else if i == open_at {
                open = self.token(&child);
            } else if i < close_at {
                between.extend(self.node(cursor));
            } else if i == close_at {
                close = self.token(&child);
            } else {
                after.extend(self.node(cursor));
            }
            if !cursor.goto_next_sibling() {
                break;
            }
            i += 1;
        }
        cursor.goto_parent();

        self.mark_trailing_token(&node, &between);
        let inner = self.syntax.list(open, between, close);
        if before.is_empty() && after.is_empty() {
            return inner;
        }
        // `"foo" "[" "0" "]"` becomes `(foo [0])`.
        let mut children = before;
        children.push(inner);
        children.append(&mut after);
        self.syntax.list(outer_open, children, outer_close)
    }

    /// The positions among the children at `cursor` of the first pair of delimiters: an open
    /// delimiter and the first matching close delimiter after it.
    fn delimiters(&self, cursor: &mut TreeCursor) -> Option<(usize, usize)> {
        let mut tokens: Vec<Option<&str>> = Vec::new();
        cursor.goto_first_child();
        loop {
            let child = cursor.node();
            // Only plain tokens can be delimiters, not lists or comments.
            let is_token = child.child_count() <= 1 && !child.is_extra();
            tokens.push(is_token.then(|| self.token(&child).text));
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();

        for (i, token) in tokens.iter().enumerate() {
            for (open, close) in self.language.delimiters {
                if *token == Some(open) {
                    let after = tokens[i + 1..]
                        .iter()
                        .position(|token| *token == Some(close));
                    if let Some(j) = after {
                        return Some((i, i + 1 + j));
                    }
                }
            }
        }
        None
    }

    /// Marks the list's last child ignorable if the language ignores it at the end of this kind
    /// of node, such as a trailing comma.
    fn mark_trailing_token(&mut self, node: &TsNode, children: &[NodeId]) {
        let Some(&last) = children.last() else {
            return;
        };
        let Shape::Atom { token, kind } = &self.syntax[last].shape else {
            return;
        };
        let ignorable = self
            .language
            .trailing
            .iter()
            .any(|&(kind, text)| node.kind() == kind && token.text == text);
        if ignorable && *kind != AtomKind::Ignorable {
            self.syntax.set_atom_kind(last, AtomKind::Ignorable);
        }
    }

    fn atom(&mut self, node: &TsNode) -> Option<NodeId> {
        // C's preprocessor has invisible `\n` nodes, which would mark lines changed.
        if node.kind() == "\n" {
            return None;
        }
        let mut token = self.token(node);
        // JSX trims the whitespace around text.
        if node.kind() == "jsx_text" {
            let leading = token.text.len() - token.text.trim_start().len();
            token = Token::new(token.text.trim(), token.span.start + leading);
        }
        let kind = if node.is_error() {
            AtomKind::Error
        } else if is_comment(node) {
            AtomKind::Comment
        } else if self.language.strings.contains(&node.kind()) {
            AtomKind::String
        } else {
            AtomKind::Normal
        };
        Some(self.syntax.atom(token, kind))
    }

    /// The node's text. A tree that doesn't match the text gives empty tokens rather than a
    /// panic.
    fn token(&self, node: &TsNode) -> Token<'s> {
        let start = node.start_byte().min(self.src.len());
        let text = self.src.get(start..node.end_byte()).unwrap_or("");
        Token::new(text, start)
    }
}

/// Whether a node is a comment. Grammars name comments `comment` or `…_comment`, and make them
/// extras, nodes that can appear anywhere. difftastic also uses each grammar's highlighting
/// queries, which rozed doesn't have here.
fn is_comment(node: &TsNode) -> bool {
    let kind = node.kind();
    !node.is_error() && (node.is_extra() || kind == "comment" || kind.ends_with("_comment"))
}

#[cfg(test)]
mod tests {
    use tree_sitter::Parser;

    use super::*;

    fn tree(language: &Language, src: &str) -> Tree {
        let grammar: tree_sitter::Language = match language.grammar {
            "json" => tree_sitter_json::LANGUAGE.into(),
            "rust" => tree_sitter_rust::LANGUAGE.into(),
            "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
            _ => unreachable!(),
        };
        let mut parser = Parser::new();
        parser.set_language(&grammar).unwrap();
        parser.parse(src, None).unwrap()
    }

    /// The nodes of `src` as an S-expression: lists in their delimiters, with `{}` for empty ones,
    /// and atoms as their text, with `#` before a comment, `$` before a string and `~` before an
    /// ignorable token.
    fn describe(grammar: &str, src: &str) -> String {
        let language = Language::for_grammar(grammar).unwrap();
        let tree = tree(language, src);
        let mut syntax = Syntax::default();
        let parsed = parse(&mut syntax, src, &tree, language);
        let nodes: Vec<String> = parsed
            .roots
            .iter()
            .map(|&id| describe_node(&syntax, id))
            .collect();
        nodes.join(" ")
    }

    fn describe_node(syntax: &Syntax, id: NodeId) -> String {
        match &syntax[id].shape {
            Shape::List {
                open,
                children,
                close,
                ..
            } => {
                let children: Vec<String> = children
                    .iter()
                    .map(|&child| describe_node(syntax, child))
                    .collect();
                let (open, close) = match (open.text, close.text) {
                    ("", "") => ("{", "}"),
                    delimiters => delimiters,
                };
                format!("{open}{}{close}", children.join(" "))
            }
            Shape::Atom { token, kind } => {
                let prefix = match kind {
                    AtomKind::Comment => "#",
                    AtomKind::String => "$",
                    AtomKind::Ignorable => "~",
                    AtomKind::Normal | AtomKind::Error => "",
                };
                format!("{prefix}{}", token.text)
            }
        }
    }

    #[test]
    fn an_empty_file_has_no_nodes() {
        assert_eq!(describe("rust", ""), "");
        assert_eq!(describe("json", " \n "), "");
    }

    #[test]
    fn delimiters_become_a_lists_own() {
        assert_eq!(describe("json", "[1, [2]]"), "[1 , [2]]");
    }

    #[test]
    fn nodes_without_delimiters_are_lists_with_empty_ones() {
        assert_eq!(describe("rust", "let x = f(a);"), "{let x = {f (a)} ;}");
    }

    #[test]
    fn tokens_before_the_delimiters_go_in_an_outer_list() {
        assert_eq!(describe("rust", "v[0];"), "{{v [0]} ;}");
    }

    #[test]
    fn strings_and_comments_are_single_atoms() {
        assert_eq!(
            describe("rust", "// note\nlet s = \"a b\";"),
            "#// note {let s = $\"a b\" ;}"
        );
        assert_eq!(
            describe("json", "{\"a\": \"x y\"}"),
            "{{$\"a\" : $\"x y\"}}"
        );
    }

    #[test]
    fn a_doc_comment_is_one_atom() {
        assert_eq!(
            describe("rust", "/// Docs.\nfn f() {}"),
            "#/// Docs. {fn f () {}}"
        );
    }

    #[test]
    fn a_trailing_comma_is_ignorable_where_the_language_says() {
        assert_eq!(describe("typescript", "f(a, b,);"), "{{f (a , b ~,)} ;}");
        // Not in a sequence expression, which isn't in TypeScript's list.
        assert_eq!(describe("typescript", "[a, b,];"), "{[a , b ~,] ;}");
    }

    #[test]
    fn counts_parse_errors() {
        let language = Language::for_grammar("rust").unwrap();
        let src = "fn f( {";
        let tree = tree(language, src);
        let mut syntax = Syntax::default();

        let parsed = parse(&mut syntax, src, &tree, language);

        assert!(parsed.errors > 0);
    }

    #[test]
    fn jsx_text_is_trimmed() {
        // The grammar reads `</` as one token, so a closing tag has no delimiters.
        assert_eq!(
            describe("tsx", "<p>  hi there  </p>;"),
            "{{<p> hi there {</ p >}} ;}"
        );
    }
}
