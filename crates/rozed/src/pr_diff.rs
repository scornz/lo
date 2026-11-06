//! A pull request in Zed's side-by-side diff (`SplittableEditor`), every changed file in one
//! scrolling view.
//!
//! Both sides' text comes from git: the left side is the merge base's, the right side the head's,
//! each in an in-memory buffer. With a trusted checkout, a file whose bytes there hash to the head's
//! blob shows the checkout's project buffer on the right instead, so language servers attach to
//! it. Modelled on Zed's `git_ui_core/src/file_diff_view.rs` (project buffer on the right) and
//! `git_ui/src/commit_view.rs` (many files in one view), without depending on either crate.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::Context as _;
use buffer_diff::BufferDiff;
use editor::{
    Addon, EditorSettings, HiddenDiffHunkRenderer, MultiBuffer, SplittableEditor,
    multibuffer_context_lines,
};
use git::status::{FileStatus, StatusCode, TrackedStatus};
use gpui::{AnyElement, App, AppContext as _, AsyncWindowContext, Entity, WeakEntity, Window};
use language::{
    Buffer, BufferId, Capability, DiskState, File, LanguageRegistry, LineEnding,
    OffsetRangeExt as _, Point, ReplicaId, Rope, TextBuffer,
};
use multi_buffer::{ExcerptBoundaryInfo, PathKey};
use project::{Project, ProjectPath, WorktreeId};
use settings::{DiffViewStyle, Settings as _};
use ui::{Icon, IconName, IconSize, Label, LabelSize, Tooltip, prelude::*};
use util::paths::PathStyle;
use util::rel_path::RelPath;
use workspace::Workspace;
use workspace::notifications::NotificationId;

use crate::github_api::{self, PullRequest};
use crate::local_copy::{self, ChangedFile, GitObject, PullRequestSource, Repo, RepoCache};
use crate::pr_link::PrLink;
use crate::trust;

/// Whether the pull request gets a checkout and language servers. The caller decides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trust {
    /// No checkout and no language servers: both sides come from git.
    Untrusted,
    /// Check the head out, trust that checkout and nothing else, and open it as the project's
    /// folder, so language servers start there. Language servers run the repository's code.
    Checkout,
}

/// Fetches the pull request, copies it locally and shows it in a new tab of `workspace`.
/// `launched` is when rozed started, for timing.
pub async fn open(
    workspace: WeakEntity<Workspace>,
    link: PrLink,
    trust: Trust,
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
    let cache = Arc::new(RepoCache::new(paths::temp_dir()));
    let local = Arc::new(
        cx.background_spawn({
            let (cache, link, pull_request) = (cache.clone(), link.clone(), pull_request.clone());
            async move { cache.prepare(&source(&link, &pull_request)) }
        })
        .await
        .with_context(|| format!("copying {link} locally"))?,
    );
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
    let checkout = match trust {
        Trust::Untrusted => {
            log::info!("{link}: not trusted, so no checkout and no language servers");
            None
        }
        Trust::Checkout => {
            let dir = cx
                .background_spawn({
                    let (cache, link, pull_request) =
                        (cache.clone(), link.clone(), pull_request.clone());
                    let local = local.clone();
                    async move { cache.check_out(&source(&link, &pull_request), &local) }
                })
                .await
                .with_context(|| format!("checking {link} out"))?;
            cx.update(|_, cx| trust::trust_checkout(&project, &dir, cx))??;
            let (worktree, _) = project
                .update(cx, |project, cx| {
                    project.find_or_create_worktree(&dir, true, cx)
                })
                .await
                .with_context(|| format!("opening {}", dir.display()))?;
            let worktree_id = worktree.read_with(cx, |worktree, _| worktree.id());
            log::info!(
                "{link}: trusted, so language servers start in {}",
                dir.display()
            );
            Some(Checkout { dir, worktree_id })
        }
    };
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
        repo_name: &link.repo,
        project: &project,
        checkout: checkout.as_ref(),
        languages: &languages,
        editor: &editor,
    };
    let mut headers = FileHeaders::default();
    let mut folded = HashSet::new();
    let mut not_shown = 0;
    for (index, file) in local.files.iter().enumerate() {
        let plan = cx
            .background_spawn({
                let repo = local.repo.clone();
                let file = file.clone();
                let checkout = checkout.as_ref().map(|checkout| checkout.dir.clone());
                async move { plan_file(&repo, &file, checkout.as_deref()) }
            })
            .await;
        let shown = match target.add_file(index, file, plan.clone(), cx).await {
            Ok(shown) => Ok(shown),
            Err(error) => {
                target
                    .add_file(index, file, FilePlan::failed(&error), cx)
                    .await
            }
        };
        match shown {
            Ok(shown) => {
                headers.statuses.insert(shown.buffer_id, status(file));
                if let Some(warning) = shown.warning {
                    log::warn!("{link}: {}: {}", file.path(), warning.detail);
                    not_shown += usize::from(warning.label == NOT_SHOWN);
                    headers.warnings.insert(shown.buffer_id, warning);
                }
                if shown.folded {
                    folded.insert(shown.buffer_id);
                }
            }
            // Only when even the placeholder that says why can't be shown.
            Err(error) => {
                not_shown += 1;
                log::error!("{link}: can't show {}: {error:#}", file.path());
            }
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
            editor.register_addon(headers);
            if !folded.is_empty() {
                editor.fold_buffers(folded, cx);
            }
        });
    });
    if not_shown > 0 {
        let message = match not_shown {
            1 => format!("{link}: 1 file couldn't be shown. Its header says why."),
            n => format!("{link}: {n} files couldn't be shown. Their headers say why."),
        };
        workspace.update(cx, |workspace, cx| {
            workspace.show_toast(
                workspace::Toast::new(NotificationId::unique::<FileHeaders>(), message),
                cx,
            )
        })?;
    }
    Ok(())
}

/// Which pull request the local copy fetches, and from where.
fn source<'a>(link: &'a PrLink, pull_request: &'a PullRequest) -> PullRequestSource<'a> {
    PullRequestSource {
        owner: &link.owner,
        repo: &link.repo,
        number: link.number,
        remote_url: &pull_request.base.repo.clone_url,
        base_branch: &pull_request.base.branch,
    }
}

/// The trusted checkout a pull request's project buffers come from.
struct Checkout {
    dir: PathBuf,
    worktree_id: WorktreeId,
}

/// What one changed file shows, worked out from git before anything goes on screen.
#[derive(Clone, Debug, PartialEq)]
struct FilePlan {
    content: Content,
    /// Shown in the file's header.
    warning: Option<Warning>,
}

#[derive(Clone, Debug, PartialEq)]
enum Content {
    /// The two sides, compared.
    Diff {
        /// The base's text. `None` when the pull request adds the file.
        old_text: Option<String>,
        new: NewSide,
    },
    /// Text that stands in for a file rozed doesn't compare: a binary file, a submodule, or one
    /// it couldn't read.
    Placeholder {
        text: String,
        /// Whether the pull request deletes the file.
        deleted: bool,
        /// Whether the file starts folded, showing only its header.
        folded: bool,
    },
}

/// Where the right side's text comes from.
#[derive(Clone, Debug, PartialEq)]
enum NewSide {
    /// The head's text, read from git.
    Git(String),
    /// The checkout's project buffer, whose bytes hash to the head's blob.
    Checkout,
    /// Nothing: the pull request deletes the file.
    Deleted,
}

/// A note in a file's header: a short label, and the reason in its tooltip.
#[derive(Clone, Debug, PartialEq)]
struct Warning {
    label: SharedString,
    detail: SharedString,
}

/// The header label of a file rozed couldn't show.
const NOT_SHOWN: &str = "Not shown";

impl FilePlan {
    fn diff(old_text: Option<String>, new: NewSide) -> Self {
        Self {
            content: Content::Diff { old_text, new },
            warning: None,
        }
    }

    fn placeholder(text: impl Into<String>, deleted: bool, folded: bool) -> Self {
        Self {
            content: Content::Placeholder {
                text: text.into(),
                deleted,
                folded,
            },
            warning: None,
        }
    }

    /// A placeholder that says why the file can't be shown, in its text and its header.
    fn failed(error: &anyhow::Error) -> Self {
        Self {
            warning: Some(Warning {
                label: NOT_SHOWN.into(),
                detail: format!("rozed couldn't show this file: {error:#}").into(),
            }),
            ..Self::placeholder(
                format!("(rozed couldn't show this file: {error:#})"),
                false,
                false,
            )
        }
    }
}

/// Works out what `file` shows. Reads git, and with a `checkout`, hashes the file there. Never
/// fails: a file it can't read gets a placeholder that says why. Blocks while git runs.
fn plan_file(repo: &Repo, file: &ChangedFile, checkout: Option<&Path>) -> FilePlan {
    try_plan_file(repo, file, checkout).unwrap_or_else(|error| FilePlan::failed(&error))
}

fn try_plan_file(
    repo: &Repo,
    file: &ChangedFile,
    checkout: Option<&Path>,
) -> anyhow::Result<FilePlan> {
    let deleted = file.new_path.is_none();
    // Checked before reading anything: a submodule's commit is in another repository, and
    // reading it as a blob would ask the remote for it.
    let sides = [&file.old_object, &file.new_object];
    if sides
        .iter()
        .any(|side| side.as_ref().is_some_and(GitObject::is_submodule))
    {
        let [old, new] = sides.map(|side| match side {
            Some(object) if object.is_submodule() => object.id.as_str(),
            Some(_) => "a file",
            None => "nothing",
        });
        return Ok(FilePlan::placeholder(
            format!("Submodule: {old} → {new}\n\n(rozed doesn't show submodule changes yet.)"),
            deleted,
            false,
        ));
    }
    if file.binary {
        return Ok(FilePlan::placeholder(
            "(binary file not shown)",
            deleted,
            true,
        ));
    }

    let old_text = file
        .old_object
        .as_ref()
        .map(|object| read_text(repo, object))
        .transpose()
        .context("reading the base's version")?;
    let (Some(path), Some(new_object)) = (&file.new_path, &file.new_object) else {
        return Ok(FilePlan::diff(old_text, NewSide::Deleted));
    };
    let from_git = |warning: Option<Warning>| -> anyhow::Result<FilePlan> {
        let text = read_text(repo, new_object).context("reading the head's version")?;
        Ok(FilePlan {
            warning,
            ..FilePlan::diff(old_text.clone(), NewSide::Git(text))
        })
    };
    let Some(checkout) = checkout else {
        return from_git(None);
    };
    match local_copy::hash_checkout_file(checkout, path) {
        Ok(Some(id)) if id == new_object.id => Ok(FilePlan::diff(old_text, NewSide::Checkout)),
        Ok(found) => from_git(Some(Warning {
            label: "Checkout differs: shown from git".into(),
            detail: format!(
                "The checkout's copy of this file {}, so this shows the head's text from git, \
                 without language servers.",
                match found {
                    Some(id) => format!("hashes to {id}, not the head's blob {}", new_object.id),
                    None => "is missing or isn't a regular file".to_string(),
                }
            )
            .into(),
        })),
        Err(error) => from_git(Some(Warning {
            label: "Checkout unchecked: shown from git".into(),
            detail: format!(
                "rozed couldn't hash the checkout's copy of this file ({error:#}), so this shows \
                 the head's text from git, without language servers."
            )
            .into(),
        })),
    }
}

/// A blob's text, with line endings normalized as Zed's buffers have them.
fn read_text(repo: &Repo, object: &GitObject) -> anyhow::Result<String> {
    let bytes = repo.read_blob(&object.id)?;
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    LineEnding::normalize(&mut text);
    Ok(text)
}

/// Where the changed files go.
struct Target<'a> {
    /// The repository's name, which starts each file's full path.
    repo_name: &'a str,
    project: &'a Entity<Project>,
    checkout: Option<&'a Checkout>,
    languages: &'a Arc<LanguageRegistry>,
    editor: &'a Entity<SplittableEditor>,
}

/// A file added to the diff.
struct Shown {
    buffer_id: BufferId,
    warning: Option<Warning>,
    folded: bool,
}

impl Target<'_> {
    /// Adds one changed file to the diff, after the ones before it.
    async fn add_file(
        &self,
        index: usize,
        file: &ChangedFile,
        plan: FilePlan,
        cx: &mut AsyncWindowContext,
    ) -> anyhow::Result<Shown> {
        let path = RelPath::from_unix_str(file.path())?.into_arc();
        let deleted = file.new_path.is_none();
        let (buffer, old_text, folded) = match plan.content {
            Content::Diff {
                old_text,
                new: NewSide::Checkout,
            } => {
                let checkout = self
                    .checkout
                    .context("a file from the checkout, without one")?;
                // A project buffer, so language servers attach to it.
                let buffer = self
                    .project
                    .update(cx, |project, cx| {
                        project.open_buffer(
                            ProjectPath {
                                worktree_id: checkout.worktree_id,
                                path: path.clone(),
                            },
                            cx,
                        )
                    })
                    .await?;
                (buffer, Some(old_text), false)
            }
            Content::Diff {
                old_text,
                new: NewSide::Git(text),
            } => (
                self.memory_buffer(&text, &path, false, cx).await?,
                Some(old_text),
                false,
            ),
            Content::Diff {
                old_text,
                new: NewSide::Deleted,
            } => (
                self.memory_buffer("", &path, true, cx).await?,
                Some(old_text),
                false,
            ),
            Content::Placeholder { text, folded, .. } => (
                self.memory_buffer(&text, &path, deleted, cx).await?,
                None,
                folded,
            ),
        };

        let snapshot = buffer.read_with(cx, |buffer, _| buffer.snapshot());
        let diff = match old_text {
            // A placeholder: nothing to compare, so no hunks.
            None => cx.new(|cx| {
                BufferDiff::new_unchanged(
                    &snapshot,
                    snapshot.language().cloned(),
                    Some(self.languages.clone()),
                    cx,
                )
            }),
            Some(old_text) => {
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
            }
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
            warning: plan.warning,
            folded,
        })
    }

    /// An in-memory buffer holding `text`: the head's text from git, or a placeholder. Language
    /// servers never see it.
    async fn memory_buffer(
        &self,
        text: &str,
        path: &Arc<RelPath>,
        deleted: bool,
        cx: &mut AsyncWindowContext,
    ) -> anyhow::Result<Entity<Buffer>> {
        let file: Arc<dyn File> = Arc::new(GitFile {
            path: path.clone(),
            full_path: Path::new(self.repo_name).join(path.as_std_path()),
            worktree_id: self
                .checkout
                .map_or_else(GitFile::no_worktree, |checkout| checkout.worktree_id),
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

/// Copies the buffer's language onto the diff's base text, which the left side shows. A buffer's
/// language can load after the diff is built; without this the left side would stay plain text
/// (Zed's `file_diff_view.rs` does the same).
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

/// What each file's header shows beyond its path: the pull request's change (added, deleted,
/// renamed or modified) rather than its status in the checkout, which is always clean, and any
/// warning.
#[derive(Default)]
struct FileHeaders {
    statuses: HashMap<BufferId, FileStatus>,
    warnings: HashMap<BufferId, Warning>,
}

impl Addon for FileHeaders {
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
        _: &App,
    ) -> Option<AnyElement> {
        let warning = self.warnings.get(&buffer.remote_id())?;
        Some(
            h_flex()
                .id(("file-warning", buffer.remote_id().to_proto()))
                .flex_shrink_0()
                .gap_1()
                .child(
                    Icon::new(IconName::Warning)
                        .size(IconSize::Small)
                        .color(Color::Warning),
                )
                .child(
                    Label::new(warning.label.clone())
                        .size(LabelSize::Small)
                        .color(Color::Warning),
                )
                .tooltip(Tooltip::text(warning.detail.clone()))
                .into_any_element(),
        )
    }
}

/// The file behind an in-memory buffer: a path in the pull request whose text came from git.
struct GitFile {
    path: Arc<RelPath>,
    full_path: PathBuf,
    worktree_id: WorktreeId,
    deleted: bool,
}

impl GitFile {
    /// The worktree an in-memory buffer claims when there's no checkout. No worktree has it.
    fn no_worktree() -> WorktreeId {
        WorktreeId::from_proto(u64::MAX)
    }
}

impl File for GitFile {
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

    use crate::local_copy::LocalPullRequest;
    use crate::local_copy::fixtures::{self, Entry};

    fn file(old: Option<&str>, new: Option<&str>) -> ChangedFile {
        ChangedFile {
            old_path: old.map(Into::into),
            new_path: new.map(Into::into),
            old_object: None,
            new_object: None,
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

    /// A pull request copied locally from a fixture, with a checkout when asked for.
    struct Copied {
        _fixture: fixtures::Fixture,
        _cache: tempfile::TempDir,
        local: LocalPullRequest,
        checkout: Option<PathBuf>,
    }

    fn copy(base: &[Entry], head: &[Entry], checkout: bool) -> Copied {
        let fixture = fixtures::pull_request(base, head);
        let cache_dir = tempfile::TempDir::new().unwrap();
        let cache = RepoCache::new(cache_dir.path());
        let local = cache.prepare(&fixture.source()).unwrap();
        let checkout = checkout.then(|| cache.check_out(&fixture.source(), &local).unwrap());
        Copied {
            _fixture: fixture,
            _cache: cache_dir,
            local,
            checkout,
        }
    }

    impl Copied {
        fn plans(&self) -> Vec<(&str, FilePlan)> {
            self.local
                .files
                .iter()
                .map(|file| {
                    let plan = plan_file(&self.local.repo, file, self.checkout.as_deref());
                    (file.path(), plan)
                })
                .collect()
        }
    }

    fn from_git(text: &str) -> NewSide {
        NewSide::Git(text.to_string())
    }

    #[test]
    fn without_a_checkout_both_sides_come_from_git() {
        let copied = copy(
            &[
                Entry::File("changed.rs", b"fn old() {}\n"),
                Entry::File("gone.rs", b"fn gone() {}\n"),
            ],
            &[
                Entry::File("added.rs", b"fn added() {}\n"),
                Entry::File("changed.rs", b"fn new() {}\r\n"),
            ],
            false,
        );
        assert_eq!(
            copied.plans(),
            vec![
                (
                    "added.rs",
                    FilePlan::diff(None, from_git("fn added() {}\n"))
                ),
                (
                    "changed.rs",
                    FilePlan::diff(Some("fn old() {}\n".into()), from_git("fn new() {}\n"))
                ),
                (
                    "gone.rs",
                    FilePlan::diff(Some("fn gone() {}\n".into()), NewSide::Deleted)
                ),
            ]
        );
    }

    #[test]
    fn with_a_checkout_files_that_hash_to_their_blob_use_it() {
        let copied = copy(
            &[Entry::File("changed.rs", b"fn old() {}\n")],
            &[
                Entry::File("added.rs", b"fn added() {}\n"),
                Entry::File("changed.rs", b"fn new() {}\n"),
            ],
            true,
        );
        assert_eq!(
            copied.plans(),
            vec![
                ("added.rs", FilePlan::diff(None, NewSide::Checkout)),
                (
                    "changed.rs",
                    FilePlan::diff(Some("fn old() {}\n".into()), NewSide::Checkout)
                ),
            ]
        );
    }

    #[test]
    fn a_checkout_file_that_differs_from_its_blob_shows_gits_text_with_a_warning() {
        let copied = copy(&[], &[Entry::File("a.rs", b"fn committed() {}\n")], true);
        std::fs::write(
            copied.checkout.as_ref().unwrap().join("a.rs"),
            "fn changed_on_disk() {}\n",
        )
        .unwrap();
        let [(_, plan)] = &copied.plans()[..] else {
            panic!("expected one file");
        };
        assert_eq!(
            plan.content,
            Content::Diff {
                old_text: None,
                new: from_git("fn committed() {}\n")
            }
        );
        let warning = plan.warning.as_ref().unwrap();
        assert_eq!(warning.label, "Checkout differs: shown from git");
        assert!(
            warning.detail.contains("not the head's blob"),
            "{warning:?}"
        );
    }

    #[test]
    fn a_case_collision_shows_each_files_own_text() {
        // On a case-insensitive disk, Lib.rs and lib.rs share one file in the checkout. Whichever
        // doesn't hash to its blob there shows git's text with a warning, so evil() still shows.
        let evil = "pub fn evil() {}\n";
        let benign = "pub fn lib() { /* benign */ }\n";
        let base = [Entry::File("lib.rs", b"pub fn lib() {}\n")];
        let head = [
            Entry::File("Lib.rs", evil.as_bytes()),
            Entry::File("lib.rs", benign.as_bytes()),
        ];

        let trusted = copy(&base, &head, true);
        let shown_from_git: Vec<_> = trusted
            .plans()
            .into_iter()
            .map(|(path, plan)| match plan.content {
                Content::Diff {
                    new: NewSide::Git(text),
                    ..
                } => {
                    assert!(plan.warning.is_some(), "{path}");
                    Some(text)
                }
                Content::Diff {
                    new: NewSide::Checkout,
                    ..
                } => {
                    assert_eq!(plan.warning, None, "{path}");
                    None
                }
                other => panic!("{path}: unexpected {other:?}"),
            })
            .collect();
        if fixtures::is_case_insensitive(trusted.checkout.as_deref().unwrap()) {
            // The checkout holds lib.rs's text under one name, so Lib.rs comes from git.
            assert_eq!(shown_from_git, vec![Some(evil.to_string()), None]);
        } else {
            assert_eq!(shown_from_git, vec![None, None]);
        }

        let untrusted = copy(&base, &head, false);
        assert_eq!(
            untrusted.plans(),
            vec![
                ("Lib.rs", FilePlan::diff(None, from_git(evil))),
                (
                    "lib.rs",
                    FilePlan::diff(Some("pub fn lib() {}\n".into()), from_git(benign))
                ),
            ]
        );
    }

    #[test]
    fn a_submodule_gets_a_placeholder_without_reading_its_commits() {
        let old = "1".repeat(40);
        let new = "2".repeat(40);
        for checkout in [false, true] {
            let copied = copy(
                &[Entry::Submodule("vendor/lib", &old)],
                &[Entry::Submodule("vendor/lib", &new)],
                checkout,
            );
            assert_eq!(
                copied.plans(),
                vec![(
                    "vendor/lib",
                    FilePlan::placeholder(
                        format!(
                            "Submodule: {old} → {new}\n\n\
                             (rozed doesn't show submodule changes yet.)"
                        ),
                        false,
                        false
                    )
                )]
            );
        }
    }

    #[test]
    fn a_file_git_cant_read_gets_a_placeholder_that_says_why() {
        let copied = copy(
            &[Entry::File("a.rs", b"fn a() {}\n")],
            &[Entry::File("a.rs", b"fn b() {}\n")],
            false,
        );
        let mut file = copied.local.files[0].clone();
        let missing = "f".repeat(40);
        file.new_object.as_mut().unwrap().id = missing.clone();

        let plan = plan_file(&copied.local.repo, &file, None);

        let Content::Placeholder { text, .. } = &plan.content else {
            panic!("{plan:?}");
        };
        assert!(
            text.starts_with("(rozed couldn't show this file: reading the head's version"),
            "{text}"
        );
        let warning = plan.warning.unwrap();
        assert_eq!(warning.label, NOT_SHOWN);
        assert!(warning.detail.contains(&missing), "{warning:?}");
    }

    #[test]
    fn binary_files_get_a_folded_placeholder() {
        let copied = copy(
            &[
                Entry::File("data.bin", b"\0\x01base"),
                Entry::File("gone.bin", b"\0\x01gone"),
            ],
            &[Entry::File("data.bin", b"\0\x01head")],
            false,
        );
        assert_eq!(
            copied.plans(),
            vec![
                (
                    "data.bin",
                    FilePlan::placeholder("(binary file not shown)", false, true)
                ),
                (
                    "gone.bin",
                    FilePlan::placeholder("(binary file not shown)", true, true)
                ),
            ]
        );
    }
}
