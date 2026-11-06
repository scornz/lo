//! The order the file list shows a pull request's files in.

use super::ChangedFile;

/// The files in path order, as indices into `files`.
///
/// Paths compare byte by byte, which is git's order and the order of GitHub's changed-file list:
/// uppercase before lowercase, and `a.rs` before `a/b.rs`. Every directory's files stay together.
pub fn path_order(files: &[ChangedFile]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..files.len()).collect();
    // Stable, so two entries with the same path keep the caller's order.
    order.sort_by(|&a, &b| files[a].path.cmp(&files[b].path));
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(paths: &[&str]) -> Vec<ChangedFile> {
        paths
            .iter()
            .map(|path| ChangedFile {
                path: path.to_string(),
                ..Default::default()
            })
            .collect()
    }

    fn paths(files: &[ChangedFile], order: Vec<usize>) -> Vec<&str> {
        order.into_iter().map(|i| files[i].path.as_str()).collect()
    }

    #[test]
    fn path_order_sorts_by_path() {
        let files = files(&["src/main.rs", "Cargo.toml", "README.md", "build.rs"]);

        assert_eq!(
            paths(&files, path_order(&files)),
            ["Cargo.toml", "README.md", "build.rs", "src/main.rs"]
        );
    }

    #[test]
    fn path_order_compares_bytes_like_git() {
        let files = files(&["src/a/b.rs", "src/a.rs", "src/a-b.rs", "src/B.rs"]);

        assert_eq!(
            paths(&files, path_order(&files)),
            ["src/B.rs", "src/a-b.rs", "src/a.rs", "src/a/b.rs"]
        );
    }

    #[test]
    fn path_order_of_nothing_is_empty() {
        assert!(path_order(&[]).is_empty());
    }
}
