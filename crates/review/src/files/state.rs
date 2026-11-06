//! How a file's diff starts out when the pull request opens.

use super::{ChangedFile, Generated};

/// Files with more changed lines than this start collapsed, and their diff loads on a key.
pub const LARGE_FILE_LINES: u64 = 5_000;

/// How a file's diff starts out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InitialState {
    /// Why the file is generated, if it is. Generated files start collapsed.
    pub generated: Option<Generated>,
    /// The file has more than [`LARGE_FILE_LINES`] changed lines. It starts collapsed, and its
    /// diff loads only on a key.
    pub large: bool,
    /// The file is binary, so it has no text diff: images show before and after, and anything
    /// else shows as "binary file changed".
    pub binary: bool,
}

impl InitialState {
    /// Whether the file starts collapsed: it's generated, large or both.
    pub fn collapsed(&self) -> bool {
        self.generated.is_some() || self.large
    }
}

/// How the file's diff starts out.
pub fn initial_state(file: &ChangedFile) -> InitialState {
    InitialState {
        generated: super::generated(file),
        large: file.changed_lines() > LARGE_FILE_LINES,
        binary: file.binary,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, additions: u32, deletions: u32) -> ChangedFile {
        ChangedFile {
            path: path.to_string(),
            additions,
            deletions,
            ..Default::default()
        }
    }

    #[test]
    fn an_ordinary_file_starts_expanded() {
        let state = initial_state(&file("src/lib.rs", 120, 30));

        assert_eq!(
            state,
            InitialState {
                generated: None,
                large: false,
                binary: false,
            }
        );
        assert!(!state.collapsed());
    }

    #[test]
    fn a_file_marked_generated_starts_collapsed() {
        let state = initial_state(&ChangedFile {
            linguist_generated: Some(true),
            ..file("src/api/schema.graphql.ts", 40, 2)
        });

        assert_eq!(state.generated, Some(Generated::Attribute));
        assert!(state.collapsed());
    }

    #[test]
    fn a_lockfile_starts_collapsed() {
        let state = initial_state(&file("pnpm-lock.yaml", 12, 4));

        assert_eq!(state.generated, Some(Generated::Lockfile));
        assert!(!state.large);
        assert!(state.collapsed());
    }

    #[test]
    fn a_lockfile_unmarked_generated_starts_expanded() {
        let state = initial_state(&ChangedFile {
            linguist_generated: Some(false),
            ..file("Cargo.lock", 12, 4)
        });

        assert_eq!(state.generated, None);
        assert!(!state.collapsed());
    }

    #[test]
    fn exactly_five_thousand_changed_lines_start_expanded() {
        let state = initial_state(&file("src/table.rs", 3_000, 2_000));

        assert!(!state.large);
        assert!(!state.collapsed());
    }

    #[test]
    fn more_than_five_thousand_changed_lines_start_collapsed() {
        let state = initial_state(&file("src/table.rs", 3_000, 2_001));

        assert!(state.large);
        assert!(state.collapsed());
    }

    #[test]
    fn additions_alone_can_make_a_file_large() {
        assert!(initial_state(&file("data/words.txt", 5_001, 0)).large);
    }

    #[test]
    fn deletions_alone_can_make_a_file_large() {
        assert!(initial_state(&file("data/words.txt", 0, 5_001)).large);
    }

    #[test]
    fn line_counts_at_their_limits_do_not_overflow() {
        assert!(initial_state(&file("data/huge.txt", u32::MAX, u32::MAX)).large);
    }

    #[test]
    fn a_large_lockfile_is_both_generated_and_large() {
        let state = initial_state(&file("package-lock.json", 9_000, 8_000));

        assert_eq!(state.generated, Some(Generated::Lockfile));
        assert!(state.large);
        assert!(state.collapsed());
    }

    #[test]
    fn a_binary_file_is_flagged_and_starts_expanded() {
        let state = initial_state(&ChangedFile {
            binary: true,
            ..file("assets/logo.png", 0, 0)
        });

        assert!(state.binary);
        assert!(!state.collapsed());
    }

    #[test]
    fn a_binary_lockfile_is_flagged_and_starts_collapsed() {
        let state = initial_state(&ChangedFile {
            binary: true,
            ..file("bun.lockb", 0, 0)
        });

        assert!(state.binary);
        assert_eq!(state.generated, Some(Generated::Lockfile));
        assert!(state.collapsed());
    }
}
