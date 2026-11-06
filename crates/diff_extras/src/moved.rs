//! Finds blocks of code that a pull request moves: deleted in one place and added in another, in
//! the same file or another file of the PR.

use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashMap};
use std::ops::{Range, RangeInclusive};

/// The fewest matching lines with a letter or digit a move has.
const MIN_LINES: usize = 3;

/// The most edited lines a move has in a row.
const MAX_EDITS_IN_A_ROW: usize = 2;

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
    /// The file's path on this side: its old path for [`Move::from`], its new path for [`Move::to`].
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
    /// Finds the moves among `files`, the changes of every file in the PR.
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
    /// with fewer edits, then the one that lands first, then the one that came from first.
    fn rank(&self) -> impl Ord {
        (
            self.length,
            self.matched,
            Reverse(self.edited),
            Reverse(self.new.start),
            Reverse(self.old().start),
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
        let mut seeds = Vec::new();
        // Every move has a matching line that counts, so only those start a lineup.
        for (new, line) in self
            .new
            .lines
            .iter()
            .enumerate()
            .filter(|(_, line)| line.counts)
        {
            let new_run = &self.new.runs[line.run];
            for &old in index.get(line.id) {
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
    /// A stretch that starts and ends with matching lines, matches on at least [`MIN_LINES`] lines
    /// with a letter or digit, and matches on at least two thirds of its lines is a candidate. A
    /// stretch short of two thirds gives way to its longest stretch that isn't, and what's either
    /// side of that is tried again.
    fn blocks(&self, new: Range<usize>, delta: isize, out: &mut impl Extend<Candidate>) {
        let mut stretches = vec![new];
        while let Some(stretch) = stretches.pop() {
            let stretch = self.trim(stretch, delta);
            let (mut length, mut matched) = (0, 0);
            for line in stretch.clone().filter(|&new| self.same(new, delta)) {
                matched += 1;
                length += usize::from(self.new.lines[line].counts);
            }
            if length < MIN_LINES {
                continue;
            }
            let edited = stretch.len() - matched;
            if matched >= 2 * edited {
                out.extend([Candidate {
                    new: stretch,
                    delta,
                    length,
                    matched,
                    edited,
                }]);
                continue;
            }
            let longest = self.longest_two_thirds(stretch.clone(), delta);
            stretches.push(stretch.start..longest.start);
            stretches.push(longest.end..stretch.end);
            stretches.push(longest);
        }
    }

    /// `new` without the edited lines at either end.
    fn trim(&self, mut new: Range<usize>, delta: isize) -> Range<usize> {
        while !new.is_empty() && !self.same(new.start, delta) {
            new.start += 1;
        }
        while !new.is_empty() && !self.same(new.end - 1, delta) {
            new.end -= 1;
        }
        new
    }

    /// The longest stretch of `new` that matches on at least two thirds of its lines. `new` starts
    /// with a matching line, so it's never empty.
    fn longest_two_thirds(&self, new: Range<usize>, delta: isize) -> Range<usize> {
        // A matching line scores 1 and an edited line -2, so a stretch matches on two thirds of its
        // lines when its score is at least 0: when the running score where it ends is at least the
        // running score where it starts. Find the furthest-apart such pair of positions.
        let mut score = Vec::with_capacity(new.len() + 1);
        score.push(0_isize);
        for line in new.clone() {
            let step = if self.same(line, delta) { 1 } else { -2 };
            score.push(score[score.len() - 1] + step);
        }
        // The lowest score at or before each position, and the highest at or after it.
        let mut lowest = score.clone();
        for i in 1..lowest.len() {
            lowest[i] = lowest[i].min(lowest[i - 1]);
        }
        let mut highest = score;
        for i in (0..highest.len() - 1).rev() {
            highest[i] = highest[i].max(highest[i + 1]);
        }
        let (mut start, mut end, mut longest) = (0, 0, 0..0);
        while start < lowest.len() && end < highest.len() {
            if lowest[start] <= highest[end] {
                if end - start > longest.len() {
                    longest = start..end;
                }
                end += 1;
            } else {
                start += 1;
            }
        }
        new.start + longest.start..new.start + longest.end
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
