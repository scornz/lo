//! The file list: the files a pull request changes, in review order or path order, with their
//! viewed marks (design §5 "File order", §6 "Viewed marks").

use std::collections::HashMap;

use review::files::{self, ChangedFile};
use review::revisions::ViewedState;

/// How a pull request changes a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileChange {
    Added,
    Deleted,
    Modified,
    Renamed,
    /// A file became a symlink or a submodule, or the other way round.
    TypeChanged,
}

impl FileChange {
    /// The letter the list shows for it, as git does.
    pub fn letter(self) -> &'static str {
        match self {
            Self::Added => "A",
            Self::Deleted => "D",
            Self::Modified => "M",
            Self::Renamed => "R",
            Self::TypeChanged => "T",
        }
    }
}

impl From<&repo_cache::Change> for FileChange {
    fn from(change: &repo_cache::Change) -> Self {
        match change {
            repo_cache::Change::Added => Self::Added,
            repo_cache::Change::Deleted => Self::Deleted,
            repo_cache::Change::Modified => Self::Modified,
            repo_cache::Change::Renamed { .. } => Self::Renamed,
            repo_cache::Change::TypeChanged => Self::TypeChanged,
        }
    }
}

/// One file in the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    /// What review order and path order see: the path, the old path of a rename, line counts and
    /// whether it's binary.
    pub file: ChangedFile,
    pub change: FileChange,
    /// GitHub's viewed mark for the account acting, or `None` when rozed doesn't know it: before
    /// GitHub answered, or without signing in.
    pub viewed: Option<ViewedState>,
}

impl FileEntry {
    pub fn path(&self) -> &str {
        &self.file.path
    }

    /// An entry for a file the local copy lists, with its `linguist-generated` attribute from the
    /// head's `.gitattributes`.
    pub fn from_local(file: &repo_cache::ChangedFile, linguist_generated: Option<bool>) -> Self {
        // A submodule has no line counts.
        let (additions, deletions) = file
            .lines
            .map_or((0, 0), |lines| (lines.added, lines.deleted));
        Self {
            file: ChangedFile {
                // A name that isn't UTF-8 shows with U+FFFD, as in the diff.
                path: file.display_path().into_owned(),
                previous_path: match file.change {
                    repo_cache::Change::Renamed { .. } => file
                        .old_path()
                        .map(|path| String::from_utf8_lossy(path).into_owned()),
                    _ => None,
                },
                additions,
                deletions,
                binary: file.binary,
                size: file.size(),
                linguist_generated,
            },
            change: FileChange::from(&file.change),
            viewed: None,
        }
    }
}

/// The order the list shows files in. One key switches between them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FileOrder {
    /// Each source file followed by its tests, then config, then generated files.
    #[default]
    Review,
    /// By path, as git and GitHub sort them.
    Path,
}

/// The list's files, both orders and the selected file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileList {
    entries: Vec<FileEntry>,
    review_order: Vec<usize>,
    path_order: Vec<usize>,
    order: FileOrder,
    /// The selected entry, by index into `entries`, so it stays selected when the order changes.
    selected: Option<usize>,
}

impl FileList {
    /// The list of `entries`, in review order, with nothing selected.
    pub fn new(entries: Vec<FileEntry>) -> Self {
        let files: Vec<ChangedFile> = entries.iter().map(|entry| entry.file.clone()).collect();
        Self {
            review_order: files::review_order(&files),
            path_order: files::path_order(&files),
            entries,
            order: FileOrder::Review,
            selected: None,
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn order(&self) -> FileOrder {
        self.order
    }

    pub fn set_order(&mut self, order: FileOrder) {
        self.order = order;
    }

    /// Switches between review order and path order.
    pub fn toggle_order(&mut self) {
        self.order = match self.order {
            FileOrder::Review => FileOrder::Path,
            FileOrder::Path => FileOrder::Review,
        };
    }

    /// Indices into the entries, in the list's order.
    fn indices(&self) -> &[usize] {
        match self.order {
            FileOrder::Review => &self.review_order,
            FileOrder::Path => &self.path_order,
        }
    }

    /// The entry at `row`, counting rows in the list's order.
    pub fn row(&self, row: usize) -> Option<&FileEntry> {
        self.entries.get(*self.indices().get(row)?)
    }

    /// The entries in the list's order.
    pub fn rows(&self) -> impl Iterator<Item = &FileEntry> {
        self.indices().iter().map(|&index| &self.entries[index])
    }

    /// Where the entry for `path` sits in review order, which is the order the diff shows files
    /// in.
    pub fn review_position(&self, path: &str) -> Option<usize> {
        self.review_order
            .iter()
            .position(|&index| self.entries[index].path() == path)
    }

    /// Each entry's position in review order, by its index in the entries: where the diff shows
    /// each file.
    pub fn review_positions(&self) -> Vec<usize> {
        let mut positions = vec![0; self.entries.len()];
        for (position, &index) in self.review_order.iter().enumerate() {
            positions[index] = position;
        }
        positions
    }

    /// Takes GitHub's viewed marks, by path. Files GitHub doesn't list keep no mark.
    pub fn set_viewed(&mut self, viewed: &HashMap<String, ViewedState>) {
        for entry in &mut self.entries {
            entry.viewed = viewed.get(entry.path()).copied();
        }
    }

    /// The selected row, in the list's order.
    pub fn selected_row(&self) -> Option<usize> {
        let selected = self.selected?;
        self.indices().iter().position(|&index| index == selected)
    }

    pub fn selected(&self) -> Option<&FileEntry> {
        self.entries.get(self.selected?)
    }

    /// Selects the file at `row`, counting rows in the list's order.
    pub fn select_row(&mut self, row: usize) {
        if let Some(&index) = self.indices().get(row) {
            self.selected = Some(index);
        }
    }

    /// Selects the file at `path`, if the list has it.
    pub fn select_path(&mut self, path: &str) {
        if let Some(index) = self.entries.iter().position(|entry| entry.path() == path) {
            self.selected = Some(index);
        }
    }

    /// Selects the next row, or the first when nothing is selected. Stays on the last row.
    pub fn select_next(&mut self) {
        let row = match self.selected_row() {
            Some(row) => (row + 1).min(self.len().saturating_sub(1)),
            None => 0,
        };
        self.select_row(row);
    }

    /// Selects the previous row, or the first when nothing is selected. Stays on the first row.
    pub fn select_previous(&mut self) {
        let row = self.selected_row().map_or(0, |row| row.saturating_sub(1));
        self.select_row(row);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use repo_cache::{Change, FileMode, FileVersion, LineCounts};

    fn entry(path: &str) -> FileEntry {
        FileEntry {
            file: ChangedFile {
                path: path.to_string(),
                additions: 1,
                ..Default::default()
            },
            change: FileChange::Modified,
            viewed: None,
        }
    }

    fn list(paths: &[&str]) -> FileList {
        FileList::new(paths.iter().map(|path| entry(path)).collect())
    }

    fn paths(list: &FileList) -> Vec<&str> {
        list.rows().map(FileEntry::path).collect()
    }

    const PULL_REQUEST: &[&str] = &[
        "Cargo.lock",
        "README.md",
        "src/lib.rs",
        "src/main.rs",
        "tests/lib.rs",
    ];

    #[test]
    fn files_start_in_review_order_and_one_switch_shows_path_order() {
        let mut list = list(PULL_REQUEST);
        assert_eq!(list.order(), FileOrder::Review);
        assert_eq!(
            paths(&list),
            [
                "README.md",
                "src/lib.rs",
                "tests/lib.rs",
                "src/main.rs",
                "Cargo.lock"
            ]
        );

        list.toggle_order();

        assert_eq!(list.order(), FileOrder::Path);
        assert_eq!(paths(&list), PULL_REQUEST);

        list.toggle_order();
        assert_eq!(list.order(), FileOrder::Review);
    }

    #[test]
    fn the_selected_file_stays_selected_when_the_order_changes() {
        let mut list = list(PULL_REQUEST);
        list.select_path("Cargo.lock");
        assert_eq!(list.selected_row(), Some(4));

        list.toggle_order();

        assert_eq!(list.selected().map(FileEntry::path), Some("Cargo.lock"));
        assert_eq!(list.selected_row(), Some(0));
    }

    #[test]
    fn moving_the_selection_follows_the_order_and_stops_at_the_ends() {
        let mut list = list(PULL_REQUEST);
        assert_eq!(list.selected(), None);

        list.select_next();
        assert_eq!(list.selected().map(FileEntry::path), Some("README.md"));
        list.select_next();
        list.select_next();
        assert_eq!(list.selected().map(FileEntry::path), Some("tests/lib.rs"));
        for _ in 0..10 {
            list.select_next();
        }
        assert_eq!(list.selected().map(FileEntry::path), Some("Cargo.lock"));
        list.select_previous();
        assert_eq!(list.selected().map(FileEntry::path), Some("src/main.rs"));

        let mut fresh = self::list(PULL_REQUEST);
        fresh.select_previous();
        assert_eq!(fresh.selected_row(), Some(0));
        for _ in 0..10 {
            fresh.select_previous();
        }
        assert_eq!(fresh.selected_row(), Some(0));
    }

    #[test]
    fn an_empty_list_selects_nothing() {
        let mut list = list(&[]);
        list.select_next();
        list.select_previous();
        list.select_row(3);
        assert_eq!(list.selected(), None);
        assert!(list.is_empty());
    }

    #[test]
    fn review_positions_give_the_diffs_order_whatever_the_lists() {
        let mut list = list(PULL_REQUEST);
        list.toggle_order();
        assert_eq!(list.review_position("README.md"), Some(0));
        assert_eq!(list.review_position("tests/lib.rs"), Some(2));
        assert_eq!(list.review_position("Cargo.lock"), Some(4));
        assert_eq!(list.review_position("missing.rs"), None);
        // Entries are in path order here, so this lists each path's place in review order.
        assert_eq!(list.review_positions(), [4, 0, 1, 3, 2]);
    }

    #[test]
    fn viewed_marks_come_from_github_by_path() {
        let mut list = list(PULL_REQUEST);
        let viewed = HashMap::from([
            ("src/lib.rs".to_string(), ViewedState::Viewed),
            ("README.md".to_string(), ViewedState::Dismissed),
            ("tests/lib.rs".to_string(), ViewedState::Unviewed),
        ]);

        list.set_viewed(&viewed);

        let marks: Vec<_> = list
            .rows()
            .map(|entry| (entry.path(), entry.viewed))
            .collect();
        assert_eq!(
            marks,
            [
                ("README.md", Some(ViewedState::Dismissed)),
                ("src/lib.rs", Some(ViewedState::Viewed)),
                ("tests/lib.rs", Some(ViewedState::Unviewed)),
                ("src/main.rs", None),
                ("Cargo.lock", None),
            ]
        );

        list.set_viewed(&HashMap::new());
        assert!(list.rows().all(|entry| entry.viewed.is_none()));
    }

    fn version(path: &str) -> Option<FileVersion> {
        Some(FileVersion {
            path: path.into(),
            oid: "1".repeat(40).parse().unwrap(),
            mode: FileMode::Regular,
            size: Some(10),
        })
    }

    #[test]
    fn entries_from_the_local_copy_keep_renames_counts_and_binary_files() {
        let renamed = repo_cache::ChangedFile {
            change: Change::Renamed { similarity: 90 },
            old: version("old/name.rs"),
            new: version("new/name.rs"),
            binary: false,
            lines: Some(LineCounts {
                added: 3,
                deleted: 2,
            }),
        };
        let deleted_binary = repo_cache::ChangedFile {
            change: Change::Deleted,
            old: version("logo.png"),
            new: None,
            binary: true,
            lines: None,
        };

        let renamed = FileEntry::from_local(&renamed, None);
        let deleted_binary = FileEntry::from_local(&deleted_binary, Some(true));

        assert_eq!(
            renamed,
            FileEntry {
                file: ChangedFile {
                    path: "new/name.rs".into(),
                    previous_path: Some("old/name.rs".into()),
                    additions: 3,
                    deletions: 2,
                    binary: false,
                    size: Some(10),
                    linguist_generated: None,
                },
                change: FileChange::Renamed,
                viewed: None,
            }
        );
        assert_eq!(deleted_binary.path(), "logo.png");
        assert_eq!(deleted_binary.change, FileChange::Deleted);
        assert!(deleted_binary.file.binary);
        assert_eq!(deleted_binary.file.linguist_generated, Some(true));
        assert_eq!(deleted_binary.file.changed_lines(), 0);
        assert_eq!(FileChange::Renamed.letter(), "R");
    }
}
