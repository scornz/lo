// Ported from difftastic (https://github.com/Wilfred/difftastic), commit 87aad1a:
//   src/parse/syntax.rs (MatchedPos, change_positions)
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

//! Where each side's tokens sit, line by line, and whether each is novel or matched with lines on
//! the other side: what the line alignment and the novel ranges are built from.

use std::ops::Range;

use super::changes::{Change, Changes};
use super::nodes::{NodeId, Shape, Span, Syntax, Token};
use super::words::{self, Word};

/// A token's part on one line: a 0-based line and byte columns within it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LineSpan {
    pub(crate) line: u32,
    pub(crate) start: usize,
    pub(crate) end: usize,
}

impl LineSpan {
    fn is_empty(&self) -> bool {
        self.start == self.end
    }
}

/// What the diff says about a token's part on a line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MatchKind {
    /// Matched with a token on the other side, which covers these lines, first to last.
    Unchanged { opposite: (u32, u32) },
    /// Only on this side.
    Novel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MatchedPos {
    pub(crate) kind: MatchKind,
    pub(crate) pos: LineSpan,
}

/// One side's source, split into lines.
pub(crate) struct Lines<'s> {
    src: &'s str,
    /// Where each line starts, including an empty last line after a final line ending.
    starts: Vec<usize>,
}

impl<'s> Lines<'s> {
    pub(crate) fn new(src: &'s str) -> Self {
        let starts = std::iter::once(0)
            .chain(src.match_indices('\n').map(|(at, _)| at + 1))
            .collect();
        Self { src, starts }
    }

    /// How many lines the file has. A final line ending doesn't start another line.
    pub(crate) fn count(&self) -> u32 {
        if self.src.is_empty() {
            0
        } else if self.src.ends_with('\n') {
            self.starts.len() as u32 - 1
        } else {
            self.starts.len() as u32
        }
    }

    /// A line's text, without its line ending. Empty past the end of the file.
    pub(crate) fn text(&self, line: u32) -> &'s str {
        match self.starts.get(line as usize) {
            Some(&start) => &self.src[start..self.end(line)],
            None => "",
        }
    }

    /// Where a line's text ends, before its `\n` or `\r\n`.
    fn end(&self, line: u32) -> usize {
        match self.starts.get(line as usize + 1) {
            Some(&next) => {
                let end = next - 1;
                if self.src[..end].ends_with('\r') {
                    end - 1
                } else {
                    end
                }
            }
            None => self.src.len(),
        }
    }

    /// The line a byte offset is on.
    fn line_of(&self, offset: usize) -> u32 {
        (self.starts.partition_point(|&start| start <= offset) - 1) as u32
    }

    /// The 0-based line and byte column of a byte offset.
    pub(crate) fn position(&self, offset: usize) -> (u32, usize) {
        let line = self.line_of(offset);
        (line, offset - self.starts[line as usize])
    }

    /// A span's part on each line it covers.
    pub(crate) fn spans(&self, span: Span) -> Vec<LineSpan> {
        (self.line_of(span.start)..=self.line_of(span.end))
            .map(|line| {
                let start = self.starts[line as usize];
                let end = self.end(line);
                LineSpan {
                    line,
                    start: span.start.saturating_sub(start).min(end - start),
                    end: span.end.min(end).saturating_sub(start),
                }
            })
            .collect()
    }
}

/// Every token's position on one side, in order, from its top-level nodes `roots`. `lines` is
/// this side's source and `opposite_lines` the other's.
pub(crate) fn change_positions(
    syntax: &Syntax,
    roots: &[NodeId],
    changes: &Changes,
    lines: &Lines,
    opposite_lines: &Lines,
) -> Vec<MatchedPos> {
    let mut walk = Walk {
        syntax,
        changes,
        lines,
        opposite_lines,
        positions: Vec::new(),
        seen_unchanged: false,
    };
    walk.nodes(roots);
    // With nothing unchanged, a zero-width match at the start of both files gives the alignment
    // something to start from.
    if !walk.seen_unchanged {
        walk.positions.insert(
            0,
            MatchedPos {
                kind: MatchKind::Unchanged { opposite: (0, 0) },
                pos: LineSpan {
                    line: 0,
                    start: 0,
                    end: 0,
                },
            },
        );
    }
    walk.positions
}

struct Walk<'a, 's> {
    syntax: &'a Syntax<'s>,
    changes: &'a Changes,
    lines: &'a Lines<'a>,
    opposite_lines: &'a Lines<'a>,
    positions: Vec<MatchedPos>,
    seen_unchanged: bool,
}

/// Which of a node's tokens a position is for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Part {
    Open,
    Close,
    Atom,
}

impl Walk<'_, '_> {
    fn nodes(&mut self, nodes: &[NodeId]) {
        for &node in nodes {
            let change = self.changes.get(node).expect("the diff marks every node");
            if matches!(change, Change::Unchanged(_)) {
                self.seen_unchanged = true;
            }
            match &self.syntax[node].shape {
                Shape::List {
                    open,
                    children,
                    close,
                    ..
                } => {
                    self.token(change, open.span, Part::Open);
                    self.nodes(children);
                    self.token(change, close.span, Part::Close);
                }
                Shape::Atom { token, .. } => self.atom(change, token),
            }
        }
    }

    fn atom(&mut self, change: Change, token: &Token) {
        // A replaced comment or string shows word by word, if it has enough words in common
        // with the other side.
        if let Change::ReplacedComment(opposite) | Change::ReplacedString(opposite) = change
            && let Shape::Atom {
                token: opposite_token,
                ..
            } = &self.syntax[opposite].shape
            && let Some(words) = words::split_atom_words(token.text, opposite_token.text)
        {
            for word in words {
                match word {
                    Word::Novel(range) => {
                        let pos = self.lines.spans(within(token.span, range));
                        self.novel(&pos);
                    }
                    Word::Unchanged { range, opposite } => {
                        let pos = self.lines.spans(within(token.span, range));
                        let opposite_pos = self
                            .opposite_lines
                            .spans(within(opposite_token.span, opposite));
                        self.unchanged(&pos, &opposite_pos);
                    }
                }
            }
            return;
        }
        self.token(change, token.span, Part::Atom);
    }

    fn token(&mut self, change: Change, span: Span, part: Part) {
        let pos = self.lines.spans(span);
        // Leave out empty parts at either end, but keep them in the middle: a multi-line string
        // can have empty lines.
        let pos = without_empty_ends(&pos);
        match change {
            Change::Unchanged(opposite) => {
                let opposite_span = match (&self.syntax[opposite].shape, part) {
                    (Shape::List { close, .. }, Part::Close) => close.span,
                    (Shape::List { open, .. }, _) => open.span,
                    (Shape::Atom { token, .. }, _) => token.span,
                };
                let opposite_pos = self.opposite_lines.spans(opposite_span);
                self.unchanged(pos, &opposite_pos);
            }
            Change::Novel | Change::ReplacedComment(_) | Change::ReplacedString(_) => {
                self.novel(pos);
            }
            Change::IgnoredPunctuation => {}
        }
    }

    /// Adds a token's parts as matched with `opposite_pos`, the opposite token's parts: as many
    /// parts as the opposite token has, so the two sides' matches pair up.
    fn unchanged(&mut self, pos: &[LineSpan], opposite_pos: &[LineSpan]) {
        let first = opposite_pos[0].line;
        let last = opposite_pos[opposite_pos.len() - 1].line;
        for &pos in pos.iter().take(opposite_pos.len()) {
            self.positions.push(MatchedPos {
                kind: MatchKind::Unchanged {
                    opposite: (first, last),
                },
                pos,
            });
        }
    }

    fn novel(&mut self, pos: &[LineSpan]) {
        for &pos in pos {
            self.positions.push(MatchedPos {
                kind: MatchKind::Novel,
                pos,
            });
        }
    }
}

/// The part of `span` at `range`, relative to its start.
fn within(span: Span, range: Range<usize>) -> Span {
    Span::new(span.start + range.start, span.start + range.end)
}

fn without_empty_ends(spans: &[LineSpan]) -> &[LineSpan] {
    let mut spans = spans;
    if let [first, rest @ ..] = spans
        && first.is_empty()
    {
        spans = rest;
    }
    if let [rest @ .., last] = spans
        && last.is_empty()
    {
        spans = rest;
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_a_span_into_lines() {
        let lines = Lines::new("ab\ncd\r\nef");

        assert_eq!(lines.count(), 3);
        assert_eq!(
            [lines.text(0), lines.text(1), lines.text(2), lines.text(3)],
            ["ab", "cd", "ef", ""]
        );
        assert_eq!(
            lines.spans(Span::new(1, 8)),
            [
                LineSpan {
                    line: 0,
                    start: 1,
                    end: 2
                },
                LineSpan {
                    line: 1,
                    start: 0,
                    end: 2
                },
                LineSpan {
                    line: 2,
                    start: 0,
                    end: 1
                },
            ]
        );
    }

    #[test]
    fn a_final_line_ending_starts_no_line() {
        assert_eq!(Lines::new("a\n").count(), 1);
        assert_eq!(Lines::new("a\n\n").count(), 2);
        assert_eq!(Lines::new("").count(), 0);
        assert_eq!(Lines::new("\n").count(), 1);
    }

    #[test]
    fn drops_empty_parts_only_at_the_ends() {
        let empty = |line| LineSpan {
            line,
            start: 0,
            end: 0,
        };
        let full = |line| LineSpan {
            line,
            start: 0,
            end: 3,
        };

        assert_eq!(
            without_empty_ends(&[empty(0), full(1), empty(2), full(3), empty(4)]),
            [full(1), empty(2), full(3)]
        );
    }
}
