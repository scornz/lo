//! Finds blocks of code that a pull request moves: deleted in one place and added in another, in
//! the same file or another file of the PR.
//!
//! [`Moves::find`] takes every file's changed lines, as a line diff produces them, and returns the
//! moves. Each [`Move`] says where its block came from and where it landed, for the "moved from
//! `api/user.ts:40`" link at each end, and lists the lines edited on the way, so the diff view
//! highlights only those. [`Moves::of_deleted`] and [`Moves::of_added`] find the move a line
//! belongs to as the view draws it.
//!
//! ```
//! use diff_extras::moved::{EditedLine, FileChanges, Moves};
//!
//! let files = [
//!     FileChanges::new("api/user.ts").delete(40, [
//!         "function fullName(user: User): string {",
//!         "  const first = user.first.trim();",
//!         "  const middle = user.middle?.trim();",
//!         "  const last = user.last.trim();",
//!         "  return [first, middle, last].filter(Boolean).join(\" \");",
//!         "}",
//!     ]),
//!     // Moved into a class, so one level deeper, with its return edited.
//!     FileChanges::new("api/names.ts").add(12, [
//!         "  fullName(user: User): string {",
//!         "    const first = user.first.trim();",
//!         "    const middle = user.middle?.trim();",
//!         "    const last = user.last.trim();",
//!         "    return [first, middle, last].filter((name) => name).join(\" \");",
//!         "  }",
//!     ]),
//! ];
//! let moves = Moves::find(&files);
//!
//! let moved = moves.of_added(1, 14).unwrap();
//! assert_eq!(moved.from.path, "api/user.ts");
//! assert_eq!(moved.from.lines, 41..=45);
//! assert_eq!(moved.to.lines, 13..=17);
//! assert_eq!(moved.edits, [EditedLine { old: 44, new: 16 }]);
//! assert_eq!(moved.edit_of_new(16), Some(EditedLine { old: 44, new: 16 }));
//! assert_eq!(moves.of_deleted(0, 42), Some(moved));
//! ```
//!
//! The function's first line changed too, so the move starts at the line after it: a move starts
//! and ends with lines that match.
//!
//! # What counts as a move
//!
//! Lines are compared with their leading and trailing whitespace trimmed, so re-indented code still
//! matches. Blank lines are left out: they can sit inside a block, in any number on either side,
//! but never count toward one.
//!
//! Consecutive deleted lines of a file form a run, and so do consecutive added lines. A lineup
//! pairs the lines of a deleted run with the lines of an added run, one for one from some offset,
//! skipping blank lines. Each pair matches or is an edited line. A block of a lineup is a move
//! when:
//!
//! 1. it starts and ends with matching lines;
//! 2. at least 3 of its matching lines have a letter or digit, so lines of only punctuation, such
//!    as `}` or `});`, can sit inside a move but can't make one;
//! 3. at least two thirds of its lines match; and
//! 4. no more than 2 edited lines come in a row.
//!
//! Going down a lineup, a block starts at each matching line not already in a block and runs to the
//! furthest matching line that keeps two thirds of its lines matching.
//!
//! Unlike git's `--color-moved`, which only matches lines exactly, a move can carry small edits.
//! Edits pair lines one for one, though: a line added or removed inside a moved block shifts the
//! lineup, so the block becomes a move on each side of it, each held to the rule on its own.
//!
//! A deleted run and an added run between the same two unchanged lines of a file are never lined
//! up. That code was changed in place, say re-indented or wrapped in a new block, not moved.
//!
//! # Competing moves
//!
//! Each line belongs to at most one move, and longer moves win. Candidates are taken by how many of
//! their matching lines have a letter or digit, then by how many lines match, then by fewest edits,
//! then by where they land and where they came from, earliest first. A candidate that overlaps one
//! already taken loses the overlap, and what's left is held to the rule again. A block added in two
//! places is one move, to the first.
//!
//! A line repeated all over a PR, such as `return Ok(());`, doesn't start a lineup: a text deleted
//! D times and added A times starts one only when D × A is at most 64. It still matches inside a
//! lineup another line started, so a block made only of such lines isn't found, but a long run of
//! them moves with the distinct lines around it.

use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashMap};
use std::ops::{Range, RangeInclusive};

/// The fewest matching lines with a letter or digit a move has.
const MIN_LINES: usize = 3;

/// The most edited lines a move has in a row.
const MAX_EDITS_IN_A_ROW: usize = 2;

/// The most lineups one text starts. A text deleted D times and added A times would start D × A of
/// them, so a line repeated all over a PR, such as `return Ok(());`, starts none. It still matches
/// inside a lineup that another line started. Without a limit, a line repeated thousands of times
/// would take time and memory that grow with the square of its count.
const MAX_LINEUPS_PER_TEXT: usize = 64;

/// One file's changed lines, as a line diff produces them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileChanges {
    /// The file's path after the change.
    pub path: String,
    /// The file's path before the change, when the PR renames it.
    pub old_path: Option<String>,
    /// Deleted lines, numbered as in the old file.
    pub deleted: Vec<Line>,
    /// Added lines, numbered as in the new file.
    pub added: Vec<Line>,
}

/// A deleted or added line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    /// The line's 1-based number in its version of the file.
    pub number: u32,
    /// The line's text, without its line ending.
    pub text: String,
}

impl FileChanges {
    /// A file at `path` with no changed lines yet. Add them with [`Self::delete`] and
    /// [`Self::add`], or push to [`Self::deleted`] and [`Self::added`]. Lines can come in any
    /// order.
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            ..Self::default()
        }
    }

    /// Marks the file as renamed from `old_path`.
    pub fn renamed_from(mut self, old_path: impl Into<String>) -> Self {
        self.old_path = Some(old_path.into());
        self
    }

    /// Adds consecutive deleted lines, the first at old line `start`.
    pub fn delete(mut self, start: u32, lines: impl IntoIterator<Item = impl AsRef<str>>) -> Self {
        self.deleted.extend(numbered(start, lines));
        self
    }

    /// Adds consecutive added lines, the first at new line `start`.
    pub fn add(mut self, start: u32, lines: impl IntoIterator<Item = impl AsRef<str>>) -> Self {
        self.added.extend(numbered(start, lines));
        self
    }

    /// The file's path before the change.
    pub fn old_path(&self) -> &str {
        self.old_path.as_deref().unwrap_or(&self.path)
    }
}

fn numbered(
    start: u32,
    lines: impl IntoIterator<Item = impl AsRef<str>>,
) -> impl Iterator<Item = Line> {
    (start..).zip(lines).map(|(number, text)| Line {
        number,
        text: text.as_ref().to_string(),
    })
}

/// The moves among a PR's changes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Moves {
    /// By where they land.
    moves: Vec<Move>,
    /// Indices into `moves`, by where they came from.
    by_from: Vec<usize>,
}

/// A block of lines deleted in one place and added in another.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Move {
    /// Where the block was deleted, in old line numbers.
    pub from: Place,
    /// Where the block was added, in new line numbers.
    pub to: Place,
    /// The lines edited on the way, as pairs of an old line and the new line it became, in order.
    /// Every other line of the block that isn't blank moved unchanged, apart from its indentation.
    pub edits: Vec<EditedLine>,
}

/// One end of a move.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    /// The file's index in the changes passed to [`Moves::find`].
    pub file: usize,
    /// The file's path on this side: its old path for [`Move::from`], its new path for
    /// [`Move::to`].
    pub path: String,
    /// The block's first and last lines. Both are lines that aren't blank.
    pub lines: RangeInclusive<u32>,
}

/// A line of a moved block that was edited on the way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditedLine {
    /// The line's number in the old file.
    pub old: u32,
    /// The number of the line it became in the new file.
    pub new: u32,
}

impl Moves {
    /// Finds the moves among `files`, the changes of every file in the PR. [`Place::file`] in the
    /// result is an index into `files`.
    pub fn find(files: &[FileChanges]) -> Self {
        let mut texts = Texts::default();
        let old = Side::new(files, |file| &file.deleted, &mut texts);
        let new = Side::new(files, |file| &file.added, &mut texts);
        let finder = Finder {
            old: &old,
            new: &new,
        };
        let mut moves: Vec<Move> = finder
            .choose(finder.candidates(texts.len()))
            .iter()
            .map(|candidate| finder.to_move(candidate, files))
            .collect();
        moves.sort_by_key(|moved| moved.to.key());
        let mut by_from: Vec<usize> = (0..moves.len()).collect();
        by_from.sort_by_key(|&index| moves[index].from.key());
        Self { moves, by_from }
    }

    /// Every move, ordered by where it lands: by file, then by line.
    pub fn all(&self) -> &[Move] {
        &self.moves
    }

    /// The move that deleted line `line`, by its old number, of the file at index `file` belongs
    /// to.
    pub fn of_deleted(&self, file: usize, line: u32) -> Option<&Move> {
        let after = self
            .by_from
            .partition_point(|&index| self.moves[index].from.key() <= (file, line));
        let moved = &self.moves[*self.by_from.get(after.checked_sub(1)?)?];
        moved.from.contains(file, line).then_some(moved)
    }

    /// The move that added line `line`, by its new number, of the file at index `file` belongs
    /// to.
    pub fn of_added(&self, file: usize, line: u32) -> Option<&Move> {
        let after = self
            .moves
            .partition_point(|moved| moved.to.key() <= (file, line));
        let moved = self.moves.get(after.checked_sub(1)?)?;
        moved.to.contains(file, line).then_some(moved)
    }
}

impl Move {
    /// The edit of old line `line`, if it's in the block and was edited on the way.
    pub fn edit_of_old(&self, line: u32) -> Option<EditedLine> {
        let index = self
            .edits
            .binary_search_by_key(&line, |edit| edit.old)
            .ok()?;
        Some(self.edits[index])
    }

    /// The edit that made new line `line`, if it's in the block and was edited on the way.
    pub fn edit_of_new(&self, line: u32) -> Option<EditedLine> {
        let index = self
            .edits
            .binary_search_by_key(&line, |edit| edit.new)
            .ok()?;
        Some(self.edits[index])
    }
}

impl Place {
    fn key(&self) -> (usize, u32) {
        (self.file, *self.lines.start())
    }

    fn contains(&self, file: usize, line: u32) -> bool {
        self.file == file && self.lines.contains(&line)
    }
}

/// Gives each distinct line text, trimmed, a number, so lines compare as numbers.
#[derive(Default)]
struct Texts<'a> {
    ids: HashMap<&'a str, u32>,
}

impl<'a> Texts<'a> {
    fn id(&mut self, text: &'a str) -> u32 {
        let next = self.ids.len() as u32;
        *self.ids.entry(text).or_insert(next)
    }

    fn len(&self) -> usize {
        self.ids.len()
    }
}

/// One side of every file's changes, the deleted or the added lines, without blank lines.
struct Side {
    /// The lines that aren't blank, by file, then by number.
    lines: Vec<Entry>,
    runs: Vec<Run>,
}

#[derive(Clone, Copy)]
struct Entry {
    /// The trimmed text's number in [`Texts`].
    id: u32,
    /// Whether the line has a letter or digit, so it counts toward a move's length. Lines of only
    /// punctuation, such as `}` or `});`, can sit inside a move but don't make one.
    counts: bool,
    number: u32,
    /// The index of its run in [`Side::runs`].
    run: usize,
}

/// Consecutive deleted or added lines: lines a move can line up with one another.
struct Run {
    file: usize,
    /// Where the run sits among the file's unchanged lines: one more than how many come before it.
    /// A deleted run and an added run of a file with the same place are a change in place.
    place: i64,
    /// The run's lines that aren't blank, as indices into [`Side::lines`].
    lines: Range<usize>,
}

impl Side {
    fn new<'a>(
        files: &'a [FileChanges],
        lines_of: fn(&FileChanges) -> &Vec<Line>,
        texts: &mut Texts<'a>,
    ) -> Self {
        let mut side = Side {
            lines: Vec::new(),
            runs: Vec::new(),
        };
        for (file, changes) in files.iter().enumerate() {
            let mut lines: Vec<&'a Line> = lines_of(changes).iter().collect();
            lines.sort_by_key(|line| line.number);
            lines.dedup_by_key(|line| line.number);
            let mut first = 0;
            while first < lines.len() {
                let mut end = first + 1;
                while end < lines.len() && lines[end].number == lines[end - 1].number + 1 {
                    end += 1;
                }
                let start = side.lines.len();
                let run = side.runs.len();
                for line in &lines[first..end] {
                    let text = line.text.trim();
                    if !text.is_empty() {
                        side.lines.push(Entry {
                            id: texts.id(text),
                            counts: text.chars().any(char::is_alphanumeric),
                            number: line.number,
                            run,
                        });
                    }
                }
                if side.lines.len() > start {
                    side.runs.push(Run {
                        file,
                        place: i64::from(lines[first].number) - first as i64,
                        lines: start..side.lines.len(),
                    });
                }
                first = end;
            }
        }
        side
    }

    /// Where each text appears on this side: `positions[starts[id]..starts[id + 1]]`.
    fn index(&self, texts: usize) -> Index {
        let mut starts = vec![0; texts + 1];
        for line in &self.lines {
            starts[line.id as usize + 1] += 1;
        }
        for id in 0..texts {
            starts[id + 1] += starts[id];
        }
        let mut next = starts.clone();
        let mut positions = vec![0; self.lines.len()];
        for (position, line) in self.lines.iter().enumerate() {
            positions[next[line.id as usize]] = position;
            next[line.id as usize] += 1;
        }
        Index { starts, positions }
    }
}

struct Index {
    starts: Vec<usize>,
    positions: Vec<usize>,
}

impl Index {
    fn get(&self, id: u32) -> &[usize] {
        &self.positions[self.starts[id as usize]..self.starts[id as usize + 1]]
    }
}

/// A block of added lines, lined up with the deleted lines `delta` positions away in
/// [`Side::lines`].
#[derive(Clone, Debug, PartialEq, Eq)]
struct Candidate {
    new: Range<usize>,
    delta: isize,
    /// How many of its matching lines have a letter or digit: its length.
    length: usize,
    /// How many of its lines match.
    matched: usize,
    /// How many of its lines were edited.
    edited: usize,
}

impl Candidate {
    fn old(&self) -> Range<usize> {
        self.new.start.wrapping_add_signed(self.delta)..self.new.end.wrapping_add_signed(self.delta)
    }

    /// Longer blocks first, then the one with more matching lines of punctuation, then the one
    /// with fewer edits, then the one that lands first, then the one that came from first. The end
    /// only makes the order total, so it agrees with `Eq`.
    fn rank(&self) -> impl Ord {
        (
            self.length,
            self.matched,
            Reverse(self.edited),
            Reverse(self.new.start),
            Reverse(self.old().start),
            Reverse(self.new.end),
        )
    }
}

impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> Ordering {
        self.rank().cmp(&other.rank())
    }
}

impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A pair of an added line and a deleted line with the same text: where a lineup starts.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Seed {
    old_run: usize,
    new_run: usize,
    delta: isize,
    new: usize,
}

struct Finder<'a> {
    old: &'a Side,
    new: &'a Side,
}

impl Finder<'_> {
    fn candidates(&self, texts: usize) -> Vec<Candidate> {
        let index = self.old.index(texts);
        let mut added = vec![0; texts];
        for line in &self.new.lines {
            added[line.id as usize] += 1;
        }
        let mut seeds = Vec::new();
        // Every move has a matching line that counts, so only those start a lineup.
        for (new, line) in self
            .new
            .lines
            .iter()
            .enumerate()
            .filter(|(_, line)| line.counts)
        {
            let olds = index.get(line.id);
            if olds.len() * added[line.id as usize] > MAX_LINEUPS_PER_TEXT {
                continue;
            }
            let new_run = &self.new.runs[line.run];
            for &old in olds {
                let old_run = &self.old.runs[self.old.lines[old].run];
                if (old_run.file, old_run.place) == (new_run.file, new_run.place) {
                    continue;
                }
                seeds.push(Seed {
                    old_run: self.old.lines[old].run,
                    new_run: line.run,
                    delta: old as isize - new as isize,
                    new,
                });
            }
        }
        seeds.sort_unstable();

        let mut candidates = Vec::new();
        let mut walked: Option<(Seed, usize)> = None;
        for seed in seeds {
            if let Some((last, end)) = walked
                && (last.old_run, last.new_run, last.delta)
                    == (seed.old_run, seed.new_run, seed.delta)
                && seed.new < end
            {
                continue;
            }
            let new = self.walk(seed);
            walked = Some((seed, new.end));
            self.blocks(new, seed.delta, &mut candidates);
        }
        candidates
    }

    /// The candidate moves among the added lines `new`, lined up with the deleted lines `delta`
    /// away. `new` has no more than [`MAX_EDITS_IN_A_ROW`] edited lines in a row.
    ///
    /// Going down the lineup, a block starts at each matching line not already in one and runs to
    /// the furthest matching line that keeps at least two thirds of its lines matching. It's a
    /// candidate when at least [`MIN_LINES`] of its matching lines have a letter or digit.
    fn blocks(&self, new: Range<usize>, delta: isize, out: &mut impl Extend<Candidate>) {
        // Running totals before each line. A matching line scores 1 and an edited line -2, so a
        // stretch matches on two thirds of its lines when the score where it ends is at least the
        // score where it starts.
        let mut score = vec![0_isize];
        let mut matched = vec![0];
        let mut length = vec![0];
        for line in new.clone() {
            let same = self.same(line, delta);
            let counts = same && self.new.lines[line].counts;
            score.push(score[score.len() - 1] + if same { 1 } else { -2 });
            matched.push(matched[matched.len() - 1] + usize::from(same));
            length.push(length[length.len() - 1] + usize::from(counts));
        }
        let is_match = |at: usize| matched[at + 1] > matched[at];
        // The highest score at or after each position, which never rises along the lineup.
        let mut highest = score.clone();
        for at in (0..new.len()).rev() {
            highest[at] = highest[at].max(highest[at + 1]);
        }

        let mut start = 0;
        while start < new.len() {
            if !is_match(start) {
                start += 1;
                continue;
            }
            let reach = highest[start + 1..].partition_point(|&high| high >= score[start]);
            let mut end = start + reach;
            while !is_match(end - 1) {
                end -= 1;
            }
            if length[end] - length[start] < MIN_LINES {
                start += 1;
                continue;
            }
            let lines = matched[end] - matched[start];
            out.extend([Candidate {
                new: new.start + start..new.start + end,
                delta,
                length: length[end] - length[start],
                matched: lines,
                edited: end - start - lines,
            }]);
            start = end;
        }
    }

    /// Takes candidates longest first, giving each line to at most one move. A candidate that
    /// overlaps one already taken is put back without the overlap, as each stretch left over.
    fn choose(&self, candidates: Vec<Candidate>) -> Vec<Candidate> {
        let mut queue = BinaryHeap::from(candidates);
        let mut old_taken = vec![false; self.old.lines.len()];
        let mut new_taken = vec![false; self.new.lines.len()];
        let mut chosen = Vec::new();
        while let Some(candidate) = queue.pop() {
            let delta = candidate.delta;
            let free = |new: usize| !new_taken[new] && !old_taken[new.wrapping_add_signed(delta)];
            if candidate.new.clone().all(free) {
                for new in candidate.new.clone() {
                    new_taken[new] = true;
                    old_taken[new.wrapping_add_signed(delta)] = true;
                }
                chosen.push(candidate);
                continue;
            }
            let mut start = candidate.new.start;
            while start < candidate.new.end {
                if !free(start) {
                    start += 1;
                    continue;
                }
                let mut end = start + 1;
                while end < candidate.new.end && free(end) {
                    end += 1;
                }
                self.blocks(start..end, delta, &mut queue);
                start = end;
            }
        }
        chosen
    }

    /// The added lines that line up with deleted lines `seed.delta` away, from the start of both
    /// runs to their end.
    fn lane(&self, seed: Seed) -> Range<usize> {
        let old = &self.old.runs[seed.old_run].lines;
        let new = &self.new.runs[seed.new_run].lines;
        let start = new.start.max(old.start.saturating_add_signed(-seed.delta));
        let end = new.end.min(old.end.saturating_add_signed(-seed.delta));
        start..end
    }

    fn same(&self, new: usize, delta: isize) -> bool {
        self.new.lines[new].id == self.old.lines[new.wrapping_add_signed(delta)].id
    }

    /// The lines around `seed` along its lineup, out to the last matching lines before more than
    /// [`MAX_EDITS_IN_A_ROW`] edited lines in a row, or the end of either run.
    fn walk(&self, seed: Seed) -> Range<usize> {
        let lane = self.lane(seed);
        let mut start = seed.new;
        let mut at = seed.new;
        while at > lane.start {
            at -= 1;
            if self.same(at, seed.delta) {
                start = at;
            } else if start - at > MAX_EDITS_IN_A_ROW {
                break;
            }
        }
        let mut end = seed.new + 1;
        for at in seed.new + 1..lane.end {
            if self.same(at, seed.delta) {
                end = at + 1;
            } else if at + 1 - end > MAX_EDITS_IN_A_ROW {
                break;
            }
        }
        start..end
    }

    fn to_move(&self, candidate: &Candidate, files: &[FileChanges]) -> Move {
        let old = candidate.old();
        let new = candidate.new.clone();
        let from_file = self.old.runs[self.old.lines[old.start].run].file;
        let to_file = self.new.runs[self.new.lines[new.start].run].file;
        let edits = new
            .clone()
            .zip(old.clone())
            .filter(|&(new, old)| self.new.lines[new].id != self.old.lines[old].id)
            .map(|(new, old)| EditedLine {
                old: self.old.lines[old].number,
                new: self.new.lines[new].number,
            })
            .collect();
        Move {
            from: Place {
                file: from_file,
                path: files[from_file].old_path().to_string(),
                lines: self.old.lines[old.start].number..=self.old.lines[old.end - 1].number,
            },
            to: Place {
                file: to_file,
                path: files[to_file].path.clone(),
                lines: self.new.lines[new.start].number..=self.new.lines[new.end - 1].number,
            },
            edits,
        }
    }
}
