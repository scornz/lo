//! Each blob's symbols, kept across commits.

use std::{
    collections::{HashMap, HashSet},
    fmt,
    num::NonZero,
    panic,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
};

use tree_sitter_tags::TagsContext;

use crate::{
    FileSymbols, Grammar, Index,
    generated::{contents_are_generated, path_is_generated},
};

/// A blob's object ID, as git names its contents.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlobId(Arc<str>);

impl BlobId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for BlobId {
    fn from(id: &str) -> Self {
        Self(id.into())
    }
}

impl From<String> for BlobId {
    fn from(id: String) -> Self {
        Self(id.into())
    }
}

impl fmt::Display for BlobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A file in a commit's tree, in a language the caller has a grammar for.
#[derive(Clone, Debug)]
pub struct File<'g> {
    /// The path from the repository's root, with `/` between parts.
    pub path: String,
    pub blob: BlobId,
    /// The blob's size in bytes, as the tree lists it, so a large file is skipped unread.
    pub size: u64,
    /// The grammar for the file's language, picked by the caller from its path.
    pub grammar: &'g Grammar,
    /// Whether the repository marks the file generated (`linguist-generated` in
    /// `.gitattributes`).
    pub generated: bool,
}

/// What the index skips.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Files larger than this, in bytes, aren't indexed.
    pub max_file_size: u64,
}

impl Default for Limits {
    /// 1 MB, the size past which the syntax-aware diff gives up too.
    fn default() -> Self {
        Self {
            max_file_size: 1024 * 1024,
        }
    }
}

/// Why a file isn't indexed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Skip {
    /// It's larger than [`Limits::max_file_size`].
    TooLarge,
    /// It's generated: marked so by the repository, minified, or starting with a code generator's
    /// marker.
    Generated,
}

/// The symbols of every blob indexed so far, kept by blob ID and grammar, so each commit reads
/// and parses only the blobs earlier commits didn't have.
#[derive(Default)]
pub struct Store {
    limits: Limits,
    blobs: HashMap<(BlobId, u32), Read>,
}

/// What reading a blob gave.
#[derive(Clone)]
enum Read {
    Symbols(Arc<FileSymbols>),
    Skipped(Skip),
}

impl Store {
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            blobs: HashMap::new(),
        }
    }

    /// The files [`Self::index`] would read: those whose blob the store hasn't read with their
    /// grammar, and that their size or path doesn't skip. Each blob is listed once.
    ///
    /// A caller that reads blobs in batches can fetch these first.
    pub fn missing<'a, 'g>(&self, files: &'a [File<'g>]) -> Vec<&'a File<'g>> {
        let mut seen = HashSet::new();
        files
            .iter()
            .filter(|file| {
                self.skip_unread(file).is_none()
                    && !self.blobs.contains_key(&key(file))
                    && seen.insert(key(file))
            })
            .collect()
    }

    /// Indexes a commit's files, reading each blob the store doesn't have with `read`, and
    /// parsing them on every core.
    ///
    /// Files are skipped unread if their listed size is over the limit, the repository marks
    /// them generated, or their path names minified code; or once read, if their contents are
    /// over the limit or generated.
    ///
    /// Stops at the first error `read` returns, keeping the blobs it read until then.
    pub fn index<E: Send>(
        &mut self,
        files: &[File<'_>],
        read: impl Fn(&File<'_>) -> Result<Vec<u8>, E> + Sync,
    ) -> Result<Index, E> {
        let missing = self.missing(files);
        let (done, error) = read_all(&missing, &read, self.limits);
        for (index, result) in done {
            self.blobs.insert(key(missing[index]), result);
        }
        if let Some(error) = error {
            return Err(error);
        }

        let mut indexed = Vec::new();
        let mut skipped = Vec::new();
        for file in files {
            let result = match self.skip_unread(file) {
                Some(skip) => Read::Skipped(skip),
                None => self.blobs[&key(file)].clone(),
            };
            match result {
                Read::Symbols(symbols) => indexed.push((file.path.clone(), symbols)),
                Read::Skipped(skip) => skipped.push((file.path.clone(), skip)),
            }
        }
        Ok(Index::new(indexed, skipped))
    }

    /// Why `file` is skipped without reading it, if it is.
    fn skip_unread(&self, file: &File<'_>) -> Option<Skip> {
        if file.size > self.limits.max_file_size {
            Some(Skip::TooLarge)
        } else if file.generated || path_is_generated(&file.path) {
            Some(Skip::Generated)
        } else {
            None
        }
    }
}

fn key(file: &File<'_>) -> (BlobId, u32) {
    (file.blob.clone(), file.grammar.id)
}

/// Reads and parses `files` on as many threads as there are cores, each taking the next file
/// as it finishes one. Returns each result with its index in `files`, and the first error.
fn read_all<E: Send>(
    files: &[&File<'_>],
    read: &(impl Fn(&File<'_>) -> Result<Vec<u8>, E> + Sync),
    limits: Limits,
) -> (Vec<(usize, Read)>, Option<E>) {
    let threads = thread::available_parallelism()
        .map_or(1, NonZero::get)
        .min(files.len());
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);

    let work = || {
        let mut context = TagsContext::new();
        let mut done = Vec::new();
        while !failed.load(Ordering::Relaxed) {
            let index = next.fetch_add(1, Ordering::Relaxed);
            let Some(file) = files.get(index) else { break };
            match read(file) {
                Ok(contents) => done.push((index, parse(&mut context, file, &contents, limits))),
                Err(error) => {
                    failed.store(true, Ordering::Relaxed);
                    return (done, Some(error));
                }
            }
        }
        (done, None)
    };

    thread::scope(|scope| {
        let workers: Vec<_> = (0..threads).map(|_| scope.spawn(work)).collect();
        let mut all = Vec::with_capacity(files.len());
        let mut first_error = None;
        for worker in workers {
            let (done, error) = worker
                .join()
                .unwrap_or_else(|panic| panic::resume_unwind(panic));
            all.extend(done);
            first_error = first_error.or(error);
        }
        (all, first_error)
    })
}

fn parse(context: &mut TagsContext, file: &File<'_>, contents: &[u8], limits: Limits) -> Read {
    if contents.len() as u64 > limits.max_file_size {
        Read::Skipped(Skip::TooLarge)
    } else if contents_are_generated(contents) {
        Read::Skipped(Skip::Generated)
    } else {
        Read::Symbols(Arc::new(FileSymbols::read_with(
            context,
            file.grammar,
            contents,
        )))
    }
}
