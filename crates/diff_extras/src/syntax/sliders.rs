// Ported from difftastic (https://github.com/Wilfred/difftastic), commit 87aad1a:
//   src/diff/sliders.rs
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

//! Chooses, among equally good diffs, the novel nodes a reader expects.
//!
//! A slider is a run of novel nodes that could sit in more than one place. With `A B` on the old
//! side and `A B` twice on the new, either pair can be the novel one; the diff search may mark
//! the first `B` and the second `A`, which is correct but hard to read. This moves such runs so
//! novel nodes sit together. Line diffs have the same problem; see
//! [diff-slider-tools](https://github.com/mhagger/diff-slider-tools).
//!
//! A nested slider is a list added around another: `old1(old2)` becoming `old1(new1(old2))` adds
//! either `new1(…)` or `(new1…)`. In C-like languages the inner delimiters should be the novel
//! ones; in JSON, where the outer list matters more, the outer ones.

use super::changes::{Change, Changes};
use super::nodes::{NodeId, Shape, Syntax};
use super::positions::Lines;

/// Fixes the sliders among one side's nodes, from its top-level nodes `nodes`. `lines` is the
/// side's source.
pub(crate) fn fix_all_sliders(
    syntax: &Syntax,
    nodes: &[NodeId],
    changes: &mut Changes,
    lines: &Lines,
    prefer_outer_delimiter: bool,
) {
    let mut sliders = Sliders {
        syntax,
        changes,
        lines,
    };
    // Some sliders take two steps to fix; more are left alone.
    sliders.fix_all_one_step(nodes);
    sliders.fix_all_one_step(nodes);
    for &node in nodes {
        if prefer_outer_delimiter {
            sliders.fix_nested_prefer_outer(node);
        } else {
            sliders.fix_nested_prefer_inner(node);
        }
    }
}

struct Sliders<'a, 's> {
    syntax: &'a Syntax<'s>,
    changes: &'a mut Changes,
    lines: &'a Lines<'a>,
}

impl Sliders<'_, '_> {
    fn change(&self, node: NodeId) -> Change {
        self.changes
            .get(node)
            .expect("the diff marks every node before fixing sliders")
    }

    fn fix_all_one_step(&mut self, nodes: &[NodeId]) {
        for &node in nodes {
            self.fix_all_one_step(self.syntax.children(node));
        }
        for (start, end) in self.novel_regions_after_unchanged(nodes) {
            self.slide_to_prev_node(nodes, start, end);
        }
        for (start, end) in self.novel_regions_before_unchanged(nodes) {
            self.slide_to_next_node(nodes, start, end);
        }
    }

    /// The first and last indices of each run of novel nodes right after an unchanged one.
    fn novel_regions_after_unchanged(&self, nodes: &[NodeId]) -> Vec<(usize, usize)> {
        let mut regions = Vec::new();
        // `Some` after an unchanged node, with the run of novel nodes since, if any.
        let mut region: Option<Option<(usize, usize)>> = None;
        for (i, &node) in nodes.iter().enumerate() {
            match self.change(node) {
                Change::Unchanged(_) => {
                    regions.extend(region.flatten());
                    region = Some(None);
                }
                Change::Novel => {
                    if let Some(run) = &mut region {
                        *run = Some((run.map_or(i, |(start, _)| start), i));
                    }
                }
                Change::ReplacedComment(_) | Change::ReplacedString(_) => {
                    regions.extend(region.take().flatten());
                }
                Change::IgnoredPunctuation => {}
            }
        }
        regions.extend(region.flatten());
        regions
    }

    /// The first and last indices of each run of novel nodes before an unchanged node or the end.
    fn novel_regions_before_unchanged(&self, nodes: &[NodeId]) -> Vec<(usize, usize)> {
        let mut regions = Vec::new();
        let mut region: Option<(usize, usize)> = None;
        for (i, &node) in nodes.iter().enumerate() {
            match self.change(node) {
                Change::Unchanged(_) => regions.extend(region.take()),
                Change::Novel => region = Some((region.map_or(i, |(start, _)| start), i)),
                Change::ReplacedComment(_) | Change::ReplacedString(_) => region = None,
                Change::IgnoredPunctuation => {}
            }
        }
        regions.extend(region);
        regions
    }

    /// Whether a node and everything below it is novel.
    fn is_novel_deep(&self, node: NodeId) -> bool {
        self.change(node) == Change::Novel
            && self
                .syntax
                .children(node)
                .iter()
                .all(|&child| self.is_novel_deep(child))
    }

    /// If the unchanged node before the run equals its last node, and is closer to its first,
    /// marks that node novel and the last node unchanged instead:
    ///
    /// ```text
    /// x UNCHANGED      x NOVEL
    /// y NOVEL     ->   y NOVEL
    /// x NOVEL          x UNCHANGED
    /// ```
    fn slide_to_prev_node(&mut self, nodes: &[NodeId], start: usize, end: usize) {
        if start == 0 || start == end {
            return;
        }
        let (start_node, last_node) = (nodes[start], nodes[end]);
        let (before_start, before_last) = (nodes[start - 1], nodes[end - 1]);
        if !self.syntax.same_content(before_start, last_node) {
            return;
        }
        if self.distance_between(before_start, start_node)
            > self.distance_between(before_last, last_node)
        {
            return;
        }
        let Change::Unchanged(opposite) = self.change(before_start) else {
            return;
        };
        if !self.syntax.same_content(before_start, opposite)
            || !nodes[start..=end]
                .iter()
                .all(|&node| self.is_novel_deep(node))
        {
            return;
        }
        self.changes.insert_deep_novel(self.syntax, before_start);
        self.changes
            .insert_deep_unchanged(self.syntax, last_node, opposite);
        self.changes
            .insert_deep_unchanged(self.syntax, opposite, last_node);
    }

    /// If the unchanged node after the run equals its first node, and is closer to its last,
    /// marks that node novel and the first node unchanged instead:
    ///
    /// ```text
    /// x NOVEL          x UNCHANGED
    /// y NOVEL     ->   y NOVEL
    /// x UNCHANGED      x NOVEL
    /// ```
    fn slide_to_next_node(&mut self, nodes: &[NodeId], start: usize, end: usize) {
        if end == nodes.len() - 1 || start == end {
            return;
        }
        let (start_node, last_node) = (nodes[start], nodes[end]);
        let (after_start, after_last) = (nodes[start + 1], nodes[end + 1]);
        if !self.syntax.same_content(after_last, start_node) {
            return;
        }
        if self.distance_between(last_node, after_last)
            >= self.distance_between(start_node, after_start)
        {
            return;
        }
        let Change::Unchanged(opposite) = self.change(after_last) else {
            return;
        };
        if !self.syntax.same_content(after_last, opposite)
            || !nodes[start..=end]
                .iter()
                .all(|&node| self.is_novel_deep(node))
        {
            return;
        }
        self.changes
            .insert_deep_unchanged(self.syntax, start_node, opposite);
        self.changes
            .insert_deep_unchanged(self.syntax, opposite, start_node);
        self.changes.insert_deep_novel(self.syntax, after_last);
    }

    /// How far `next` starts after `prev` ends: lines apart, or else columns apart.
    fn distance_between(&self, prev: NodeId, next: NodeId) -> (u32, usize) {
        let prev_end = self.lines.position(self.last_offset(prev));
        let next_start = self.lines.position(self.first_offset(next));
        if prev_end.0 == next_start.0 {
            (0, next_start.1.saturating_sub(prev_end.1))
        } else {
            (next_start.0.saturating_sub(prev_end.0), 0)
        }
    }

    fn first_offset(&self, node: NodeId) -> usize {
        match &self.syntax[node].shape {
            Shape::List { open, .. } => open.span.start,
            Shape::Atom { token, .. } => token.span.start,
        }
    }

    fn last_offset(&self, node: NodeId) -> usize {
        match &self.syntax[node].shape {
            Shape::List { close, .. } => close.span.end,
            Shape::Atom { token, .. } => token.span.end,
        }
    }

    /// For a list whose delimiters are novel and which has one unchanged descendant, a list with
    /// the same delimiters, marks the inner list's delimiters novel and the outer one's unchanged
    /// instead, so `old1(novel(old2()))` shows `novel(…)` as added.
    fn fix_nested_prefer_inner(&mut self, node: NodeId) {
        if self.change(node) == Change::Novel {
            let mut found = Vec::new();
            self.unchanged_descendants(self.syntax.children(node), &mut found);
            if let [inner] = found[..]
                && matches!(self.syntax[inner].shape, Shape::List { .. })
            {
                self.move_match(inner, node);
            }
        }
        for &child in self.syntax.children(node) {
            self.fix_nested_prefer_inner(child);
        }
    }

    /// For a list whose delimiters are unchanged and which has one unchanged descendant, a list
    /// with the same delimiters but novel ones itself, marks the outer list's delimiters novel and
    /// the inner one's unchanged instead, so `(old-1 (novel (old-2)))` shows `(novel …)` as added.
    fn fix_nested_prefer_outer(&mut self, node: NodeId) {
        if matches!(self.change(node), Change::Unchanged(_)) {
            let mut candidates = Vec::new();
            self.unchanged_descendants_for_outer_slider(
                self.syntax.children(node),
                &mut candidates,
            );
            if let [inner] = candidates[..]
                && matches!(self.syntax[inner].shape, Shape::List { .. })
                && self.change(inner) == Change::Novel
            {
                self.move_match(node, inner);
            }
        }
        for &child in self.syntax.children(node) {
            self.fix_nested_prefer_outer(child);
        }
    }

    /// Moves the match of list `from`'s delimiters to list `to`, which becomes unchanged while
    /// `from` becomes novel, if their delimiters are the same. The other side's matched list
    /// then points at `to`, so lines still pair up by it.
    fn move_match(&mut self, from: NodeId, to: NodeId) {
        if !self.syntax.same_delimiters(from, to) {
            return;
        }
        let change = self.change(from);
        self.changes.insert(to, change);
        self.changes.insert(from, Change::Novel);
        if let Change::Unchanged(opposite) = change {
            self.changes.insert(opposite, Change::Unchanged(to));
        }
    }

    /// The unchanged nodes among `nodes` and below novel ones, stopping once there are two.
    fn unchanged_descendants(&self, nodes: &[NodeId], found: &mut Vec<NodeId>) {
        if found.len() > 1 {
            return;
        }
        for &node in nodes {
            match self.change(node) {
                Change::Unchanged(_) => found.push(node),
                Change::Novel | Change::ReplacedComment(_) | Change::ReplacedString(_) => {
                    self.unchanged_descendants(self.syntax.children(node), found);
                }
                Change::IgnoredPunctuation => {}
            }
        }
    }

    /// The candidates for an outer slider among `nodes` and below: novel lists with unchanged
    /// children. An unchanged node is added too, and ends the search, since its delimiters can't
    /// slide. Stops once there are two.
    fn unchanged_descendants_for_outer_slider(&self, nodes: &[NodeId], found: &mut Vec<NodeId>) {
        if found.len() > 1 {
            return;
        }
        for &node in nodes {
            if matches!(self.change(node), Change::Unchanged(_)) {
                found.push(node);
                break;
            }
            if let Shape::List { children, .. } = &self.syntax[node].shape {
                let has_unchanged_children = children
                    .iter()
                    .any(|&child| matches!(self.change(child), Change::Unchanged(_)));
                if has_unchanged_children {
                    found.push(node);
                } else {
                    self.unchanged_descendants_for_outer_slider(children, found);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::nodes::{AtomKind, Token, sexp};

    fn atom<'s>(syntax: &mut Syntax<'s>, src: &'s str, start: usize) -> NodeId {
        syntax.atom(Token::new(&src[start..start + 1], start), AtomKind::Comment)
    }

    /// Slides at the start when the unchanged node is closer than the last novel one.
    #[test]
    fn test_slider_at_start() {
        let mut syntax = Syntax::default();
        // `a` at column 0 and `b` at column 10 of line 0, `a` at column 3 of line 1.
        let lhs_src = "a         b\n   a";
        let lhs = [
            atom(&mut syntax, lhs_src, 0),
            atom(&mut syntax, lhs_src, 10),
            atom(&mut syntax, lhs_src, 15),
        ];
        let rhs = [atom(&mut syntax, "a", 0)];
        syntax.init(&lhs, &rhs);
        let mut changes = Changes::new(&syntax);
        changes.insert(lhs[0], Change::Unchanged(rhs[0]));
        changes.insert(lhs[1], Change::Novel);
        changes.insert(lhs[2], Change::Novel);

        fix_all_sliders(&syntax, &lhs, &mut changes, &Lines::new(lhs_src), false);

        assert_eq!(changes.get(lhs[0]), Some(Change::Novel));
        assert_eq!(changes.get(lhs[1]), Some(Change::Novel));
        assert_eq!(changes.get(lhs[2]), Some(Change::Unchanged(rhs[0])));
        assert_eq!(changes.get(rhs[0]), Some(Change::Unchanged(lhs[2])));
    }

    /// Slides at the end when the unchanged node is closer than the first novel one.
    #[test]
    fn test_slider_at_end() {
        let mut syntax = Syntax::default();
        // `a` at column 0 of line 0, `b` at column 10 and `a` at column 12 of line 1.
        let lhs_src = "a\n          b a";
        let lhs = [
            atom(&mut syntax, lhs_src, 0),
            atom(&mut syntax, lhs_src, 12),
            atom(&mut syntax, lhs_src, 14),
        ];
        let rhs = [atom(&mut syntax, "a", 0)];
        syntax.init(&lhs, &rhs);
        let mut changes = Changes::new(&syntax);
        changes.insert(lhs[0], Change::Novel);
        changes.insert(lhs[1], Change::Novel);
        changes.insert(lhs[2], Change::Unchanged(rhs[0]));

        fix_all_sliders(&syntax, &lhs, &mut changes, &Lines::new(lhs_src), false);

        assert_eq!(changes.get(rhs[0]), Some(Change::Unchanged(lhs[0])));
        assert_eq!(changes.get(lhs[0]), Some(Change::Unchanged(rhs[0])));
        assert_eq!(changes.get(lhs[1]), Some(Change::Novel));
        assert_eq!(changes.get(lhs[2]), Some(Change::Novel));
    }

    #[test]
    fn test_slider_two_steps() {
        let mut syntax = Syntax::default();
        let rhs_src = "A B X\n A B";
        let lhs = sexp(&mut syntax, "A B");
        let rhs = sexp(&mut syntax, rhs_src);
        syntax.init(&lhs, &rhs);
        let mut changes = Changes::new(&syntax);
        changes.insert(rhs[0], Change::Unchanged(lhs[0]));
        changes.insert(rhs[1], Change::Unchanged(lhs[1]));
        for &node in &rhs[2..] {
            changes.insert(node, Change::Novel);
        }

        fix_all_sliders(&syntax, &rhs, &mut changes, &Lines::new(rhs_src), false);

        assert_eq!(changes.get(rhs[0]), Some(Change::Novel));
        assert_eq!(changes.get(rhs[1]), Some(Change::Novel));
        assert_eq!(changes.get(rhs[2]), Some(Change::Novel));
        assert_eq!(changes.get(rhs[3]), Some(Change::Unchanged(lhs[0])));
        assert_eq!(changes.get(rhs[4]), Some(Change::Unchanged(lhs[1])));
    }

    /// A list that's partly unchanged, with some novel children, doesn't slide.
    #[test]
    fn test_slider_partially_unchanged() {
        let mut syntax = Syntax::default();
        let lhs_src = "(A B) X \n (A B)";
        let lhs = sexp(&mut syntax, lhs_src);
        let rhs = sexp(&mut syntax, "((novel) A B)");
        syntax.init(&lhs, &rhs);
        let lhs_children = syntax.children(lhs[0]).to_vec();
        let rhs_children = syntax.children(rhs[0]).to_vec();
        let mut changes = Changes::new(&syntax);
        changes.insert(lhs[0], Change::Unchanged(rhs[0]));
        changes.insert(lhs[1], Change::Novel);
        changes.insert_deep_novel(&syntax, lhs[2]);
        changes.insert(lhs_children[0], Change::Unchanged(rhs_children[1]));
        changes.insert(lhs_children[1], Change::Unchanged(rhs_children[2]));

        fix_all_sliders(&syntax, &lhs, &mut changes, &Lines::new(lhs_src), false);

        assert_eq!(
            changes.get(lhs[2]),
            Some(Change::Novel),
            "the novel node at the end is left alone"
        );
    }

    #[test]
    fn a_nested_slider_prefers_the_inner_delimiters() {
        let mut syntax = Syntax::default();
        let lhs = sexp(&mut syntax, "(x)");
        let rhs = sexp(&mut syntax, "(new (x))");
        syntax.init(&lhs, &rhs);
        let outer = rhs[0];
        let [new, inner] = syntax.children(outer)[..] else {
            panic!()
        };
        let x = syntax.children(inner)[0];
        let mut changes = Changes::new(&syntax);
        // The search matched the old list with the new inner one.
        changes.insert(outer, Change::Novel);
        changes.insert(new, Change::Novel);
        changes.insert_deep_unchanged(&syntax, inner, lhs[0]);
        changes.insert_deep_unchanged(&syntax, lhs[0], inner);
        let rhs_src = "(new (x))";

        fix_all_sliders(&syntax, &rhs, &mut changes, &Lines::new(rhs_src), false);

        assert_eq!(changes.get(outer), Some(Change::Unchanged(lhs[0])));
        assert_eq!(changes.get(inner), Some(Change::Novel));
        assert_eq!(
            changes.get(x),
            Some(Change::Unchanged(syntax.children(lhs[0])[0]))
        );
        assert_eq!(changes.get(lhs[0]), Some(Change::Unchanged(outer)));
    }
}
