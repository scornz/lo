//! A commit's index: every indexed file's symbols, by path, and lookups by name across them.

use std::sync::Arc;

use crate::{Definition, FileSymbols, Reference, Skip};

/// The symbols of a commit's files, built by [`Store::index`](crate::Store::index).
#[derive(Debug, Default)]
pub struct Index {
    /// Sorted by path.
    files: Vec<(String, Arc<FileSymbols>)>,
    /// Sorted by path.
    skipped: Vec<(String, Skip)>,
}

/// How near a definition is to the file asking for it. Nearer ranks sort first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Rank {
    /// In the asking file itself.
    SameFile,
    /// In another file in the asking file's directory.
    SameDirectory,
    /// In a file one of the asking file's imports names.
    Imported,
    /// Anywhere else.
    Other,
}

/// A definition or reference found in a commit's files.
#[derive(Debug)]
pub struct Found<'a, T> {
    pub path: &'a str,
    /// The file's symbols, to find what contains [`Self::symbol`].
    pub file: &'a FileSymbols,
    pub symbol: &'a T,
}

// Not derived, which would need `T: Clone`.
impl<T> Clone for Found<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Found<'_, T> {}

impl Index {
    pub(crate) fn new(
        mut files: Vec<(String, Arc<FileSymbols>)>,
        mut skipped: Vec<(String, Skip)>,
    ) -> Self {
        files.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        skipped.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        Self { files, skipped }
    }

    /// The definitions of `name`, nearest to the file at `from_path` first (design §7, "Name
    /// collisions"): that file's own, then those in its directory, then the rest. Within a rank,
    /// they're in path order, and in the order they appear in each file.
    ///
    /// The file at `from_path` needn't be in the index. Definitions in other languages aren't
    /// left out; they rank like any other file.
    pub fn definitions(&self, name: &str, from_path: &str) -> Vec<(Rank, Found<'_, Definition>)> {
        let from_directory = directory(from_path);
        let mut found = Vec::new();
        for (path, file) in &self.files {
            if !file.defines(name) {
                continue;
            }
            let rank = if path == from_path {
                Rank::SameFile
            } else if directory(path) == from_directory {
                Rank::SameDirectory
            } else {
                Rank::Other
            };
            found.extend(
                file.definitions()
                    .iter()
                    .filter(|definition| &*definition.name == name)
                    .map(|symbol| (rank, Found { path, file, symbol })),
            );
        }
        // Stable, so path order and file order stay within each rank.
        found.sort_by_key(|(rank, _)| *rank);
        found
    }

    /// Every reference to `name`, in path order and then in the order they appear in each file.
    pub fn references(&self, name: &str) -> Vec<Found<'_, Reference>> {
        self.files
            .iter()
            .filter(|(_, file)| file.refers_to(name))
            .flat_map(|(path, file)| {
                file.references()
                    .iter()
                    .filter(move |reference| &*reference.name == name)
                    .map(move |symbol| Found { path, file, symbol })
            })
            .collect()
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

/// The directory part of a path, `""` at the root.
fn directory(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(directory, _)| directory)
}
