//! The syntax-aware diff: compares two versions of a file by their syntax trees rather than by
//! their lines, with [difftastic](https://github.com/Wilfred/difftastic)'s algorithm.
//!
//! [`compare`] takes both versions' text and tree-sitter trees, which the caller parses with
//! Zed's grammars, and the [`Language`] settings for their grammar. It returns rows of aligned
//! lines for a side-by-side view. Each row has a real line from either side or both, with the
//! byte ranges on it that are novel: tokens the other side doesn't have.
//!
//! ```
//! use diff_extras::syntax::{Language, Limits, Source, compare};
//!
//! let old = "let total = price;\n";
//! let new = "let total = price * quantity;\n";
//! let grammar: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
//! let mut parser = tree_sitter::Parser::new();
//! parser.set_language(&grammar).unwrap();
//! let (old_tree, new_tree) = (parser.parse(old, None), parser.parse(new, None));
//!
//! let diff = compare(
//!     Language::for_grammar("rust"),
//!     Source { text: old, tree: old_tree.as_ref() },
//!     Source { text: new, tree: new_tree.as_ref() },
//!     &Limits::default(),
//! )
//! .unwrap();
//!
//! let row = &diff.rows[0];
//! assert!(row.old.as_ref().unwrap().novel.is_empty());
//! // `price` matches; `*` and `quantity` are new.
//! let new_line = row.new.as_ref().unwrap();
//! assert_eq!(new_line.number, 1);
//! assert_eq!(new_line.novel, [18..19, 20..28]);
//! ```
//!
//! # How it works
//!
//! Each tree becomes difftastic's syntax nodes: atoms, and lists with their delimiters. Nodes
//! that are obviously unchanged are marked first, such as those both sides start and end with.
//! For the rest, the diff is the cheapest route through a graph whose edges mark nodes unchanged
//! or novel, found with Dijkstra's algorithm. Lines then pair up by their unchanged tokens.
//!
//! Past difftastic's limits, or with no grammar, [`compare`] returns a [`Fallback`] and the
//! caller keeps the line diff.

mod align;
mod changes;
mod dijkstra;
mod graph;
mod language;
mod lcs;
mod nodes;
mod parse;
mod positions;
mod unchanged;

use std::ops::Range;

use tree_sitter::Tree;

pub use language::Language;

use changes::Changes;
use nodes::Syntax;
use positions::{Lines, MatchKind, MatchedPos};

/// One version of a file.
#[derive(Clone, Copy, Debug)]
pub struct Source<'a> {
    /// The file's text.
    pub text: &'a str,
    /// The text's tree-sitter tree, or `None` if its language has no grammar.
    pub tree: Option<&'a Tree>,
}

/// Which version of the file: the old one, on the left, or the new one, on the right.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    Old,
    New,
}

/// How far [`compare`] goes before it gives up. The defaults are difftastic's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The most bytes either version may have.
    pub bytes: usize,
    /// The most places the search may reach in the diff graph of any one changed part of the
    /// file. Large parts with many changes reach the most.
    pub graph: usize,
    /// The most tree-sitter parse errors both versions may have between them. A tree with
    /// errors can make a misleading diff.
    pub parse_errors: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            bytes: 1_000_000,
            graph: 3_000_000,
            parse_errors: 0,
        }
    }
}

/// Why [`compare`] didn't compare a file by syntax, so the caller keeps the line diff.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fallback {
    /// The file's language has no grammar, or no [`Language`] settings.
    NoGrammar,
    /// A version of the file has more bytes than [`Limits::bytes`], or the diff would take
    /// more than [`Limits::graph`].
    TooLarge,
    /// The trees have more parse errors than [`Limits::parse_errors`].
    ParseErrors { count: usize },
}

/// The two versions of a file, aligned into rows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyntaxDiff {
    /// Every line of both versions, each once and in order.
    pub rows: Vec<Row>,
}

/// A row of the side-by-side view: a line of the old version, of the new one, or of both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub old: Option<Line>,
    pub new: Option<Line>,
}

/// A line of one version of the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    /// The line's 1-based number in its version of the file.
    pub number: u32,
    /// The byte ranges of the line, without its line ending, that are novel: tokens the other
    /// version doesn't have. In order, and neither overlapping nor touching.
    pub novel: Vec<Range<usize>>,
}

impl Row {
    /// The row's line on `side`, if it has one.
    pub fn line(&self, side: Side) -> Option<&Line> {
        match side {
            Side::Old => self.old.as_ref(),
            Side::New => self.new.as_ref(),
        }
    }
}

/// Compares two versions of a file by their syntax trees. `language` is the settings for their
/// grammar, from [`Language::for_grammar`].
pub fn compare(
    language: Option<&Language>,
    old: Source<'_>,
    new: Source<'_>,
    limits: &Limits,
) -> Result<SyntaxDiff, Fallback> {
    let (Some(language), Some(old_tree), Some(new_tree)) = (language, old.tree, new.tree) else {
        return Err(Fallback::NoGrammar);
    };
    if old.text.len() > limits.bytes || new.text.len() > limits.bytes {
        return Err(Fallback::TooLarge);
    }

    let mut syntax = Syntax::default();
    let lhs = parse::parse(&mut syntax, old.text, old_tree, language);
    let rhs = parse::parse(&mut syntax, new.text, new_tree, language);
    let errors = lhs.errors + rhs.errors;
    if errors > limits.parse_errors {
        return Err(Fallback::ParseErrors { count: errors });
    }
    let (lhs, rhs) = (lhs.roots, rhs.roots);
    syntax.init(&lhs, &rhs);

    let mut changes = Changes::new(&syntax);
    let sections = unchanged::mark_unchanged(&syntax, &lhs, &rhs, &mut changes);
    for (lhs_section, rhs_section) in sections {
        // Each part is diffed on its own, so its last top-level node has no next sibling.
        syntax.link_siblings(&lhs_section);
        syntax.link_siblings(&rhs_section);
        dijkstra::mark_syntax(
            &syntax,
            lhs_section.first().copied(),
            rhs_section.first().copied(),
            &mut changes,
            limits.graph,
        )
        .map_err(|_| Fallback::TooLarge)?;
    }

    let old_lines = Lines::new(old.text);
    let new_lines = Lines::new(new.text);
    let lhs_positions =
        positions::change_positions(&syntax, &lhs, &changes, &old_lines, &new_lines);
    let rhs_positions =
        positions::change_positions(&syntax, &rhs, &changes, &new_lines, &old_lines);
    Ok(rows(&lhs_positions, &rhs_positions, &old_lines, &new_lines))
}

/// The aligned rows, with each line's novel ranges.
fn rows(
    lhs_positions: &[MatchedPos],
    rhs_positions: &[MatchedPos],
    old_lines: &Lines,
    new_lines: &Lines,
) -> SyntaxDiff {
    let old_novel = novel_ranges(lhs_positions, old_lines.count());
    let new_novel = novel_ranges(rhs_positions, new_lines.count());
    let line = |line: Option<u32>, novel: &[Vec<Range<usize>>]| {
        // The alignment can place a line just past the end of an empty file.
        let ranges = novel.get(line? as usize)?;
        Some(Line {
            number: line? + 1,
            novel: ranges.clone(),
        })
    };
    let pairs = align::all_matched_lines_filled(lhs_positions, rhs_positions, old_lines, new_lines);
    let rows = pairs
        .into_iter()
        .filter_map(|(old, new)| {
            let row = Row {
                old: line(old, &old_novel),
                new: line(new, &new_novel),
            };
            (row.old.is_some() || row.new.is_some()).then_some(row)
        })
        .collect();
    SyntaxDiff { rows }
}

/// Each line's novel byte ranges, sorted and merged.
fn novel_ranges(positions: &[MatchedPos], line_count: u32) -> Vec<Vec<Range<usize>>> {
    let mut novel = vec![Vec::new(); line_count as usize];
    for mp in positions {
        if mp.kind == MatchKind::Novel
            && mp.pos.start < mp.pos.end
            && let Some(ranges) = novel.get_mut(mp.pos.line as usize)
        {
            ranges.push(mp.pos.start..mp.pos.end);
        }
    }
    for ranges in &mut novel {
        ranges.sort_by_key(|range| range.start);
        let mut merged: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
        for range in ranges.drain(..) {
            match merged.last_mut() {
                Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
                _ => merged.push(range),
            }
        }
        *ranges = merged;
    }
    novel
}
