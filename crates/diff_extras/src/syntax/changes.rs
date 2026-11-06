// Ported from difftastic (https://github.com/Wilfred/difftastic), commit 87aad1a:
//   src/diff/changes.rs
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

//! Whether each syntax node changed, as the diff decides.

use super::nodes::{AtomKind, NodeId, Shape, Syntax};

/// How a node compares with the other side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Change {
    /// Unchanged, matched with this node on the other side. For a list, only its delimiters are
    /// known to match: its children may have changed.
    Unchanged(NodeId),
    /// Trailing punctuation the diff ignores, such as the comma that's the only difference
    /// between `[1, 2,]` and `[1, 2]`.
    IgnoredPunctuation,
    /// A comment replaced by this similar one on the other side.
    ReplacedComment(NodeId),
    /// A string replaced by this similar one on the other side.
    ReplacedString(NodeId),
    /// Only on this side.
    Novel,
}

/// Each node's [`Change`], once the diff has decided it.
#[derive(Debug)]
pub(crate) struct Changes {
    changes: Vec<Option<Change>>,
}

impl Changes {
    pub(crate) fn new(syntax: &Syntax) -> Self {
        Self {
            changes: vec![None; syntax.len()],
        }
    }

    pub(crate) fn get(&self, node: NodeId) -> Option<Change> {
        self.changes[node as usize]
    }

    pub(crate) fn insert(&mut self, node: NodeId, change: Change) {
        self.changes[node as usize] = Some(change);
    }

    /// Marks `node` and everything below it unchanged, matched with `opposite` and what's below
    /// it.
    pub(crate) fn insert_deep_unchanged(
        &mut self,
        syntax: &Syntax,
        node: NodeId,
        opposite: NodeId,
    ) {
        self.insert(node, Change::Unchanged(opposite));
        let children = syntax.children(node);
        let opposite_children = syntax.children(opposite);
        for (&child, &opposite_child) in children.iter().zip(opposite_children) {
            self.insert_deep_unchanged(syntax, child, opposite_child);
        }
        // Lists with the same content can differ by an ignorable last child, which matches
        // nothing.
        if children.len() > opposite_children.len() {
            let last = *children.last().unwrap();
            debug_assert!(matches!(
                syntax[last].shape,
                Shape::Atom {
                    kind: AtomKind::Ignorable,
                    ..
                }
            ));
            self.insert(last, Change::IgnoredPunctuation);
        }
    }
}
