// Ported from difftastic (https://github.com/Wilfred/difftastic), commit 87aad1a:
//   src/diff/shortest_path.rs
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

//! Finds the cheapest route through the diff [graph](super::graph) with Dijkstra's algorithm, and
//! marks each node on it unchanged, replaced or novel.
//!
//! difftastic chose Dijkstra over A* because its graph has no good distance heuristic: an edge
//! that matches a whole subtree crosses a long way for a low cost. Finding smaller parts of the
//! trees to diff (see [`super::unchanged`]) does more for speed.

use super::changes::{Change, Changes};
use super::graph::{Edge, Graph, Vertex, VertexId};
use super::nodes::{NodeId, Syntax};

/// The search reached more vertices than its limit allows.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ExceededGraphLimit;

/// Marks the nodes from `lhs` and `rhs` on, and their siblings after them, unchanged, replaced or
/// novel, giving up past `graph_limit` places in the graph.
pub(crate) fn mark_syntax(
    syntax: &Syntax,
    lhs: Option<NodeId>,
    rhs: Option<NodeId>,
    changes: &mut Changes,
    graph_limit: usize,
) -> Result<(), ExceededGraphLimit> {
    let mut graph = Graph::new(syntax);
    let route = shortest_path(&mut graph, Vertex::start(lhs, rhs), graph_limit)?;
    for (edge, vertex) in route {
        mark_step(syntax, edge, graph.vertices[vertex as usize], changes);
    }
    Ok(())
}

/// What the search knows about a vertex.
#[derive(Clone, Copy)]
struct Reached {
    distance: u32,
    /// The vertex before it on the cheapest route found so far, and the edge from there.
    from: Option<(VertexId, Edge)>,
}

/// The cheapest route from `start` to the end: each edge, with the vertex it leaves.
fn shortest_path(
    graph: &mut Graph,
    start: Vertex,
    graph_limit: usize,
) -> Result<Vec<(Edge, VertexId)>, ExceededGraphLimit> {
    let start = graph.add_start(start);
    let mut reached = vec![Reached {
        distance: 0,
        from: None,
    }];
    let mut queue = BucketQueue::default();
    queue.push(0, start);
    let mut neighbours = Vec::new();

    let end = loop {
        let Some((distance, current)) = queue.pop() else {
            unreachable!("the end is always reachable, by marking everything novel");
        };
        // An entry left behind when a cheaper route was found.
        if distance > reached[current as usize].distance {
            continue;
        }
        if graph.vertices[current as usize].is_end() {
            break current;
        }

        neighbours.clear();
        graph.neighbours(current, &mut neighbours);
        reached.resize(
            graph.vertices.len(),
            Reached {
                distance: u32::MAX,
                from: None,
            },
        );
        for &(edge, next) in &neighbours {
            let distance = distance + edge.cost();
            if distance < reached[next as usize].distance {
                reached[next as usize] = Reached {
                    distance,
                    from: Some((current, edge)),
                };
                queue.push(distance, next);
            }
        }

        if graph.size() > graph_limit {
            return Err(ExceededGraphLimit);
        }
    };

    let mut route = Vec::new();
    let mut current = end;
    while let Some((previous, edge)) = reached[current as usize].from {
        route.push((edge, previous));
        current = previous;
    }
    route.reverse();
    Ok(route)
}

/// The vertices to visit, cheapest first, and among equally cheap ones the last pushed first.
///
/// A ring of buckets, one per distance (Dial's algorithm): Dijkstra's algorithm never pushes a
/// vertex cheaper than the last it popped, nor dearer by more than the costliest edge, so the
/// distances waiting fit in the ring, one to a bucket. difftastic uses a radix heap.
struct BucketQueue {
    buckets: Vec<Vec<VertexId>>,
    /// The distance of the last vertex popped.
    distance: u32,
    len: usize,
}

impl Default for BucketQueue {
    fn default() -> Self {
        Self {
            buckets: vec![Vec::new(); Edge::MAX_COST as usize + 1],
            distance: 0,
            len: 0,
        }
    }
}

impl BucketQueue {
    fn push(&mut self, distance: u32, vertex: VertexId) {
        debug_assert!((self.distance..=self.distance + Edge::MAX_COST).contains(&distance));
        let index = distance as usize % self.buckets.len();
        self.buckets[index].push(vertex);
        self.len += 1;
    }

    fn pop(&mut self) -> Option<(u32, VertexId)> {
        if self.len == 0 {
            return None;
        }
        loop {
            let index = self.distance as usize % self.buckets.len();
            if let Some(vertex) = self.buckets[index].pop() {
                self.len -= 1;
                return Some((self.distance, vertex));
            }
            self.distance += 1;
        }
    }
}

/// Marks the nodes an edge decides, at the vertex it leaves.
fn mark_step(syntax: &Syntax, edge: Edge, vertex: Vertex, changes: &mut Changes) {
    let lhs = vertex.lhs;
    let rhs = vertex.rhs;
    match edge {
        Edge::UnchangedNode { .. } => {
            let (lhs, rhs) = (lhs.unwrap(), rhs.unwrap());
            changes.insert_deep_unchanged(syntax, lhs, rhs);
            changes.insert_deep_unchanged(syntax, rhs, lhs);
        }
        // The delimiters are unchanged; the children are decided by later edges.
        Edge::EnterUnchangedDelimiter { .. } => {
            let (lhs, rhs) = (lhs.unwrap(), rhs.unwrap());
            changes.insert(lhs, Change::Unchanged(rhs));
            changes.insert(rhs, Change::Unchanged(lhs));
        }
        Edge::ReplacedComment { levenshtein_pct } | Edge::ReplacedString { levenshtein_pct } => {
            let (lhs, rhs) = (lhs.unwrap(), rhs.unwrap());
            // Texts this different are shown as novel, not word by word.
            if levenshtein_pct > 20 {
                let replaced: fn(NodeId) -> Change = match edge {
                    Edge::ReplacedComment { .. } => Change::ReplacedComment,
                    _ => Change::ReplacedString,
                };
                changes.insert(lhs, replaced(rhs));
                changes.insert(rhs, replaced(lhs));
            } else {
                changes.insert(lhs, Change::Novel);
                changes.insert(rhs, Change::Novel);
            }
        }
        Edge::NovelAtomLhs | Edge::EnterNovelDelimiterLhs => {
            changes.insert(lhs.unwrap(), Change::Novel);
        }
        Edge::NovelAtomRhs | Edge::EnterNovelDelimiterRhs => {
            changes.insert(rhs.unwrap(), Change::Novel);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::nodes::{AtomKind, Token, sexp};

    const LIMIT: usize = 3_000_000;

    /// The edges of the cheapest route between the two sides' top-level nodes.
    fn route(syntax: &mut Syntax, lhs: &[NodeId], rhs: &[NodeId]) -> Vec<Edge> {
        syntax.init(lhs, rhs);
        let mut graph = Graph::new(syntax);
        let start = Vertex::start(lhs.first().copied(), rhs.first().copied());
        shortest_path(&mut graph, start, LIMIT)
            .unwrap()
            .into_iter()
            .map(|(edge, _)| edge)
            .collect()
    }

    fn comment<'s>(syntax: &mut Syntax<'s>, text: &'s str) -> NodeId {
        syntax.atom(Token::new(text, 0), AtomKind::Comment)
    }

    const UNCHANGED: Edge = Edge::UnchangedNode {
        depth_difference: 0,
        probably_punctuation: false,
    };

    #[test]
    fn identical_atoms() {
        let mut syntax = Syntax::default();
        let lhs = sexp(&mut syntax, "foo");
        let rhs = sexp(&mut syntax, "foo");

        assert_eq!(route(&mut syntax, &lhs, &rhs), [UNCHANGED]);
    }

    #[test]
    fn extra_atom_lhs() {
        let mut syntax = Syntax::default();
        let lhs = sexp(&mut syntax, "[foo]");
        let rhs = sexp(&mut syntax, "[]");

        assert_eq!(
            route(&mut syntax, &lhs, &rhs),
            [
                Edge::EnterUnchangedDelimiter {
                    depth_difference: 0
                },
                Edge::NovelAtomLhs,
            ]
        );
    }

    #[test]
    fn repeated_atoms() {
        let mut syntax = Syntax::default();
        let lhs = sexp(&mut syntax, "[]");
        let rhs = sexp(&mut syntax, "[foo foo]");

        assert_eq!(
            route(&mut syntax, &lhs, &rhs),
            [
                Edge::EnterUnchangedDelimiter {
                    depth_difference: 0
                },
                Edge::NovelAtomRhs,
                Edge::NovelAtomRhs,
            ]
        );
    }

    #[test]
    fn atom_after_empty_list() {
        let mut syntax = Syntax::default();
        let lhs = sexp(&mut syntax, "[() foo]");
        let rhs = sexp(&mut syntax, "{() foo}");

        assert_eq!(
            route(&mut syntax, &lhs, &rhs),
            [
                Edge::EnterNovelDelimiterRhs,
                Edge::EnterNovelDelimiterLhs,
                UNCHANGED,
                UNCHANGED,
            ]
        );
    }

    #[test]
    fn a_list_entered_together_is_left_together() {
        // Entering both lists together, `c` inside one can't match `c` after the other, so
        // the route shows a change: here, both lists' delimiters.
        let mut syntax = Syntax::default();
        let lhs = sexp(&mut syntax, "(a b c)");
        let rhs = sexp(&mut syntax, "(a b) c");

        assert_eq!(
            route(&mut syntax, &lhs, &rhs),
            [
                Edge::EnterNovelDelimiterRhs,
                Edge::EnterNovelDelimiterLhs,
                UNCHANGED,
                UNCHANGED,
                Edge::UnchangedNode {
                    depth_difference: 1,
                    probably_punctuation: false
                },
            ]
        );
    }

    #[test]
    fn replace_similar_comment() {
        let mut syntax = Syntax::default();
        let lhs = comment(&mut syntax, "the quick brown fox");
        let rhs = comment(&mut syntax, "the quick brown cat");

        assert_eq!(
            route(&mut syntax, &[lhs], &[rhs]),
            [Edge::ReplacedComment {
                levenshtein_pct: 84
            }]
        );
    }

    #[test]
    fn replace_very_different_comment() {
        let mut syntax = Syntax::default();
        let lhs = comment(&mut syntax, "the quick brown fox");
        let rhs = comment(&mut syntax, "foo bar");

        assert_eq!(
            route(&mut syntax, &[lhs], &[rhs]),
            [Edge::ReplacedComment {
                levenshtein_pct: 11
            }]
        );
    }

    #[test]
    fn replace_comment_prefer_most_similar() {
        let mut syntax = Syntax::default();
        let lhs = [
            comment(&mut syntax, "the quick brown fox"),
            comment(&mut syntax, "the quick brown thing"),
        ];
        let rhs = comment(&mut syntax, "the quick brown fox.");

        assert_eq!(
            route(&mut syntax, &lhs, &[rhs]),
            [
                Edge::ReplacedComment {
                    levenshtein_pct: 95
                },
                Edge::NovelAtomLhs,
            ]
        );
    }

    #[test]
    fn mark_syntax_equal_atoms() {
        let mut syntax = Syntax::default();
        let lhs = sexp(&mut syntax, "foo")[0];
        let rhs = sexp(&mut syntax, "foo")[0];
        syntax.init(&[lhs], &[rhs]);
        let mut changes = Changes::new(&syntax);

        mark_syntax(&syntax, Some(lhs), Some(rhs), &mut changes, LIMIT).unwrap();

        assert_eq!(changes.get(lhs), Some(Change::Unchanged(rhs)));
        assert_eq!(changes.get(rhs), Some(Change::Unchanged(lhs)));
    }

    #[test]
    fn mark_syntax_different_atoms() {
        let mut syntax = Syntax::default();
        let lhs = sexp(&mut syntax, "foo")[0];
        let rhs = sexp(&mut syntax, "bar")[0];
        syntax.init(&[lhs], &[rhs]);
        let mut changes = Changes::new(&syntax);

        mark_syntax(&syntax, Some(lhs), Some(rhs), &mut changes, LIMIT).unwrap();

        assert_eq!(changes.get(lhs), Some(Change::Novel));
        assert_eq!(changes.get(rhs), Some(Change::Novel));
    }

    #[test]
    fn a_very_different_comment_is_novel() {
        let mut syntax = Syntax::default();
        let lhs = comment(&mut syntax, "the quick brown fox");
        let rhs = comment(&mut syntax, "foo bar");
        syntax.init(&[lhs], &[rhs]);
        let mut changes = Changes::new(&syntax);

        mark_syntax(&syntax, Some(lhs), Some(rhs), &mut changes, LIMIT).unwrap();

        assert_eq!(changes.get(lhs), Some(Change::Novel));
        assert_eq!(changes.get(rhs), Some(Change::Novel));
    }

    #[test]
    fn marks_every_node_of_both_sides() {
        let mut syntax = Syntax::default();
        let lhs = sexp(&mut syntax, "(a (b c) d) e");
        let rhs = sexp(&mut syntax, "(a (b x) d) [e]");
        syntax.init(&lhs, &rhs);
        let mut changes = Changes::new(&syntax);

        mark_syntax(&syntax, Some(lhs[0]), Some(rhs[0]), &mut changes, LIMIT).unwrap();

        for id in 0..syntax.len() as NodeId {
            assert!(changes.get(id).is_some(), "node {id} has no change");
        }
    }

    #[test]
    fn gives_up_past_the_graph_limit() {
        let mut syntax = Syntax::default();
        let lhs = sexp(&mut syntax, "(a b c d e f g h)");
        let rhs = sexp(&mut syntax, "(1 2 3 4 5 6 7 8)");
        syntax.init(&lhs, &rhs);
        let mut changes = Changes::new(&syntax);

        let result = mark_syntax(&syntax, Some(lhs[0]), Some(rhs[0]), &mut changes, 10);

        assert_eq!(result, Err(ExceededGraphLimit));
    }
}
