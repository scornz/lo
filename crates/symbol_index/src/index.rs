//! A commit's index: every indexed file's symbols, by path.

use std::sync::Arc;

use crate::{FileSymbols, Skip};

/// The symbols of a commit's files, built by [`Store::index`](crate::Store::index).
#[derive(Debug, Default)]
pub struct Index {
    /// Sorted by path.
    files: Vec<(String, Arc<FileSymbols>)>,
    /// Sorted by path.
    skipped: Vec<(String, Skip)>,
}

impl Index {
    pub(crate) fn new(
        mut files: Vec<(String, Arc<FileSymbols>)>,
        mut skipped: Vec<(String, Skip)>,
    ) -> Self {
        files.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        skipped.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        Self { files, skipped }
    }

    /// The definitions and references in the file at `path`, for its outline. `None` if the
    /// commit has no such file, it's in a language without a grammar, or it was skipped.
    pub fn symbols_in(&self, path: &str) -> Option<&FileSymbols> {
        let index = self
            .files
            .binary_search_by(|(probe, _)| probe.as_str().cmp(path))
            .ok()?;
        Some(&self.files[index].1)
    }

    /// Why the file at `path` wasn't indexed, if it was skipped.
    pub fn skipped(&self, path: &str) -> Option<Skip> {
        let index = self
            .skipped
            .binary_search_by(|(probe, _)| probe.as_str().cmp(path))
            .ok()?;
        Some(self.skipped[index].1)
    }
}
