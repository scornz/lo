//! Finds blocks of code that a pull request moves: deleted in one place and added in another, in
//! the same file or another file of the PR.

use std::collections::HashMap;
use std::ops::{Range, RangeInclusive};

/// The fewest matching lines a move has.
const MIN_LINES: usize = 3;

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
    pub fn delete(
        mut self,
        start: u32,
        lines: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.deleted.extend(numbered(start, lines));
        self
    }

    /// Adds consecutive added lines, the first at new line `start`.
    pub fn add(mut self, start: u32, lines: impl IntoIterator<Item = impl Into<String>>) -> Self {
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
    lines: impl IntoIterator<Item = impl Into<String>>,
) -> impl Iterator<Item = Line> {
    (start..).zip(lines).map(|(number, text)| Line {
        number,
        text: text.into(),
    })
}

/// The moves among a PR's changes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Moves {
    moves: Vec<Move>,
}

/// A block of lines deleted in one place and added in another.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Move {
    /// Where the block was deleted, in old line numbers.
    pub from: Place,
    /// Where the block was added, in new line numbers.
    pub to: Place,
    /// The lines edited on the way, as pairs of an old line and the new line it became. Every other
    /// line of the block that isn't blank moved unchanged, apart from its indentation.
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
            .candidates(texts.len())
            .into_iter()
            .map(|candidate| finder.to_move(&candidate, files))
            .collect();
        moves.sort_by_key(|moved| (moved.to.file, *moved.to.lines.start()));
        Self { moves }
    }

    /// Every move, ordered by where it lands: by file, then by line.
    pub fn all(&self) -> &[Move] {
        &self.moves
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
    number: u32,
    /// The index of its run in [`Side::runs`].
    run: usize,
}

/// Consecutive deleted or added lines: lines a move can line up with one another.
struct Run {
    file: usize,
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
                            number: line.number,
                            run,
                        });
                    }
                }
                if side.lines.len() > start {
                    side.runs.push(Run {
                        file,
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
#[derive(Clone, Debug)]
struct Candidate {
    new: Range<usize>,
    delta: isize,
}

impl Candidate {
    fn old(&self) -> Range<usize> {
        self.new.start.wrapping_add_signed(self.delta)..self.new.end.wrapping_add_signed(self.delta)
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
        for (new, line) in self.new.lines.iter().enumerate() {
            for &old in index.get(line.id) {
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
            if new.len() >= MIN_LINES {
                candidates.push(Candidate {
                    new,
                    delta: seed.delta,
                });
            }
        }
        candidates
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

    /// The matching lines around `seed` along its lineup.
    fn walk(&self, seed: Seed) -> Range<usize> {
        let lane = self.lane(seed);
        let mut start = seed.new;
        while start > lane.start && self.same(start - 1, seed.delta) {
            start -= 1;
        }
        let mut end = seed.new + 1;
        while end < lane.end && self.same(end, seed.delta) {
            end += 1;
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
