//! A pull request in Zed's side-by-side diff (`SplittableEditor`), every changed file in one
//! scrolling view.
//!
//! The right side shows project buffers opened from a checkout of the head, so language servers
//! work there. The left side shows the base's text, read from git. Modelled on Zed's
//! `git_ui_core/src/file_diff_view.rs` (project buffer on the right) and
//! `git_ui/src/commit_view.rs` (many files in one view), without depending on either crate.
//!
//! No changed file is ever left out. One that can't be shown as text (binary, too large, or
//! failing to load) gets a placeholder saying why, and the title counts them. A submodule shows
//! "Subproject commit a → b", and a header badge marks renames, type and mode changes, and
//! changes to line endings alone.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Context as _;
use buffer_diff::BufferDiff;
use editor::{
    Addon, EditorSettings, HiddenDiffHunkRenderer, MultiBuffer, SplittableEditor,
    multibuffer_context_lines,
};
use git::status::{FileStatus, StatusCode, TrackedStatus};
use gpui::{
    AnyElement, App, AppContext as _, AsyncWindowContext, Entity, IntoElement as _,
    ParentElement as _, SharedString, Styled as _, WeakEntity, Window, div,
};
use language::{
    Buffer, BufferId, Capability, DiskState, File, LanguageRegistry, LineEnding,
    OffsetRangeExt as _, Point, ReplicaId, Rope, TextBuffer,
};
use multi_buffer::{ExcerptBoundaryInfo, PathKey};
use project::{Project, ProjectPath, WorktreeId};
use repo_cache::{
    Change, ChangedFile, FileMode, HeadSource, Oid, PullRequestRefs, Repo, RepoCache, Token,
};
use settings::{DiffViewStyle, Settings as _};
use theme::ActiveTheme as _;
use util::paths::PathStyle;
use util::rel_path::RelPath;
use workspace::Workspace;

use crate::github_api;
use crate::local_copies;
use crate::pr_link::PrLink;
use crate::trust;

/// Files with more changed lines than this get a placeholder instead of loading (design §5).
const LARGE_FILE_LINES: u32 = 5_000;

/// A pull request copied locally.
struct LocalPullRequest {
    repo: Repo,
    /// A checkout of `head`.
    checkout: PathBuf,
    /// The commit the diff starts from: where the head branched from the base branch.
    base: Oid,
    /// The pull request's head commit.
    head: Oid,
    /// What changed between `base` and `head`, in git's order.
    files: Vec<ChangedFile>,
}

/// Fetches the pull request, copies it locally and shows it in a new tab of `workspace`.
/// `launched` is when rozed started, for timing.
pub async fn open(
    workspace: WeakEntity<Workspace>,
    link: PrLink,
    launched: Instant,
    cx: &mut AsyncWindowContext,
) -> anyhow::Result<()> {
    let (http, token) = cx.update(|_, cx| (cx.http_client(), local_copies::token(cx)))?;
    let pull_request = github_api::fetch_pull_request(http, &link, token.as_ref()).await?;
    log::info!(
        "{link}: {} merges into {} ({:?} after launch)",
        pull_request.head.sha,
        pull_request.base.branch,
        launched.elapsed()
    );

    let cache = cx.update(|_, cx| local_copies::cache(cx))?.await?;
    let started = Instant::now();
    let local = cx
        .background_spawn({
            let link = link.clone();
            let pull_request = pull_request.clone();
            async move { copy(&cache, &link, &pull_request, token) }
        })
        .await
        .with_context(|| format!("copying {link} locally"))?;
    log::info!(
        "{link}: local copy ready in {:?}, {} changed files, base {}, head {}",
        started.elapsed(),
        local.files.len(),
        local.base,
        local.head
    );

    let project = workspace.update(cx, |workspace, _| workspace.project().clone())?;
    // Trust the checkout rozed just made, and nothing else, so rust-analyzer starts there. There's
    // no trust prompt yet (design §7), so this trusts whatever pull request rozed was given.
    cx.update(|_, cx| trust::trust_path(&project, &local.checkout, cx))?;
    let (worktree, _) = project
        .update(cx, |project, cx| {
            project.find_or_create_worktree(&local.checkout, true, cx)
        })
        .await
        .with_context(|| format!("opening {}", local.checkout.display()))?;
    let worktree_id = worktree.read_with(cx, |worktree, _| worktree.id());
    let languages = project.read_with(cx, |project, _| project.languages().clone());

    let title = format!("{link}: {}", pull_request.title);
    let multi_buffer = cx.new(|cx| {
        let mut multi_buffer = MultiBuffer::new(Capability::ReadOnly).with_title(title.clone());
        multi_buffer.set_all_diff_hunks_expanded(cx);
        multi_buffer
    });
    let editor = workspace.update_in(cx, |workspace, window, cx| {
        let workspace_entity = cx.entity();
        let editor = cx.new(|cx| {
            let editor = SplittableEditor::new(
                EditorSettings::get_global(cx).diff_view_style,
                multi_buffer.clone(),
                project.clone(),
                workspace_entity,
                window,
                cx,
            );
            // Hides the controls for staging and restoring hunks.
            editor.set_diff_hunk_renderer(Some(Arc::new(HiddenDiffHunkRenderer)), cx);
            editor.rhs_editor().update(cx, |editor, cx| {
                editor.set_show_bookmarks(false, cx);
                editor.set_show_breakpoints(false, cx);
            });
            editor
        });
        workspace.add_item_to_active_pane(Box::new(editor.clone()), None, true, window, cx);
        editor
    })?;

    let target = Target {
        local: &local,
        project: &project,
        worktree_id,
        languages: &languages,
        editor: &editor,
    };
    let mut statuses = HashMap::new();
    let mut badges = HashMap::new();
    let mut not_shown = 0;
    for (index, file) in local.files.iter().enumerate() {
        let contents = match target.contents(file, cx).await {
            Ok(contents) => contents,
            Err(error) => {
                log::warn!("{link}: can't show {}: {error:#}", file.path());
                Contents::placeholder(format!("(can't show this file: {error:#})"))
            }
        };
        let shown = match target.add_file(index, file, contents, cx).await {
            Ok(shown) => shown,
            Err(error) => {
                // The file's own buffer failed; show why in one that can't.
                log::warn!("{link}: can't show {}: {error:#}", file.path());
                let contents = Contents::placeholder(format!("(can't show this file: {error:#})"));
                target.add_file(index, file, contents, cx).await?
            }
        };
        statuses.insert(shown.buffer_id, status(file));
        if let Some(badge) = shown.badge {
            badges.insert(shown.buffer_id, badge);
        }
        if shown.placeholder {
            not_shown += 1;
        }
        if index == 0 {
            log::info!(
                "{link}: first file shown {:?} after launch",
                launched.elapsed()
            );
        }
    }
    log::info!(
        "{link}: all {} files shown {:?} after launch, {not_shown} as placeholders",
        local.files.len(),
        launched.elapsed()
    );

    if not_shown > 0 {
        let files = if not_shown == 1 { "file" } else { "files" };
        multi_buffer.update(cx, |multi_buffer, cx| {
            multi_buffer.set_title(format!("{title} ({not_shown} {files} not shown)"), cx)
        });
    }
    editor.update(cx, |editor, cx| {
        editor.rhs_editor().update(cx, |editor, _| {
            editor.register_addon(FileAddon { statuses, badges });
        });
    });
    Ok(())
}

/// Fetches the pull request into its repository's clone, finds where it branched from its base,
/// lists what changed and checks the head out. Blocks while git runs.
fn copy(
    cache: &RepoCache,
    link: &PrLink,
    pull_request: &github_api::PullRequest,
    token: Option<Token>,
) -> anyhow::Result<LocalPullRequest> {
    // The repository's canonical name, not the link's, so a renamed one keeps its clone.
    let (owner, name) = pull_request.base.repo.owner_and_name()?;
    let repo = cache.open(owner, name, token)?;
    let fetched = repo
        .with_progress({
            let link = link.clone();
            move |progress| {
                log::debug!(
                    "{link}: {} {}/{}",
                    progress.stage,
                    progress.done,
                    progress.total.map_or("?".into(), |total| total.to_string())
                )
            }
        })
        .fetch_pull(&PullRequestRefs {
            number: link.number,
            head: pull_request.head.sha.parse().ok(),
            base_branch: pull_request.base.branch.clone(),
            base: pull_request.base.sha.parse().ok(),
        })?;
    match &fetched.head_source {
        HeadSource::PullRef => {}
        HeadSource::Api { pull_ref } => log::warn!(
            "{link}: showing GitHub's head {}, though refs/pull/{}/head is still {pull_ref}",
            fetched.head,
            link.number
        ),
        HeadSource::PullRefNotApi { api } => log::warn!(
            "{link}: GitHub's API says the head is {api}, which GitHub no longer has; showing \
             refs/pull/{}/head, {}",
            link.number,
            fetched.head
        ),
    }
    let base = repo
        .merge_base(&fetched.head, &fetched.base_tip)?
        .with_context(|| {
            format!(
                "{link} shares no history with its base branch {}",
                pull_request.base.branch
            )
        })?;
    let files = repo.changed_files(&base, &fetched.head)?;
    let checkout = repo.checkout(link.number, &fetched.head)?;
    Ok(LocalPullRequest {
        repo,
        checkout,
        base,
        head: fetched.head,
        files,
    })
}

/// What one file shows.
struct Contents {
    /// The left side's text. `None` when the file is new.
    old_text: Option<String>,
    right: Right,
    /// What the file's header says about it besides its path and status.
    badge: Option<SharedString>,
}

/// What the right side of one file shows.
enum Right {
    /// The checkout's file, as a project buffer, so language servers attach to it.
    Project,
    /// Text of rozed's own, for a file with no text at the head (`deleted`) or one rozed shows
    /// differently: a submodule, or a placeholder.
    Text {
        text: String,
        deleted: bool,
        /// A placeholder saying why the file isn't shown, the same on both sides.
        placeholder: bool,
    },
}

impl Contents {
    /// A placeholder saying why the file isn't shown.
    fn placeholder(reason: String) -> Self {
        Contents {
            old_text: None,
            right: Right::Text {
                text: reason,
                deleted: false,
                placeholder: true,
            },
            badge: None,
        }
    }
}

/// A file once it's in the diff.
struct Shown {
    buffer_id: BufferId,
    badge: Option<SharedString>,
    placeholder: bool,
}

/// Where the changed files go.
struct Target<'a> {
    local: &'a LocalPullRequest,
    project: &'a Entity<Project>,
    worktree_id: WorktreeId,
    languages: &'a Arc<LanguageRegistry>,
    editor: &'a Entity<SplittableEditor>,
}

impl Target<'_> {
    /// Reads what `file` shows from git.
    async fn contents(
        &self,
        file: &ChangedFile,
        cx: &mut AsyncWindowContext,
    ) -> anyhow::Result<Contents> {
        let badge = badge(file);
        if file.is_submodule() {
            let subproject =
                |version: &repo_cache::FileVersion| format!("Subproject commit {}\n", version.oid);
            return Ok(Contents {
                old_text: file.old.as_ref().map(subproject),
                right: Right::Text {
                    text: file.new.as_ref().map(subproject).unwrap_or_default(),
                    deleted: file.new.is_none(),
                    placeholder: false,
                },
                badge,
            });
        }
        if file.binary {
            return Ok(Contents {
                badge,
                ..Contents::placeholder("(binary file not shown)".into())
            });
        }
        if let Some(lines) = file.lines
            && lines.added + lines.deleted > LARGE_FILE_LINES
        {
            return Ok(Contents {
                badge,
                ..Contents::placeholder(format!(
                    "(large diff not loaded: {} lines added, {} deleted)",
                    lines.added, lines.deleted
                ))
            });
        }

        let repo = self.local.repo.clone();
        let old = file.old.as_ref().map(|old| old.oid.clone());
        let new = file.new.as_ref().map(|new| new.oid.clone());
        let (old_bytes, new_bytes) = cx
            .background_spawn(async move {
                let read = |oid: Option<Oid>| -> anyhow::Result<Option<Vec<u8>>> {
                    let Some(oid) = oid else { return Ok(None) };
                    let bytes = repo
                        .read_blob(&oid)?
                        .with_context(|| format!("git has no blob {oid}"))?;
                    Ok(Some(bytes))
                };
                anyhow::Ok((read(old)?, read(new)?))
            })
            .await?;
        // Both sides' line endings are normalized for the diff, so a change to them alone
        // would otherwise look like no change.
        let line_endings_only = match (&old_bytes, &new_bytes) {
            (Some(old), Some(new)) => old != new && normalized(old) == normalized(new),
            _ => false,
        };
        let badge = match (badge, line_endings_only) {
            (Some(badge), true) => Some(format!("{badge} · line endings only").into()),
            (None, true) => Some("line endings only".into()),
            (badge, false) => badge,
        };
        let old_text = old_bytes.map(|bytes| {
            let mut text = String::from_utf8_lossy(&bytes).into_owned();
            LineEnding::normalize(&mut text);
            text
        });
        let right = if file.new.is_some() {
            Right::Project
        } else {
            Right::Text {
                text: String::new(),
                deleted: true,
                placeholder: false,
            }
        };
        Ok(Contents {
            old_text,
            right,
            badge,
        })
    }

    /// Adds one changed file to the diff, after the ones before it.
    async fn add_file(
        &self,
        index: usize,
        file: &ChangedFile,
        contents: Contents,
        cx: &mut AsyncWindowContext,
    ) -> anyhow::Result<Shown> {
        let path = RelPath::from_unix_str(file.path())?.into_arc();
        let Contents {
            old_text,
            right,
            badge,
        } = contents;
        let (buffer, placeholder) = match right {
            Right::Project => {
                let buffer = self
                    .project
                    .update(cx, |project, cx| {
                        project.open_buffer(
                            ProjectPath {
                                worktree_id: self.worktree_id,
                                path: path.clone(),
                            },
                            cx,
                        )
                    })
                    .await?;
                (buffer, false)
            }
            Right::Text {
                text,
                deleted,
                placeholder,
            } => (
                self.placeholder(&text, &path, deleted, !placeholder, cx)
                    .await?,
                placeholder,
            ),
        };

        let snapshot = buffer.read_with(cx, |buffer, _| buffer.snapshot());
        let diff = if placeholder {
            // The same text on both sides, saying why.
            cx.new(|cx| {
                BufferDiff::new_unchanged(
                    &snapshot,
                    snapshot.language().cloned(),
                    Some(self.languages.clone()),
                    cx,
                )
            })
        } else {
            let diff = cx.new(|cx| {
                BufferDiff::new(
                    &snapshot.text,
                    snapshot.language().cloned(),
                    Some(self.languages.clone()),
                    cx,
                )
            });
            diff.update(cx, |diff, cx| {
                diff.set_base_text(
                    old_text.map(|text| Arc::from(text.as_str())),
                    snapshot.text.clone(),
                    cx,
                )
            })
            .await;
            diff
        };
        cx.update(|_, cx| follow_language(&buffer, &diff, cx))?;

        let ranges = cx.update(|_, cx| {
            let diff_snapshot = diff.read(cx).snapshot(cx);
            let ranges: Vec<_> = diff_snapshot
                .hunks(&snapshot)
                .map(|hunk| hunk.buffer_range.to_point(&snapshot))
                .collect();
            if ranges.is_empty() {
                vec![Point::zero()..snapshot.max_point()]
            } else {
                ranges
            }
        })?;
        // The sort prefix keeps files in the order git listed them.
        let key = PathKey::with_sort_prefix(index as u64, path);
        let buffer_id = buffer.read_with(cx, |buffer, _| buffer.remote_id());
        self.editor.update_in(cx, |editor, window, cx| {
            editor.update_excerpts_for_path(
                key,
                buffer,
                ranges,
                multibuffer_context_lines(cx),
                diff,
                cx,
            );
            if editor.diff_view_style() == DiffViewStyle::Split {
                editor.split(window, cx);
            }
        })?;
        Ok(Shown {
            buffer_id,
            badge,
            placeholder,
        })
    }

    /// An in-memory buffer holding `text`, for a file whose right side isn't the checkout's: a
    /// deleted file, a submodule or a placeholder. With `detect_language`, highlighted as the
    /// path's language.
    async fn placeholder(
        &self,
        text: &str,
        path: &Arc<RelPath>,
        deleted: bool,
        detect_language: bool,
        cx: &mut AsyncWindowContext,
    ) -> anyhow::Result<Entity<Buffer>> {
        let file: Arc<dyn File> = Arc::new(HistoricFile {
            path: path.clone(),
            full_path: self.local.checkout.join(path.as_std_path()),
            worktree_id: self.worktree_id,
            deleted,
        });
        let text = Rope::from(text);
        let language = if detect_language {
            cx.update(|_, cx| self.languages.language_for_file(&file, Some(&text), cx))?
        } else {
            None
        };
        let language = match language {
            Some(language) => self
                .languages
                .load_language(language)
                .await
                .ok()
                .and_then(Result::ok),
            None => None,
        };
        let languages = self.languages.clone();
        let buffer = cx.new(|cx| {
            let text = TextBuffer::new_normalized(
                ReplicaId::LOCAL,
                cx.entity_id().as_non_zero_u64().into(),
                LineEnding::default(),
                text,
            );
            let mut buffer = Buffer::build(text, Some(file), Capability::ReadOnly, cx);
            buffer.set_language_registry(languages);
            buffer.set_language_async(language, cx);
            buffer
        });
        Ok(buffer)
    }
}

/// Copies the buffer's language onto the diff's base text, which the left side shows. A project
/// buffer's language can load after the diff is built; without this the left side would stay
/// plain text (Zed's `file_diff_view.rs` does the same).
fn follow_language(buffer: &Entity<Buffer>, diff: &Entity<BufferDiff>, cx: &mut App) {
    let base_text = diff.read(cx).base_text_buffer().downgrade();
    let copy = move |buffer: &Entity<Buffer>, cx: &mut App| {
        let language = buffer.read(cx).language().cloned();
        base_text
            .update(cx, |base_text, cx| {
                base_text.set_language_async(language, cx)
            })
            .ok();
    };
    if buffer.read(cx).language().is_some() {
        copy(buffer, cx);
    }
    cx.subscribe(buffer, move |buffer, event, cx| {
        if let language::BufferEvent::LanguageChanged(_) = event {
            copy(&buffer, cx);
        }
    })
    .detach();
}

/// The status a changed file shows in its header.
fn status(file: &ChangedFile) -> FileStatus {
    let code = match file.change {
        Change::Added => StatusCode::Added,
        Change::Deleted => StatusCode::Deleted,
        Change::Renamed { .. } => StatusCode::Renamed,
        Change::TypeChanged => StatusCode::TypeChanged,
        Change::Modified => StatusCode::Modified,
    };
    FileStatus::Tracked(TrackedStatus {
        index_status: code,
        worktree_status: StatusCode::Unmodified,
    })
}

/// What a changed file's header says besides its path and status, from what git listed: a
/// rename's old path, a change of type or mode, a submodule. Line endings are added once the
/// contents are read.
fn badge(file: &ChangedFile) -> Option<SharedString> {
    let mut notes = Vec::new();
    if let Change::Renamed { .. } = file.change
        && let Some(old_path) = file.old_path()
    {
        notes.push(format!("renamed from {old_path}"));
    }
    if file.is_submodule() {
        notes.push("submodule".to_string());
    }
    if let (Some(old), Some(new)) = (&file.old, &file.new)
        && old.mode != new.mode
    {
        if file.change == Change::TypeChanged {
            notes.push(format!("{} → {}", kind(old.mode), kind(new.mode)));
        } else {
            notes.push(format!("mode {} → {}", mode(old.mode), mode(new.mode)));
        }
    }
    if file.is_mode_change_only() {
        notes.push("mode only".to_string());
    }
    (!notes.is_empty()).then(|| notes.join(" · ").into())
}

/// git's number for a mode, as GitHub shows it.
fn mode(mode: FileMode) -> &'static str {
    match mode {
        FileMode::Regular => "100644",
        FileMode::Executable => "100755",
        FileMode::Symlink => "120000",
        FileMode::Submodule => "160000",
    }
}

fn kind(mode: FileMode) -> &'static str {
    match mode {
        FileMode::Regular | FileMode::Executable => "file",
        FileMode::Symlink => "symlink",
        FileMode::Submodule => "submodule",
    }
}

/// `bytes` with every line ending as `\n`.
fn normalized(bytes: &[u8]) -> Vec<u8> {
    let mut normalized = Vec::with_capacity(bytes.len());
    let mut bytes = bytes.iter().copied().peekable();
    while let Some(byte) = bytes.next() {
        if byte == b'\r' {
            bytes.next_if_eq(&b'\n');
            normalized.push(b'\n');
        } else {
            normalized.push(byte);
        }
    }
    normalized
}

/// Each changed file's header: the pull request's change (added, deleted, renamed or modified)
/// rather than its status in the checkout, which is always clean, and a badge with what the
/// status doesn't say.
struct FileAddon {
    statuses: HashMap<BufferId, FileStatus>,
    badges: HashMap<BufferId, SharedString>,
}

impl Addon for FileAddon {
    fn to_any(&self) -> &dyn std::any::Any {
        self
    }

    fn override_status_for_buffer_id(&self, buffer_id: BufferId, _: &App) -> Option<FileStatus> {
        self.statuses.get(&buffer_id).copied()
    }

    fn render_buffer_header_controls(
        &self,
        _: &ExcerptBoundaryInfo,
        buffer: &language::BufferSnapshot,
        _: &Window,
        cx: &App,
    ) -> Option<AnyElement> {
        let badge = self.badges.get(&buffer.remote_id())?.clone();
        let colors = cx.theme().colors();
        Some(
            div()
                .flex_none()
                .px_1()
                .rounded_sm()
                .text_xs()
                .bg(colors.element_background)
                .text_color(colors.text_muted)
                .child(badge)
                .into_any_element(),
        )
    }
}

/// The file behind a placeholder buffer: a path in the pull request with no text at the head.
struct HistoricFile {
    path: Arc<RelPath>,
    full_path: PathBuf,
    worktree_id: WorktreeId,
    deleted: bool,
}

impl File for HistoricFile {
    fn as_local(&self) -> Option<&dyn language::LocalFile> {
        None
    }

    fn disk_state(&self) -> DiskState {
        DiskState::Historic {
            was_deleted: self.deleted,
        }
    }

    fn path(&self) -> &Arc<RelPath> {
        &self.path
    }

    fn full_path(&self, _: &App) -> PathBuf {
        self.full_path.clone()
    }

    fn path_style(&self, _: &App) -> PathStyle {
        PathStyle::local()
    }

    fn file_name<'a>(&'a self, _: &'a App) -> &'a str {
        self.path.file_name().unwrap_or_default()
    }

    fn worktree_id(&self, _: &App) -> WorktreeId {
        self.worktree_id
    }

    fn to_proto(&self, _: &App) -> language::proto::File {
        language::proto::File {
            worktree_id: self.worktree_id.to_proto(),
            entry_id: None,
            path: self.path.as_unix_str().to_string(),
            mtime: None,
            is_deleted: self.deleted,
            is_historic: true,
        }
    }

    fn is_private(&self) -> bool {
        false
    }

    fn can_open(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use repo_cache::{FileVersion, LineCounts};

    fn version(path: &str, oid: char, mode: FileMode) -> Option<FileVersion> {
        Some(FileVersion {
            path: path.into(),
            oid: oid.to_string().repeat(40).parse().unwrap(),
            mode,
        })
    }

    fn file(change: Change, old: Option<FileVersion>, new: Option<FileVersion>) -> ChangedFile {
        ChangedFile {
            change,
            old,
            new,
            binary: false,
            lines: Some(LineCounts {
                added: 1,
                deleted: 1,
            }),
        }
    }

    fn code(status: FileStatus) -> StatusCode {
        match status {
            FileStatus::Tracked(TrackedStatus { index_status, .. }) => index_status,
            other => panic!("{other:?} isn't a tracked status"),
        }
    }

    const REGULAR: FileMode = FileMode::Regular;

    #[test]
    fn headers_show_the_pull_requests_change() {
        let a = || version("a", '1', REGULAR);
        let b = || version("b", '2', REGULAR);
        let cases = [
            (file(Change::Added, None, a()), StatusCode::Added),
            (file(Change::Deleted, a(), None), StatusCode::Deleted),
            (
                file(Change::Renamed { similarity: 90 }, a(), b()),
                StatusCode::Renamed,
            ),
            (file(Change::Modified, a(), a()), StatusCode::Modified),
            (file(Change::TypeChanged, a(), a()), StatusCode::TypeChanged),
        ];
        for (file, expected) in cases {
            assert_eq!(code(status(&file)), expected, "{file:?}");
        }
    }

    #[test]
    fn badges_say_what_the_status_doesnt() {
        let badge_of = |file: ChangedFile| badge(&file).map(|badge| badge.to_string());
        assert_eq!(
            badge_of(file(
                Change::Renamed { similarity: 90 },
                version("old/a.rs", '1', REGULAR),
                version("new/a.rs", '2', REGULAR),
            )),
            Some("renamed from old/a.rs".into())
        );
        assert_eq!(
            badge_of(file(
                Change::Modified,
                version("run.sh", '1', REGULAR),
                version("run.sh", '1', FileMode::Executable),
            )),
            Some("mode 100644 → 100755 · mode only".into())
        );
        assert_eq!(
            badge_of(file(
                Change::Modified,
                version("run.sh", '1', REGULAR),
                version("run.sh", '2', FileMode::Executable),
            )),
            Some("mode 100644 → 100755".into())
        );
        assert_eq!(
            badge_of(file(
                Change::TypeChanged,
                version("link", '1', FileMode::Symlink),
                version("link", '2', REGULAR),
            )),
            Some("symlink → file".into())
        );
        assert_eq!(
            badge_of(file(
                Change::Modified,
                version("vendor/lib", '1', FileMode::Submodule),
                version("vendor/lib", '2', FileMode::Submodule),
            )),
            Some("submodule".into())
        );
        assert_eq!(
            badge_of(file(
                Change::Modified,
                version("a", '1', REGULAR),
                version("a", '2', REGULAR)
            )),
            None
        );
    }

    #[test]
    fn line_endings_normalize_to_newlines() {
        assert_eq!(normalized(b"a\r\nb\rc\n"), b"a\nb\nc\n");
        assert_eq!(normalized(b"a\nb"), b"a\nb");
        assert_eq!(normalized(b"a\r\r\n"), b"a\n\n");
    }
}
