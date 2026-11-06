// Ported from difftastic (https://github.com/Wilfred/difftastic), commit 87aad1a:
//   src/display/context.rs
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

//! Lines up the two sides' lines into rows, from the lines their unchanged tokens match.
//!
//! Lines whose tokens match pair up first, in order. Lines with only novel tokens slot in
//! between, blank lines before a pair pair up too, and every line left over is placed in order,
//! paired with a lone line on the other side where one is next to it.

use std::cmp::Ordering;

use super::positions::{Lines, MatchKind, MatchedPos};

/// A row: a 0-based line on either side, or both.
pub(crate) type LinePair = (Option<u32>, Option<u32>);

/// Every line of both sides, aligned into rows. Each line appears once, in order.
pub(crate) fn all_matched_lines_filled(
    lhs_mps: &[MatchedPos],
    rhs_mps: &[MatchedPos],
    lhs_lines: &Lines,
    rhs_lines: &Lines,
) -> Vec<LinePair> {
    let matched_lines = add_ends(
        &all_matched_lines(lhs_mps, rhs_mps),
        lhs_lines.count(),
        rhs_lines.count(),
    );
    compact_gaps(&ensure_contiguous(&match_preceding_blanks(
        &matched_lines,
        lhs_lines,
        rhs_lines,
    )))
}

/// Adds the lines before the first matched line and after the last on each side, such as blank
/// lines at the start or end, which have no tokens.
fn add_ends(matched_lines: &[LinePair], lhs_count: u32, rhs_count: u32) -> Vec<LinePair> {
    let lhs_min = matched_lines.iter().find_map(|pair| pair.0);
    let rhs_min = matched_lines.iter().find_map(|pair| pair.1);
    let lhs_max = matched_lines.iter().rev().find_map(|pair| pair.0);
    let rhs_max = matched_lines.iter().rev().find_map(|pair| pair.1);

    let mut result = Vec::new();
    if let (Some(lhs_min), Some(rhs_min)) = (lhs_min, rhs_min) {
        fill(&mut result, 0..lhs_min, 0..rhs_min);
    }
    result.extend_from_slice(matched_lines);
    if let (Some(lhs_max), Some(rhs_max)) = (lhs_max, rhs_max) {
        fill(
            &mut result,
            lhs_max + 1..lhs_count.max(lhs_max + 1),
            rhs_max + 1..rhs_count.max(rhs_max + 1),
        );
    }
    result
}

/// Pairs up `lhs` and `rhs` line by line, then the leftover lines of the longer one alone.
fn fill(result: &mut Vec<LinePair>, lhs: std::ops::Range<u32>, rhs: std::ops::Range<u32>) {
    let mut lhs = lhs.peekable();
    let mut rhs = rhs.peekable();
    while let (Some(_), Some(_)) = (lhs.peek(), rhs.peek()) {
        result.push((lhs.next(), rhs.next()));
    }
    result.extend(lhs.map(|line| (Some(line), None)));
    result.extend(rhs.map(|line| (None, Some(line))));
}

fn all_matched_lines(lhs_mps: &[MatchedPos], rhs_mps: &[MatchedPos]) -> Vec<LinePair> {
    let lhs_matched_lines = matched_lines_from_unchanged(lhs_mps);
    let lines = merge_in_opposite_lines(&lhs_matched_lines, &all_lines(rhs_mps));
    flip(&merge_in_opposite_lines(&flip(&lines), &all_lines(lhs_mps)))
}

/// Every line with a position on it, in order.
fn all_lines(mps: &[MatchedPos]) -> Vec<u32> {
    let mut lines: Vec<u32> = mps.iter().map(|mp| mp.pos.line).collect();
    lines.sort_unstable();
    lines.dedup();
    lines
}

/// Pairs each line that has an unchanged token with the line its match is on, keeping both
/// sides in order: a line pairs only with a later line than the last pair's.
fn matched_lines_from_unchanged(mps: &[MatchedPos]) -> Vec<LinePair> {
    let mut highest_line: Option<u32> = None;
    let mut highest_opposite_line: Option<u32> = None;
    let mut result = Vec::new();
    for mp in mps {
        let MatchKind::Unchanged {
            opposite: (first, last),
        } = mp.kind
        else {
            continue;
        };
        let opposite_line = match highest_opposite_line {
            Some(highest) => (highest < last).then(|| first.max(highest + 1)),
            None => Some(first),
        };
        let is_later = highest_line.is_none_or(|highest| mp.pos.line > highest);
        if let Some(opposite_line) = opposite_line
            && is_later
        {
            result.push((Some(mp.pos.line), Some(opposite_line)));
            highest_line = Some(mp.pos.line);
            highest_opposite_line = Some(opposite_line);
        }
    }
    result
}

/// Adds the other side's `novel_opposite_lines` that aren't in a pair yet, each before the first
/// pair after it.
fn merge_in_opposite_lines(
    matched_lines: &[LinePair],
    novel_opposite_lines: &[u32],
) -> Vec<LinePair> {
    let mut result = Vec::new();
    let mut i = 0;
    for &(line, opposite_line) in matched_lines {
        if let Some(opposite_line) = opposite_line {
            while let Some(&novel) = novel_opposite_lines.get(i) {
                match novel.cmp(&opposite_line) {
                    Ordering::Less => result.push((None, Some(novel))),
                    Ordering::Equal => {}
                    Ordering::Greater => break,
                }
                i += 1;
            }
        }
        result.push((line, opposite_line));
    }
    result.extend(
        novel_opposite_lines[i..]
            .iter()
            .map(|&novel| (None, Some(novel))),
    );
    result
}

fn flip(pairs: &[LinePair]) -> Vec<LinePair> {
    pairs.iter().map(|&(a, b)| (b, a)).collect()
}

fn is_blank(lines: &Lines, line: u32) -> bool {
    lines.text(line).is_empty()
}

/// The blank lines right before `current` on both sides, back to `prev` if given, paired up.
fn match_blanks_before(
    current: (u32, u32),
    prev: Option<(u32, u32)>,
    lhs_lines: &Lines,
    rhs_lines: &Lines,
) -> Vec<LinePair> {
    let (mut lhs, mut rhs) = current;
    let mut result = Vec::new();
    while lhs > 0 && rhs > 0 {
        (lhs, rhs) = (lhs - 1, rhs - 1);
        if let Some((prev_lhs, prev_rhs)) = prev
            && (lhs <= prev_lhs || rhs <= prev_rhs)
        {
            break;
        }
        if !is_blank(lhs_lines, lhs) || !is_blank(rhs_lines, rhs) {
            break;
        }
        result.push((Some(lhs), Some(rhs)));
    }
    result.reverse();
    result
}

/// Before each pair, pairs up the blank lines that come right before it on both sides. This
/// lines things up much better. Blank lines after a pair need nothing: the lines left over are
/// placed side by side anyway (see [`ensure_contiguous`]).
fn match_preceding_blanks(
    line_nums: &[LinePair],
    lhs_lines: &Lines,
    rhs_lines: &Lines,
) -> Vec<LinePair> {
    let mut prev_lhs = None;
    let mut prev_rhs = None;
    let mut result = Vec::new();
    for &(lhs, rhs) in line_nums {
        if let (Some(lhs), Some(rhs)) = (lhs, rhs) {
            let prev = prev_lhs.zip(prev_rhs);
            // Only before the first pair, or between pairs.
            if prev.is_some() || (prev_lhs.is_none() && prev_rhs.is_none()) {
                result.extend(match_blanks_before((lhs, rhs), prev, lhs_lines, rhs_lines));
            }
        }
        result.push((lhs, rhs));
        prev_lhs = lhs.or(prev_lhs);
        prev_rhs = rhs.or(prev_rhs);
    }
    result
}

/// Adds the lines missing between those already placed, each on its own row:
///
/// ```text
/// 1 11        1 11
/// 3 14   ->   2 --
/// 4 --        - 12
///             - 13
///             3 14
///             4 --
/// ```
fn ensure_contiguous(lines: &[LinePair]) -> Vec<LinePair> {
    let mut result = Vec::new();
    let mut lhs_prev: Option<u32> = None;
    let mut rhs_prev: Option<u32> = None;
    for &(lhs, rhs) in lines {
        if let (Some(lhs), Some(prev)) = (lhs, lhs_prev) {
            result.extend((prev + 1..lhs).map(|line| (Some(line), None)));
        }
        if let (Some(rhs), Some(prev)) = (rhs, rhs_prev) {
            result.extend((prev + 1..rhs).map(|line| (None, Some(line))));
        }
        lhs_prev = lhs.or(lhs_prev);
        rhs_prev = rhs.or(rhs_prev);
        result.push((lhs, rhs));
    }
    result
}

/// Pairs lone lines on one side with lone lines on the other next to them:
///
/// ```text
/// 10 --        10 20
/// 11 --   ->   11 --
/// -- 20        12 21
/// 12 21
/// ```
fn compact_gaps(items: &[LinePair]) -> Vec<LinePair> {
    let mut result = Vec::new();
    // The latest lone lines, all on the same side.
    let mut unpaired: std::collections::VecDeque<LinePair> = std::collections::VecDeque::new();
    for &(lhs, rhs) in items {
        match (lhs, rhs) {
            (Some(lhs), None) => match unpaired.front() {
                Some(&(None, Some(rhs))) => {
                    result.push((Some(lhs), Some(rhs)));
                    unpaired.pop_front();
                }
                _ => unpaired.push_back((Some(lhs), None)),
            },
            (None, Some(rhs)) => match unpaired.front() {
                Some(&(Some(lhs), None)) => {
                    result.push((Some(lhs), Some(rhs)));
                    unpaired.pop_front();
                }
                _ => unpaired.push_back((None, Some(rhs))),
            },
            _ => {
                result.extend(unpaired.drain(..));
                result.push((lhs, rhs));
            }
        }
    }
    result.extend(unpaired);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::positions::LineSpan;

    fn pos(line: u32) -> LineSpan {
        LineSpan {
            line,
            start: 2,
            end: 3,
        }
    }

    fn unchanged(line: u32, opposite: u32) -> MatchedPos {
        MatchedPos {
            kind: MatchKind::Unchanged {
                opposite: (opposite, opposite),
            },
            pos: pos(line),
        }
    }

    fn novel(line: u32) -> MatchedPos {
        MatchedPos {
            kind: MatchKind::Novel,
            pos: pos(line),
        }
    }

    #[test]
    fn test_all_matched_lines() {
        let lhs = [novel(0), unchanged(1, 1)];
        let rhs = [unchanged(1, 1)];

        assert_eq!(
            all_matched_lines(&lhs, &rhs),
            [(Some(0), None), (Some(1), Some(1))]
        );
    }

    #[test]
    fn test_matched_lines_novel_on_same_line() {
        assert_eq!(
            matched_lines_from_unchanged(&[novel(1), unchanged(1, 1)]),
            [(Some(1), Some(1))]
        );
    }

    #[test]
    fn test_all_lines() {
        assert_eq!(
            all_lines(&[unchanged(0, 0), unchanged(1, 1), novel(2)]),
            [0, 1, 2]
        );
    }

    #[test]
    fn test_merge_in_opposite_lines() {
        assert_eq!(
            merge_in_opposite_lines(&[(Some(1), Some(1))], &[0, 3]),
            [(None, Some(0)), (Some(1), Some(1)), (None, Some(3))]
        );
    }

    #[test]
    fn test_compact_gaps() {
        assert_eq!(
            compact_gaps(&[(Some(0), None), (None, Some(0)), (Some(1), Some(1))]),
            [(Some(0), Some(0)), (Some(1), Some(1))]
        );
    }

    #[test]
    fn test_compact_gaps_with_larger_gap() {
        assert_eq!(
            compact_gaps(&[
                (Some(0), None),
                (Some(1), None),
                (None, Some(0)),
                (None, Some(1)),
            ]),
            [(Some(0), Some(0)), (Some(1), Some(1))]
        );
    }

    #[test]
    fn test_match_preceding_blanks() {
        let lhs = Lines::new("x\n\n\ny");
        let rhs = Lines::new("x\nextra\n\n\ny");

        assert_eq!(
            match_preceding_blanks(&[(Some(0), Some(0)), (Some(3), Some(4))], &lhs, &rhs),
            [
                (Some(0), Some(0)),
                (Some(1), Some(2)),
                (Some(2), Some(3)),
                (Some(3), Some(4)),
            ]
        );
    }

    #[test]
    fn test_match_preceding_blanks_first_pair() {
        let lhs = Lines::new("\n\ny");
        let rhs = Lines::new("extra\n\n\ny");

        assert_eq!(
            match_preceding_blanks(&[(Some(2), Some(3))], &lhs, &rhs),
            [(Some(0), Some(1)), (Some(1), Some(2)), (Some(2), Some(3))]
        );
    }

    #[test]
    fn test_match_blanks_between() {
        let lhs = Lines::new("x\n\n\ny");
        let rhs = Lines::new("x\nextra\n\n\ny");

        assert_eq!(
            match_blanks_before((3, 4), Some((0, 0)), &lhs, &rhs),
            [(Some(1), Some(2)), (Some(2), Some(3))]
        );
    }

    #[test]
    fn test_matched_lines_blank_at_ends() {
        let lines = Lines::new("\nfoo\n\n");
        let mps = [unchanged(1, 1)];

        assert_eq!(
            all_matched_lines_filled(&mps, &mps, &lines, &lines),
            [(Some(0), Some(0)), (Some(1), Some(1)), (Some(2), Some(2))]
        );
    }

    #[test]
    fn test_ensure_contiguous() {
        assert_eq!(
            ensure_contiguous(&[(Some(1), Some(11)), (Some(3), Some(14)), (Some(4), None)]),
            [
                (Some(1), Some(11)),
                (Some(2), None),
                (None, Some(12)),
                (None, Some(13)),
                (Some(3), Some(14)),
                (Some(4), None),
            ]
        );
    }
}
