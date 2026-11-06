//! Helpers for describing moves compactly in assertions.

use diff_extras::moved::{FileChanges, Move, Moves};

/// A small function, used as the moved block in most tests.
pub const TOTAL: [&str; 4] = [
    "fn total(items: &[Item]) -> u32 {",
    "    let sum: u32 = items.iter().map(|item| item.price).sum();",
    "    sum + tax(sum)",
    "}",
];

/// The moves found in `files`, each as `from -> to`, such as `a.rs:10-13 -> b.rs:1-4`, followed by
/// its edited line pairs, such as `edited 11/2 12/3`.
pub fn moves(files: &[FileChanges]) -> Vec<String> {
    Moves::find(files).all().iter().map(describe).collect()
}

pub fn describe(moved: &Move) -> String {
    let mut text = format!(
        "{}:{}-{} -> {}:{}-{}",
        moved.from.path,
        moved.from.lines.start(),
        moved.from.lines.end(),
        moved.to.path,
        moved.to.lines.start(),
        moved.to.lines.end(),
    );
    if !moved.edits.is_empty() {
        text.push_str(" edited");
        for edit in &moved.edits {
            text.push_str(&format!(" {}/{}", edit.old, edit.new));
        }
    }
    text
}

/// `lines`, each indented by `spaces` more spaces.
pub fn indented(spaces: usize, lines: &[&str]) -> Vec<String> {
    lines
        .iter()
        .map(|line| format!("{}{line}", " ".repeat(spaces)))
        .collect()
}
