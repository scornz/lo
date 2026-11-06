// Ported from difftastic (https://github.com/Wilfred/difftastic), commit 87aad1a:
//   src/diff/graph.rs
//   src/diff/stack.rs
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

//! The graph whose cheapest route is the diff.
//!
//! A vertex points at the next node on each side still to be matched: walking both trees depth
//! first, every node before those two has been decided. Each edge decides the nodes at its
//! vertex and moves past them. If they're equal, an edge marks them unchanged and moves past both;
//! if they're lists with the same delimiters, another enters both; and an edge marks either one
//! novel, stepping over an atom or into a list. With `X A` on the old side and `A` on the new,
//! the cheapest route marks `X` novel and then `A` unchanged.
//!
//! A vertex also records which lists the route has entered and not left. A list entered on both
//! sides at once must be left on both at once: otherwise `(a b c)` and `(a b) c` would show no
//! change, matching `c` outside the list with `c` inside it.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::hash::{BuildHasherDefault, Hash, Hasher};

use super::nodes::{AtomKind, NodeId, Shape, Syntax};

/// A step in the diff, with its [`cost`](Edge::cost).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Edge {
    UnchangedNode {
        depth_difference: u32,
        /// Whether the node looks like punctuation. Matching a variable name is more useful than
        /// matching a comma, so this costs more.
        probably_punctuation: bool,
    },
    EnterUnchangedDelimiter {
        depth_difference: u32,
    },
    ReplacedComment {
        levenshtein_pct: u8,
    },
    ReplacedString {
        levenshtein_pct: u8,
    },
    NovelAtomLhs,
    NovelAtomRhs,
    EnterNovelDelimiterLhs,
    EnterNovelDelimiterRhs,
}

impl Edge {
    /// The most any edge costs: a replacement with nothing in common.
    pub(crate) const MAX_COST: u32 = 600;

    pub(crate) fn cost(self) -> u32 {
        match self {
            // Matching nodes is always best. Every cost is above zero, and a match between
            // depths further apart costs more, up to 40.
            Edge::UnchangedNode {
                depth_difference,
                probably_punctuation,
            } => {
                // Punctuation costs enough that unchanged variables always win, even against
                // replacements on both sides, which can each cost up to 100 less than two novel
                // nodes.
                let punctuation = if probably_punctuation { 200 } else { 0 };
                1 + depth_difference.min(40) + punctuation
            }
            // Entering matching lists is good: cheaper than novel edges, dearer than unchanged
            // ones. Close to unchanged ones, so the search explores a little less.
            Edge::EnterUnchangedDelimiter { depth_difference } => 10 + depth_difference.min(40),
            Edge::NovelAtomLhs
            | Edge::NovelAtomRhs
            | Edge::EnterNovelDelimiterLhs
            | Edge::EnterNovelDelimiterRhs => 300,
            // Replacing a comment or string beats two novel atoms, but only just.
            Edge::ReplacedComment { levenshtein_pct }
            | Edge::ReplacedString { levenshtein_pct } => 500 + u32::from(100 - levenshtein_pct),
        }
    }
}

/// A vertex, as an index into [`Graph::vertices`].
pub(crate) type VertexId = u32;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Vertex {
    /// The next old node to decide, if any is left in the current list.
    pub(crate) lhs: Option<NodeId>,
    /// The next new node to decide, if any is left in the current list.
    pub(crate) rhs: Option<NodeId>,
    /// The list `lhs` is in. Tells apart the ends of different lists when `lhs` is `None`.
    lhs_parent: Option<NodeId>,
    /// The list `rhs` is in.
    rhs_parent: Option<NodeId>,
    /// The lists entered and not yet left, innermost first.
    entered: StackId,
}

impl Vertex {
    pub(crate) fn start(lhs: Option<NodeId>, rhs: Option<NodeId>) -> Self {
        Self {
            lhs,
            rhs,
            lhs_parent: None,
            rhs_parent: None,
            entered: StackId::EMPTY,
        }
    }

    /// Whether every node is decided and every list left.
    pub(crate) fn is_end(&self) -> bool {
        self.lhs.is_none() && self.rhs.is_none() && self.entered == StackId::EMPTY
    }
}

/// A list the route entered and hasn't left.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Entered {
    /// Lists entered on one side at a time, each side's a stack in [`Graph::delimiters`]. Either
    /// side can leave its innermost one on its own.
    Either { lhs: StackId, rhs: StackId },
    /// A list entered on both sides at once, which both sides leave together.
    Both { lhs: NodeId, rhs: NodeId },
}

/// Where a vertex points on one side: at a node, or at the end of a list.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Position {
    At(NodeId),
    EndOf(Option<NodeId>),
}

/// What tells vertices apart. Vertices that point at the same nodes but entered different lists
/// differ only in whether they can leave a list on one side alone; the search keeps at most two
/// of them (see [`Graph::vertex`]). Telling every stack of entered lists apart would make the
/// graph grow exponentially with the trees' depth.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Place {
    lhs: Position,
    rhs: Position,
    can_leave_one_side: bool,
}

/// The graph, built as the search reaches its vertices.
pub(crate) struct Graph<'a, 's> {
    syntax: &'a Syntax<'s>,
    pub(crate) vertices: Vec<Vertex>,
    /// Stacks of lists entered on one side alone.
    delimiters: Stacks<NodeId>,
    /// Stacks of [`Entered`] lists.
    entered: Stacks<Entered>,
    /// The vertices at each place: one, or two with different stacks of entered lists.
    seen: FastMap<Place, (VertexId, Option<VertexId>)>,
}

impl<'a, 's> Graph<'a, 's> {
    pub(crate) fn new(syntax: &'a Syntax<'s>) -> Self {
        Self {
            syntax,
            vertices: Vec::new(),
            delimiters: Stacks::default(),
            entered: Stacks::default(),
            seen: FastMap::default(),
        }
    }

    /// How many places the search has reached: the measure of the graph's size.
    pub(crate) fn size(&self) -> usize {
        self.seen.len()
    }

    /// Adds the starting vertex.
    pub(crate) fn add_start(&mut self, start: Vertex) -> VertexId {
        self.push(start)
    }

    fn push(&mut self, vertex: Vertex) -> VertexId {
        self.vertices.push(vertex);
        (self.vertices.len() - 1) as VertexId
    }

    /// The vertex `vertex` describes: an existing one at the same place with the same entered
    /// lists, or a new one. A place keeps at most two vertices; past that, the second stands for
    /// any other. The first vertex reached is the cheapest, so it has entered the most lists on
    /// both sides together, which is what's wanted.
    fn vertex(&mut self, vertex: Vertex) -> VertexId {
        let place = Place {
            lhs: vertex
                .lhs
                .map_or(Position::EndOf(vertex.lhs_parent), Position::At),
            rhs: vertex
                .rhs
                .map_or(Position::EndOf(vertex.rhs_parent), Position::At),
            can_leave_one_side: matches!(
                self.entered.top(vertex.entered),
                Some((Entered::Either { .. }, _))
            ),
        };
        let next = self.vertices.len() as VertexId;
        match self.seen.entry(place) {
            Entry::Occupied(mut entry) => match *entry.get() {
                (_, Some(second)) => second,
                (first, None) if self.vertices[first as usize].entered == vertex.entered => first,
                (first, None) => {
                    entry.insert((first, Some(next)));
                    self.vertices.push(vertex);
                    next
                }
            },
            Entry::Vacant(entry) => {
                entry.insert((next, None));
                self.vertices.push(vertex);
                next
            }
        }
    }

    /// The edges out of vertex `id`, each with the vertex it leads to.
    pub(crate) fn neighbours(&mut self, id: VertexId, out: &mut Vec<(Edge, VertexId)>) {
        let syntax = self.syntax;
        let v = self.vertices[id as usize];
        let after = |node: NodeId| syntax[node].next_sibling;
        let first_child = |node: NodeId| syntax.children(node).first().copied();

        if let (Some(lhs), Some(rhs)) = (v.lhs, v.rhs) {
            let depth_difference = syntax[lhs].depth.abs_diff(syntax[rhs].depth);
            if syntax.same_content(lhs, rhs) {
                let edge = Edge::UnchangedNode {
                    depth_difference,
                    probably_punctuation: looks_like_punctuation(syntax, lhs),
                };
                let next = self.advance(Vertex {
                    lhs: after(lhs),
                    rhs: after(rhs),
                    ..v
                });
                out.push((edge, next));
            }

            // The delimiters match, but the children may not.
            if syntax.same_delimiters(lhs, rhs) {
                let entered = self.entered.push(v.entered, Entered::Both { lhs, rhs });
                let next = self.advance(Vertex {
                    lhs: first_child(lhs),
                    rhs: first_child(rhs),
                    lhs_parent: Some(lhs),
                    rhs_parent: Some(rhs),
                    entered,
                });
                out.push((Edge::EnterUnchangedDelimiter { depth_difference }, next));
            }

            if let Some(edge) = replacement(syntax, lhs, rhs) {
                let next = self.advance(Vertex {
                    lhs: after(lhs),
                    rhs: after(rhs),
                    ..v
                });
                out.push((edge, next));
            }
        }

        if let Some(lhs) = v.lhs {
            let step = match syntax[lhs].shape {
                Shape::Atom { .. } => (
                    Edge::NovelAtomLhs,
                    Vertex {
                        lhs: after(lhs),
                        ..v
                    },
                ),
                Shape::List { .. } => (
                    Edge::EnterNovelDelimiterLhs,
                    Vertex {
                        lhs: first_child(lhs),
                        lhs_parent: Some(lhs),
                        entered: self.enter_lhs(v.entered, lhs),
                        ..v
                    },
                ),
            };
            let next = self.advance(step.1);
            out.push((step.0, next));
        }

        if let Some(rhs) = v.rhs {
            let step = match syntax[rhs].shape {
                Shape::Atom { .. } => (
                    Edge::NovelAtomRhs,
                    Vertex {
                        rhs: after(rhs),
                        ..v
                    },
                ),
                Shape::List { .. } => (
                    Edge::EnterNovelDelimiterRhs,
                    Vertex {
                        rhs: first_child(rhs),
                        rhs_parent: Some(rhs),
                        entered: self.enter_rhs(v.entered, rhs),
                        ..v
                    },
                ),
            };
            let next = self.advance(step.1);
            out.push((step.0, next));
        }
    }

    /// The vertex after leaving every list `vertex` has finished.
    fn advance(&mut self, vertex: Vertex) -> VertexId {
        let vertex = self.leave_finished_lists(vertex);
        self.vertex(vertex)
    }

    /// Leaves lists for as long as it can: a side with no node left leaves its innermost list
    /// if it entered it alone, and when neither side has a node left, both leave a list they
    /// entered together. Each continues after the list it left.
    fn leave_finished_lists(&mut self, mut v: Vertex) -> Vertex {
        let syntax = self.syntax;
        loop {
            if v.lhs.is_none()
                && let Some((list, entered)) = self.leave_lhs(v.entered)
            {
                v.lhs = syntax[list].next_sibling;
                v.lhs_parent = syntax[list].parent;
                v.entered = entered;
                continue;
            }
            if v.rhs.is_none()
                && let Some((list, entered)) = self.leave_rhs(v.entered)
            {
                v.rhs = syntax[list].next_sibling;
                v.rhs_parent = syntax[list].parent;
                v.entered = entered;
                continue;
            }
            if v.lhs.is_none()
                && v.rhs.is_none()
                && let Some((Entered::Both { lhs, rhs }, entered)) = self.entered.top(v.entered)
            {
                v.lhs = syntax[lhs].next_sibling;
                v.rhs = syntax[rhs].next_sibling;
                v.lhs_parent = syntax[lhs].parent;
                v.rhs_parent = syntax[rhs].parent;
                v.entered = entered;
                continue;
            }
            return v;
        }
    }

    /// Enters `list` on the old side alone.
    fn enter_lhs(&mut self, entered: StackId, list: NodeId) -> StackId {
        let (lhs, rhs, rest) = match self.entered.top(entered) {
            Some((Entered::Either { lhs, rhs }, rest)) => (lhs, rhs, rest),
            _ => (StackId::EMPTY, StackId::EMPTY, entered),
        };
        let lhs = self.delimiters.push(lhs, list);
        self.entered.push(rest, Entered::Either { lhs, rhs })
    }

    /// Enters `list` on the new side alone.
    fn enter_rhs(&mut self, entered: StackId, list: NodeId) -> StackId {
        let (lhs, rhs, rest) = match self.entered.top(entered) {
            Some((Entered::Either { lhs, rhs }, rest)) => (lhs, rhs, rest),
            _ => (StackId::EMPTY, StackId::EMPTY, entered),
        };
        let rhs = self.delimiters.push(rhs, list);
        self.entered.push(rest, Entered::Either { lhs, rhs })
    }

    /// Leaves the innermost list the old side entered alone, if that's the innermost entered.
    fn leave_lhs(&mut self, entered: StackId) -> Option<(NodeId, StackId)> {
        let (Entered::Either { lhs, rhs }, mut rest) = self.entered.top(entered)? else {
            return None;
        };
        let (list, lhs) = self.delimiters.top(lhs)?;
        if lhs != StackId::EMPTY || rhs != StackId::EMPTY {
            rest = self.entered.push(rest, Entered::Either { lhs, rhs });
        }
        Some((list, rest))
    }

    /// Leaves the innermost list the new side entered alone, if that's the innermost entered.
    fn leave_rhs(&mut self, entered: StackId) -> Option<(NodeId, StackId)> {
        let (Entered::Either { lhs, rhs }, mut rest) = self.entered.top(entered)? else {
            return None;
        };
        let (list, rhs) = self.delimiters.top(rhs)?;
        if lhs != StackId::EMPTY || rhs != StackId::EMPTY {
            rest = self.entered.push(rest, Entered::Either { lhs, rhs });
        }
        Some((list, rest))
    }
}

/// Whether a node is a comma, semicolon or full stop. Deliberately cautious, since punctuation
/// is hard to recognize across languages.
fn looks_like_punctuation(syntax: &Syntax, node: NodeId) -> bool {
    matches!(&syntax[node].shape, Shape::Atom { token, .. } if matches!(token.text, "," | ";" | "."))
}

/// An edge that replaces one comment or string with a different one, if both nodes are comments
/// or both are strings.
fn replacement(syntax: &Syntax, lhs: NodeId, rhs: NodeId) -> Option<Edge> {
    let (
        Shape::Atom {
            token: lhs_token,
            kind: lhs_kind,
        },
        Shape::Atom {
            token: rhs_token,
            kind: rhs_kind,
        },
    ) = (&syntax[lhs].shape, &syntax[rhs].shape)
    else {
        return None;
    };
    if lhs_kind != rhs_kind || lhs_token.text == rhs_token.text {
        return None;
    }
    let levenshtein_pct = similarity_percent(lhs_token.text, rhs_token.text);
    match lhs_kind {
        AtomKind::Comment => Some(Edge::ReplacedComment { levenshtein_pct }),
        AtomKind::String => Some(Edge::ReplacedString { levenshtein_pct }),
        _ => None,
    }
}

/// How alike two texts are, from 0 to 100: their Levenshtein distance in characters, as a
/// percentage of the longer one's length, taken from 100.
pub(crate) fn similarity_percent(a: &str, b: &str) -> u8 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let longer = a.len().max(b.len());
    if longer == 0 {
        return 100;
    }
    // One row of the edit-distance table at a time.
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, a_char) in a.iter().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, b_char) in b.iter().enumerate() {
            let substituted = diagonal + usize::from(a_char != b_char);
            diagonal = row[j + 1];
            row[j + 1] = substituted.min(row[j] + 1).min(row[j + 1] + 1);
        }
    }
    let distance = row[b.len()];
    ((1.0 - distance as f64 / longer as f64) * 100.0).round() as u8
}

/// A stack in a [`Stacks`]. Equal stacks have the same id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct StackId(u32);

impl StackId {
    pub(crate) const EMPTY: Self = Self(0);
}

/// Persistent stacks that share their tails. Each (top, rest) pair is stored once, so equal
/// stacks have the same [`StackId`] and compare in constant time.
struct Stacks<T> {
    /// Stack `StackId(n)` is `frames[n - 1]`: its top and the rest of the stack.
    frames: Vec<(T, StackId)>,
    ids: FastMap<(T, StackId), StackId>,
}

impl<T> Default for Stacks<T> {
    fn default() -> Self {
        Self {
            frames: Vec::new(),
            ids: FastMap::default(),
        }
    }
}

impl<T: Copy + Eq + Hash> Stacks<T> {
    fn push(&mut self, rest: StackId, top: T) -> StackId {
        let next = StackId(self.frames.len() as u32 + 1);
        *self.ids.entry((top, rest)).or_insert_with(|| {
            self.frames.push((top, rest));
            next
        })
    }

    /// The stack's top and the rest of it, or `None` if it's empty.
    fn top(&self, stack: StackId) -> Option<(T, StackId)> {
        stack
            .0
            .checked_sub(1)
            .map(|index| self.frames[index as usize])
    }
}

/// A hash map with FxHash, the fast hash rustc and Firefox use. The graph hashes millions of
/// small integer keys, none of them chosen by an attacker.
pub(crate) type FastMap<K, V> = HashMap<K, V, BuildHasherDefault<FastHasher>>;

#[derive(Default)]
pub(crate) struct FastHasher(u64);

impl FastHasher {
    fn add(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
}

impl Hasher for FastHasher {
    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut word = [0; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            self.add(u64::from_le_bytes(word));
        }
    }

    fn write_u8(&mut self, n: u8) {
        self.add(n.into());
    }

    fn write_u32(&mut self, n: u32) {
        self.add(n.into());
    }

    fn write_u64(&mut self, n: u64) {
        self.add(n);
    }

    fn write_usize(&mut self, n: usize) {
        self.add(n as u64);
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn similarity_is_levenshtein_distance_over_the_longer_length() {
        assert_eq!(
            similarity_percent("the quick brown fox", "the quick brown cat"),
            84
        );
        assert_eq!(similarity_percent("the quick brown fox", "foo bar"), 11);
        assert_eq!(similarity_percent("", ""), 100);
        assert_eq!(similarity_percent("abc", ""), 0);
        assert_eq!(similarity_percent("kitten", "sitting"), 57);
    }

    #[test]
    fn no_edge_costs_more_than_the_most() {
        let costliest = [
            Edge::UnchangedNode {
                depth_difference: u32::MAX,
                probably_punctuation: true,
            },
            Edge::EnterUnchangedDelimiter {
                depth_difference: u32::MAX,
            },
            Edge::ReplacedComment { levenshtein_pct: 0 },
            Edge::ReplacedString { levenshtein_pct: 0 },
            Edge::NovelAtomLhs,
            Edge::EnterNovelDelimiterRhs,
        ];

        assert_eq!(
            costliest.map(Edge::cost).into_iter().max(),
            Some(Edge::MAX_COST)
        );
    }

    #[test]
    fn equal_stacks_share_an_id() {
        let mut stacks = Stacks::default();
        let a = stacks.push(StackId::EMPTY, 1);
        let ab = stacks.push(a, 2);
        let a_again = stacks.push(StackId::EMPTY, 1);

        assert_eq!(a, a_again);
        assert_eq!(stacks.push(a_again, 2), ab);
        assert_eq!(stacks.top(ab), Some((2, a)));
        assert_eq!(stacks.top(StackId::EMPTY), None);
    }
}
