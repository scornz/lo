// Ported from difftastic (https://github.com/Wilfred/difftastic), commit 87aad1a:
//   src/parse/syntax.rs
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

//! difftastic's syntax tree. Every node is an atom, such as an identifier, a string literal or a
//! comment, or a list: children between an open and a close delimiter, such as `(` and `)`. A
//! list built from a tree-sitter node with no delimiters, such as a statement, has empty ones.
//!
//! Both sides' nodes live in one [`Syntax`], so a [`NodeId`] names a node on either side. After
//! [`Syntax::init`], each node knows its parent, next sibling and depth, and has a content id that
//! it shares with every node of the same content, on either side, wherever it sits.

use std::borrow::Cow;
use std::collections::HashMap;
use std::ops::Index;

/// A node of either side, as an index into [`Syntax`].
pub(crate) type NodeId = u32;

/// A byte range in one side's source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Span {
    pub(crate) start: usize,
    pub(crate) end: usize,
}

impl Span {
    pub(crate) fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }
}

/// An atom's text or a delimiter, and where it sits in its side's source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Token<'s> {
    pub(crate) text: &'s str,
    pub(crate) span: Span,
}

impl<'s> Token<'s> {
    pub(crate) fn new(text: &'s str, start: usize) -> Self {
        Self {
            text,
            span: Span::new(start, start + text.len()),
        }
    }

    /// An empty delimiter at `offset`, for a list with no delimiters.
    pub(crate) fn empty(offset: usize) -> Self {
        Self::new("", offset)
    }
}

/// What an atom is, as far as diffing cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum AtomKind {
    /// An identifier, a keyword, a number or punctuation.
    Normal,
    /// A string literal. A changed one can be shown word by word.
    String,
    /// A comment. A changed one can be shown word by word, and a multi-line comment compares equal
    /// to one that only differs in its lines' indentation.
    Comment,
    /// A tree-sitter `ERROR` node.
    Error,
    /// Trailing punctuation, such as the last comma in `[1, 2,]`. A list compares equal to the
    /// same list without it, but the diff still sees it, so `[1,]` against `[]` marks the comma.
    Ignorable,
}

/// A node's shape: a list or an atom.
#[derive(Debug)]
pub(crate) enum Shape<'s> {
    List {
        open: Token<'s>,
        children: Vec<NodeId>,
        close: Token<'s>,
        /// How many nodes are below this one: children, grandchildren and so on.
        descendants: u32,
    },
    Atom {
        token: Token<'s>,
        kind: AtomKind,
    },
}

#[derive(Debug)]
pub(crate) struct Node<'s> {
    pub(crate) shape: Shape<'s>,
    /// The list this node is a child of. `None` for a top-level node.
    pub(crate) parent: Option<NodeId>,
    /// The node after this one with the same parent. A diff of part of a file resets it for that
    /// part's top-level nodes (see [`Syntax::link_siblings`]), so the last of them has none.
    pub(crate) next_sibling: Option<NodeId>,
    /// How many lists this node is inside.
    pub(crate) depth: u32,
    /// Shared by every node, on either side, with the same content.
    pub(crate) content_id: u32,
    /// Whether no other node on this node's side has the same content.
    pub(crate) unique: bool,
}

/// Both sides' syntax nodes.
#[derive(Debug, Default)]
pub(crate) struct Syntax<'s> {
    nodes: Vec<Node<'s>>,
}

impl<'s> Index<NodeId> for Syntax<'s> {
    type Output = Node<'s>;

    fn index(&self, id: NodeId) -> &Node<'s> {
        &self.nodes[id as usize]
    }
}

impl<'s> Syntax<'s> {
    /// Adds an atom. A trailing line ending, which some parsers include in a comment, is dropped:
    /// lines align better on real content.
    pub(crate) fn atom(&mut self, mut token: Token<'s>, kind: AtomKind) -> NodeId {
        for ending in ['\n', '\r'] {
            if let Some(text) = token.text.strip_suffix(ending) {
                token.text = text;
                token.span.end -= 1;
            }
        }
        self.push(Shape::Atom { token, kind })
    }

    /// Adds a list. Empty atoms, which come from incomplete or malformed code, are left out. A
    /// list with no delimiters and one child is just that child.
    pub(crate) fn list(
        &mut self,
        open: Token<'s>,
        children: Vec<NodeId>,
        close: Token<'s>,
    ) -> NodeId {
        let children: Vec<NodeId> = children
            .into_iter()
            .filter(|&child| !self.is_empty_atom(child))
            .collect();
        if let [child] = children[..]
            && open.text.is_empty()
            && close.text.is_empty()
        {
            return child;
        }
        let descendants = children
            .iter()
            .map(|&child| self.descendants(child) + 1)
            .sum();
        self.push(Shape::List {
            open,
            children,
            close,
            descendants,
        })
    }

    fn push(&mut self, shape: Shape<'s>) -> NodeId {
        let id = self.nodes.len() as NodeId;
        self.nodes.push(Node {
            shape,
            parent: None,
            next_sibling: None,
            depth: 0,
            content_id: 0,
            unique: false,
        });
        id
    }

    /// Changes an atom's kind, as when its list makes it ignorable.
    pub(crate) fn set_atom_kind(&mut self, id: NodeId, new_kind: AtomKind) {
        if let Shape::Atom { kind, .. } = &mut self.nodes[id as usize].shape {
            *kind = new_kind;
        }
    }

    fn is_empty_atom(&self, id: NodeId) -> bool {
        matches!(&self[id].shape, Shape::Atom { token, .. } if token.text.is_empty())
    }

    /// How many nodes there are, on both sides.
    pub(crate) fn len(&self) -> usize {
        self.nodes.len()
    }

    /// The node's children; none for an atom.
    pub(crate) fn children(&self, id: NodeId) -> &[NodeId] {
        match &self[id].shape {
            Shape::List { children, .. } => children,
            Shape::Atom { .. } => &[],
        }
    }

    /// How many nodes are below this one.
    pub(crate) fn descendants(&self, id: NodeId) -> u32 {
        match &self[id].shape {
            Shape::List { descendants, .. } => *descendants,
            Shape::Atom { .. } => 0,
        }
    }

    /// Whether two nodes, on either side, have the same content.
    pub(crate) fn same_content(&self, a: NodeId, b: NodeId) -> bool {
        self[a].content_id == self[b].content_id
    }

    /// Whether both nodes are lists with the same delimiters.
    pub(crate) fn same_delimiters(&self, a: NodeId, b: NodeId) -> bool {
        match (&self[a].shape, &self[b].shape) {
            (
                Shape::List {
                    open: a_open,
                    close: a_close,
                    ..
                },
                Shape::List {
                    open: b_open,
                    close: b_close,
                    ..
                },
            ) => a_open.text == b_open.text && a_close.text == b_close.text,
            _ => false,
        }
    }

    /// Sets each node's parent, next sibling, depth, content id and uniqueness, for the
    /// top-level nodes of the old side, `lhs`, and of the new side, `rhs`.
    pub(crate) fn init(&mut self, lhs: &[NodeId], rhs: &[NodeId]) {
        let lhs_nodes = self.preorder(lhs);
        let rhs_nodes = self.preorder(rhs);
        for (roots, nodes) in [(lhs, &lhs_nodes), (rhs, &rhs_nodes)] {
            self.link_siblings(roots);
            for &root in roots {
                self.nodes[root as usize].parent = None;
                self.nodes[root as usize].depth = 0;
            }
            // Parents before their children, so a parent's depth is set first.
            for &id in nodes.iter() {
                let depth = self[id].depth + 1;
                for i in 0..self.children(id).len() {
                    let child = self.children(id)[i];
                    let next_sibling = self.children(id).get(i + 1).copied();
                    let node = &mut self.nodes[child as usize];
                    node.parent = Some(id);
                    node.next_sibling = next_sibling;
                    node.depth = depth;
                }
            }
        }

        let mut ids = HashMap::new();
        for nodes in [&lhs_nodes, &rhs_nodes] {
            // Children before their parents, so a list's key can use its children's ids.
            for &id in nodes.iter().rev() {
                let key = self.content_key(id);
                let next = ids.len() as u32 + 1;
                self.nodes[id as usize].content_id = *ids.entry(key).or_insert(next);
            }
        }

        for nodes in [&lhs_nodes, &rhs_nodes] {
            let mut counts: HashMap<u32, u32> = HashMap::new();
            for &id in nodes.iter() {
                *counts.entry(self[id].content_id).or_default() += 1;
            }
            for &id in nodes.iter() {
                self.nodes[id as usize].unique = counts[&self[id].content_id] == 1;
            }
        }
    }

    /// Links `nodes`, a run of siblings, each to the next. The last gets no next sibling.
    pub(crate) fn link_siblings(&mut self, nodes: &[NodeId]) {
        for (i, &id) in nodes.iter().enumerate() {
            self.nodes[id as usize].next_sibling = nodes.get(i + 1).copied();
        }
    }

    /// `roots` and every node below them, each before its children.
    fn preorder(&self, roots: &[NodeId]) -> Vec<NodeId> {
        let mut order = Vec::new();
        let mut stack: Vec<NodeId> = roots.iter().rev().copied().collect();
        while let Some(id) = stack.pop() {
            order.push(id);
            stack.extend(self.children(id).iter().rev());
        }
        order
    }

    /// What makes two nodes the same content. Ignorable atoms are left out of a list's, and a
    /// multi-line comment's lines are compared without their indentation.
    fn content_key(&self, id: NodeId) -> ContentKey<'s> {
        match &self[id].shape {
            Shape::List {
                open,
                children,
                close,
                ..
            } => ContentKey::List {
                open: open.text,
                close: close.text,
                children: children
                    .iter()
                    .filter(|&&child| {
                        !matches!(
                            self[child].shape,
                            Shape::Atom {
                                kind: AtomKind::Ignorable,
                                ..
                            }
                        )
                    })
                    .map(|&child| self[child].content_id)
                    .collect(),
            },
            Shape::Atom { token, kind } => {
                let comment = *kind == AtomKind::Comment;
                let text = if comment && token.text.contains('\n') {
                    Cow::Owned(
                        token
                            .text
                            .split('\n')
                            .map(str::trim_start)
                            .collect::<Vec<_>>()
                            .join("\n"),
                    )
                } else {
                    Cow::Borrowed(token.text)
                };
                ContentKey::Atom { text, comment }
            }
        }
    }
}

#[derive(PartialEq, Eq, Hash)]
enum ContentKey<'s> {
    List {
        open: &'s str,
        close: &'s str,
        children: Vec<u32>,
    },
    Atom {
        text: Cow<'s, str>,
        comment: bool,
    },
}

/// Builds nodes from S-expressions for tests: `(`, `[` and `{` open lists, and anything else
/// between spaces is an atom.
#[cfg(test)]
pub(crate) fn sexp<'s>(syntax: &mut Syntax<'s>, src: &'s str) -> Vec<NodeId> {
    fn parse<'s>(syntax: &mut Syntax<'s>, src: &'s str, at: &mut usize) -> Vec<NodeId> {
        let mut nodes = Vec::new();
        while *at < src.len() {
            let start = *at;
            let c = src[start..].chars().next().unwrap();
            match c {
                ' ' | '\n' => *at += 1,
                '(' | '[' | '{' => {
                    *at += 1;
                    let children = parse(syntax, src, at);
                    let close = Token::new(&src[*at..*at + 1], *at);
                    *at += 1;
                    nodes.push(syntax.list(
                        Token::new(&src[start..start + 1], start),
                        children,
                        close,
                    ));
                }
                ')' | ']' | '}' => return nodes,
                _ => {
                    let end = src[start..]
                        .find([' ', '\n', '(', ')', '[', ']', '{', '}'])
                        .map_or(src.len(), |len| start + len);
                    *at = end;
                    nodes.push(syntax.atom(Token::new(&src[start..end], start), AtomKind::Normal));
                }
            }
        }
        nodes
    }
    parse(syntax, src, &mut 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn atom<'s>(syntax: &mut Syntax<'s>, text: &'s str, kind: AtomKind) -> NodeId {
        syntax.atom(Token::new(text, 0), kind)
    }

    #[test]
    fn a_comment_differs_from_an_atom_with_the_same_text() {
        let mut syntax = Syntax::default();
        let comment = atom(&mut syntax, "foo", AtomKind::Comment);
        let normal = atom(&mut syntax, "foo", AtomKind::Normal);
        syntax.init(&[comment], &[normal]);

        assert!(!syntax.same_content(comment, normal));
    }

    #[test]
    fn an_atom_drops_a_trailing_carriage_return() {
        let mut syntax = Syntax::default();
        let id = atom(&mut syntax, "foo\r", AtomKind::Comment);

        let Shape::Atom { token, .. } = &syntax[id].shape else {
            panic!("not an atom");
        };
        assert_eq!(token.text, "foo");
        assert_eq!(token.span, Span::new(0, 3));
    }

    #[test]
    fn an_atom_drops_a_trailing_newline() {
        let mut syntax = Syntax::default();
        let id = atom(&mut syntax, ";; hello\n", AtomKind::Comment);

        let Shape::Atom { token, .. } = &syntax[id].shape else {
            panic!("not an atom");
        };
        assert_eq!(token.text, ";; hello");
        assert_eq!(token.span, Span::new(0, 8));
    }

    #[test]
    fn a_list_without_delimiters_of_one_child_is_the_child() {
        let mut syntax = Syntax::default();
        let foo = atom(&mut syntax, "foo", AtomKind::Normal);

        let list = syntax.list(Token::empty(0), vec![foo], Token::empty(3));

        assert_eq!(list, foo);
    }

    #[test]
    fn a_list_leaves_out_empty_atoms() {
        let mut syntax = Syntax::default();
        let empty = syntax.atom(Token::new("", 2), AtomKind::Normal);

        let list = syntax.list(Token::new("(", 0), vec![empty], Token::new(")", 3));

        assert!(syntax.children(list).is_empty());
    }

    #[test]
    fn a_multiline_comment_ignores_its_lines_indentation() {
        let mut syntax = Syntax::default();
        let lhs = atom(&mut syntax, "foo\nbar", AtomKind::Comment);
        let rhs = atom(&mut syntax, "foo\n    bar", AtomKind::Comment);
        syntax.init(&[lhs], &[rhs]);

        assert!(syntax.same_content(lhs, rhs));
    }

    #[test]
    fn lists_with_the_same_children_share_a_content_id() {
        let mut syntax = Syntax::default();
        let lhs = sexp(&mut syntax, "(a (b c)) d");
        let rhs = sexp(&mut syntax, "x (a (b c))");
        syntax.init(&lhs, &rhs);

        assert!(syntax.same_content(lhs[0], rhs[1]));
        assert!(!syntax.same_content(lhs[0], rhs[0]));
    }

    #[test]
    fn a_list_ignores_ignorable_atoms_in_its_content() {
        let mut syntax = Syntax::default();
        let one = syntax.atom(Token::new("1", 1), AtomKind::Normal);
        let comma = syntax.atom(Token::new(",", 2), AtomKind::Ignorable);
        let lhs = syntax.list(Token::new("[", 0), vec![one, comma], Token::new("]", 3));
        let one = syntax.atom(Token::new("1", 1), AtomKind::Normal);
        let rhs = syntax.list(Token::new("[", 0), vec![one], Token::new("]", 2));
        syntax.init(&[lhs], &[rhs]);

        assert!(syntax.same_content(lhs, rhs));
    }

    #[test]
    fn init_links_parents_siblings_and_depths() {
        let mut syntax = Syntax::default();
        let lhs = sexp(&mut syntax, "a (b (c d))");
        syntax.init(&lhs, &[]);

        let [a, outer] = lhs[..] else { panic!() };
        let [b, inner] = syntax.children(outer)[..] else {
            panic!()
        };
        let [c, d] = syntax.children(inner)[..] else {
            panic!()
        };
        assert_eq!(syntax[a].next_sibling, Some(outer));
        assert_eq!(syntax[outer].next_sibling, None);
        assert_eq!(syntax[b].next_sibling, Some(inner));
        assert_eq!(syntax[c].parent, Some(inner));
        assert_eq!(syntax[inner].parent, Some(outer));
        assert_eq!(syntax[outer].parent, None);
        assert_eq!(
            (syntax[a].depth, syntax[b].depth, syntax[d].depth),
            (0, 1, 2)
        );
        assert_eq!(syntax.descendants(outer), 4);
    }

    #[test]
    fn uniqueness_counts_each_side_on_its_own() {
        let mut syntax = Syntax::default();
        let lhs = sexp(&mut syntax, "a b a");
        let rhs = sexp(&mut syntax, "a b");
        syntax.init(&lhs, &rhs);

        assert!(!syntax[lhs[0]].unique);
        assert!(syntax[lhs[1]].unique);
        assert!(syntax[rhs[0]].unique);
    }
}
