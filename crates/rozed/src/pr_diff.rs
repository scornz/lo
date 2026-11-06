//! A pull request in Zed's side-by-side diff (`SplittableEditor`), every changed file in one
//! scrolling view.
//!
//! Both sides' text comes from git: the left side is the merge base's, the right side the head's,
//! each in an in-memory buffer. With a trusted checkout, a file whose bytes there hash to the head's
//! blob shows the checkout's project buffer on the right instead, so language servers attach to
//! it. Modelled on Zed's `git_ui_core/src/file_diff_view.rs` (project buffer on the right) and
//! `git_ui/src/commit_view.rs` (many files in one view), without depending on either crate.
//!
//! The local copy is `repo_cache`'s. No changed file is left out: one rozed can't show as text
//! (binary, more than 5,000 changed lines or 1 MiB, or failing to load) gets a placeholder saying
//! why, and the title counts the ones not shown. A submodule shows "Subproject commit a" against
//! "Subproject commit b", and a badge in the header marks renames, type and mode changes, names
//! that aren't UTF-8 (shown with U+FFFD), and changes to line endings alone.

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

use repo_cache::{
    Change, ChangedFile, FileMode, FileVersion, HeadSource, Oid, PullRequestRefs, Repo, RepoCache,
    Token,
};

use crate::github_api::{self, PullRequest};
use crate::local_copies;
use crate::pr_banner::{BannerKind, Banners, BannersAddon};
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

/// Fetches the pull request, copies it locally and shows it in a new tab of `workspace`, from git
/// first. When `trust` asks for a checkout, it's made in the background, and each file whose
/// checkout copy matches the head switches to the checkout's project buffer when it's ready, or a
/// banner says why there's none. `launched` is when rozed started, for timing.
pub async fn open(
    workspace: WeakEntity<Workspace>,
    link: PrLink,
    trust: Trust,
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

    // Waits for the launch check, which finds git.
    let cache = cx.update(|_, cx| local_copies::cache(cx))?.await?;
    let started = Instant::now();
    let local = Arc::new(
        cx.background_spawn({
            let (link, pull_request) = (link.clone(), pull_request.clone());
            async move { copy(&cache, &link, &pull_request, token) }
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

    let project = workspace.update(cx, |workspace, _| workspace.project().clone())?;
    let languages = project.read_with(cx, |project, _| project.languages().clone());
    let banners = cx.new(|_| Banners::default());
    banners.update(cx, |banners, cx| {
        if let Some(text) = head_banner(&link, &local.head, &local.head_source) {
            banners.show(BannerKind::HeadMismatch, Severity::Warning, text, cx);
        }
        if trust == Trust::Checkout {
            banners.show(
                BannerKind::CheckingOut,
                Severity::Info,
                format!(
                    "Checking {link} out for language servers. Until it's ready, every file \
                     shows from git."
                ),
                cx,
            );
        }
    });

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
                editor.register_addon(BannersAddon(banners.clone()));
                // Keeps the clone open, so no cleanup removes it or its checkout while the tab
                // shows it.
                editor.register_addon(OpenClone {
                    _repo: local.repo.clone(),
                });
            });
            editor
        });
        workspace.add_item_to_active_pane(Box::new(editor.clone()), None, true, window, cx);
        editor
    })?;

    let target = Target {
        repo_name: &local.name,
        project: &project,
        checkout: None,
        languages: &languages,
        editor: &editor,
    };
    let mut headers = FileHeaders::default();
    let mut folded = HashSet::new();
    // Each file's buffer and what it shows, for switching to the checkout.
    let mut shown_files = Vec::with_capacity(local.files.len());
    // Files whose header says they couldn't be shown, and files not loaded.
    let mut not_shown = 0;
    let mut not_loaded = 0;
    for (index, file) in local.files.iter().enumerate() {
        let plan = cx
            .background_spawn({
                let repo = local.repo.clone();
                let file = file.clone();
                async move { plan_file(&repo, &file, None) }
            })
            .await;
        let shown = match target.add_file(index, file, plan.clone(), cx).await {
            Ok(shown) => Ok((shown, plan)),
            Err(error) => {
                let plan = FilePlan::failed(&error);
                target
                    .add_file(index, file, plan.clone(), cx)
                    .await
                    .map(|shown| (shown, plan))
            }
        };
        match shown {
            Ok((shown, plan)) => {
                headers.statuses.insert(shown.buffer_id, status(file));
                if let Some(badge) = shown.badge {
                    headers.badges.insert(shown.buffer_id, badge);
                }
                if let Some(warning) = shown.warning {
                    log::warn!("{link}: {}: {}", file.display_path(), warning.detail);
                    not_shown += usize::from(warning.label == NOT_SHOWN);
                    not_loaded += usize::from(warning.label == NOT_LOADED);
                    headers.warnings.insert(shown.buffer_id, warning);
                }
                if shown.folded {
                    folded.insert(shown.buffer_id);
                }
                shown_files.push(Some((shown.buffer_id, plan)));
            }
            // Only when even the placeholder that says why can't be shown.
            Err(error) => {
                not_shown += 1;
                log::error!("{link}: can't show {}: {error:#}", file.display_path());
                shown_files.push(None);
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

    if not_shown + not_loaded > 0 {
        let count = not_shown + not_loaded;
        let files = if count == 1 { "file" } else { "files" };
        multi_buffer.update(cx, |multi_buffer, cx| {
            multi_buffer.set_title(format!("{title} ({count} {files} not shown)"), cx)
        });
    }
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

    if trust == Trust::Untrusted {
        log::info!("{link}: not trusted, so no checkout and no language servers");
        return Ok(());
    }
    let checkout = check_out(&local, &link, &project, cx).await;
    banners.update(cx, |banners, cx| banners.hide(BannerKind::CheckingOut, cx));
    let checkout = match checkout {
        Ok(checkout) => checkout,
        Err(error) => {
            log::warn!("{link}: {error:#}");
            banners.update(cx, |banners, cx| {
                banners.show(
                    BannerKind::CheckoutFailed,
                    Severity::Warning,
                    format!(
                        "rozed couldn't check {link} out, so language servers are off and every \
                         file shows from git: {error:#}"
                    ),
                    cx,
                )
            });
            return Ok(());
        }
    };
    let target = Target {
        checkout: Some(&checkout),
        ..target
    };
    for (index, (file, shown)) in local.files.iter().zip(&shown_files).enumerate() {
        let Some((buffer_id, first)) = shown else {
            continue;
        };
        let plan = cx
            .background_spawn({
                let repo = local.repo.clone();
                let file = file.clone();
                let dir = checkout.dir.clone();
                async move { plan_file(&repo, &file, Some(&dir)) }
            })
            .await;
        match swap(first, plan) {
            Swap::Keep => {}
            Swap::Warn(warning) => {
                log::warn!("{link}: {}: {}", file.display_path(), warning.detail);
                update_headers(&editor, cx, |headers| {
                    headers.warnings.insert(*buffer_id, warning);
                });
            }
            Swap::ToCheckout(plan) => match target.add_file(index, file, plan, cx).await {
                Ok(shown) => update_headers(&editor, cx, |headers| {
                    headers.move_to(*buffer_id, shown.buffer_id);
                    if let Some(badge) = shown.badge {
                        headers.badges.insert(shown.buffer_id, badge);
                    }
                }),
                Err(error) => log::warn!(
                    "{link}: can't show {} from the checkout, so it stays from git: {error:#}",
                    file.display_path()
                ),
            },
        }
    }
    log::info!(
        "{link}: checkout ready and swapped in {:?} after launch",
        launched.elapsed()
    );
    Ok(())
}

/// Checks the head out, trusts that checkout and nothing else, and opens it as a folder of
/// `project`, so language servers start there.
async fn check_out(
    local: &Arc<LocalPullRequest>,
    link: &PrLink,
    project: &Entity<Project>,
    cx: &mut AsyncWindowContext,
) -> anyhow::Result<Checkout> {
    let dir = cx
        .background_spawn({
            let (local, number) = (local.clone(), link.number);
            async move { local.repo.checkout(number, &local.head) }
        })
        .await
        .with_context(|| format!("checking {link} out"))?;
    cx.update(|_, cx| trust::trust_checkout(project, &dir, cx))??;
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
    Ok(Checkout { dir, worktree_id })
}

/// What a file shown from git does once the checkout is ready.
#[derive(Debug, PartialEq)]
enum Swap {
    /// Shows the checkout's project buffer, planned so.
    ToCheckout(FilePlan),
    /// Stays from git, and its header says why: the checkout's copy differs from the head's.
    Warn(Warning),
    /// Stays as it is.
    Keep,
}

/// What a file whose first plan, without a checkout, was `first` does with `with_checkout`, its
/// plan once the checkout is ready. Only a file shown from git can switch.
fn swap(first: &FilePlan, with_checkout: FilePlan) -> Swap {
    let from_git = matches!(
        first.content,
        Content::Diff {
            new: NewSide::Git(_),
            ..
        }
    );
    if !from_git {
        return Swap::Keep;
    }
    match with_checkout {
        FilePlan {
            content:
                Content::Diff {
                    new: NewSide::Checkout,
                    ..
                },
            ..
        } => Swap::ToCheckout(with_checkout),
        FilePlan {
            warning: Some(warning),
            ..
        } => Swap::Warn(warning),
        _ => Swap::Keep,
    }
}

/// Changes the file headers of `editor`'s pull request.
fn update_headers(
    editor: &Entity<SplittableEditor>,
    cx: &mut AsyncWindowContext,
    change: impl FnOnce(&mut FileHeaders),
) {
    editor.update(cx, |editor, cx| {
        editor.rhs_editor().update(cx, |editor, cx| {
            if let Some(headers) = editor.addon_mut::<FileHeaders>() {
                change(headers);
            }
            cx.notify();
        })
    });
}

/// What the banner says when the head shown isn't the one GitHub's API reports, because GitHub
/// no longer has that one. `None` when it is GitHub's head.
fn head_banner(link: &PrLink, head: &Oid, head_source: &HeadSource) -> Option<String> {
    match head_source {
        // GitHub moves refs/pull/<n>/head a moment after a push; the API's head is what shows.
        HeadSource::PullRef | HeadSource::Api { .. } => None,
        HeadSource::PullRefNotApi { api } => Some(format!(
            "GitHub says this pull request's head is {}, which GitHub no longer has. This shows \
             refs/pull/{}/head, {}, instead.",
            api.short(7),
            link.number,
            head.short(7)
        )),
    }
}

/// Keeps a pull request's clone open while its tab is, so no rozed's cleanup removes the clone or
/// its checkout from under it.
struct OpenClone {
    _repo: Repo,
}

impl Addon for OpenClone {
    fn to_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// A pull request in the local copy.
struct LocalPullRequest {
    repo: Repo,
    /// The repository's canonical name, GitHub's, which starts each file's full path.
    name: String,
    /// The commit the diff starts from: where the head branched from the base branch.
    base: Oid,
    /// The head shown: GitHub's, checked against `refs/pull/<n>/head`.
    head: Oid,
    /// Where the head came from.
    head_source: HeadSource,
    /// What changed between `base` and `head`, in git's order.
    files: Vec<ChangedFile>,
}

/// Fetches the pull request into its repository's clone, finds where it branched from its base
/// and lists what changed. Makes no checkout. Blocks while git runs.
fn copy(
    cache: &RepoCache,
    link: &PrLink,
    pull_request: &PullRequest,
    token: Option<Token>,
) -> anyhow::Result<LocalPullRequest> {
    // The repository's canonical name, not the link's, so a renamed one keeps its clone.
    let (owner, name) = pull_request.base.repo.owner_and_name()?;
    let repo = cache.open(owner, name, token)?;
    // A SHA from the API that isn't one only turns off what it's for, and says so.
    let api_oid = |what: &str, sha: &str| match sha.parse::<Oid>() {
        Ok(oid) => Some(oid),
        Err(error) => {
            log::warn!("{link}: GitHub's API gave a {what} that isn't a commit ID: {error}");
            None
        }
    };
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
            head: api_oid(
                "head, so the head isn't checked against it,",
                &pull_request.head.sha,
            ),
            base_branch: pull_request.base.branch.clone(),
            base: api_oid(
                "base, so a deleted base branch can't fall back to it,",
                &pull_request.base.sha,
            ),
        })?;
    match &fetched.head_source {
        HeadSource::PullRef => {}
        // GitHub updates refs/pull/<n>/head moments after a push, so the two can briefly differ.
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
    Ok(LocalPullRequest {
        repo,
        name: name.to_string(),
        base,
        head: fetched.head,
        head_source: fetched.head_source,
        files,
    })
}

/// Files with more changed lines than this get a placeholder instead of loading (design §5).
const LARGE_FILE_LINES: u32 = 5_000;

/// Files larger than this, on either side, get a placeholder too: a minified bundle or generated
/// data can be megabytes in a few lines.
const LARGE_FILE_BYTES: u64 = 1024 * 1024;

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
    /// What the file's header says about the change besides its status.
    badge: Option<SharedString>,
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

/// The header label of a file too large to load.
const NOT_LOADED: &str = "Not loaded";

impl FilePlan {
    fn diff(old_text: Option<String>, new: NewSide) -> Self {
        Self {
            content: Content::Diff { old_text, new },
            warning: None,
            badge: None,
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
            badge: None,
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
    let plan = try_plan_file(repo, file, checkout).unwrap_or_else(|error| FilePlan::failed(&error));
    // Line endings are noted when the contents were compared.
    let badge = [badge(file), plan.badge]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    FilePlan {
        badge: (!badge.is_empty()).then(|| badge.join(" · ").into()),
        ..plan
    }
}

fn try_plan_file(
    repo: &Repo,
    file: &ChangedFile,
    checkout: Option<&Path>,
) -> anyhow::Result<FilePlan> {
    let deleted = file.new.is_none();
    if file.binary {
        return Ok(FilePlan::placeholder(
            "(binary file not shown)",
            deleted,
            true,
        ));
    }
    if let Some(lines) = file.lines
        && lines.added + lines.deleted > LARGE_FILE_LINES
    {
        let summary = format!(
            "{} lines added and {} deleted, more than the {LARGE_FILE_LINES} rozed loads at once",
            lines.added, lines.deleted
        );
        return Ok(FilePlan {
            warning: Some(Warning {
                label: NOT_LOADED.into(),
                detail: format!("This file's diff isn't loaded: {summary}.").into(),
            }),
            ..FilePlan::placeholder(
                format!("(large diff not loaded: {summary})"),
                deleted,
                false,
            )
        });
    }

    if let Some(size) = file.size()
        && size > LARGE_FILE_BYTES
    {
        let summary = format!(
            "{}, more than the 1 MiB rozed loads at once",
            mebibytes(size)
        );
        return Ok(FilePlan {
            warning: Some(Warning {
                label: NOT_LOADED.into(),
                detail: format!("This file isn't loaded: {summary}.").into(),
            }),
            ..FilePlan::placeholder(
                format!("(large file not loaded: {summary})"),
                deleted,
                false,
            )
        });
    }

    let old_bytes = file
        .old
        .as_ref()
        .map(|old| read_side(repo, old))
        .transpose()
        .context("reading the base's version")?;
    let Some(new) = &file.new else {
        return Ok(FilePlan::diff(old_bytes.map(text), NewSide::Deleted));
    };
    let new_bytes = read_side(repo, new).context("reading the head's version")?;
    // Both sides' line endings are normalized for the diff, so a change to them alone would
    // otherwise look like no change at all.
    let badge = old_bytes
        .as_ref()
        .filter(|old| **old != new_bytes && normalized(old) == normalized(&new_bytes))
        .map(|_| SharedString::from("line endings only"));
    let old_text = old_bytes.map(text);
    let new_text = text(new_bytes.clone());
    let from_git = |warning: Option<Warning>| FilePlan {
        warning,
        badge: badge.clone(),
        ..FilePlan::diff(old_text.clone(), NewSide::Git(new_text.clone()))
    };
    // A submodule is never a file in the checkout.
    let Some(checkout) = checkout.filter(|_| new.mode != FileMode::Submodule) else {
        return Ok(from_git(None));
    };
    Ok(match repo.hash_checkout_file(checkout, &new.path) {
        Ok(Some(id)) if id == new.oid => FilePlan {
            badge: badge.clone(),
            ..FilePlan::diff(old_text.clone(), NewSide::Checkout)
        },
        Ok(found) => from_git(Some(Warning {
            label: "Checkout differs: shown from git".into(),
            detail: format!(
                "The checkout's copy of this file {}, so this shows the head's text from git, \
                 without language servers.",
                match found {
                    Some(id) => format!("hashes to {id}, not the head's blob {}", new.oid),
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
    })
}

/// `bytes` in mebibytes, as "2.5 MiB".
fn mebibytes(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}

/// One side's bytes: a blob's from git, or for a submodule the line GitHub shows, since its
/// commit is in another repository and reading it would ask the remote for it.
fn read_side(repo: &Repo, version: &FileVersion) -> anyhow::Result<Vec<u8>> {
    if version.mode == FileMode::Submodule {
        return Ok(format!("Subproject commit {}\n", version.oid).into_bytes());
    }
    repo.read_blob(&version.oid)?
        .with_context(|| format!("git has no blob {}", version.oid))
}

/// Text with line endings normalized as Zed's buffers have them.
fn text(bytes: Vec<u8>) -> String {
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    LineEnding::normalize(&mut text);
    text
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
    badge: Option<SharedString>,
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
        // A name that isn't UTF-8 shows with U+FFFD, and its badge says so.
        let path = RelPath::from_unix_str(file.display_path().as_ref())?.into_arc();
        let deleted = file.new.is_none();
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
            badge: plan.badge,
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

/// What a changed file's header says about the change besides its status, from what git listed:
/// a name that isn't UTF-8, a rename's old path, a submodule, a change of type or mode.
fn badge(file: &ChangedFile) -> Option<SharedString> {
    let mut notes = Vec::new();
    let paths = [file.old_path(), file.new_path()];
    if paths
        .into_iter()
        .flatten()
        .any(|path| std::str::from_utf8(path).is_err())
    {
        notes.push("name isn't UTF-8".to_string());
    }
    if let Change::Renamed { .. } = file.change
        && let Some(old_path) = file.old_path()
    {
        notes.push(format!(
            "renamed from {}",
            String::from_utf8_lossy(old_path)
        ));
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

/// What each file's header shows beyond its path: the pull request's change (added, deleted,
/// renamed or modified) rather than its status in the checkout, which is always clean, a badge
/// with what the status doesn't say, and any warning.
#[derive(Default)]
struct FileHeaders {
    statuses: HashMap<BufferId, FileStatus>,
    badges: HashMap<BufferId, SharedString>,
    warnings: HashMap<BufferId, Warning>,
}

impl FileHeaders {
    /// Moves what the header of buffer `from` shows to buffer `to`, which replaced it, leaving out
    /// any warning: the new buffer is the checkout's, which matched.
    fn move_to(&mut self, from: BufferId, to: BufferId) {
        if let Some(status) = self.statuses.remove(&from) {
            self.statuses.insert(to, status);
        }
        if let Some(badge) = self.badges.remove(&from) {
            self.badges.insert(to, badge);
        }
        self.warnings.remove(&from);
    }
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
        let badge = self.badges.get(&buffer.remote_id());
        let warning = self.warnings.get(&buffer.remote_id());
        if badge.is_none() && warning.is_none() {
            return None;
        }
        Some(
            h_flex()
                .flex_shrink_0()
                .gap_2()
                .children(badge.map(|badge| {
                    Label::new(badge.clone())
                        .size(LabelSize::Small)
                        .color(Color::Muted)
                }))
                .children(warning.map(|warning| {
                    h_flex()
                        .id(("file-warning", buffer.remote_id().to_proto()))
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
                }))
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

    use repo_cache::{Git, LineCounts};
    use tempfile::TempDir;

    use fixtures::Entry;

    fn version(path: &str, oid: char, mode: FileMode) -> Option<FileVersion> {
        Some(FileVersion {
            path: path.into(),
            oid: oid.to_string().repeat(40).parse().unwrap(),
            mode,
            size: Some(10),
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

    const REGULAR: FileMode = FileMode::Regular;

    fn code(status: FileStatus) -> StatusCode {
        match status {
            FileStatus::Tracked(TrackedStatus { index_status, .. }) => index_status,
            other => panic!("{other:?} isn't a tracked status"),
        }
    }

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
    fn a_head_github_no_longer_has_gets_a_banner() {
        let link: PrLink = "https://github.com/sharkdp/fd/pull/2212".parse().unwrap();
        let oid = |c: char| -> Oid { c.to_string().repeat(40).parse().unwrap() };
        let banner = |head_source| head_banner(&link, &oid('a'), &head_source);

        assert_eq!(banner(HeadSource::PullRef), None);
        // GitHub moves refs/pull/<n>/head a moment after a push, and rozed shows the API's head.
        assert_eq!(banner(HeadSource::Api { pull_ref: oid('b') }), None);
        assert_eq!(
            banner(HeadSource::PullRefNotApi { api: oid('c') }),
            Some(
                "GitHub says this pull request's head is ccccccc, which GitHub no longer has. \
                 This shows refs/pull/2212/head, aaaaaaa, instead."
                    .to_string()
            )
        );
    }

    #[test]
    fn only_files_shown_from_git_swap_to_the_checkout() {
        let warning = Warning {
            label: "Checkout differs: shown from git".into(),
            detail: "detail".into(),
        };
        let from_checkout = FilePlan::diff(None, NewSide::Checkout);
        let differs = FilePlan {
            warning: Some(warning.clone()),
            ..FilePlan::diff(None, from_git("text"))
        };
        let shown_from_git = FilePlan::diff(None, from_git("text"));
        let placeholder = FilePlan::placeholder("(binary file not shown)", false, true);

        assert_eq!(
            swap(&shown_from_git, from_checkout.clone()),
            Swap::ToCheckout(from_checkout.clone())
        );
        assert_eq!(swap(&shown_from_git, differs), Swap::Warn(warning));
        assert_eq!(swap(&shown_from_git, shown_from_git.clone()), Swap::Keep);
        // A placeholder or a deleted file never comes from the checkout.
        assert_eq!(swap(&placeholder, from_checkout.clone()), Swap::Keep);
        let deleted = FilePlan::diff(Some("text".into()), NewSide::Deleted);
        assert_eq!(swap(&deleted, from_checkout), Swap::Keep);
    }

    #[test]
    fn line_endings_normalize_to_newlines() {
        assert_eq!(normalized(b"a\r\nb\rc\n"), b"a\nb\nc\n");
        assert_eq!(normalized(b"a\nb"), b"a\nb");
        assert_eq!(normalized(b"a\r\r\n"), b"a\n\n");
    }

    /// A pull request copied locally from a fixture, with a checkout when asked for.
    struct Copied {
        _fixture: fixtures::Fixture,
        _cache: TempDir,
        local: LocalPullRequest,
        checkout: Option<PathBuf>,
    }

    fn copy(base: &[Entry], head: &[Entry], checkout: bool) -> Copied {
        let fixture = fixtures::pull_request(base, head);
        let cache_dir = TempDir::new().unwrap();
        let cache = RepoCache::new(cache_dir.path(), Git::find().unwrap())
            .unwrap()
            .with_remote_base(fixture.base_url());
        let repo = cache.open("owner", "repo", None).unwrap();
        let fetched = repo
            .fetch_pull(&PullRequestRefs {
                number: 1,
                head: None,
                base_branch: "main".into(),
                base: None,
            })
            .unwrap();
        let base = repo
            .merge_base(&fetched.head, &fetched.base_tip)
            .unwrap()
            .unwrap();
        let files = repo.changed_files(&base, &fetched.head).unwrap();
        let local = LocalPullRequest {
            repo,
            name: "repo".into(),
            base,
            head: fetched.head,
            head_source: fetched.head_source,
            files,
        };
        let checkout = checkout.then(|| local.repo.checkout(1, &local.head).unwrap());
        Copied {
            _fixture: fixture,
            _cache: cache_dir,
            local,
            checkout,
        }
    }

    impl Copied {
        /// Each file's path, as shown, and plan.
        fn plans(&self) -> Vec<(String, FilePlan)> {
            self.local
                .files
                .iter()
                .map(|file| {
                    let plan = plan_file(&self.local.repo, file, self.checkout.as_deref());
                    (file.display_path().into_owned(), plan)
                })
                .collect()
        }
    }

    #[test]
    fn a_change_to_line_endings_alone_gets_a_badge() {
        for checkout in [false, true] {
            let copied = copy(
                &[Entry::File("a.txt", b"one\ntwo\n")],
                &[Entry::File("a.txt", b"one\r\ntwo\r\n")],
                checkout,
            );
            let new = if checkout {
                NewSide::Checkout
            } else {
                from_git("one\ntwo\n")
            };
            assert_eq!(
                copied.plans(),
                vec![(
                    "a.txt".into(),
                    FilePlan {
                        badge: Some("line endings only".into()),
                        ..FilePlan::diff(Some("one\ntwo\n".into()), new)
                    }
                )]
            );
        }
    }

    #[test]
    fn a_file_with_more_than_5000_changed_lines_isnt_loaded() {
        let big: String = (0..5_001).map(|i| format!("line {i}\n")).collect();
        let copied = copy(&[], &[Entry::File("big.txt", big.as_bytes())], false);

        let [(_, plan)] = &copied.plans()[..] else {
            panic!("expected one file");
        };

        assert_eq!(
            plan.content,
            Content::Placeholder {
                text: "(large diff not loaded: 5001 lines added and 0 deleted, more than the \
                       5000 rozed loads at once)"
                    .into(),
                deleted: false,
                folded: false,
            }
        );
        assert_eq!(plan.warning.as_ref().unwrap().label, NOT_LOADED);
    }

    #[test]
    fn a_file_larger_than_a_mebibyte_isnt_loaded() {
        // One line, so the line count alone wouldn't stop it.
        let minified = "x".repeat(2 * LARGE_FILE_BYTES as usize);
        let copied = copy(
            &[Entry::File("bundle.min.js", b"x")],
            &[Entry::File("bundle.min.js", minified.as_bytes())],
            false,
        );

        let [(_, plan)] = &copied.plans()[..] else {
            panic!("expected one file");
        };

        let Content::Placeholder { text, .. } = &plan.content else {
            panic!("{plan:?}");
        };
        assert_eq!(
            text,
            "(large file not loaded: 2.0 MiB, more than the 1 MiB rozed loads at once)"
        );
        assert_eq!(plan.warning.as_ref().unwrap().label, NOT_LOADED);
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
                    "added.rs".into(),
                    FilePlan::diff(None, from_git("fn added() {}\n"))
                ),
                (
                    "changed.rs".into(),
                    FilePlan::diff(Some("fn old() {}\n".into()), from_git("fn new() {}\n"))
                ),
                (
                    "gone.rs".into(),
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
                ("added.rs".into(), FilePlan::diff(None, NewSide::Checkout)),
                (
                    "changed.rs".into(),
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
                ("Lib.rs".into(), FilePlan::diff(None, from_git(evil))),
                (
                    "lib.rs".into(),
                    FilePlan::diff(Some("pub fn lib() {}\n".into()), from_git(benign))
                ),
            ]
        );
    }

    #[test]
    fn a_submodule_shows_both_commits_without_reading_them() {
        // Neither commit is in the fixture, so reading either would fail.
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
                    "vendor/lib".into(),
                    FilePlan {
                        badge: Some("submodule".into()),
                        ..FilePlan::diff(
                            Some(format!("Subproject commit {old}\n")),
                            from_git(&format!("Subproject commit {new}\n"))
                        )
                    }
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
        file.new.as_mut().unwrap().oid = missing.parse().unwrap();

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
    fn names_that_arent_utf8_show_with_a_badge() {
        let copied = copy(
            &[],
            &[
                Entry::RawFile(b"caf\xe9.txt", b"latin-1\n"),
                Entry::File("tab\tname.txt", b"a tab\n"),
            ],
            false,
        );

        assert_eq!(
            copied.plans(),
            vec![
                (
                    "caf\u{fffd}.txt".into(),
                    FilePlan {
                        badge: Some("name isn't UTF-8".into()),
                        ..FilePlan::diff(None, from_git("latin-1\n"))
                    }
                ),
                (
                    "tab\tname.txt".into(),
                    FilePlan::diff(None, from_git("a tab\n"))
                ),
            ]
        );
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
                    "data.bin".into(),
                    FilePlan::placeholder("(binary file not shown)", false, true)
                ),
                (
                    "gone.bin".into(),
                    FilePlan::placeholder("(binary file not shown)", true, true)
                ),
            ]
        );
    }

    /// Pull requests in real repositories, served over `file://` as GitHub serves them.
    mod fixtures {
        use std::ffi::{OsStr, OsString};
        use std::io::Write as _;
        use std::os::unix::ffi::OsStrExt as _;
        use std::path::Path;
        use std::process::{Command, Stdio};

        use tempfile::TempDir;

        /// An entry in a fixture commit's tree.
        pub enum Entry<'a> {
            File(&'a str, &'a [u8]),
            /// A file whose name is any bytes, such as Latin-1.
            RawFile(&'a [u8], &'a [u8]),
            /// A submodule at the given commit, which the fixture doesn't have.
            Submodule(&'a str, &'a str),
        }

        /// A bare repository `owner/repo` with `main` at a base commit and pull request #1 at a
        /// head commit on top.
        pub struct Fixture {
            dir: TempDir,
        }

        impl Fixture {
            /// What `RepoCache::with_remote_base` takes to fetch from here.
            pub fn base_url(&self) -> String {
                format!("file://{}", self.dir.path().display())
            }
        }

        /// A pull request from a base commit holding `base` to a head commit holding `head`.
        pub fn pull_request(base: &[Entry], head: &[Entry]) -> Fixture {
            let dir = TempDir::new().unwrap();
            let git_dir = dir.path().join("owner/repo.git");
            std::fs::create_dir_all(&git_dir).unwrap();
            run(&git_dir, &["init", "-q", "--bare"], None, None);
            run(
                &git_dir,
                &["config", "uploadpack.allowFilter", "true"],
                None,
                None,
            );
            run(
                &git_dir,
                &["config", "uploadpack.allowAnySHA1InWant", "true"],
                None,
                None,
            );
            let index = dir.path().join("index");
            let base = commit(&git_dir, &index, base, None);
            let head = commit(&git_dir, &index, head, Some(&base));
            run(
                &git_dir,
                &["update-ref", "refs/heads/main", &base],
                None,
                None,
            );
            run(
                &git_dir,
                &["update-ref", "refs/pull/1/head", &head],
                None,
                None,
            );
            Fixture { dir }
        }

        /// Writes a commit whose tree holds exactly `entries`, and returns its ID.
        fn commit(git_dir: &Path, index: &Path, entries: &[Entry], parent: Option<&str>) -> String {
            let _ = std::fs::remove_file(index);
            for entry in entries {
                let blob = |contents: &[u8]| {
                    run(
                        git_dir,
                        &["hash-object", "-w", "--stdin"],
                        Some(contents),
                        None,
                    )
                };
                let (mode, id, path) = match entry {
                    Entry::File(path, contents) => ("100644", blob(contents), path.as_bytes()),
                    Entry::RawFile(path, contents) => ("100644", blob(contents), *path),
                    Entry::Submodule(path, commit) => {
                        ("160000", commit.to_string(), path.as_bytes())
                    }
                };
                let mut cacheinfo = OsString::from(format!("{mode},{id},"));
                cacheinfo.push(OsStr::from_bytes(path));
                run(
                    git_dir,
                    &[
                        OsStr::new("update-index"),
                        OsStr::new("--add"),
                        OsStr::new("--cacheinfo"),
                        &cacheinfo,
                    ],
                    None,
                    Some(index),
                );
            }
            let tree = run(git_dir, &["write-tree"], None, Some(index));
            let mut args = vec!["commit-tree", tree.as_str(), "-m", "fixture"];
            if let Some(parent) = parent {
                args.extend(["-p", parent]);
            }
            run(git_dir, &args, None, None)
        }

        /// Whether the disk under `dir` treats names that differ only in case as one.
        pub fn is_case_insensitive(dir: &Path) -> bool {
            let probe = TempDir::new_in(dir).unwrap();
            std::fs::write(probe.path().join("case"), "").unwrap();
            probe.path().join("CASE").exists()
        }

        fn run<S: AsRef<OsStr> + std::fmt::Debug>(
            dir: &Path,
            args: &[S],
            stdin: Option<&[u8]>,
            index: Option<&Path>,
        ) -> String {
            let mut command = Command::new("git");
            command
                .arg("-C")
                .arg(dir)
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_AUTHOR_NAME", "rozed test")
                .env("GIT_AUTHOR_EMAIL", "test@rozed.invalid")
                .env("GIT_COMMITTER_NAME", "rozed test")
                .env("GIT_COMMITTER_EMAIL", "test@rozed.invalid")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            if let Some(index) = index {
                command.env("GIT_INDEX_FILE", index);
            }
            let mut child = command.spawn().unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(stdin.unwrap_or_default())
                .unwrap();
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap().trim().to_string()
        }
    }
}
