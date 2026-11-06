//! Indexes the source files under a directory and reports how long it took.
//!
//! ```sh
//! cargo run --release -p symbol_index --example index_tree -- <directory>
//! ```

use std::{
    collections::HashMap,
    convert::Infallible,
    env,
    hash::{DefaultHasher, Hash, Hasher},
    path::{Path, PathBuf},
    process::ExitCode,
    time::Instant,
};

use symbol_index::{BlobId, File, Grammar, Skip, Store, queries};

fn main() -> ExitCode {
    let Some(root) = env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: index_tree <directory>");
        return ExitCode::from(2);
    };
    let grammars = Grammars::new();

    let start = Instant::now();
    let mut paths = Vec::new();
    walk(&root, &mut paths);
    paths.sort();
    let contents: Vec<(String, Vec<u8>)> = paths
        .iter()
        .filter_map(|path| {
            let relative = path.strip_prefix(&root).ok()?.to_str()?.replace('\\', "/");
            grammars.for_path(&relative)?;
            Some((relative, std::fs::read(path).ok()?))
        })
        .collect();
    let files: Vec<File> = contents
        .iter()
        .map(|(path, bytes)| File {
            path: path.clone(),
            blob: blob_id(bytes),
            size: bytes.len() as u64,
            grammar: grammars.for_path(path).unwrap(),
            generated: false,
        })
        .collect();
    let by_path: HashMap<&str, &[u8]> = contents
        .iter()
        .map(|(path, bytes)| (path.as_str(), bytes.as_slice()))
        .collect();
    let read =
        |file: &File| -> Result<Vec<u8>, Infallible> { Ok(by_path[file.path.as_str()].to_vec()) };
    let bytes: usize = contents.iter().map(|(_, bytes)| bytes.len()).sum();
    println!(
        "read {} files, {:.1} MB, in {:?}",
        files.len(),
        bytes as f64 / 1e6,
        start.elapsed()
    );

    let mut store = Store::default();
    let start = Instant::now();
    let index = store.index(&files, read).unwrap();
    let first = start.elapsed();

    let start = Instant::now();
    let again = store.index(&files, read).unwrap();
    let second = start.elapsed();

    let (mut definitions, mut references, mut indexed) = (0, 0, 0);
    let mut names = Vec::new();
    for file in &files {
        if let Some(symbols) = index.symbols_in(&file.path) {
            indexed += 1;
            definitions += symbols.definitions().len();
            references += symbols.references().len();
            names.extend(
                symbols
                    .references()
                    .iter()
                    .take(2)
                    .map(|r| (r.name.clone(), &file.path)),
            );
        }
    }
    let too_large = count(&index, &files, Skip::TooLarge);
    let generated = count(&index, &files, Skip::Generated);
    println!(
        "indexed {indexed} files ({too_large} too large, {generated} generated): \
         {definitions} definitions, {references} references"
    );
    println!("first index: {first:?}; again, every blob reused: {second:?}");

    let start = Instant::now();
    let mut found = 0;
    for (name, from) in &names {
        found += again.definitions(name, from, "").len();
        found += again.references(name).len();
    }
    let lookups = start.elapsed();
    println!(
        "{} lookups of definitions and references ({found} results) in {lookups:?}, {:?} each",
        names.len(),
        lookups / names.len().max(1) as u32
    );
    ExitCode::SUCCESS
}

struct Grammars {
    rust: Grammar,
    typescript: Grammar,
    tsx: Grammar,
    python: Grammar,
    go: Grammar,
}

impl Grammars {
    fn new() -> Self {
        Self {
            rust: Grammar::new(tree_sitter_rust::LANGUAGE.into(), queries::RUST).unwrap(),
            typescript: Grammar::new(
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
                queries::TYPESCRIPT,
            )
            .unwrap(),
            tsx: Grammar::new(tree_sitter_typescript::LANGUAGE_TSX.into(), queries::TSX).unwrap(),
            python: Grammar::new(tree_sitter_python::LANGUAGE.into(), queries::PYTHON).unwrap(),
            go: Grammar::new(tree_sitter_go::LANGUAGE.into(), queries::GO).unwrap(),
        }
    }

    fn for_path(&self, path: &str) -> Option<&Grammar> {
        match path.rsplit_once('.')?.1 {
            "rs" => Some(&self.rust),
            "ts" | "mts" | "cts" => Some(&self.typescript),
            // Zed reads JavaScript with the TSX grammar.
            "tsx" | "js" | "jsx" | "mjs" | "cjs" => Some(&self.tsx),
            "py" => Some(&self.python),
            "go" => Some(&self.go),
            _ => None,
        }
    }
}

/// Every file under `directory`, leaving out hidden directories, `node_modules` and `target`.
fn walk(directory: &Path, paths: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => {
                if !name.starts_with('.') && name != "node_modules" && name != "target" {
                    walk(&path, paths);
                }
            }
            Ok(kind) if kind.is_file() => paths.push(path),
            _ => {}
        }
    }
}

fn blob_id(contents: &[u8]) -> BlobId {
    let mut hasher = DefaultHasher::new();
    contents.hash(&mut hasher);
    BlobId::from(format!("{:016x}", hasher.finish()))
}

fn count(index: &symbol_index::Index, files: &[File], skip: Skip) -> usize {
    files
        .iter()
        .filter(|file| index.skipped(&file.path) == Some(skip))
        .count()
}
