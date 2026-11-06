//! A pull request in Zed's side-by-side diff (`SplittableEditor`), every changed file in one
//! scrolling view.
//!
//! The right side shows project buffers opened from a checkout of the head, so language servers
//! work there. The left side shows the base's text, read from git. Modelled on Zed's
//! `git_ui_core/src/file_diff_view.rs` (project buffer on the right) and
//! `git_ui/src/commit_view.rs` (many files in one view), without depending on either crate.

use std::collections::{HashMap, HashSet};
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
use gpui::{App, AppContext as _, AsyncWindowContext, Entity, WeakEntity};
use language::{
    Buffer, BufferId, Capability, DiskState, File, LanguageRegistry, LineEnding,
    OffsetRangeExt as _, Point, ReplicaId, Rope, TextBuffer,
};
use multi_buffer::PathKey;
use project::{Project, ProjectPath, WorktreeId};
use settings::{DiffViewStyle, Settings as _};
use util::paths::PathStyle;
use util::rel_path::RelPath;
use workspace::Workspace;

use crate::github_api;
use crate::local_copy::{ChangedFile, LocalPullRequest, PullRequestSource, RepoCache};
use crate::pr_link::PrLink;

/// Fetches the pull request, copies it locally and shows it in a new tab of `workspace`.
/// `launched` is when rozed started, for timing.
pub async fn open(
    workspace: WeakEntity<Workspace>,
    link: PrLink,
    launched: Instant,
    cx: &mut AsyncWindowContext,
) -> anyhow::Result<()> {
    let http = cx.update(|_, cx| cx.http_client())?;
    let pull_request = github_api::fetch_pull_request(http, &link).await?;
    log::info!(
        "{link}: {} merges into {} ({:?} after launch)",
        pull_request.head.sha,
        pull_request.base.branch,
        launched.elapsed()
    );

    let started = Instant::now();
    let local = cx
        .background_spawn({
            let link = link.clone();
            let pull_request = pull_request.clone();
            async move {
                RepoCache::new(paths::temp_dir()).prepare(&PullRequestSource {
                    owner: &link.owner,
                    repo: &link.repo,
                    number: link.number,
                    remote_url: &pull_request.base.repo.clone_url,
                    base_branch: &pull_request.base.branch,
                })
            }
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
    if local.head != pull_request.head.sha {
        // GitHub updates refs/pull/<n>/head moments after a push, so the two can briefly differ.
        log::warn!(
            "{link}: GitHub's API says the head is {}, but refs/pull/{}/head is {}",
            pull_request.head.sha,
            link.number,
            local.head
        );
    }

    let project = workspace.update(cx, |workspace, _| workspace.project().clone())?;
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
        let mut multi_buffer = MultiBuffer::new(Capability::ReadOnly).with_title(title);
        multi_buffer.set_all_diff_hunks_expanded(cx);
        multi_buffer
    });
    let editor = workspace.update_in(cx, |workspace, window, cx| {
        let workspace_entity = cx.entity();
        let editor = cx.new(|cx| {
            let editor = SplittableEditor::new(
                EditorSettings::get_global(cx).diff_view_style,
                multi_buffer,
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
    let mut binary_buffers = HashSet::new();
    for (index, file) in local.files.iter().enumerate() {
        match target.add_file(index, file, cx).await {
            Ok(buffer_id) => {
                statuses.insert(buffer_id, status(file));
                if file.binary {
                    binary_buffers.insert(buffer_id);
                }
            }
            Err(error) => log::warn!("{link}: can't show {}: {error:#}", file.path()),
        }
        if index == 0 {
            log::info!(
                "{link}: first file shown {:?} after launch",
                launched.elapsed()
            );
        }
    }
    log::info!(
        "{link}: all {} files shown {:?} after launch",
        local.files.len(),
        launched.elapsed()
    );

    editor.update(cx, |editor, cx| {
        editor.rhs_editor().update(cx, |editor, cx| {
            editor.register_addon(StatusAddon { statuses });
            if !binary_buffers.is_empty() {
                editor.fold_buffers(binary_buffers, cx);
            }
        });
    });
    Ok(())
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
    /// Adds one changed file to the diff, after the ones before it.
    async fn add_file(
        &self,
        index: usize,
        file: &ChangedFile,
        cx: &mut AsyncWindowContext,
    ) -> anyhow::Result<BufferId> {
        let path = RelPath::from_unix_str(file.path())?.into_arc();
        let old_text = match (&file.old_path, file.binary) {
            (Some(old_path), false) => {
                let repo = self.local.repo.clone();
                let base = self.local.base.clone();
                let old_path = old_path.clone();
                let bytes = cx
                    .background_spawn(async move { repo.read_file(&base, &old_path) })
                    .await?;
                let mut text = String::from_utf8_lossy(&bytes).into_owned();
                LineEnding::normalize(&mut text);
                Some(text)
            }
            _ => None,
        };

        let buffer = if file.binary {
            self.placeholder("(binary file not shown)", &path, false, cx)
                .await?
        } else if file.new_path.is_some() {
            // A real project buffer, so language servers attach to it.
            self.project
                .update(cx, |project, cx| {
                    project.open_buffer(
                        ProjectPath {
                            worktree_id: self.worktree_id,
                            path: path.clone(),
                        },
                        cx,
                    )
                })
                .await?
        } else {
            self.placeholder("", &path, true, cx).await?
        };

        let snapshot = buffer.read_with(cx, |buffer, _| buffer.snapshot());
        let diff = if file.binary {
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
        Ok(buffer_id)
    }

    /// An in-memory buffer for a file that has no text at the head: deleted or binary.
    async fn placeholder(
        &self,
        text: &str,
        path: &Arc<RelPath>,
        deleted: bool,
        cx: &mut AsyncWindowContext,
    ) -> anyhow::Result<Entity<Buffer>> {
        let file: Arc<dyn File> = Arc::new(HistoricFile {
            path: path.clone(),
            full_path: self.local.checkout.join(path.as_std_path()),
            worktree_id: self.worktree_id,
            deleted,
        });
        let text = Rope::from(text);
        let language =
            cx.update(|_, cx| self.languages.language_for_file(&file, Some(&text), cx))?;
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
    let code = match (&file.old_path, &file.new_path) {
        (None, _) => StatusCode::Added,
        (_, None) => StatusCode::Deleted,
        (Some(old), Some(new)) if old != new => StatusCode::Renamed,
        _ => StatusCode::Modified,
    };
    FileStatus::Tracked(TrackedStatus {
        index_status: code,
        worktree_status: StatusCode::Unmodified,
    })
}

/// Shows each file's change in the pull request (added, deleted, renamed or modified) in its
/// header, rather than its status in the checkout, which is always clean.
struct StatusAddon {
    statuses: HashMap<BufferId, FileStatus>,
}

impl Addon for StatusAddon {
    fn to_any(&self) -> &dyn std::any::Any {
        self
    }

    fn override_status_for_buffer_id(&self, buffer_id: BufferId, _: &App) -> Option<FileStatus> {
        self.statuses.get(&buffer_id).copied()
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

    fn file(old: Option<&str>, new: Option<&str>) -> ChangedFile {
        ChangedFile {
            old_path: old.map(Into::into),
            new_path: new.map(Into::into),
            binary: false,
        }
    }

    fn code(status: FileStatus) -> StatusCode {
        match status {
            FileStatus::Tracked(TrackedStatus { index_status, .. }) => index_status,
            other => panic!("{other:?} isn't a tracked status"),
        }
    }

    #[test]
    fn headers_show_the_pull_requests_change() {
        assert_eq!(code(status(&file(None, Some("a")))), StatusCode::Added);
        assert_eq!(code(status(&file(Some("a"), None))), StatusCode::Deleted);
        assert_eq!(
            code(status(&file(Some("a"), Some("b")))),
            StatusCode::Renamed
        );
        assert_eq!(
            code(status(&file(Some("a"), Some("a")))),
            StatusCode::Modified
        );
    }
}
