//! Finding moves stays fast on a large pull request.

use std::time::{Duration, Instant};

use diff_extras::moved::{FileChanges, Moves};

const FILES: usize = 200;
const MOVES: usize = 100;
const BLOCK: usize = 20;
const NOISE_HUNKS: usize = 4;
const NOISE_LINES: usize = 10;
/// Unchanged lines between hunks.
const CONTEXT: u32 = 5;

/// Lines that repeat all over a real diff.
const COMMON: [&str; 12] = [
    "return Ok(());",
    "let mut out = Vec::new();",
    "if value.is_none() {",
    "} else {",
    "return None;",
    "Ok(out)",
    "let value = value?;",
    "#[derive(Clone, Debug)]",
    "pub fn new() -> Self {",
    "Self::default()",
    "for item in items {",
    "out.push(item);",
];
const PUNCTUATION: [&str; 5] = ["}", "});", "]", ")", ""];

/// A small deterministic random number generator, so the diff is the same on every run.
struct Random(u64);

impl Random {
    fn below(&mut self, bound: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) % bound as u64) as usize
    }
}

/// A changed line that isn't part of a planted move: punctuation, a line common all over the diff,
/// a line found in a few places, or a line found only on its side of the diff.
fn noise(random: &mut Random, side: &str, file: usize, number: u32) -> String {
    match random.below(20) {
        0..5 => PUNCTUATION[random.below(PUNCTUATION.len())].to_string(),
        5..8 => COMMON[random.below(COMMON.len())].to_string(),
        8..13 => {
            let n = random.below(1000);
            format!("let shared_{n} = lookup({n});")
        }
        _ => format!("let {side}_{file}_{number} = compute({number});"),
    }
}

/// Lines no other line matches, around each planted block so its lineup stops at its edges.
fn guards(side: &str, file: usize, number: u32) -> Vec<String> {
    (0..3)
        .map(|n| format!("// {side} guard {file} {number} {n}"))
        .collect()
}

/// The block planted for move `k`, as deleted.
fn block(k: usize) -> Vec<String> {
    (0..BLOCK)
        .map(|j| match j % 7 {
            2 => "    }".to_string(),
            4 => String::new(),
            _ => format!("    let block_{k}_{j} = item.field_{j}.compute({k});"),
        })
        .collect()
}

/// Move `k`'s block as added: every third one re-indented, every third one with an edit.
fn block_moved(k: usize) -> Vec<String> {
    let mut lines = block(k);
    match k % 3 {
        0 => lines = lines.iter().map(|line| format!("        {line}")).collect(),
        1 => lines[BLOCK / 2] = format!("    let block_{k}_edited = changed();"),
        _ => {}
    }
    lines
}

fn source_file(k: usize) -> usize {
    k
}

fn destination_file(k: usize) -> usize {
    (k * 37 + 11) % FILES
}

/// A planted move: where its block is deleted and added, and whether it's edited.
struct Planted {
    from: (usize, u32),
    to: (usize, u32),
}

/// A 20,000-line diff over 200 files: 100 blocks of 20 lines moved between files, a third of them
/// re-indented and a third edited, among hunks of other changed lines.
fn large_diff() -> (Vec<FileChanges>, Vec<Planted>) {
    let mut random = Random(42);
    let mut files: Vec<FileChanges> = (0..FILES)
        .map(|file| FileChanges::new(format!("src/file_{file}.rs")))
        .collect();
    let mut planted: Vec<Planted> = (0..MOVES)
        .map(|_| Planted {
            from: (0, 0),
            to: (0, 0),
        })
        .collect();
    for (index, changes) in files.iter_mut().enumerate() {
        let (mut old, mut new) = (1, 1);
        for hunk in 0..NOISE_HUNKS {
            let deleted: Vec<String> = (0..NOISE_LINES as u32)
                .map(|n| noise(&mut random, "old", index, old + n))
                .collect();
            let added: Vec<String> = (0..NOISE_LINES as u32)
                .map(|n| noise(&mut random, "new", index, new + n))
                .collect();
            *changes = std::mem::take(changes)
                .delete(old, &deleted)
                .add(new, &added);
            old += NOISE_LINES as u32 + CONTEXT;
            new += NOISE_LINES as u32 + CONTEXT;

            if hunk == 1
                && let Some(k) = (0..MOVES).find(|&k| source_file(k) == index)
            {
                let mut deleted = guards("old", index, old);
                deleted.extend(block(k));
                deleted.extend(guards("old", index, old + 100));
                *changes = std::mem::take(changes).delete(old, &deleted);
                planted[k].from = (index, old + 3);
                old += deleted.len() as u32 + CONTEXT;
                new += CONTEXT;
            }
            if hunk == 2 {
                for k in (0..MOVES).filter(|&k| destination_file(k) == index) {
                    let mut added = guards("new", index, new);
                    added.extend(block_moved(k));
                    added.extend(guards("new", index, new + 100));
                    *changes = std::mem::take(changes).add(new, &added);
                    planted[k].to = (index, new + 3);
                    new += added.len() as u32 + CONTEXT;
                    old += CONTEXT;
                }
            }
        }
    }
    (files, planted)
}

#[test]
fn finds_moves_in_a_20000_line_diff_within_100_ms() {
    let (files, planted) = large_diff();
    let lines: usize = files
        .iter()
        .map(|file| file.deleted.len() + file.added.len())
        .sum();
    assert!(lines >= 20_000, "{lines} changed lines");

    let start = Instant::now();
    let moves = Moves::find(&files);
    let elapsed = start.elapsed();

    for (k, planted) in planted.iter().enumerate() {
        let (from_file, from) = planted.from;
        let (to_file, to) = planted.to;
        let moved = moves
            .of_added(to_file, to)
            .unwrap_or_else(|| panic!("move {k} wasn't found"));
        let last = BLOCK as u32 - 1;
        assert_eq!(moved.from.file, from_file, "move {k}");
        assert_eq!(moved.from.lines, from..=from + last, "move {k}");
        assert_eq!(moved.to.file, to_file, "move {k}");
        assert_eq!(moved.to.lines, to..=to + last, "move {k}");
        assert_eq!(moved.edits.len(), usize::from(k % 3 == 1), "move {k}");
    }

    // Debug builds are much slower; CI runs the tests in one.
    let limit = if cfg!(debug_assertions) {
        Duration::from_secs(1)
    } else {
        Duration::from_millis(100)
    };
    eprintln!(
        "found {} moves among {lines} changed lines in {elapsed:?}",
        moves.all().len()
    );
    assert!(
        elapsed < limit,
        "took {elapsed:?} to find moves among {lines} changed lines"
    );
}

#[test]
fn a_line_repeated_thousands_of_times_stays_fast() {
    // A table of the same line, with a few distinct lines in the middle, moved to another file.
    let mut table = vec!["value = 0,"; 1500];
    table.extend(["first = 1,", "second = 2,", "third = 3,"]);
    table.extend(vec!["value = 0,"; 1500]);
    let files = [
        FileChanges::new("a.rs").delete(1, &table),
        FileChanges::new("b.rs").add(1, &table),
    ];

    let start = Instant::now();
    let moves = Moves::find(&files);
    let elapsed = start.elapsed();

    let found: Vec<_> = moves
        .all()
        .iter()
        .map(|moved| (moved.from.lines.clone(), moved.to.lines.clone()))
        .collect();
    assert_eq!(found, [(1..=3003, 1..=3003)]);
    let limit = if cfg!(debug_assertions) {
        Duration::from_secs(1)
    } else {
        Duration::from_millis(100)
    };
    eprintln!("found a 3,003-line move in {elapsed:?}");
    assert!(elapsed < limit, "took {elapsed:?}");
}

#[test]
fn a_long_lineup_with_every_other_line_edited_stays_fast() {
    // 20,000 lines that line up, with every other line edited: never two thirds matching.
    let old: Vec<String> = (0..10_000)
        .flat_map(|n| {
            [
                format!("let kept_{n} = {n};"),
                format!("let old_{n} = {n};"),
            ]
        })
        .collect();
    let new: Vec<String> = (0..10_000)
        .flat_map(|n| {
            [
                format!("let kept_{n} = {n};"),
                format!("let new_{n} = {n};"),
            ]
        })
        .collect();
    let files = [
        FileChanges::new("a.rs").delete(1, &old),
        FileChanges::new("b.rs").add(1, &new),
    ];

    let start = Instant::now();
    let moves = Moves::find(&files);
    let elapsed = start.elapsed();

    assert!(moves.all().is_empty());
    let limit = if cfg!(debug_assertions) {
        Duration::from_secs(1)
    } else {
        Duration::from_millis(100)
    };
    eprintln!("checked a 20,000-line lineup in {elapsed:?}");
    assert!(elapsed < limit, "took {elapsed:?}");
}
