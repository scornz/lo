//! Grammars for each language, built once per test binary, and blobs to index.

use std::{
    collections::HashMap,
    convert::Infallible,
    hash::{DefaultHasher, Hash, Hasher},
    sync::{LazyLock, Mutex},
};

use symbol_index::{BlobId, File, FileSymbols, Grammar, Index, queries};

pub static RUST: LazyLock<Grammar> =
    LazyLock::new(|| Grammar::new(tree_sitter_rust::LANGUAGE.into(), queries::RUST).unwrap());

pub static TYPESCRIPT: LazyLock<Grammar> = LazyLock::new(|| {
    Grammar::new(
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        queries::TYPESCRIPT,
    )
    .unwrap()
});

pub static TSX: LazyLock<Grammar> = LazyLock::new(|| {
    Grammar::new(tree_sitter_typescript::LANGUAGE_TSX.into(), queries::TSX).unwrap()
});

/// Zed reads JavaScript with the TSX grammar.
pub static JAVASCRIPT: LazyLock<Grammar> = LazyLock::new(|| {
    Grammar::new(
        tree_sitter_typescript::LANGUAGE_TSX.into(),
        queries::JAVASCRIPT,
    )
    .unwrap()
});

pub static PYTHON: LazyLock<Grammar> =
    LazyLock::new(|| Grammar::new(tree_sitter_python::LANGUAGE.into(), queries::PYTHON).unwrap());

pub static GO: LazyLock<Grammar> =
    LazyLock::new(|| Grammar::new(tree_sitter_go::LANGUAGE.into(), queries::GO).unwrap());

/// A file's definitions as `kind name`, with `< parent` when one encloses it.
pub fn definitions(symbols: &FileSymbols) -> Vec<String> {
    symbols
        .definitions()
        .iter()
        .map(|definition| {
            let mut line = format!("{} {}", definition.kind, definition.name);
            if let Some(parent) = symbols.parent(definition) {
                line.push_str(&format!(" < {}", parent.name));
            }
            line
        })
        .collect()
}

/// A file's references as `kind name`, with `in definition` when one encloses it.
pub fn references(symbols: &FileSymbols) -> Vec<String> {
    symbols
        .references()
        .iter()
        .map(|reference| {
            let mut line = format!("{} {}", reference.kind, reference.name);
            if let Some(enclosing) = symbols.enclosing(reference) {
                line.push_str(&format!(" in {}", enclosing.name));
            }
            line
        })
        .collect()
}

/// The grammar for a path, by its extension, as the app picks Zed's.
pub fn grammar_for(path: &str) -> &'static Grammar {
    match path.rsplit_once('.').map(|(_, extension)| extension) {
        Some("rs") => &RUST,
        Some("ts" | "mts" | "cts") => &TYPESCRIPT,
        Some("tsx") => &TSX,
        Some("js" | "jsx" | "mjs" | "cjs") => &JAVASCRIPT,
        Some("py") => &PYTHON,
        Some("go") => &GO,
        _ => panic!("no grammar for {path}"),
    }
}

/// A blob ID for some contents: a hash, like git's.
pub fn blob_id(contents: &str) -> BlobId {
    let mut hasher = DefaultHasher::new();
    contents.hash(&mut hasher);
    BlobId::from(format!("{:016x}", hasher.finish()))
}

/// Blobs in memory, counting how many times each is read.
#[derive(Default)]
pub struct Blobs {
    contents: Mutex<HashMap<BlobId, Vec<u8>>>,
    reads: Mutex<Vec<String>>,
}

impl Blobs {
    /// A file of a commit, its contents kept to be read.
    pub fn file(&self, path: &str, contents: &str) -> File<'static> {
        let blob = blob_id(contents);
        self.contents
            .lock()
            .unwrap()
            .insert(blob.clone(), contents.as_bytes().to_vec());
        File {
            path: path.to_string(),
            blob,
            size: contents.len() as u64,
            grammar: grammar_for(path),
            generated: false,
        }
    }

    /// A commit's files from `(path, contents)` pairs.
    pub fn commit(&self, files: &[(&str, &str)]) -> Vec<File<'static>> {
        files
            .iter()
            .map(|(path, contents)| self.file(path, contents))
            .collect()
    }

    pub fn read(&self, file: &File<'_>) -> Result<Vec<u8>, Infallible> {
        self.reads.lock().unwrap().push(file.path.clone());
        Ok(self.contents.lock().unwrap()[&file.blob].clone())
    }

    /// The paths read so far, sorted, and forgets them.
    pub fn take_reads(&self) -> Vec<String> {
        let mut reads = std::mem::take(&mut *self.reads.lock().unwrap());
        reads.sort();
        reads
    }
}

/// The names a file defines, in order.
pub fn names(index: &Index, path: &str) -> Vec<String> {
    index
        .symbols_in(path)
        .unwrap_or_else(|| panic!("{path} isn't indexed"))
        .definitions()
        .iter()
        .map(|definition| definition.name.to_string())
        .collect()
}
