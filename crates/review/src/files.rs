//! The files a pull request changes: the order they're listed in and how each one starts out
//! (design §5, "File order", "Generated files", "Very large files").

mod generated;
mod kind;
mod order;
mod state;

pub use generated::{Generated, generated, is_lockfile};
pub use kind::{FileKind, kind};
pub use order::{path_order, review_order};
pub use state::{InitialState, LARGE_FILE_BYTES, LARGE_FILE_LINES, initial_state};

/// One file a pull request changes, as GitHub's changed-file list describes it, plus the
/// `linguist-generated` attribute the caller looks up in the head revision's `.gitattributes`.
///
/// Paths are relative to the repository root and `/`-separated, as GitHub and git report them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChangedFile {
    /// The file's path in the head revision. For a deleted file, its path in the base.
    pub path: String,
    /// The file's path in the base revision when the pull request renames it. Review order
    /// places a renamed file by its new path.
    pub previous_path: Option<String>,
    /// Lines added. Zero for a binary file.
    pub additions: u32,
    /// Lines deleted. Zero for a binary file.
    pub deletions: u32,
    /// Whether git sees the file as binary.
    pub binary: bool,
    /// The larger side's size in bytes, when it's known: a file of few lines can still be huge.
    pub size: Option<u64>,
    /// The file's `linguist-generated` attribute: `Some(true)` when it's set (`linguist-generated`
    /// or `linguist-generated=true`), `Some(false)` when it's unset (`-linguist-generated` or
    /// `linguist-generated=false`), and `None` when `.gitattributes` doesn't mention it.
    pub linguist_generated: Option<bool>,
}

impl ChangedFile {
    /// Lines added plus lines deleted, GitHub's count of changes. A line edited in place counts
    /// twice, once deleted and once added.
    pub fn changed_lines(&self) -> u64 {
        u64::from(self.additions) + u64::from(self.deletions)
    }
}

/// A path's last component.
fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}
