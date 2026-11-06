// Ported from difftastic (https://github.com/Wilfred/difftastic), commit 87aad1a:
//   src/diff/unchanged.rs
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

//! Finds syntax nodes that are obviously unchanged, so the graph search runs on smaller parts of
//! the trees: the nodes both sides start and end with, inside lists whose delimiters match, and
//! long enough runs of top-level nodes a linear diff finds on both sides.

use super::changes::{Change, Changes};
use super::lcs::{self, DiffResult};
use super::nodes::{NodeId, Shape, Syntax};

/// The fewest nodes a run of unchanged nodes found by the linear diff has, to be split out. A
/// smaller run, such as one `}`, could split apart regions that belong together.
const TINY_TREE_THRESHOLD: u32 = 10;

/// The fewest subtrees two lists share, each unique on its side, for them to count as mostly
/// unchanged.
const MOSTLY_UNCHANGED_MIN_COMMON_CHILDREN: usize = 4;

/// Old and new nodes still to diff with the graph search.
pub(crate) type Section = (Vec<NodeId>, Vec<NodeId>);

/// Marks the nodes that are obviously unchanged and returns the parts still to diff.
pub(crate) fn mark_unchanged(
    syntax: &Syntax,
    lhs: &[NodeId],
    rhs: &[NodeId],
    changes: &mut Changes,
) -> Vec<Section> {
    let (_, lhs, rhs) = shrink_unchanged_at_ends(syntax, lhs, rhs, changes);
    let mut sections = Vec::new();
    for (lhs, rhs) in split_mostly_unchanged_toplevel(syntax, &lhs, &rhs) {
        let (_, lhs, rhs) = shrink_unchanged_at_ends(syntax, &lhs, &rhs, changes);
        sections.extend(split_unchanged(syntax, &lhs, &rhs, changes));
    }
    sections
}

#[derive(Debug, PartialEq, Eq)]
enum ChangeState {
    /// Both sides are one list each with the same delimiters, though their children may differ.
    UnchangedDelimiter,
    /// Both sides are the same.
    UnchangedNode,
    /// The sides aren't obviously the same.
    PossiblyChanged,
}

fn mark_both_unchanged(syntax: &Syntax, changes: &mut Changes, lhs: NodeId, rhs: NodeId) {
    changes.insert_deep_unchanged(syntax, lhs, rhs);
    changes.insert_deep_unchanged(syntax, rhs, lhs);
}

/// Marks long runs of unchanged nodes that a linear diff of `lhs` and `rhs` finds, and returns
/// the parts between them.
fn split_unchanged(
    syntax: &Syntax,
    lhs: &[NodeId],
    rhs: &[NodeId],
    changes: &mut Changes,
) -> Vec<Section> {
    let mut sections = Vec::new();
    for (state, lhs, rhs) in split_unchanged_toplevel(syntax, lhs, rhs) {
        match state {
            ChangeState::UnchangedDelimiter => {
                for (&lhs, &rhs) in lhs.iter().zip(&rhs) {
                    changes.insert(lhs, Change::Unchanged(rhs));
                    changes.insert(rhs, Change::Unchanged(lhs));
                }
            }
            ChangeState::UnchangedNode => {
                for (&lhs, &rhs) in lhs.iter().zip(&rhs) {
                    mark_both_unchanged(syntax, changes, lhs, rhs);
                }
            }
            ChangeState::PossiblyChanged => sections.push((lhs, rhs)),
        }
    }
    sections
}

/// If both sides are one list with the same delimiters, splits their children further, keeping
/// the lists as unchanged delimiters. Otherwise the sides stay whole, so the graph search can use
/// the outer delimiters.
fn split_changed_singleton_list(
    syntax: &Syntax,
    lhs: &[NodeId],
    rhs: &[NodeId],
) -> Vec<(ChangeState, Vec<NodeId>, Vec<NodeId>)> {
    if let Some((lhs_children, rhs_children)) = as_singleton_list_children(syntax, lhs, rhs) {
        let mut split = split_unchanged_toplevel(syntax, lhs_children, rhs_children);
        if split.len() > 1 {
            split.insert(
                0,
                (ChangeState::UnchangedDelimiter, lhs.to_vec(), rhs.to_vec()),
            );
            return split;
        }
    }
    vec![(ChangeState::PossiblyChanged, lhs.to_vec(), rhs.to_vec())]
}

/// The content ids of the nodes at and below `node` that are unique on their side.
fn unique_content_ids(syntax: &Syntax, node: NodeId, ids: &mut Vec<u32>) {
    if syntax[node].unique {
        ids.push(syntax[node].content_id);
    }
    for &child in syntax.children(node) {
        unique_content_ids(syntax, child, ids);
    }
}

/// How many subtrees at or below `node` are unique on its side and among `opposite_unique`,
/// not counting inside one that is.
fn count_unique_subtrees(syntax: &Syntax, node: NodeId, opposite_unique: &[u32]) -> usize {
    if syntax[node].unique
        && opposite_unique
            .binary_search(&syntax[node].content_id)
            .is_ok()
    {
        return 1;
    }
    syntax
        .children(node)
        .iter()
        .map(|&child| count_unique_subtrees(syntax, child, opposite_unique))
        .sum()
}

/// How many subtrees of `lhs` are unique on its side and also unique on the other side within
/// `rhs`.
fn count_common_unique(syntax: &Syntax, lhs: NodeId, rhs: NodeId) -> usize {
    let mut rhs_unique = Vec::new();
    unique_content_ids(syntax, rhs, &mut rhs_unique);
    rhs_unique.sort_unstable();
    count_unique_subtrees(syntax, lhs, &rhs_unique)
}

/// Whether both nodes are lists sharing enough unique subtrees to be mostly the same, whatever
/// their delimiters.
fn is_mostly_unchanged_list(syntax: &Syntax, lhs: NodeId, rhs: NodeId) -> bool {
    matches!(
        (&syntax[lhs].shape, &syntax[rhs].shape),
        (Shape::List { .. }, Shape::List { .. })
    ) && count_common_unique(syntax, lhs, rhs) >= MOSTLY_UNCHANGED_MIN_COMMON_CHILDREN
}

/// Splits off the mostly unchanged lists the sides start and end with, so each is diffed on its
/// own. Two lists in a row with a small change each, such as `(1 2 3 4) (a b c d)` becoming `(1 2
/// novel 3 4) (a b novel-2 c d)`, then shrink much further.
fn split_mostly_unchanged_toplevel(
    syntax: &Syntax,
    mut lhs: &[NodeId],
    mut rhs: &[NodeId],
) -> Vec<Section> {
    let mut leading = Vec::new();
    while let (Some(&l), Some(&r)) = (lhs.first(), rhs.first()) {
        if !is_mostly_unchanged_list(syntax, l, r) {
            break;
        }
        leading.push((vec![l], vec![r]));
        (lhs, rhs) = (&lhs[1..], &rhs[1..]);
    }

    let mut trailing = Vec::new();
    while let (Some(&l), Some(&r)) = (lhs.last(), rhs.last()) {
        if !is_mostly_unchanged_list(syntax, l, r) {
            break;
        }
        trailing.push((vec![l], vec![r]));
        (lhs, rhs) = (&lhs[..lhs.len() - 1], &rhs[..rhs.len() - 1]);
    }

    let mut sections = leading;
    if !lhs.is_empty() || !rhs.is_empty() {
        sections.push((lhs.to_vec(), rhs.to_vec()));
    }
    sections.extend(trailing.into_iter().rev());
    sections
}

/// Whether a run of equal node pairs has at least [`TINY_TREE_THRESHOLD`] nodes.
fn is_big_enough(syntax: &Syntax, pairs: &[(NodeId, NodeId)]) -> bool {
    let size: u32 = pairs
        .iter()
        .map(|&(lhs, _)| syntax.descendants(lhs) + 1)
        .sum();
    size >= TINY_TREE_THRESHOLD
}

/// Diffs the two sides' nodes linearly by content id. Runs of equal nodes big enough are
/// unchanged; what's between is possibly changed.
fn split_unchanged_toplevel(
    syntax: &Syntax,
    lhs: &[NodeId],
    rhs: &[NodeId],
) -> Vec<(ChangeState, Vec<NodeId>, Vec<NodeId>)> {
    let lhs_ids: Vec<u32> = lhs.iter().map(|&id| syntax[id].content_id).collect();
    let rhs_ids: Vec<u32> = rhs.iter().map(|&id| syntax[id].content_id).collect();
    let mut split = TopLevelSplit {
        syntax,
        result: Vec::new(),
        unchanged: Vec::new(),
        lhs_changed: Vec::new(),
        rhs_changed: Vec::new(),
    };
    for step in lcs::slice(&lhs_ids, &rhs_ids) {
        match step {
            DiffResult::Both(i, j) => split.unchanged.push((lhs[i], rhs[j])),
            DiffResult::Left(i) => {
                split.end_unchanged_run();
                split.lhs_changed.push(lhs[i]);
            }
            DiffResult::Right(j) => {
                split.end_unchanged_run();
                split.rhs_changed.push(rhs[j]);
            }
        }
    }
    split.end_unchanged_run();
    split.end_changed_run();
    split.result
}

/// The state of [`split_unchanged_toplevel`]: the parts so far, the current run of equal nodes
/// and the changed nodes before it.
struct TopLevelSplit<'a, 's> {
    syntax: &'a Syntax<'s>,
    result: Vec<(ChangeState, Vec<NodeId>, Vec<NodeId>)>,
    unchanged: Vec<(NodeId, NodeId)>,
    lhs_changed: Vec<NodeId>,
    rhs_changed: Vec<NodeId>,
}

impl TopLevelSplit<'_, '_> {
    /// Ends a run of equal nodes: a part of its own if it's big enough, after the changed nodes
    /// before it, or else one with those changed nodes.
    fn end_unchanged_run(&mut self) {
        if self.unchanged.is_empty() {
            return;
        }
        if is_big_enough(self.syntax, &self.unchanged) {
            self.end_changed_run();
            let (lhs, rhs) = self.unchanged.drain(..).unzip();
            self.result.push((ChangeState::UnchangedNode, lhs, rhs));
        } else {
            for (lhs, rhs) in self.unchanged.drain(..) {
                self.lhs_changed.push(lhs);
                self.rhs_changed.push(rhs);
            }
        }
    }

    fn end_changed_run(&mut self) {
        if self.lhs_changed.is_empty() && self.rhs_changed.is_empty() {
            return;
        }
        self.result.extend(split_changed_singleton_list(
            self.syntax,
            &self.lhs_changed,
            &self.rhs_changed,
        ));
        self.lhs_changed.clear();
        self.rhs_changed.clear();
    }
}

/// If each side is one list and their delimiters match, their children.
fn as_singleton_list_children<'a>(
    syntax: &'a Syntax,
    lhs: &[NodeId],
    rhs: &[NodeId],
) -> Option<(&'a [NodeId], &'a [NodeId])> {
    match (lhs, rhs) {
        (&[lhs], &[rhs]) if syntax.same_delimiters(lhs, rhs) => {
            Some((syntax.children(lhs), syntax.children(rhs)))
        }
        _ => None,
    }
}

/// If each side is one list and their delimiters match, shrinks their children's unchanged ends.
/// If anything inside was unchanged, marks the lists' delimiters unchanged and returns what's
/// left of their children.
fn shrink_unchanged_delimiters(
    syntax: &Syntax,
    lhs: &[NodeId],
    rhs: &[NodeId],
    changes: &mut Changes,
) -> (bool, Vec<NodeId>, Vec<NodeId>) {
    if let Some((lhs_children, rhs_children)) = as_singleton_list_children(syntax, lhs, rhs) {
        let (changed_later, lhs_shrunk, rhs_shrunk) =
            shrink_unchanged_at_ends(syntax, lhs_children, rhs_children, changes);
        if changed_later {
            changes.insert(lhs[0], Change::Unchanged(rhs[0]));
            changes.insert(rhs[0], Change::Unchanged(lhs[0]));
            return (true, lhs_shrunk, rhs_shrunk);
        }
    }
    (false, lhs.to_vec(), rhs.to_vec())
}

/// Marks the equal nodes the sides start and end with unchanged, going into a list both sides
/// are left with. Returns whether it marked anything, and the nodes left.
fn shrink_unchanged_at_ends(
    syntax: &Syntax,
    mut lhs: &[NodeId],
    mut rhs: &[NodeId],
    changes: &mut Changes,
) -> (bool, Vec<NodeId>, Vec<NodeId>) {
    let mut changed = false;
    // No size threshold here: an equal node at either end can't split apart regions that
    // belong together.
    while let (Some(&l), Some(&r)) = (lhs.first(), rhs.first()) {
        if !syntax.same_content(l, r) {
            break;
        }
        mark_both_unchanged(syntax, changes, l, r);
        changed = true;
        (lhs, rhs) = (&lhs[1..], &rhs[1..]);
    }
    while let (Some(&l), Some(&r)) = (lhs.last(), rhs.last()) {
        if !syntax.same_content(l, r) {
            break;
        }
        mark_both_unchanged(syntax, changes, l, r);
        changed = true;
        (lhs, rhs) = (&lhs[..lhs.len() - 1], &rhs[..rhs.len() - 1]);
    }

    if lhs.len() == 1 && rhs.len() == 1 {
        let (changed_later, lhs, rhs) = shrink_unchanged_delimiters(syntax, lhs, rhs, changes);
        (changed || changed_later, lhs, rhs)
    } else {
        (changed, lhs.to_vec(), rhs.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::nodes::sexp;

    /// `lhs` and `rhs` as S-expressions, with their info set.
    fn parse<'s>(syntax: &mut Syntax<'s>, lhs: &'s str, rhs: &'s str) -> (Vec<u32>, Vec<u32>) {
        let lhs = sexp(syntax, lhs);
        let rhs = sexp(syntax, rhs);
        syntax.init(&lhs, &rhs);
        (lhs, rhs)
    }

    #[test]
    fn test_shrink_unchanged_at_start() {
        let mut syntax = Syntax::default();
        let (lhs, rhs) = parse(&mut syntax, "unchanged A B", "unchanged X");
        let mut changes = Changes::new(&syntax);

        let (_, lhs_after, rhs_after) = shrink_unchanged_at_ends(&syntax, &lhs, &rhs, &mut changes);

        assert_eq!(changes.get(lhs[0]), Some(Change::Unchanged(rhs[0])));
        assert_eq!(changes.get(rhs[0]), Some(Change::Unchanged(lhs[0])));
        assert_eq!((lhs_after.len(), rhs_after.len()), (2, 1));
    }

    #[test]
    fn test_shrink_unchanged_at_end() {
        let mut syntax = Syntax::default();
        let (lhs, rhs) = parse(&mut syntax, "A B unchanged", "X unchanged");
        let mut changes = Changes::new(&syntax);

        let (_, lhs_after, rhs_after) = shrink_unchanged_at_ends(&syntax, &lhs, &rhs, &mut changes);

        assert_eq!(changes.get(lhs[2]), Some(Change::Unchanged(rhs[1])));
        assert_eq!(changes.get(rhs[1]), Some(Change::Unchanged(lhs[2])));
        assert_eq!((lhs_after.len(), rhs_after.len()), (2, 1));
    }

    #[test]
    fn test_shrink_unchanged_nested() {
        let mut syntax = Syntax::default();
        let (lhs, rhs) = parse(
            &mut syntax,
            "unchanged-before (more-unchanged (A))",
            "unchanged-before (more-unchanged (B))",
        );
        let mut changes = Changes::new(&syntax);

        let (_, lhs_after, rhs_after) = shrink_unchanged_at_ends(&syntax, &lhs, &rhs, &mut changes);

        // Only what's inside the innermost lists may have changed.
        assert_eq!(lhs_after.len(), 1);
        assert!(matches!(syntax[lhs_after[0]].shape, Shape::List { .. }));
        assert_eq!(rhs_after.len(), 1);
        assert!(matches!(syntax[rhs_after[0]].shape, Shape::List { .. }));
        assert_eq!(changes.get(lhs_after[0]), None);
        assert_eq!(changes.get(rhs_after[0]), None);
    }

    #[test]
    fn test_split_unchanged_toplevel_at_start() {
        let mut syntax = Syntax::default();
        // Bigger than TINY_TREE_THRESHOLD.
        let (lhs, rhs) = parse(
            &mut syntax,
            "(unchanged (1 2 3 4 5 6 7 8 9 10)) A B",
            "(unchanged (1 2 3 4 5 6 7 8 9 10)) X",
        );
        let mut changes = Changes::new(&syntax);

        let sections = split_unchanged(&syntax, &lhs, &rhs, &mut changes);

        assert_eq!(changes.get(lhs[0]), Some(Change::Unchanged(rhs[0])));
        assert_eq!(changes.get(rhs[0]), Some(Change::Unchanged(lhs[0])));
        assert_eq!(sections, [(vec![lhs[1], lhs[2]], vec![rhs[1]])]);
    }

    #[test]
    fn test_split_unchanged_toplevel_at_end() {
        let mut syntax = Syntax::default();
        let (lhs, rhs) = parse(
            &mut syntax,
            "A B (unchanged (1 2 3 4 5 6 7 8 9 10))",
            "X (unchanged (1 2 3 4 5 6 7 8 9 10))",
        );
        let mut changes = Changes::new(&syntax);

        let sections = split_unchanged(&syntax, &lhs, &rhs, &mut changes);

        assert_eq!(changes.get(lhs[2]), Some(Change::Unchanged(rhs[1])));
        assert_eq!(changes.get(rhs[1]), Some(Change::Unchanged(lhs[2])));
        assert_eq!(sections, [(vec![lhs[0], lhs[1]], vec![rhs[0]])]);
    }

    #[test]
    fn test_split_preserves_outer_delimiters() {
        let mut syntax = Syntax::default();
        let (lhs, rhs) = parse(&mut syntax, "(A)", "(B)");
        let mut changes = Changes::new(&syntax);

        let sections = split_unchanged(&syntax, &lhs, &rhs, &mut changes);

        // The lists stay whole, their delimiters undecided, for the graph search.
        assert_eq!(sections, [(lhs.clone(), rhs.clone())]);
        assert_eq!(changes.get(lhs[0]), None);
        assert_eq!(changes.get(rhs[0]), None);
    }

    #[test]
    fn test_split_unchanged_middle() {
        let mut syntax = Syntax::default();
        let (lhs, rhs) = parse(
            &mut syntax,
            "novel-lhs (unchanged (1 2 3 4 5 6 7 8 9 10)) novel-lhs-2",
            "novel-rhs (unchanged (1 2 3 4 5 6 7 8 9 10)) novel-rhs-2",
        );
        let mut changes = Changes::new(&syntax);

        let sections = split_unchanged(&syntax, &lhs, &rhs, &mut changes);

        assert_eq!(
            sections,
            [(vec![lhs[0]], vec![rhs[0]]), (vec![lhs[2]], vec![rhs[2]])]
        );
        assert_eq!(changes.get(lhs[1]), Some(Change::Unchanged(rhs[1])));
        assert_eq!(changes.get(rhs[1]), Some(Change::Unchanged(lhs[1])));
    }

    #[test]
    fn test_split_unchanged_multiple() {
        let mut syntax = Syntax::default();
        let (lhs, rhs) = parse(
            &mut syntax,
            "novel-lhs (unchanged-1 (1 2 3 4 5 6 7 8 9 10)) (unchanged-2 (1 2 3 4 5 6 7 8 9 10)) novel-lhs-2",
            "novel-rhs (unchanged-1 (1 2 3 4 5 6 7 8 9 10)) (unchanged-2 (1 2 3 4 5 6 7 8 9 10)) novel-rhs-2",
        );
        let mut changes = Changes::new(&syntax);

        let sections = split_unchanged(&syntax, &lhs, &rhs, &mut changes);

        assert_eq!(
            sections,
            [(vec![lhs[0]], vec![rhs[0]]), (vec![lhs[3]], vec![rhs[3]])]
        );
    }

    #[test]
    fn test_split_unchanged_outer_delimiter() {
        let mut syntax = Syntax::default();
        let (lhs, rhs) = parse(
            &mut syntax,
            "(novel-lhs-before (1 2 3 4 5 6 7 8 9 10) novel-lhs-after)",
            "(novel-rhs-before (1 2 3 4 5 6 7 8 9 10) novel-rhs-after)",
        );
        let mut changes = Changes::new(&syntax);

        let sections = split_unchanged(&syntax, &lhs, &rhs, &mut changes);

        assert_eq!(sections.len(), 2);
        assert_eq!(changes.get(lhs[0]), Some(Change::Unchanged(rhs[0])));
    }

    #[test]
    fn test_split_mostly_unchanged_toplevel() {
        let mut syntax = Syntax::default();
        let (lhs, rhs) = parse(
            &mut syntax,
            "(1 2 3 4 5 6 7 8 9 10) (91 92 93 94 95 96 97 98 99 100)",
            "(1 2 3 4 5 novel-1 6 7 8 9 10) (91 92 93 94 95 novel-2 96 97 98 99 100)",
        );

        assert_eq!(
            split_mostly_unchanged_toplevel(&syntax, &lhs, &rhs).len(),
            2
        );
    }

    #[test]
    fn test_count_common_unique() {
        let mut syntax = Syntax::default();
        // Two subtrees are unique on both sides and shared: shared-1 and (shared-2a shared-2b).
        let (lhs, rhs) = parse(
            &mut syntax,
            "(shared-1 (shared-2a shared-2b) not-unique not-unique)",
            "(shared-1 (shared-2a shared-2b) not-unique)",
        );

        assert_eq!(count_common_unique(&syntax, lhs[0], rhs[0]), 2);
    }

    #[test]
    fn test_similar_with_common_grandchildren() {
        let mut syntax = Syntax::default();
        let (lhs, rhs) = parse(
            &mut syntax,
            "((novel-lhs 1 2 3 4 5)) x",
            "((novel-rhs 1 2 3 4 5)) y",
        );

        assert_eq!(
            split_mostly_unchanged_toplevel(&syntax, &lhs, &rhs).len(),
            2
        );
    }

    #[test]
    fn test_similar_ignore_delimiter() {
        let mut syntax = Syntax::default();
        let (lhs, rhs) = parse(
            &mut syntax,
            "(novel-lhs 1 2 3 4 5) x",
            "[novel-rhs 1 2 3 4 5] y",
        );

        assert_eq!(
            split_mostly_unchanged_toplevel(&syntax, &lhs, &rhs).len(),
            2
        );
    }

    #[test]
    fn marks_everything_but_the_sections_left_to_diff() {
        let mut syntax = Syntax::default();
        let (lhs, rhs) = parse(
            &mut syntax,
            "(fn a (1 2 3 4 5 6 7 8 9 10)) (fn b (x)) (fn c (1 2 3 4 5 6 7 8 9 10))",
            "(fn a (1 2 3 4 5 6 7 8 9 10)) (fn b (y)) (fn c (1 2 3 4 5 6 7 8 9 10))",
        );
        let mut changes = Changes::new(&syntax);

        let sections = mark_unchanged(&syntax, &lhs, &rhs, &mut changes);

        // Down to the list around `x` and `y`.
        let [(lhs_section, rhs_section)] = &sections[..] else {
            panic!("{sections:?}");
        };
        let lhs_x = syntax.children(lhs[1])[2];
        let rhs_y = syntax.children(rhs[1])[2];
        assert_eq!(
            (lhs_section.as_slice(), rhs_section.as_slice()),
            (&[lhs_x][..], &[rhs_y][..])
        );
        assert_eq!(changes.get(lhs[1]), Some(Change::Unchanged(rhs[1])));
    }
}
