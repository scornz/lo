//! GitHub's diff of a pull request, which its tab shows until the local copy is ready (design §11,
//! "First open").
//!
//! GitHub lists the files a pull request changes with each file's patch, its hunks only. They show
//! read-only in the same side-by-side diff as the local copy's, each line at its own row, so line
//! numbers are the file's. The rows between hunks are fillers that never show: each excerpt is
//! exactly one hunk, and excerpts can't be expanded. Zed diffs the two sides again, so each filler
//! is unique and the same on both sides, and a file whose diff still marks a line outside GitHub's
//! hunks shows GitHub's patch as text instead. A file GitHub gives no patch for, a binary file or
//! one whose diff is too large, gets a placeholder saying so until the local copy fills it in.

use std::ops::Range;
use std::time::Instant;

use buffer_diff::BufferDiffSnapshot;
use git::status::{FileStatus as GitStatus, StatusCode, TrackedStatus};
use github::files::{FileStatus, PullFile, PullFiles, ReadFiles};
use github::patch::{HunkLine, Patch};
use github::poll::RequestQueue;
use github::{GithubClient, GithubError, PullRef};
use gpui::{AppContext as _, AsyncWindowContext, WeakEntity};
use language::{BufferSnapshot, Point};
use ui::SharedString;

use crate::file_display::initial_collapse;
use crate::file_list::{FileChange, FileEntry};
use crate::pr_diff::{self, FileHeader, FilePlan, NewSide, Target, Warning};
use crate::pr_link::PrLink;
use crate::pr_tab::PrTab;

/// How a tab reads GitHub's list of files.
#[derive(Clone)]
pub(crate) enum FilesReader {
    /// With its account's client, one page at a time through the account's request queue.
    Account {
        client: GithubClient,
        queue: RequestQueue,
    },
    /// Without signing in, with the app's one client without an account
    /// ([`crate::github_api::anonymous_client`]).
    Anonymous(GithubClient),
}

/// How many pages of a pull request's files a tab reads without signing in. GitHub allows 60
/// requests an hour per IP address without a token, and opening a pull request needs one.
pub(crate) const ANONYMOUS_PAGES: usize = 3;

/// Without signing in, no page of files is read while this few requests are left, so they stay
/// for opening pull requests.
pub(crate) const ANONYMOUS_RESERVE: u32 = 10;

impl FilesReader {
    /// The files GitHub lists for `pull`, with their patches, as far as the reader reads.
    pub async fn read(&self, pull: &PullRef) -> Result<PullFiles, GithubError> {
        match self {
            Self::Account { client, queue } => {
                client
                    .read_pull_files(pull, Some(queue), ReadFiles::ALL)
                    .await
            }
            Self::Anonymous(client) => {
                let limits = ReadFiles {
                    max_pages: ANONYMOUS_PAGES,
                    keep_requests: ANONYMOUS_RESERVE,
                };
                client.read_pull_files(pull, None, limits).await
            }
        }
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
    /// The head commit GitHub listed the files at, when their links say.
    pub head: Option<String>,
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
        tab.show_github_diff(entries, editor.clone(), source.head.clone(), window, cx)
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
                let plan = match error.downcast_ref::<OutsideGithubsHunks>() {
                    Some(OutsideGithubsHunks(why)) => {
                        log::warn!("{link}: showing GitHub's patch of {path} as text: {why}");
                        patch_text_plan(file, why)
                    }
                    None => FilePlan::failed(&error),
                };
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
/// filler row for each line between hunks, which the patch doesn't have.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Sides {
    /// The base's text, or `None` when the pull request adds the file.
    pub old: Option<String>,
    /// The head's text, empty when the pull request deletes the file.
    pub new: String,
    /// The head's rows each hunk covers, the only rows that show. A hunk that ends deleting lines
    /// covers the row after it too, where its deleted lines show.
    pub rows: Vec<Range<u32>>,
    /// GitHub's hunks, which every hunk of Zed's diff of the two sides must lie inside.
    pub hunks: Vec<HunkRows>,
}

/// The rows one of GitHub's hunks covers on each side.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HunkRows {
    pub old: Range<u32>,
    pub new: Range<u32>,
}

/// Zed diffs the two sides again rather than taking GitHub's hunks, and lines repeated across
/// GitHub's hunks can pair up differently, so its diff can mark lines GitHub didn't. This
/// checks that every hunk of `diff`, of `buffer` against its base, lies inside one of GitHub's
/// `hunks` on both sides, and says where the first that doesn't is.
pub(crate) fn check_hunks(
    diff: &BufferDiffSnapshot,
    buffer: &BufferSnapshot,
    hunks: &[HunkRows],
) -> Result<(), String> {
    let base = diff.base_text();
    // A range of points as rows: a hunk ends at the start of the row after it, or partway along
    // the last row when the text has no newline at its end.
    let rows = |range: Range<Point>| {
        let end = range.end.row + u32::from(range.end.column > 0);
        range.start.row..end.max(range.start.row)
    };
    for hunk in diff.hunks(&buffer.text) {
        let new = rows(hunk.range.clone());
        let old = rows(
            base.offset_to_point(hunk.diff_base_byte_range.start)
                ..base.offset_to_point(hunk.diff_base_byte_range.end),
        );
        let inside = |outer: &Range<u32>, inner: &Range<u32>| {
            outer.start <= inner.start && inner.end <= outer.end
        };
        if !hunks
            .iter()
            .any(|github| inside(&github.old, &old) && inside(&github.new, &new))
        {
            return Err(format!(
                "Zed's diff of its hunks puts a change at line {} of the base and {} of the head, \
                 outside them",
                old.start + 1,
                new.start + 1
            ));
        }
    }
    Ok(())
}

/// The most rows GitHub's diff lays a file out over: a small change far down a long file would
/// otherwise fill both sides with that many rows. The local copy doesn't load files over 1 MiB,
/// which few files this long are under.
pub(crate) const MAX_ROWS: u32 = 50_000;

/// Lays `patch`'s hunks out at their rows. `added` says the pull request adds the file, so it has
/// no base. Fails, saying why, when the hunks can't be laid out so: when they don't line up, reach
/// past [`MAX_ROWS`], or a line holds a carriage return, which the buffer would split it at.
pub(crate) fn sides(patch: &Patch, added: bool) -> Result<Sides, String> {
    if let Some(last) = patch.hunks.last() {
        let reach = last.old.end.max(last.new.end);
        if reach > MAX_ROWS {
            return Err(format!(
                "its hunks reach line {}, past the {} lines GitHub's diff lays out",
                thousands(reach),
                thousands(MAX_ROWS)
            ));
        }
    }
    let mut old = String::new();
    let mut new = String::new();
    let (mut old_rows, mut new_rows) = (0, 0);
    let mut fillers = 0;
    let mut rows = Vec::with_capacity(patch.hunks.len());
    let mut hunks = Vec::with_capacity(patch.hunks.len());
    for (index, hunk) in patch.hunks.iter().enumerate() {
        // The lines before a hunk are unchanged, so both sides have as many.
        let gap = hunk.new.start.checked_sub(new_rows);
        if gap.is_none() || hunk.old.start.checked_sub(old_rows) != gap {
            return Err(format!(
                "its hunk {} starts at line {} of the base but {} of the head",
                index + 1,
                hunk.old.start + 1,
                hunk.new.start + 1
            ));
        }
        for _ in 0..gap.unwrap_or_default() {
            let filler = filler(fillers);
            push_line(&mut old, &mut old_rows, &filler);
            push_line(&mut new, &mut new_rows, &filler);
            fillers += 1;
        }
        for line in &hunk.lines {
            if line.text().contains('\r') {
                return Err("a line of it holds a carriage return".to_string());
            }
            if line.is_old() {
                push_line(&mut old, &mut old_rows, line.text());
            }
            if line.is_new() {
                push_line(&mut new, &mut new_rows, line.text());
            }
        }
        let ends_deleting = matches!(hunk.lines.last(), Some(HunkLine::Deleted(_)));
        // The row after a hunk that ends deleting lines shows, so it must be the file's.
        if ends_deleting
            && let Some(next) = patch.hunks.get(index + 1)
            && next.new.start > hunk.new.end
        {
            return Err(format!(
                "its hunk {} ends deleting lines before lines it doesn't show",
                index + 1
            ));
        }
        rows.push(hunk.new.start..hunk.new.end + u32::from(ends_deleting));
        hunks.push(HunkRows {
            old: hunk.old.clone(),
            new: hunk.new.clone(),
        });
    }
    if patch.old_missing_newline {
        old.pop();
    }
    if patch.new_missing_newline {
        new.pop();
    }
    Ok(Sides {
        old: (!added).then_some(old),
        new,
        rows,
        hunks,
    })
}

/// The `index`-th row between hunks, on both sides: a line no file has, each different, and the
/// same on both sides, so Zed's diff pairs the two sides' fillers with each other and nothing
/// else. They never show: each excerpt is one hunk's rows.
fn filler(index: u32) -> String {
    format!("\u{E000}{index}")
}

/// `n` with a comma between each group of three digits.
pub(crate) fn thousands(n: impl Into<u64>) -> String {
    let digits = n.into().to_string();
    let mut text = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            text.push(',');
        }
        text.push(digit);
    }
    text
}

fn push_line(text: &mut String, rows: &mut u32, line: &str) {
    text.push_str(line);
    text.push('\n');
    *rows += 1;
}

/// Why a file of GitHub's diff shows as GitHub's patch, as text: its hunks couldn't be shown at
/// their rows, or Zed's diff of them didn't match GitHub's.
#[derive(Debug)]
pub(crate) struct OutsideGithubsHunks(pub String);

impl std::fmt::Display for OutsideGithubsHunks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for OutsideGithubsHunks {}

/// The label in the header of a file that shows as GitHub's patch, as text.
pub(crate) const GITHUBS_PATCH: &str = "GitHub's patch";

/// GitHub's `patch` of a file, as text, for when its hunks can't show at their rows, and `why`.
pub(crate) fn patch_text(patch: &str, deleted: bool, why: &str) -> FilePlan {
    FilePlan {
        warning: Some(Warning {
            label: GITHUBS_PATCH.into(),
            detail: format!(
                "This is GitHub's patch of this file, as text: rozed can't show it as a diff, \
                 since {why}. The diff shows once the local copy is ready."
            )
            .into(),
        }),
        ..FilePlan::placeholder(patch, deleted)
    }
}

/// The label in the header of a file GitHub's diff leaves out.
pub(crate) const NOT_IN_GITHUBS_DIFF: &str = "Not in GitHub's diff";

/// What `file` shows in GitHub's diff: its hunks, or a placeholder until the local copy is ready.
/// It starts collapsed as the local copy's would (`file_display::initial_collapse`).
pub(crate) fn plan(file: &PullFile) -> FilePlan {
    let added = file.status == FileStatus::Added;
    let deleted = file.status == FileStatus::Removed;
    let plan = match file.parsed_patch() {
        Some(Ok(patch)) if !patch.hunks.is_empty() => match sides(&patch, added) {
            Ok(sides) => {
                let new = if deleted {
                    NewSide::Deleted
                } else {
                    NewSide::Git(sides.new)
                };
                FilePlan::diff(sides.old, new)
                    .with_rows(sides.rows)
                    .inside(sides.hunks)
            }
            Err(why) => patch_text(file.patch.as_deref().unwrap_or_default(), deleted, &why),
        },
        Some(Err(error)) => left_out(
            deleted,
            format!("rozed couldn't read GitHub's diff of this file ({error})"),
        ),
        // GitHub gives no patch for a rename alone, nor for a binary file renamed and changed.
        _ if file.changes == 0 && file.status == FileStatus::Renamed => FilePlan::placeholder(
            "(Renamed. GitHub shows no diff of its contents, which are unchanged or binary. The \
             local copy's diff shows which once it's ready.)",
            deleted,
        ),
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
    with_header(file, plan)
}

/// `file`'s patch as text, for when its hunks didn't lie where GitHub put them (`why`), with
/// what its header says as [`plan`] would have it.
pub(crate) fn patch_text_plan(file: &PullFile, why: &str) -> FilePlan {
    let deleted = file.status == FileStatus::Removed;
    with_header(
        file,
        patch_text(file.patch.as_deref().unwrap_or_default(), deleted, why),
    )
}

/// `plan` with what `file`'s header says besides its status: a badge, and whether it starts
/// collapsed. GitHub's list says nothing of `.gitattributes` or sizes, so lockfiles count by name
/// and large files by their changed lines.
fn with_header(file: &PullFile, plan: FilePlan) -> FilePlan {
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
            // Without a patch or a changed line, GitHub doesn't say how many lines changed: a
            // binary file, or a rename or mode change alone.
            binary: file.patch.is_none() && file.changes == 0,
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

    use std::path::Path;
    use std::sync::Arc;

    use buffer_diff::BufferDiff;
    use github::patch::parse_patch;

    use crate::file_display::Collapsed;
    use gpui::TestAppContext;

    use crate::pr_diff::Content;

    fn sides_of(patch: &str, added: bool) -> Sides {
        sides(&parse_patch(patch).unwrap(), added).unwrap()
    }

    /// Row `row` of `text`.
    fn row(text: &str, row: usize) -> &str {
        text.split('\n').nth(row).unwrap()
    }

    /// Whether Zed's diff of `sides` stays inside GitHub's hunks.
    async fn zeds_diff_of(sides: &Sides, cx: &mut TestAppContext) -> Result<(), String> {
        let buffer = cx.new(|cx| language::Buffer::local(sides.new.clone(), cx));
        let snapshot = buffer.read_with(cx, |buffer, _| buffer.snapshot());
        let diff = cx.new(|cx| BufferDiff::new(&snapshot.text, None, None, cx));
        let old = sides.old.as_deref().map(Arc::from);
        diff.update(cx, |diff, cx| {
            diff.set_base_text(old, snapshot.text.clone(), cx)
        })
        .await;
        diff.read_with(cx, |diff, cx| {
            check_hunks(&diff.snapshot(cx), &snapshot, &sides.hunks)
        })
    }

    /// GitHub's patches for two files in rozed's history.
    const SHIP: &str = include_str!("../tests/fixtures/github_patches/fc5ac97-ship.rs.patch");
    const OPEN_PULL_REQUESTS: &str =
        include_str!("../tests/fixtures/github_patches/94c4db2-open-pull-requests.json.patch");

    /// GitHub's patch of `old` to `new`, as git makes it.
    fn github_patch(old: &str, new: &str, dir: &Path) -> Option<String> {
        let (a, b) = (dir.join("a"), dir.join("b"));
        std::fs::write(&a, old).unwrap();
        std::fs::write(&b, new).unwrap();
        let output = std::process::Command::new("git")
            .args([
                "diff",
                "--no-index",
                "--no-color",
                "--diff-algorithm=myers",
                "-U3",
            ])
            .arg(&a)
            .arg(&b)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        let diff = String::from_utf8(output.stdout).unwrap();
        let start = diff.find("\n@@ ")?;
        Some(diff[start + 1..].trim_end_matches('\n').to_string())
    }

    /// Files of short, often repeated lines, as code has: blank lines, braces, `}`.
    fn repetitive_files(count: usize) -> Vec<(String, String)> {
        let mut seed: u64 = 0x1234_5678_9abc_def0;
        let mut random = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let alphabet = ["", "", "", "", "a", "b", "}", "    x", "fn f() {", "# h"];
        let pick = move |random: &mut dyn FnMut(u64) -> u64| {
            alphabet[random(alphabet.len() as u64) as usize]
        };
        (0..count)
            .map(|_| {
                let length = 20 + random(60) as usize;
                let base: Vec<&str> = (0..length).map(|_| pick(&mut random)).collect();
                let mut head = base.clone();
                for _ in 0..1 + random(4) {
                    let at = random(head.len() as u64 + 1) as usize;
                    match random(3) {
                        0 => head.insert(at, pick(&mut random)),
                        1 if at < head.len() => {
                            head.remove(at);
                        }
                        _ if at < head.len() => head[at] = "edited",
                        _ => {}
                    }
                }
                let text = |lines: &[&str]| lines.iter().map(|line| format!("{line}\n")).collect();
                (text(&base), text(&head))
            })
            .collect()
    }

    #[gpui::test]
    async fn zeds_diff_of_githubs_hunks_stays_inside_them(cx: &mut TestAppContext) {
        // fc5ac97's ship.rs: a test ending in a blank line, inserted after blank lines that
        // repeat its own last lines.
        let mut outside = Vec::new();
        if let Err(error) = zeds_diff_of(&sides_of(SHIP, false), cx).await {
            outside.push(format!("fc5ac97's ship.rs: {error}"));
        }

        let dir = tempfile::TempDir::new().unwrap();
        for (index, (old, new)) in repetitive_files(500).iter().enumerate() {
            let Some(patch) = github_patch(old, new, dir.path()) else {
                continue;
            };
            let sides = sides_of(&patch, false);
            if let Err(error) = zeds_diff_of(&sides, cx).await {
                outside.push(format!("file {index}: {error}\n{patch}"));
            }
        }
        assert!(outside.is_empty(), "{}", outside.join("\n\n"));
    }

    #[test]
    fn hunks_sit_at_their_own_rows_on_both_sides() {
        let sides = sides_of(
            "@@ -2,3 +2,4 @@ fn main() {\n a\n-b\n+B\n+B2\n c\n@@ -10,2 +11,2 @@\n x\n-y\n+Y",
            false,
        );
        let old = sides.old.as_deref().unwrap();
        assert_eq!((row(old, 1), row(old, 2), row(old, 3)), ("a", "b", "c"));
        assert_eq!((row(old, 9), row(old, 10)), ("x", "y"));
        assert_eq!(old.lines().count(), 11);
        let new = &sides.new;
        assert_eq!(
            (row(new, 1), row(new, 2), row(new, 3), row(new, 4)),
            ("a", "B", "B2", "c")
        );
        assert_eq!((row(new, 10), row(new, 11)), ("x", "Y"));
        assert_eq!(new.lines().count(), 12);
        // Each row between hunks has a filler of its own, the same on both sides.
        let fillers = |text: &str, rows: Vec<usize>| -> Vec<String> {
            rows.into_iter().map(|r| row(text, r).to_string()).collect()
        };
        let mut old_fillers = fillers(old, vec![0]);
        old_fillers.extend(fillers(old, (4..9).collect()));
        let mut new_fillers = fillers(new, vec![0]);
        new_fillers.extend(fillers(new, (5..10).collect()));
        assert_eq!(old_fillers, new_fillers);
        assert_eq!(old_fillers, (0..6).map(filler).collect::<Vec<_>>());
        assert_eq!(sides.rows, [1..5, 10..12]);
        assert_eq!(
            sides.hunks,
            [
                HunkRows {
                    old: 1..4,
                    new: 1..5
                },
                HunkRows {
                    old: 9..11,
                    new: 10..12
                }
            ]
        );
    }

    #[test]
    fn hunks_that_dont_line_up_have_no_sides() {
        // One line before the first hunk on one side, two on the other.
        let patch = parse_patch("@@ -2 +3 @@\n-a\n+b").unwrap();
        assert!(
            sides(&patch, false)
                .unwrap_err()
                .contains("starts at line 2 of the base")
        );
        // Three lines between the hunks on one side, two on the other.
        let patch = parse_patch("@@ -1 +1 @@\n-a\n+b\n@@ -5 +4 @@\n-c\n+d").unwrap();
        assert!(sides(&patch, false).unwrap_err().contains("hunk 2"));
        // A carriage return alone would split the line in the buffer.
        let patch = parse_patch("@@ -1 +1 @@\n-a\r b\n+c").unwrap();
        assert!(
            sides(&patch, false)
                .unwrap_err()
                .contains("carriage return")
        );
    }

    #[gpui::test]
    async fn changes_paired_across_githubs_hunks_fail_the_check(cx: &mut TestAppContext) {
        // 94c4db2's poll cassette: two entries of a list swapped, so GitHub's hunks delete and add
        // the same lines, which Zed's diff pairs up across the rows between them.
        let sides = sides_of(OPEN_PULL_REQUESTS, false);
        let error = zeds_diff_of(&sides, cx).await.unwrap_err();
        assert!(error.contains("outside them"), "{error}");
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
                hunks: vec![HunkRows {
                    old: 0..0,
                    new: 0..2
                }],
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
                hunks: vec![HunkRows {
                    old: 0..2,
                    new: 0..0
                }],
            }
        );

        let sides = sides_of("@@ -4,3 +4,1 @@\n keep\n-gone\n-gone too", false);
        assert_eq!(sides.rows, [3..5]);
        assert_eq!(
            sides.new,
            format!("{}\n{}\n{}\nkeep\n", filler(0), filler(1), filler(2))
        );

        // Before the next hunk, the row after the deleted lines would be a filler.
        let patch = parse_patch("@@ -4,2 +4 @@\n keep\n-gone\n@@ -9 +8 @@\n-x\n+y").unwrap();
        assert!(
            super::sides(&patch, false)
                .unwrap_err()
                .contains("ends deleting lines")
        );
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
            blob_url: None,
            contents_url: None,
        }
    }

    #[test]
    fn a_file_with_a_patch_shows_its_hunks_alone() {
        let plan = plan(&file(
            FileStatus::Modified,
            Some("@@ -2 +2,2 @@\n-a\n+b\n+c"),
        ));
        let (old, new) = (
            format!("{}\na\n", filler(0)),
            format!("{}\nb\nc\n", filler(0)),
        );
        assert_eq!(
            plan,
            FilePlan::diff(Some(old), NewSide::Git(new))
                .with_rows(vec![1..3])
                .inside(vec![HunkRows {
                    old: 1..2,
                    new: 1..3
                }])
        );

        let removed = plan_of(FileStatus::Removed, Some("@@ -1 +0,0 @@\n-gone"));
        assert_eq!(
            removed,
            FilePlan::diff(Some("gone\n".into()), NewSide::Deleted)
                .with_rows(vec![0..1])
                .inside(vec![HunkRows {
                    old: 0..1,
                    new: 0..0
                }])
        );

        // Hunks that can't show at their rows show as GitHub's patch, as text.
        let patch = "@@ -2 +3 @@\n-a\n+b";
        let misaligned = plan_of(FileStatus::Modified, Some(patch));
        assert_eq!(placeholder(&misaligned), patch);
        assert_eq!(misaligned.warning.unwrap().label, GITHUBS_PATCH);
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
    fn a_patch_shown_as_text_keeps_its_collapse_and_badge() {
        let mut lockfile = file(FileStatus::Copied, Some("@@ -1 +1 @@\n-a\n+b"));
        lockfile.filename = "web/yarn.lock".into();
        lockfile.previous_filename = Some("yarn.lock".into());

        let plan = patch_text_plan(&lockfile, "a hunk lies outside GitHub's");

        assert_eq!(
            plan.collapsed,
            Some(Collapsed::Generated(review::files::Generated::Lockfile))
        );
        assert_eq!(plan.badge.as_deref(), Some("copied from yarn.lock"));
        assert_eq!(plan.warning.as_ref().unwrap().label, GITHUBS_PATCH);
        assert!(placeholder(&plan).contains("+b"));
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
        // GitHub says the same of a binary file renamed and changed, so it doesn't say "without
        // changes".
        assert_eq!(
            placeholder(&unchanged),
            "(Renamed. GitHub shows no diff of its contents, which are unchanged or binary. The \
             local copy's diff shows which once it's ready.)"
        );
        assert_eq!(unchanged.warning, None);
        assert_eq!(unchanged.badge, None);
        assert_eq!(unchanged.collapsed, None);
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
        // Its line counts aren't known, rather than none.
        assert!(entry.file.binary);
        assert!(
            !file_entry(&file(FileStatus::Modified, Some("@@ -1 +1 @@\n-a\n+b")))
                .file
                .binary
        );
    }

    #[test]
    fn hunks_far_down_a_long_file_show_as_githubs_patch() {
        // Laying the hunk out at its row would fill 60,000 rows on each side.
        let patch = "@@ -60000 +60000 @@\n-a\n+b";
        let error = super::sides(&parse_patch(patch).unwrap(), false).unwrap_err();
        assert!(error.contains("line 60,000"), "{error}");
        let plan = plan_of(FileStatus::Modified, Some(patch));
        assert_eq!(placeholder(&plan), patch);
        assert_eq!(plan.warning.as_ref().unwrap().label, GITHUBS_PATCH);

        // Up to the limit, it's laid out.
        let patch = format!("@@ -{MAX_ROWS} +{MAX_ROWS} @@\n-a\n+b");
        assert!(super::sides(&parse_patch(&patch).unwrap(), false).is_ok());
    }
}
