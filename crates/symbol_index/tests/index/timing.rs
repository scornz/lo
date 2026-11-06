//! Indexing stays fast on thousands of real files: rozed's own Rust and Zed's, from the
//! `third_party/zed` submodule the workspace builds against.

use std::{
    collections::HashMap,
    convert::Infallible,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use symbol_index::{File, Rank, Store};

use crate::support::{RUST, blob_id};

/// Every `.rs` file under `directory`, leaving out `target` directories.
fn rust_files(directory: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(directory).unwrap().flatten() {
        let path = entry.path();
        let kind = entry.file_type().unwrap();
        if kind.is_dir() && entry.file_name() != "target" {
            rust_files(&path, files);
        } else if kind.is_file() && path.extension().is_some_and(|e| e == "rs") {
            files.push(path);
        }
    }
}

/// A commit holding `contents`, by path.
fn commit(contents: &HashMap<String, String>) -> Vec<File<'static>> {
    contents
        .iter()
        .map(|(path, text)| File {
            path: path.clone(),
            blob: blob_id(text),
            size: text.len() as u64,
            grammar: &RUST,
            generated: false,
        })
        .collect()
}

fn reader(
    contents: &HashMap<String, String>,
) -> impl Fn(&File<'_>) -> Result<Vec<u8>, Infallible> + Sync + '_ {
    |file| Ok(contents[&file.path].as_bytes().to_vec())
}

/// Debug builds are slower; CI runs the tests in one, on fewer cores.
fn limit(release: Duration) -> Duration {
    if cfg!(debug_assertions) {
        release * 10
    } else {
        release
    }
}

#[test]
fn thousands_of_files_index_quickly_and_a_new_commit_reads_only_its_changes() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut paths = Vec::new();
    rust_files(&root.join("crates"), &mut paths);
    rust_files(&root.join("third_party/zed/crates"), &mut paths);
    let mut contents: HashMap<String, String> = paths
        .iter()
        .filter_map(|path| {
            let relative = path.strip_prefix(&root).ok()?.to_str()?.to_string();
            Some((relative, std::fs::read_to_string(path).ok()?))
        })
        .collect();
    assert!(
        contents.len() >= 1_000,
        "only {} files: is third_party/zed checked out?",
        contents.len()
    );
    let bytes: usize = contents.values().map(String::len).sum();

    let mut store = Store::default();
    let files = commit(&contents);
    let start = Instant::now();
    let index = store.index(&files, reader(&contents)).unwrap();
    let first = start.elapsed();

    let definitions: usize = files
        .iter()
        .filter_map(|file| index.symbols_in(&file.path))
        .map(|symbols| symbols.definitions().len())
        .sum();
    eprintln!(
        "indexed {} files, {:.1} MB, {definitions} definitions, in {first:?}",
        files.len(),
        bytes as f64 / 1e6
    );
    assert!(definitions > files.len() * 10);
    assert!(first < limit(Duration::from_secs(2)), "took {first:?}");

    // The next commit changes one file.
    let changed = "crates/symbol_index/src/lib.rs".to_string();
    contents
        .get_mut(&changed)
        .unwrap()
        .push_str("\npub fn added() {}\n");
    let files = commit(&contents);
    assert_eq!(store.missing(&files).len(), 1);
    let start = Instant::now();
    let index = store.index(&files, reader(&contents)).unwrap();
    let next = start.elapsed();
    eprintln!("indexed the next commit in {next:?}");
    assert!(next < limit(Duration::from_millis(50)), "took {next:?}");

    let start = Instant::now();
    let found = index.definitions("added", "crates/lo/src/main.rs", "use symbol_index::added;");
    let lookup = start.elapsed();
    // Zed has other functions named `added`; the import ranks this one first.
    assert!(found.len() > 1);
    assert_eq!(
        (found[0].0, found[0].1.path),
        (Rank::Imported, changed.as_str())
    );
    eprintln!("looked up a definition in {lookup:?}");
    assert!(lookup < limit(Duration::from_millis(20)), "took {lookup:?}");
}
