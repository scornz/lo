//! GitHub's diff of a pull request, which its tab shows until the local copy is ready (design §11,
//! "First open").
//!
//! GitHub lists the files a pull request changes with each file's patch, its hunks only. They show
//! read-only in the same side-by-side diff as the local copy's, each line at its own row, so line
//! numbers are the file's. The rows between hunks are blank stand-ins that never show: each
//! excerpt is exactly one hunk, and excerpts can't be expanded. A file GitHub gives no patch for,
//! a binary file or one whose diff is too large, gets a placeholder saying so until the local copy
//! fills it in.

use std::ops::Range;
use std::time::Instant;

use git::status::{FileStatus as GitStatus, StatusCode, TrackedStatus};
use github::files::{FileStatus, Pages, PullFile};
use github::patch::{HunkLine, Patch};
use github::poll::RequestQueue;
use github::{GithubClient, GithubError, PullRef};
use gpui::{AppContext as _, AsyncWindowContext, WeakEntity};
use ui::SharedString;

use crate::file_display::{Collapsed, initial_collapse};
use crate::file_list::{FileChange, FileEntry};
use crate::pr_diff::{self, FileHeader, FilePlan, NewSide, Target, Warning};
use crate::pr_link::PrLink;
use crate::pr_tab::PrTab;

/// How a tab reads GitHub's list of files: with its account's client, one page at a time through
/// the account's request queue, or without signing in.
#[derive(Clone)]
pub(crate) struct FilesReader {
    pub client: GithubClient,
    /// The account's queue; `None` without signing in.
    pub queue: Option<RequestQueue>,
}

impl FilesReader {
    /// Every file GitHub lists for `pull`, with its patch.
    pub async fn read(&self, pull: &PullRef) -> Result<Vec<PullFile>, GithubError> {
        let mut files = Vec::new();
        let mut pages = Pages::new(pull);
        while let Some(path) = pages.next_path() {
            let page = match &self.queue {
                Some(queue) => queue.read(|| self.client.pull_files_page(&path)).await?,
                None => self.client.pull_files_page(&path).await?,
            };
            files.extend(page.files);
            pages.read(page.next)?;
        }
        Ok(files)
    }
}

/// Which pull request GitHub's diff is of, and what to call it.
#[derive(Clone, Debug)]
pub(crate) struct GithubDiff {
    pub link: PrLink,
    /// The diff's title: the link and the pull request's title.
    pub title: String,
    /// The repository's name as GitHub has it now, which starts each file's full path.
    pub repo_name: String,
}

/// Shows GitHub's diff of the pull request, `files`, in `tab`, unless the local copy's diff got
/// there first. `launched` is when the tab opened, for timing.
pub(crate) async fn show(
    tab: WeakEntity<PrTab>,
    source: GithubDiff,
    files: Vec<PullFile>,
    launched: Instant,
    cx: &mut AsyncWindowContext,
) -> anyhow::Result<()> {
    let link = &source.link;
    let plans = cx
        .background_spawn({
            let files = files.clone();
            async move { files.iter().map(plan).collect::<Vec<_>>() }
        })
        .await;
    let (workspace, style) =
        tab.read_with(cx, |tab, _| (tab.workspace().clone(), tab.diff_style()))?;
    let project = workspace.update(cx, |workspace, _| workspace.project().clone())?;
    let languages = project.read_with(cx, |project, _| project.languages().clone());
    let (_, editor) = pr_diff::new_diff_editor(&workspace, &project, &source.title, style, cx)?;
    let entries = files.iter().map(file_entry).collect();
    let positions = tab.update_in(cx, |tab, window, cx| {
        tab.show_github_diff(entries, editor.clone(), window, cx)
    })?;
    let Some(positions) = positions else {
        log::info!("{link}: the local copy's diff came first, so GitHub's doesn't show");
        return Ok(());
    };

    let mut target = Target {
        repo_name: &source.repo_name,
        project: &project,
        checkout: None,
        languages: &languages,
        editor: &editor,
        hide_whitespace: false,
    };
    for (index, (file, plan)) in files.iter().zip(plans).enumerate() {
        target.hide_whitespace = tab.read_with(cx, |tab, _| tab.hides_whitespace())?;
        let (position, path) = (positions[index], file.filename.as_str());
        let deleted = file.status == FileStatus::Removed;
        let shown = match target
            .add_file(position, path, deleted, plan, header(file), cx)
            .await
        {
            Ok(shown) => Ok(shown),
            Err(error) => {
                let plan = FilePlan::failed(&error);
                target
                    .add_file(position, path, deleted, plan, header(file), cx)
                    .await
            }
        };
        if let Err(error) = shown {
            log::error!("{link}: can't show {path} from GitHub's diff: {error:#}");
        }
        if index == 0 {
            log::info!(
                "{link}: GitHub's diff showed its first file {:?} after launch",
                launched.elapsed()
            );
        }
    }
    // A file added while the whitespace key was pressed may show the other way.
    tab.update_in(cx, |tab, window, cx| tab.sync_whitespace(window, cx))?;
    log::info!(
        "{link}: GitHub's diff of {} files shown {:?} after launch",
        files.len(),
        launched.elapsed()
    );
    Ok(())
}

/// What a file's header says about GitHub's change to it.
fn header(file: &PullFile) -> FileHeader {
    FileHeader {
        status: status(file.status),
        renamed_from: file
            .previous_filename
            .clone()
            .filter(|_| file.status == FileStatus::Renamed)
            .map(Into::into),
    }
}

/// The status a file's header shows for GitHub's.
fn status(status: FileStatus) -> GitStatus {
    let code = match status {
        FileStatus::Added | FileStatus::Copied => StatusCode::Added,
        FileStatus::Removed => StatusCode::Deleted,
        FileStatus::Renamed => StatusCode::Renamed,
        FileStatus::Changed => StatusCode::TypeChanged,
        FileStatus::Modified | FileStatus::Unchanged | FileStatus::Other => StatusCode::Modified,
    };
    GitStatus::Tracked(TrackedStatus {
        index_status: code,
        worktree_status: StatusCode::Unmodified,
    })
}

/// A file's two sides as far as its patch shows them: each hunk's lines at their own rows, and a
/// blank row for each line between hunks, which the patch doesn't have.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Sides {
    /// The base's text, or `None` when the pull request adds the file.
    pub old: Option<String>,
    /// The head's text, empty when the pull request deletes the file.
    pub new: String,
    /// The head's rows each hunk covers, the only rows that show. A hunk that ends deleting lines
    /// covers the row after it too, where its deleted lines show.
    pub rows: Vec<Range<u32>>,
}

/// Lays `patch`'s hunks out at their rows. `added` says the pull request adds the file, so it has
/// no base.
pub(crate) fn sides(patch: &Patch, added: bool) -> Sides {
    let mut old = String::new();
    let mut new = String::new();
    let (mut old_rows, mut new_rows) = (0, 0);
    let mut rows = Vec::with_capacity(patch.hunks.len());
    for hunk in &patch.hunks {
        pad(&mut old, &mut old_rows, hunk.old.start);
        pad(&mut new, &mut new_rows, hunk.new.start);
        for line in &hunk.lines {
            if line.is_old() {
                push_line(&mut old, &mut old_rows, line.text());
            }
            if line.is_new() {
                push_line(&mut new, &mut new_rows, line.text());
            }
        }
        let ends_deleting = matches!(hunk.lines.last(), Some(HunkLine::Deleted(_)));
        rows.push(hunk.new.start..hunk.new.end + u32::from(ends_deleting));
    }
    if patch.old_missing_newline {
        old.pop();
    }
    if patch.new_missing_newline {
        new.pop();
    }
    Sides {
        old: (!added).then_some(old),
        new,
        rows,
    }
}

/// Blank rows up to `row`.
fn pad(text: &mut String, rows: &mut u32, row: u32) {
    while *rows < row {
        text.push('\n');
        *rows += 1;
    }
}

fn push_line(text: &mut String, rows: &mut u32, line: &str) {
    text.push_str(line);
    text.push('\n');
    *rows += 1;
}

/// The label in the header of a file GitHub's diff leaves out.
pub(crate) const NOT_IN_GITHUBS_DIFF: &str = "Not in GitHub's diff";

/// What `file` shows in GitHub's diff: its hunks, or a placeholder until the local copy is ready.
/// It starts collapsed as the local copy's would (`file_display::initial_collapse`).
pub(crate) fn plan(file: &PullFile) -> FilePlan {
    let added = file.status == FileStatus::Added;
    let deleted = file.status == FileStatus::Removed;
    let plan = match file.parsed_patch() {
        Some(Ok(patch)) if !patch.hunks.is_empty() => {
            let sides = sides(&patch, added);
            let new = if deleted {
                NewSide::Deleted
            } else {
                NewSide::Git(sides.new)
            };
            FilePlan::diff(sides.old, new).with_rows(sides.rows)
        }
        Some(Err(error)) => left_out(
            deleted,
            format!("rozed couldn't read GitHub's diff of this file ({error})"),
        ),
        // GitHub gives no patch for a rename or a change of mode alone.
        _ if file.changes == 0 && file.status == FileStatus::Renamed => FilePlan {
            collapsed: Some(Collapsed::NoChanges),
            ..FilePlan::placeholder("(renamed without changes)", deleted)
        },
        _ if file.changes == 0 => left_out(
            deleted,
            "GitHub shows no diff of this file: it's binary, or no lines changed".to_string(),
        ),
        _ => left_out(
            deleted,
            format!(
                "GitHub doesn't show this file's diff, which is too large: {} lines added and {} \
                 deleted",
                file.additions, file.deletions
            ),
        ),
    };
    // GitHub's list says nothing of `.gitattributes` or sizes, so lockfiles by name and files
    // with too many changed lines.
    let collapsed = initial_collapse(&file_entry(file).file, true);
    FilePlan {
        badge: badge(file),
        collapsed: collapsed.or(plan.collapsed),
        ..plan
    }
}

/// A placeholder for a file GitHub's diff leaves out, which says why.
fn left_out(deleted: bool, why: String) -> FilePlan {
    FilePlan {
        warning: Some(Warning {
            label: NOT_IN_GITHUBS_DIFF.into(),
            detail: format!("{why}. It shows once the local copy is ready.").into(),
        }),
        ..FilePlan::placeholder(
            format!("({why}. It shows once the local copy is ready.)"),
            deleted,
        )
    }
}

/// What a file's header says about the change besides its status and a rename.
fn badge(file: &PullFile) -> Option<SharedString> {
    let from = file.previous_filename.as_deref();
    match (file.status, from) {
        (FileStatus::Copied, Some(from)) => Some(format!("copied from {from}").into()),
        (FileStatus::Changed, _) => Some("type or mode changed".into()),
        _ => None,
    }
}

/// How the file list shows `file` until the local copy lists the files.
pub(crate) fn file_entry(file: &PullFile) -> FileEntry {
    let renamed = matches!(file.status, FileStatus::Renamed | FileStatus::Copied);
    FileEntry {
        file: review::files::ChangedFile {
            path: file.filename.clone(),
            previous_path: file.previous_filename.clone().filter(|_| renamed),
            additions: file.additions,
            deletions: file.deletions,
            binary: false,
            // GitHub's list gives no sizes.
            size: None,
            linguist_generated: None,
        },
        change: change(file.status),
        viewed: None,
    }
}

/// How GitHub's status shows in the file list.
pub(crate) fn change(status: FileStatus) -> FileChange {
    match status {
        FileStatus::Added | FileStatus::Copied => FileChange::Added,
        FileStatus::Removed => FileChange::Deleted,
        FileStatus::Renamed => FileChange::Renamed,
        FileStatus::Modified | FileStatus::Changed | FileStatus::Unchanged | FileStatus::Other => {
            FileChange::Modified
        }
    }
}

#[cfg(test)]
mod tests {
    // Rows are ranges, and one hunk has one.
    #![allow(clippy::single_range_in_vec_init)]

    use super::*;

    use github::patch::parse_patch;

    use crate::pr_diff::Content;

    fn sides_of(patch: &str, added: bool) -> Sides {
        sides(&parse_patch(patch).unwrap(), added)
    }

    /// Row `row` of `text`.
    fn row(text: &str, row: usize) -> &str {
        text.split('\n').nth(row).unwrap()
    }

    #[test]
    fn hunks_sit_at_their_own_rows_on_both_sides() {
        let sides = sides_of(
            "@@ -2,3 +2,4 @@ fn main() {\n a\n-b\n+B\n+B2\n c\n@@ -10,2 +11,2 @@\n x\n-y\n+Y",
            false,
        );
        let old = sides.old.as_deref().unwrap();
        assert_eq!(
            (row(old, 0), row(old, 1), row(old, 2), row(old, 3)),
            ("", "a", "b", "c")
        );
        assert_eq!((row(old, 9), row(old, 10)), ("x", "y"));
        assert_eq!(old.lines().count(), 11);
        let new = &sides.new;
        assert_eq!(
            (row(new, 1), row(new, 2), row(new, 3), row(new, 4)),
            ("a", "B", "B2", "c")
        );
        assert_eq!((row(new, 10), row(new, 11)), ("x", "Y"));
        assert_eq!(new.lines().count(), 12);
        // The rows between hunks are blank on both sides.
        assert!((4..9).all(|r| row(old, r).is_empty()));
        assert!((5..10).all(|r| row(new, r).is_empty()));
        assert_eq!(sides.rows, [1..5, 10..12]);
    }

    #[test]
    fn an_added_file_has_no_base() {
        let sides = sides_of("@@ -0,0 +1,2 @@\n+one\n+two", true);
        assert_eq!(
            sides,
            Sides {
                old: None,
                new: "one\ntwo\n".into(),
                rows: vec![0..2],
            }
        );
    }

    #[test]
    fn a_hunk_that_ends_deleting_lines_shows_the_row_after_it() {
        let sides = sides_of("@@ -1,2 +0,0 @@\n-one\n-two", false);
        assert_eq!(
            sides,
            Sides {
                old: Some("one\ntwo\n".into()),
                new: String::new(),
                rows: vec![0..1],
            }
        );

        let sides = sides_of("@@ -4,3 +4,1 @@\n keep\n-gone\n-gone too", false);
        assert_eq!(sides.rows, [3..5]);
        assert_eq!(sides.new, "\n\n\nkeep\n");
    }

    #[test]
    fn a_missing_newline_at_the_end_stays_missing() {
        let sides = sides_of(
            "@@ -1 +1 @@\n-old\n\\ No newline at end of file\n+new",
            false,
        );
        assert_eq!(sides.old.as_deref(), Some("old"));
        assert_eq!(sides.new, "new\n");
    }

    fn file(status: FileStatus, patch: Option<&str>) -> PullFile {
        PullFile {
            filename: "src/lib.rs".into(),
            status,
            additions: 2,
            deletions: 1,
            changes: 3,
            patch: patch.map(str::to_string),
            previous_filename: None,
        }
    }

    #[test]
    fn a_file_with_a_patch_shows_its_hunks_alone() {
        let plan = plan(&file(
            FileStatus::Modified,
            Some("@@ -2 +2,2 @@\n-a\n+b\n+c"),
        ));
        assert_eq!(
            plan,
            FilePlan::diff(Some("\na\n".into()), NewSide::Git("\nb\nc\n".into()))
                .with_rows(vec![1..3])
        );

        let removed = plan_of(FileStatus::Removed, Some("@@ -1 +0,0 @@\n-gone"));
        assert_eq!(
            removed,
            FilePlan::diff(Some("gone\n".into()), NewSide::Deleted).with_rows(vec![0..1])
        );
    }

    fn plan_of(status: FileStatus, patch: Option<&str>) -> FilePlan {
        plan(&file(status, patch))
    }

    fn placeholder(plan: &FilePlan) -> &str {
        match &plan.content {
            Content::Placeholder { text, .. } => text,
            other => panic!("expected a placeholder, got {other:?}"),
        }
    }

    #[test]
    fn a_file_github_gives_no_patch_for_says_why_until_the_local_copy_is_ready() {
        let large = plan_of(FileStatus::Modified, None);
        assert_eq!(
            placeholder(&large),
            "(GitHub doesn't show this file's diff, which is too large: 2 lines added and 1 \
             deleted. It shows once the local copy is ready.)"
        );
        assert_eq!(large.warning.as_ref().unwrap().label, NOT_IN_GITHUBS_DIFF);

        let mut binary = file(FileStatus::Added, None);
        (binary.additions, binary.deletions, binary.changes) = (0, 0, 0);
        let binary = plan(&binary);
        assert!(placeholder(&binary).contains("it's binary"), "{binary:?}");
        assert_eq!(binary.warning.as_ref().unwrap().label, NOT_IN_GITHUBS_DIFF);

        let unreadable = plan_of(FileStatus::Modified, Some("@@ -1 +1 @@\n*x"));
        assert!(
            placeholder(&unreadable).starts_with("(rozed couldn't read GitHub's diff"),
            "{unreadable:?}"
        );
        assert!(unreadable.warning.is_some());
    }

    #[test]
    fn lockfiles_start_collapsed_in_githubs_diff_too() {
        let mut lockfile = file(FileStatus::Modified, Some("@@ -1 +1 @@\n-a\n+b"));
        lockfile.filename = "web/pnpm-lock.yaml".into();
        assert_eq!(
            plan(&lockfile).collapsed,
            Some(Collapsed::Generated(review::files::Generated::Lockfile))
        );
        assert_eq!(
            plan(&file(FileStatus::Modified, Some("@@ -1 +1 @@\n-a\n+b"))).collapsed,
            None
        );
    }

    #[test]
    fn a_rename_says_where_from() {
        let mut renamed = file(FileStatus::Renamed, None);
        renamed.previous_filename = Some("src/old.rs".into());
        (renamed.additions, renamed.deletions, renamed.changes) = (0, 0, 0);
        let unchanged = plan(&renamed);
        assert_eq!(placeholder(&unchanged), "(renamed without changes)");
        assert_eq!(unchanged.warning, None);
        assert_eq!(unchanged.badge, None);
        assert_eq!(unchanged.collapsed, Some(Collapsed::NoChanges));
        // The header shows "src/old.rs →" before the new path.
        assert_eq!(header(&renamed).renamed_from.as_deref(), Some("src/old.rs"));

        // With changes, it shows them, and says where it came from the same way.
        let mut edited = file(FileStatus::Renamed, Some("@@ -1 +1 @@\n-a\n+b"));
        edited.previous_filename = Some("src/old.rs".into());
        assert_eq!(
            plan(&edited).content,
            Content::Diff {
                old_text: Some("a\n".into()),
                new: NewSide::Git("b\n".into())
            }
        );
        assert_eq!(header(&edited).renamed_from.as_deref(), Some("src/old.rs"));
        // A copy isn't a rename: its badge says where from.
        let mut copied = edited.clone();
        copied.status = FileStatus::Copied;
        assert_eq!(header(&copied).renamed_from, None);
        assert_eq!(
            plan(&copied).badge.as_deref(),
            Some("copied from src/old.rs")
        );

        let entry = file_entry(&renamed);
        assert_eq!(entry.change, FileChange::Renamed);
        assert_eq!(entry.file.previous_path.as_deref(), Some("src/old.rs"));
    }
}
