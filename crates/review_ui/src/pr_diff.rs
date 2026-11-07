//! A pull request in Zed's side-by-side diff (`SplittableEditor`), every changed file in one
//! scrolling view.
//!
//! Both sides' text comes from git, each in an in-memory buffer: the left side is the old side of
//! what the diff compares ([`ShownComparison`]), the merge base's for the whole pull request, and
//! the right side its new side, a head. With a trusted checkout, a file whose bytes there hash to
//! the head's blob shows the checkout's project buffer on the right instead, so language servers
//! attach to it. Modelled on Zed's `git_ui_core/src/file_diff_view.rs` (project buffer on the right) and
//! `git_ui/src/commit_view.rs` (many files in one view), without depending on either crate.
//!
//! The local copy is `repo_cache`'s. No changed file is left out. Generated files, lockfiles and
//! files with no changes to their text start collapsed, and files with more than 5,000 changed
//! lines or over 1 MiB start collapsed without loading, until a key loads them; each header says
//! why. An image shows before and after; any other binary file says it changed. A file Lo
//! can't read gets a placeholder saying why, and the title counts the ones not shown. A submodule
//! shows "Subproject commit a" against "Subproject commit b". A header shows a rename as
//! "old →" before the new path, and a badge marks type and mode changes, names that aren't UTF-8
//! (shown with U+FFFD), and changes to line endings alone.

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::Context as _;
use buffer_diff::BufferDiff;
use editor::display_map::{DisplayRow, DisplaySnapshot, ToDisplayPoint as _};
use editor::{
    Addon, Bias, DisplayPoint, HiddenDiffHunkRenderer, MultiBuffer, SelectionEffects,
    SplittableEditor,
};
use futures::FutureExt as _;
use futures::channel::{mpsc, oneshot};
use git::status::{FileStatus, StatusCode, TrackedStatus};
use gpui::{
    AnyElement, App, AppContext as _, AsyncWindowContext, Div, ElementId, Entity, Stateful, Task,
    WeakEntity, Window,
};
use language::{
    Buffer, BufferId, Capability, DiskState, File, LanguageRegistry, LineEnding, Point, ReplicaId,
    Rope, TextBuffer,
};
use multi_buffer::{ExcerptBoundaryInfo, PathKey};
use project::{Project, ProjectPath, WorktreeId};
use settings::DiffViewStyle;
use ui::{Icon, IconName, IconSize, Label, LabelSize, Tooltip, prelude::*};
use util::paths::PathStyle;
use util::rel_path::RelPath;
use workspace::Workspace;
use workspace::notifications::NotificationId;

use repo_cache::{
    Change, ChangedFile, FileMode, FileVersion, HeadSource, Oid, Progress, PullRequestRefs, Repo,
    RepoCache, Token,
};

use crate::api_diff::{self, HunkRows, OutsideGithubsHunks};
use crate::diff_decorations::{self, Badge, BadgeStyle, DiffDecorations, NewDecorator};
use crate::file_display::{
    Collapsed, ImageSide, Images, binary_text, initial_collapse, too_large_to_show,
    without_whitespace_changes,
};
use crate::file_list::FileEntry;
use crate::github_api::PullRequest;
use crate::github_layer::Saved;
use crate::local_copies;
use crate::pr_banner::BannerKind;
use crate::pr_link::PrLink;
use crate::pr_tab::{DiffSource, PrTab, PullHandle};
use crate::shown_comparison::ShownComparison;
use crate::shown_files::{self, ShownFile, ShownFiles};
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

/// Copies the pull request locally, signed in with `token` for a private repository, and shows
/// the local copy's diff in `tab`, from git first; GitHub gave the pull request at `answered_at`.
/// While it copies, the tab shows git's progress, and a fetch that fails for a reason that may
/// pass is tried again, with a banner saying why.
/// When `trust` asks for a checkout, it's made in the background once the diff shows, and each
/// file whose checkout copy matches the head switches to the checkout's project buffer when it's
/// ready, or a banner says why there's none. `launched` is when the tab opened, for timing.
#[allow(clippy::too_many_arguments)]
pub async fn open(
    tab: PullHandle,
    workspace: WeakEntity<Workspace>,
    link: PrLink,
    pull_request: PullRequest,
    answered_at: SystemTime,
    token: Option<Token>,
    trust: Trust,
    launched: Instant,
    cx: &mut AsyncWindowContext,
) -> anyhow::Result<()> {
    log::info!(
        "{link}: {} merges into {} ({:?} after launch)",
        pull_request.head.sha,
        pull_request.base.branch,
        launched.elapsed()
    );
    // Waits for the launch check, which finds git.
    let cache = cx.update(|_, cx| local_copies::cache(cx))?.await?;
    let copy_once = |progress, cx: &mut AsyncWindowContext| {
        let (cache, link, pull_request, token) = (
            cache.clone(),
            link.clone(),
            pull_request.clone(),
            token.clone(),
        );
        cx.background_spawn(async move { copy(&cache, &link, &pull_request, token, progress) })
    };
    let (local, comparison) = copy_with_retries(&tab, &link, copy_once, cx).await?;
    let title = format!(
        "{link}: {}",
        crate::window_title::without_bidi(&pull_request.title)
    );
    load_local_copy(
        tab,
        workspace,
        link,
        title,
        local,
        comparison,
        answered_at,
        trust,
        launched,
        cx,
    )
    .await
}

/// Shows the pull request from the local copy, without GitHub, when GitHub can't be reached
/// (design §11 "Offline"): the head Lo saved as GitHub's, `headRefOid`, if the copy fetched
/// it, against the saved base (see [`copy_offline`]). Returns whether the copy had them;
/// without them, it shows nothing. It reads only what's in the clone: no fetch, no download, no
/// write, and no checkout or language servers.
pub async fn open_offline(
    tab: PullHandle,
    workspace: WeakEntity<Workspace>,
    link: PrLink,
    saved: &Saved,
    launched: Instant,
    cx: &mut AsyncWindowContext,
) -> anyhow::Result<bool> {
    let details = &saved.details;
    let Ok(head) = details.head_ref_oid.parse::<Oid>() else {
        log::warn!("{link}: the saved head isn't a commit ID");
        return Ok(false);
    };
    let base = SavedBase {
        branch: details.base_ref_name.clone(),
        tip: details.base_ref_oid.parse().ok(),
    };
    // The clone goes by the repository's name as GitHub gave it with the saved details.
    let (owner, name) = match details.url.parse::<PrLink>() {
        Ok(saved_link) => (saved_link.owner, saved_link.repo),
        Err(_) => (link.owner.clone(), link.repo.clone()),
    };
    let cache = cx.update(|_, cx| local_copies::cache(cx))?.await?;
    let local = cx
        .background_spawn({
            let (number, head) = (link.number, head.clone());
            async move { copy_offline(&cache, &owner, &name, number, &head, &base) }
        })
        .await
        .with_context(|| format!("reading {link} from its local copy"))?;
    let (local, comparison) = match local {
        Ok(local) => local,
        Err(lacks) => {
            log::info!("{link}: offline, and the local copy {lacks}");
            return Ok(false);
        }
    };
    log::info!(
        "{link}: offline, so showing the local copy's head {head}, {} changed files",
        comparison.files().len()
    );
    let title = format!(
        "{link}: {}",
        crate::window_title::without_bidi(&details.title)
    );
    let polled_at = saved.fetched_at;
    load_local_copy(
        tab,
        workspace,
        link,
        title,
        Arc::new(local),
        Arc::new(comparison),
        polled_at,
        Trust::Untrusted,
        launched,
        cx,
    )
    .await?;
    Ok(true)
}

/// How long to wait before trying a failed copy again the first time. Each failure doubles it, up
/// to [`LAST_RETRY`].
const FIRST_RETRY: Duration = Duration::from_secs(5);
const LAST_RETRY: Duration = Duration::from_secs(5 * 60);

/// How many times a copy that fails for a reason Lo doesn't know is tried before Lo stops
/// and offers to try again. One that fails on the network, or on a rate limit, is tried for as long
/// as the tab is open.
const UNKNOWN_TRIES: usize = 5;

/// Copies the pull request locally with `copy_once`, which starts one try and sends git's
/// progress, showing that progress in `tab`. A copy that fails for a reason that may pass, such as
/// the network, is tried again, waiting longer each time, while a banner in the tab says why. One
/// that keeps failing for a reason Lo doesn't know stops after [`UNKNOWN_TRIES`] until the
/// banner's button asks for another try. Any other failure is returned.
pub(crate) async fn copy_with_retries(
    tab: &PullHandle,
    link: &PrLink,
    mut copy_once: impl FnMut(
        mpsc::UnboundedSender<CopyProgress>,
        &mut AsyncWindowContext,
    ) -> Task<anyhow::Result<(LocalPullRequest, ShownComparison)>>,
    cx: &mut AsyncWindowContext,
) -> anyhow::Result<(Arc<LocalPullRequest>, Arc<ShownComparison>)> {
    let mut wait = FIRST_RETRY;
    let mut unknown = 0;
    loop {
        let started = Instant::now();
        let (progress, mut updates) = mpsc::unbounded();
        // git reports progress from a thread of its own, so the updates are read on a timer:
        // awaiting them would wake this task from that thread.
        let mut copying = copy_once(progress, cx).fuse();
        let copied = loop {
            let mut tick = cx.background_executor().timer(PROGRESS_EVERY).fuse();
            futures::select_biased! {
                copied = copying => break copied,
                _ = tick => {
                    if let Some(update) = latest(&mut updates) {
                        tab.update(cx, |tab, cx| tab.copy_progress(&update, cx))?;
                    }
                }
            }
        };
        let error = match copied {
            Ok((local, comparison)) => {
                log::info!(
                    "{link}: local copy ready in {:?}, {} changed files, base {}, head {}",
                    started.elapsed(),
                    comparison.files().len(),
                    local.base,
                    local.head
                );
                tab.update(cx, |tab, cx| tab.copy_retried(None, cx))?;
                return Ok((Arc::new(local), Arc::new(comparison)));
            }
            Err(error) => error,
        };
        let Some(retry) = retry(&error) else {
            return Err(error.context(format!("copying {link} locally")));
        };
        let why = format!("{error:#}");
        if retry == Retry::Few {
            unknown += 1;
            if unknown >= UNKNOWN_TRIES {
                log::warn!(
                    "{link}: copying it locally failed {unknown} times: {why}; waiting to be asked \
                     to try again"
                );
                let (asked, asking) = oneshot::channel();
                tab.update(cx, |tab, cx| tab.copy_stopped(&why, unknown, asked, cx))?;
                // The tab dropped the button's sender: it closed, or loads anew.
                if asking.await.is_err() {
                    return Err(error.context(format!("copying {link} locally")));
                }
                log::info!("{link}: trying to copy it locally again, as asked");
                (unknown, wait) = (0, FIRST_RETRY);
                continue;
            }
        }
        log::warn!("{link}: copying it locally: {why}; trying again in {wait:?}");
        count_down(tab, &why, wait, cx).await?;
        wait = next_wait(wait);
    }
}

/// The wait before the next try, after waiting `wait` before this one: twice as long, up to
/// [`LAST_RETRY`].
pub(crate) fn next_wait(wait: Duration) -> Duration {
    (wait * 2).min(LAST_RETRY)
}

/// Says in `tab`'s banner that the copy failed for `why` and counts `wait` down, a second at a
/// time. It ends when `wait` is up on the executor's clock or the wall clock, whichever comes
/// first: the executor's timers don't run while the Mac sleeps.
async fn count_down(
    tab: &PullHandle,
    why: &str,
    wait: Duration,
    cx: &mut AsyncWindowContext,
) -> anyhow::Result<()> {
    let executor = cx.background_executor().clone();
    let (started, started_at) = (executor.now(), SystemTime::now());
    loop {
        let waited = executor
            .now()
            .duration_since(started)
            .max(started_at.elapsed().unwrap_or_default());
        let left = wait.saturating_sub(waited);
        if left.is_zero() {
            break;
        }
        tab.update(cx, |tab, cx| tab.copy_retried(Some((why, Some(left))), cx))?;
        executor.timer(left.min(Duration::from_secs(1))).await;
    }
    tab.update(cx, |tab, cx| tab.copy_retried(Some((why, None)), cx))?;
    Ok(())
}

/// How often the tab shows how far a copy has got.
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

/// The latest of the updates `updates` has waiting, if any: of a burst, only the latest is worth
/// showing.
fn latest(updates: &mut mpsc::UnboundedReceiver<CopyProgress>) -> Option<CopyProgress> {
    let mut latest = None;
    while let Ok(update) = updates.try_recv() {
        latest = Some(update);
    }
    latest
}

/// How often a failed copy is tried again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Retry {
    /// For as long as it takes: the network failed, or GitHub is limiting requests, which passes.
    Always,
    /// A few times: the fetch failed for a reason Lo doesn't know.
    Few,
}

/// Whether a failed copy may work if tried again, and how often to try: a fetch that failed on
/// the network, on a rate limit, or for a reason Lo doesn't know.
fn retry(error: &anyhow::Error) -> Option<Retry> {
    use repo_cache::{Error, FetchFailure};
    error
        .chain()
        .find_map(|cause| match cause.downcast_ref::<Error>()? {
            Error::Fetch {
                failure: FetchFailure::Network | FetchFailure::RateLimited,
                ..
            } => Some(Retry::Always),
            Error::Fetch {
                failure: FetchFailure::Other,
                ..
            } => Some(Retry::Few),
            _ => None,
        })
}

/// What only loading the pull request does, besides showing its diff.
#[derive(Clone, Copy)]
struct Load {
    /// When GitHub gave the head the local copy fetched.
    polled_at: SystemTime,
    trust: Trust,
}

/// Loads the pull request's local copy, `local`, into `tab`, showing `comparison` of it: the
/// whole pull request when it opens, of the head GitHub gave at `polled_at`. While the tab shows a
/// diff, GitHub's or an older local copy's, this one is built out of sight and replaces it once
/// every file is in, scrolled to the same file and line; otherwise it shows at once and fills in
/// as files load.
///
/// What only a load does goes with it: the tab takes `local` as the pull request's copy and
/// builds its revisions, banners say when the head isn't GitHub's or GitHub's diff showed another,
/// and with `trust`, the head is checked out for language servers, which the files of a
/// comparison of that head then show from. To show other revisions afterwards, use
/// [`show_comparison`].
#[allow(clippy::too_many_arguments)]
pub(crate) async fn load_local_copy(
    tab: PullHandle,
    workspace: WeakEntity<Workspace>,
    link: PrLink,
    title: String,
    local: Arc<LocalPullRequest>,
    comparison: Arc<ShownComparison>,
    polled_at: SystemTime,
    trust: Trust,
    launched: Instant,
    cx: &mut AsyncWindowContext,
) -> anyhow::Result<()> {
    let load = Load { polled_at, trust };
    show(
        tab,
        workspace,
        link,
        title,
        local,
        comparison,
        Some(load),
        launched,
        cx,
    )
    .await
}

/// Shows `comparison` of `local`, the pull request's local copy, in `tab`, in place of the diff
/// it shows, at the same file and line, as when the reviewer picks other revisions: built out of
/// sight, then swapped in, or at once when nothing shows yet. The file list lists its files, and
/// its new side is the head on screen, so `e` marks files only when that's GitHub's latest.
///
/// Nothing only a load does happens: the revisions, the banners about the load, and the
/// checkout stay as they are, and every file shows from git.
#[allow(clippy::too_many_arguments)]
pub async fn show_comparison(
    tab: PullHandle,
    workspace: WeakEntity<Workspace>,
    link: PrLink,
    title: String,
    local: Arc<LocalPullRequest>,
    comparison: Arc<ShownComparison>,
    launched: Instant,
    cx: &mut AsyncWindowContext,
) -> anyhow::Result<()> {
    show(
        tab, workspace, link, title, local, comparison, None, launched, cx,
    )
    .await
}

/// [`load_local_copy`] with `load`, else [`show_comparison`].
#[allow(clippy::too_many_arguments)]
async fn show(
    tab: PullHandle,
    workspace: WeakEntity<Workspace>,
    link: PrLink,
    title: String,
    local: Arc<LocalPullRequest>,
    comparison: Arc<ShownComparison>,
    load: Option<Load>,
    launched: Instant,
    cx: &mut AsyncWindowContext,
) -> anyhow::Result<()> {
    let project = workspace.update(cx, |workspace, _| workspace.project().clone())?;
    let languages = project.read_with(cx, |project, _| project.languages().clone());
    let banners = tab.update(cx, |tab, _| tab.banners().clone())?;
    // Only the head the load checked out shows from the checkout.
    let trust = load
        .map(|load| load.trust)
        .filter(|_| *comparison.new_side() == local.head)
        .unwrap_or(Trust::Untrusted);
    // What the banners say about this diff, once it shows.
    let show_banners = |cx: &mut AsyncWindowContext| {
        banners.update(cx, |banners, cx| {
            if load.is_some()
                && let Some(text) = head_banner(&link, &local.head, &local.head_source)
            {
                banners.show(BannerKind::HeadMismatch, Severity::Warning, text, cx);
            }
            match comparison.attributes_unread() {
                Some(why) => banners.show(
                    BannerKind::AttributesUnread,
                    Severity::Info,
                    format!(
                        "Lo couldn't read this pull request's .gitattributes, so files it \
                         marks generated start expanded: {why}"
                    ),
                    cx,
                ),
                None => banners.hide(BannerKind::AttributesUnread, cx),
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
        })
    };

    let style = tab.update(cx, |tab, _| tab.diff_style())?;
    let (multi_buffer, editor) = new_diff_editor(&tab, &workspace, &project, &title, style, cx)?;
    editor.update(cx, |editor, cx| {
        editor.rhs_editor().update(cx, |editor, _| {
            // Keeps the clone open, so no cleanup removes it or its checkout while the tab shows
            // it, and lets a large file load on a key.
            if let Some(files) = editor.addon_mut::<ShownFiles>() {
                files.comparison = Some(comparison.clone());
            }
        })
    });
    // The tab lists the files and says where each goes in the diff, at once, or with a diff on
    // screen, once this one replaces it, so the list always matches the diff shown.
    let entries = comparison.entries();
    let new_side = comparison.new_side().clone();
    let list = |tab: &mut PrTab, cx: &mut gpui::Context<PrTab>| match load {
        Some(load) => {
            tab.set_local_copy(local.clone());
            tab.copied(entries.clone(), &new_side, load.polled_at, cx)
        }
        None => tab.compared(entries.clone(), &new_side, cx),
    };
    let (replacing, positions) = tab.update_in(cx, |tab, window, cx| {
        if let Some(source) = tab.diff_source() {
            return (Some(source), tab.positions_of(&entries));
        }
        let positions = list(tab, cx);
        tab.show_diff(editor.clone(), DiffSource::LocalCopy, window, cx);
        (None, positions)
    })?;
    if replacing.is_none() {
        show_banners(cx);
    }

    let mut target = Target {
        repo_name: comparison.repo_name(),
        project: &project,
        checkout: None,
        languages: &languages,
        editor: &editor,
        hide_whitespace: false,
    };
    let (shown_files, not_shown) = add_files(
        &tab,
        &mut target,
        &comparison,
        &positions,
        &link,
        launched,
        cx,
    )
    .await?;

    if not_shown > 0 {
        let files = if not_shown == 1 { "file" } else { "files" };
        multi_buffer.update(cx, |multi_buffer, cx| {
            multi_buffer.set_title(format!("{title} ({not_shown} {files} not shown)"), cx)
        });
    }
    tab.update_in(cx, |tab, window, cx| {
        if replacing.is_some() {
            list(tab, cx);
            if load.is_some() {
                tab.say_if_moved_on(&new_side, cx);
            }
            tab.switch_diff(editor.clone(), window, cx);
        }
        tab.diff_loaded(window, cx);
    })?;
    if let Some(replaced) = replacing {
        show_banners(cx);
        let replaced = match replaced {
            DiffSource::GitHub => "GitHub's diff",
            DiffSource::LocalCopy => "another local copy's",
        };
        log::info!(
            "{link}: switched from {replaced} to the local copy's diff {:?} after launch",
            launched.elapsed()
        );
    }
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

    if load.is_none() {
        return Ok(());
    }
    if trust == Trust::Untrusted {
        log::info!("{link}: not trusted, so no checkout and no language servers");
        return Ok(());
    }
    let checkout = check_out(&local, &link, &project, cx).await;
    banners.update(cx, |banners, cx| banners.hide(BannerKind::CheckingOut, cx));
    let checkout = match checkout {
        Ok(checkout) => {
            // Its worktree, and the language servers in it, go with the last tab using it.
            tab.update(cx, |tab, _| tab.uses_checkout(checkout.worktree_id))?;
            checkout
        }
        Err(error) => {
            log::warn!("{link}: {error:#}");
            banners.update(cx, |banners, cx| {
                banners.show(
                    BannerKind::CheckoutFailed,
                    Severity::Warning,
                    format!(
                        "Lo couldn't check {link} out, so language servers are off and every \
                         file shows from git: {error:#}"
                    ),
                    cx,
                )
            });
            return Ok(());
        }
    };
    // A large file loaded on a key from now on shows from the checkout too.
    editor.update(cx, |editor, cx| {
        editor.rhs_editor().update(cx, |editor, _| {
            if let Some(files) = editor.addon_mut::<ShownFiles>() {
                files.checkout = Some(checkout.clone());
            }
        })
    });
    let mut target = Target {
        checkout: Some(&checkout),
        ..target
    };
    for (index, (file, shown)) in comparison.files().iter().zip(&shown_files).enumerate() {
        let Some((buffer_id, first)) = shown else {
            continue;
        };
        let (file, generated) = (&file.change, file.generated);
        let plan = cx
            .background_spawn({
                let repo = comparison.repo().clone();
                let file = file.clone();
                let dir = checkout.dir.clone();
                async move { plan_file(&repo, &file, generated, Some(&dir), false) }
            })
            .await;
        match swap(first, plan) {
            Swap::Keep => {}
            Swap::Warn(warning) => {
                let path = file.display_path();
                log::warn!("{link}: {path}: {}", warning.detail);
                cx.update(|window, cx| {
                    shown_files::set_warning(&editor, &path, *buffer_id, warning, window, cx)
                })?;
            }
            Swap::ToCheckout(plan) => {
                target.hide_whitespace = tab.update(cx, |tab, _| tab.hides_whitespace())?;
                if let Err(error) = target
                    .add_file(
                        positions[index],
                        &file.display_path(),
                        file.new.is_none(),
                        *plan,
                        FileHeader::of(file),
                        cx,
                    )
                    .await
                {
                    log::warn!(
                        "{link}: can't show {} from the checkout, so it stays from git: {error:#}",
                        file.display_path()
                    )
                }
            }
        }
    }
    log::info!(
        "{link}: checkout ready and swapped in {:?} after launch",
        launched.elapsed()
    );
    Ok(())
}

/// Adds each of `comparison`'s files to `target`, at its place in `positions`, telling `tab` how
/// far it's got. Returns each file's buffer and plan, for switching to the checkout, and how many
/// files couldn't be shown.
async fn add_files(
    tab: &PullHandle,
    target: &mut Target<'_>,
    comparison: &ShownComparison,
    positions: &[usize],
    link: &PrLink,
    launched: Instant,
    cx: &mut AsyncWindowContext,
) -> anyhow::Result<(Vec<Option<(BufferId, FilePlan)>>, usize)> {
    let mut shown_files = Vec::with_capacity(comparison.files().len());
    let mut not_shown = 0;
    for (index, file) in comparison.files().iter().enumerate() {
        target.hide_whitespace = tab.update(cx, |tab, cx| {
            tab.loading_diff(index + 1, comparison.files().len(), cx);
            tab.hides_whitespace()
        })?;
        let (file, generated) = (&file.change, file.generated);
        let plan = cx
            .background_spawn({
                let repo = comparison.repo().clone();
                let file = file.clone();
                async move { plan_file(&repo, &file, generated, None, false) }
            })
            .await;
        let position = positions[index];
        let (path, deleted) = (file.display_path(), file.new.is_none());
        let header = FileHeader::of(file);
        let shown = match target
            .add_file(position, &path, deleted, plan.clone(), header.clone(), cx)
            .await
        {
            Ok(shown) => Ok((shown, plan)),
            Err(error) => {
                let plan = FilePlan::failed(&error);
                target
                    .add_file(position, &path, deleted, plan.clone(), header, cx)
                    .await
                    .map(|shown| (shown, plan))
            }
        };
        match shown {
            Ok((shown, plan)) => {
                if let Some(warning) = &shown.warning {
                    log::warn!("{link}: {}: {}", file.display_path(), warning.detail);
                    not_shown += usize::from(warning.label == NOT_SHOWN);
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
        comparison.files().len(),
        launched.elapsed()
    );
    Ok((shown_files, not_shown))
}

/// Loads `path`, a file too large to have been loaded with the others, into `diff` in place of
/// its placeholder, from the trusted checkout once there is one, and opens it, unless it's too
/// large to show at all. Does nothing in GitHub's diff, which has nothing to load.
pub(crate) async fn load_large_file(
    diff: Entity<SplittableEditor>,
    project: Entity<Project>,
    path: String,
    hide_whitespace: bool,
    cx: &mut AsyncWindowContext,
) -> anyhow::Result<()> {
    let found = cx.update(|_, cx| {
        shown_files::read(&diff, cx, |files| {
            let comparison = files.comparison.clone()?;
            let index = comparison
                .files()
                .iter()
                .position(|file| file.change.display_path() == path)?;
            let position = usize::try_from(files.get(&path)?.key.sort_prefix?).ok()?;
            Some((comparison, files.checkout.clone(), index, position))
        })
        .flatten()
    })?;
    let Some((comparison, checkout, index, position)) = found else {
        return Ok(());
    };
    let file = comparison.files()[index].change.clone();
    let generated = comparison.files()[index].generated;
    let started = Instant::now();
    let plan = cx
        .background_spawn({
            let (repo, file) = (comparison.repo().clone(), file.clone());
            let dir = checkout.as_ref().map(|checkout| checkout.dir.clone());
            async move { plan_file(&repo, &file, generated, dir.as_deref(), true) }
        })
        .await;
    // Too large even to load: it stays collapsed, saying so.
    let opens = !matches!(plan.collapsed, Some(Collapsed::TooLarge { .. }));
    let languages = project.read_with(cx, |project, _| project.languages().clone());
    let target = Target {
        repo_name: comparison.repo_name(),
        project: &project,
        checkout: checkout.as_ref(),
        languages: &languages,
        editor: &diff,
        hide_whitespace,
    };
    let shown = target
        .add_file(
            position,
            &path,
            file.new.is_none(),
            plan,
            FileHeader::of(&file),
            cx,
        )
        .await?;
    if opens {
        diff.update(cx, |diff, cx| {
            diff.rhs_editor()
                .update(cx, |rhs, cx| rhs.unfold_buffer(shown.buffer_id, cx))
        });
    }
    log::info!("{path}: loaded on request in {:?}", started.elapsed());
    Ok(())
}

/// Every feature's decorator, in the order their badges show in a file's header. A feature that
/// draws on the diff adds its own here, one line (see [`crate::diff_decorations`]).
const DECORATORS: &[NewDecorator] = &[
    diff_decorations::collapsed,
    diff_decorations::change_notes,
    diff_decorations::warnings,
    diff_decorations::renamed_from,
    shown_files::image_blocks,
];

/// A new, empty side-by-side or inline diff of `title` for `tab`, read-only and without the
/// controls for staging and restoring hunks, with every feature's decorator.
pub(crate) fn new_diff_editor(
    tab: &PullHandle,
    workspace: &WeakEntity<Workspace>,
    project: &Entity<Project>,
    title: &str,
    style: DiffViewStyle,
    cx: &mut AsyncWindowContext,
) -> anyhow::Result<(Entity<MultiBuffer>, Entity<SplittableEditor>)> {
    let multi_buffer = cx.new(|cx| {
        let mut multi_buffer = MultiBuffer::new(Capability::ReadOnly).with_title(title.to_string());
        multi_buffer.set_all_diff_hunks_expanded(cx);
        multi_buffer
    });
    let editor = workspace.update_in(cx, |_, window, cx| {
        let workspace_entity = cx.entity();
        cx.new(|cx| {
            let editor = SplittableEditor::new(
                style,
                multi_buffer.clone(),
                project.clone(),
                workspace_entity,
                window,
                cx,
            );
            editor.set_diff_hunk_renderer(Some(Arc::new(HiddenDiffHunkRenderer)), cx);
            editor.rhs_editor().update(cx, |editor, cx| {
                editor.set_show_bookmarks(false, cx);
                editor.set_show_breakpoints(false, cx);
                editor.register_addon(FileHeaders::default());
                editor.register_addon(ShownFiles::default());
                editor.register_addon(DiffDecorations::new(tab.tab(), DECORATORS, cx));
            });
            editor
        })
    })?;
    Ok((multi_buffer, editor))
}

/// Where a diff is scrolled to: the file and line at the top of the view.
#[derive(Clone, Debug, PartialEq)]
pub struct DiffPosition {
    /// The file's path in the pull request.
    pub path: String,
    /// The row, from 0, in the head's version of the file: the line's number less one.
    pub row: u32,
    /// How many rows below the top of the view the row shows: the rows of its file's header, or
    /// a fraction above zero when the row is scrolled partly out of view.
    pub below_top: f64,
}

/// The file and line at the top of `diff`'s view, or the first one below it when a file's header,
/// or a deleted line, is at the top.
pub fn scroll_position(diff: &Entity<SplittableEditor>, cx: &mut App) -> Option<DiffPosition> {
    let editor = diff.read(cx).rhs_editor().clone();
    editor.update(cx, |editor, cx| {
        let top = editor.scroll_position(cx).y.max(0.);
        let display = editor.display_snapshot(cx);
        let top_row = DisplayPoint::new(DisplayRow(top.floor() as u32), 0);
        let point = display.display_point_to_point(top_row, Bias::Right);
        let (path, point) = file_point(&display, point)?;
        let shown_at = f64::from(point.1.to_display_point(&display).row().0);
        Some(DiffPosition {
            path,
            row: point.0.row,
            below_top: shown_at - top,
        })
    })
}

/// The file and point that `point` of `display`'s multibuffer shows, with the multibuffer's point
/// for it, or the first file's line below it. A deleted line is a row of its base's buffer, which
/// has no file, so it gives the start of the file's next line.
fn file_point(display: &DisplaySnapshot, mut point: Point) -> Option<(String, (Point, Point))> {
    let buffer_snapshot = display.buffer_snapshot();
    loop {
        if let Some((buffer, buffer_point)) = buffer_snapshot.point_to_buffer_point(point)
            && let Some(file) = buffer.file()
        {
            return Some((file.path().as_unix_str().to_string(), (buffer_point, point)));
        }
        if point.row >= buffer_snapshot.max_point().row {
            return None;
        }
        point = Point::new(point.row + 1, 0);
    }
}

/// Scrolls `diff` so `position`'s file and line show where they did, or the nearest line of that
/// file it shows. Returns whether `diff` has the file.
pub fn scroll_to(
    diff: &Entity<SplittableEditor>,
    position: &DiffPosition,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    let editor = diff.read(cx).rhs_editor().clone();
    let multi_buffer = editor.read(cx).buffer().clone();
    let buffer = multi_buffer.read(cx).all_buffers_iter().find(|buffer| {
        buffer
            .read(cx)
            .file()
            .is_some_and(|file| file.path().as_unix_str() == position.path)
    });
    let Some(anchor) = buffer.and_then(|buffer| {
        multi_buffer
            .read(cx)
            .buffer_point_to_anchor(&buffer, Point::new(position.row, 0), cx)
    }) else {
        return false;
    };
    editor.update(cx, |editor, cx| {
        let display = editor.display_snapshot(cx);
        let row = f64::from(anchor.to_display_point(&display).row().0);
        editor.set_scroll_position(
            gpui::point(0., (row - position.below_top).max(0.)),
            window,
            cx,
        );
    });
    true
}

/// Where a diff's cursor is: a file, and a point in the head's version of it.
#[derive(Clone, Debug, PartialEq)]
pub struct DiffCursor {
    pub path: String,
    pub point: Point,
}

/// Where `diff`'s cursor is, when it shows in the view.
pub fn cursor_position(diff: &Entity<SplittableEditor>, cx: &mut App) -> Option<DiffCursor> {
    let editor = diff.read(cx).rhs_editor().clone();
    editor.update(cx, |editor, cx| {
        let display = editor.display_snapshot(cx);
        let head = editor.selections.newest::<Point>(&display).head();
        let row = head.to_display_point(&display).row().0;
        let top = editor.scroll_position(cx).y.max(0.);
        // Unknown until the editor is laid out, when it counts as in view.
        let in_view = editor.visible_line_count().is_none_or(|lines| {
            let row = f64::from(row);
            row >= top.floor() && row < top + lines
        });
        if !in_view {
            return None;
        }
        let (path, (point, _)) = file_point(&display, head)?;
        Some(DiffCursor { path, point })
    })
}

/// Puts `diff`'s cursor at `cursor`, or the nearest point of its file the diff shows, without
/// scrolling. Returns whether `diff` has the file.
pub fn put_cursor(
    diff: &Entity<SplittableEditor>,
    cursor: &DiffCursor,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    let editor = diff.read(cx).rhs_editor().clone();
    let multi_buffer = editor.read(cx).buffer().clone();
    let Some(anchor) = buffer_anchor(&multi_buffer, &cursor.path, cursor.point, cx) else {
        return false;
    };
    editor.update(cx, |editor, cx| {
        editor.change_selections(SelectionEffects::no_scroll(), window, cx, |selections| {
            selections.select_anchor_ranges([anchor..anchor])
        });
    });
    true
}

/// The anchor in `multi_buffer` at `point` of the file at `path`.
fn buffer_anchor(
    multi_buffer: &Entity<MultiBuffer>,
    path: &str,
    point: Point,
    cx: &App,
) -> Option<multi_buffer::Anchor> {
    let buffer = multi_buffer.read(cx).all_buffers_iter().find(|buffer| {
        buffer
            .read(cx)
            .file()
            .is_some_and(|file| file.path().as_unix_str() == path)
    })?;
    multi_buffer
        .read(cx)
        .buffer_point_to_anchor(&buffer, point, cx)
}

/// Checks the pull request's head out, trusts that checkout and nothing else, and opens it as a
/// folder of `project`, so language servers start there.
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
    ToCheckout(Box<FilePlan>),
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
        } => Swap::ToCheckout(Box::new(with_checkout)),
        FilePlan {
            warning: Some(warning),
            ..
        } => Swap::Warn(warning),
        _ => Swap::Keep,
    }
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

/// How far copying a pull request locally has got.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CopyProgress {
    /// git is fetching, and said this.
    Fetching(Progress),
    /// Listing the files the pull request changes, which downloads what git needs to count lines.
    Listing,
}

impl CopyProgress {
    /// What the tab's status line says.
    pub(crate) fn text(&self) -> String {
        match self {
            Self::Fetching(progress) => {
                let stage = progress.stage.to_lowercase();
                match (progress.percent(), progress.total) {
                    (Some(percent), Some(total)) => {
                        format!("Copying it locally: {stage}, {percent}% of {total}…")
                    }
                    _ => format!("Copying it locally: {stage}, {}…", progress.done),
                }
            }
            Self::Listing => "Copying it locally: listing the changed files…".to_string(),
        }
    }
}

/// A pull request in the local copy: its clone, and where its whole diff starts and ends. What
/// the diff compares is a [`ShownComparison`] of it.
#[cfg_attr(test, derive(Clone))]
pub struct LocalPullRequest {
    pub(crate) repo: Repo,
    /// The repository's canonical owner and name, GitHub's. The name starts each file's full
    /// path.
    pub(crate) owner: String,
    pub(crate) name: String,
    pub(crate) number: u64,
    /// Where the head branched from the base branch.
    pub(crate) base: Oid,
    /// The head: GitHub's, checked against `refs/pull/<n>/head`.
    pub(crate) head: Oid,
    /// Where the head came from.
    pub(crate) head_source: HeadSource,
}

impl std::fmt::Display for LocalPullRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}#{}", self.owner, self.name, self.number)
    }
}

/// The base GitHub gave with a pull request's saved details.
struct SavedBase {
    branch: String,
    /// The branch's tip then.
    tip: Option<Oid>,
}

/// The whole pull request from its clone alone, or what the clone lacks for it: `head`, if the clone
/// fetched it as one of the pull request's heads, from where it branched off the saved base.
/// That's `base.tip` when the clone has it pinned as one of the pull request's bases; else the
/// tip of `base.branch` as last fetched, if the pull request was ever fetched against that
/// branch; else nothing, since another branch's tip, such as the one before a stack's layer was
/// retargeted, would show other changes than GitHub's. The clone must also have every changed
/// file's contents, since nothing is downloaded. Never makes a clone, writes to one or waits for
/// its lock. Blocks while git runs.
fn copy_offline(
    cache: &RepoCache,
    owner: &str,
    name: &str,
    number: u64,
    head: &Oid,
    base: &SavedBase,
) -> anyhow::Result<Result<(LocalPullRequest, ShownComparison), String>> {
    let Some(repo) = cache.open_offline(owner, name)? else {
        return Ok(Err("doesn't exist".into()));
    };
    if !repo.pull_revisions(number)?.contains(head) {
        return Ok(Err(format!("doesn't have its head {head}")));
    }
    let pinned = match &base.tip {
        Some(tip) if repo.has_base(number, tip)? => Some(tip.clone()),
        _ => None,
    };
    let base_tip = match pinned {
        Some(tip) => tip,
        None => match repo.branch_base(number, &base.branch)? {
            Some(tip) => tip,
            None => {
                return Ok(Err(format!(
                    "was never fetched against its base branch {}",
                    base.branch
                )));
            }
        },
    };
    let Some(merge_base) = repo.merge_base(head, &base_tip)? else {
        return Ok(Err(format!(
            "finds no history its head shares with {base_tip}"
        )));
    };
    if !repo.has_diff_contents(&merge_base, head)? {
        return Ok(Err(format!(
            "lacks some changed files' contents from {merge_base} to {head}"
        )));
    }
    let local = LocalPullRequest {
        repo,
        owner: owner.to_string(),
        name: name.to_string(),
        number,
        base: merge_base,
        head: head.clone(),
        head_source: HeadSource::PullRef,
    };
    // Offline, the .gitattributes files are read only if the clone has them.
    let comparison = ShownComparison::full(&local)?;
    Ok(Ok((local, comparison)))
}

/// Fetches the pull request into its repository's clone, finds where it branched from its base
/// and lists what changed in the whole of it. Makes no checkout. Blocks while git runs.
fn copy(
    cache: &RepoCache,
    link: &PrLink,
    pull_request: &PullRequest,
    token: Option<Token>,
    progress: mpsc::UnboundedSender<CopyProgress>,
) -> anyhow::Result<(LocalPullRequest, ShownComparison)> {
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
            let (link, progress) = (link.clone(), progress.clone());
            move |update| {
                log::debug!(
                    "{link}: {} {}/{}",
                    update.stage,
                    update.done,
                    update.total.map_or("?".into(), |total| total.to_string())
                );
                // The tab may have closed.
                progress.unbounded_send(CopyProgress::Fetching(update)).ok();
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
    progress.unbounded_send(CopyProgress::Listing).ok();
    let local = LocalPullRequest {
        repo,
        owner: owner.to_string(),
        name: name.to_string(),
        number: link.number,
        base,
        head: fetched.head,
        head_source: fetched.head_source,
    };
    let comparison = ShownComparison::full(&local)?;
    Ok((local, comparison))
}

/// The key a file's excerpts sit under in the diff: its position in the tab's order, then its
/// path, so the diff lists files in that order.
pub fn path_key(position: usize, path: Arc<RelPath>) -> PathKey {
    PathKey::with_sort_prefix(position as u64, path)
}

/// The trusted checkout a pull request's project buffers come from.
#[derive(Clone)]
pub(crate) struct Checkout {
    pub(crate) dir: PathBuf,
    pub(crate) worktree_id: WorktreeId,
}

/// What one changed file shows, worked out before anything goes on screen: from git, or from
/// GitHub's patch.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FilePlan {
    pub(crate) content: Content,
    /// Shown in the file's header.
    pub(crate) warning: Option<Warning>,
    /// What the file's header says about the change besides its status.
    pub(crate) badge: Option<SharedString>,
    /// The head's rows that show, when only they're known, as from GitHub's patch: each range
    /// shows as it is, and can't be expanded. `None` shows each change with the lines around it.
    pub(crate) rows: Option<Vec<Range<u32>>>,
    /// GitHub's hunks, which Zed's diff of the two sides must lie inside. When it doesn't, adding
    /// the file fails with [`OutsideGithubsHunks`] and shows nothing.
    pub(crate) hunks: Option<Vec<HunkRows>>,
    /// Why the file starts collapsed, as its header says. A file whose text didn't change starts
    /// collapsed too, which showing it finds.
    pub(crate) collapsed: Option<Collapsed>,
    /// A changed image's before and after, which show under the file's last line.
    pub(crate) images: Option<Images>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Content {
    /// The two sides, compared.
    Diff {
        /// The base's text. `None` when the pull request adds the file.
        old_text: Option<String>,
        new: NewSide,
    },
    /// Text that stands in for a file Lo doesn't compare: a binary file that isn't an image, a
    /// file too large to load at once, or one it couldn't read.
    Placeholder {
        text: String,
        /// Whether the pull request deletes the file.
        deleted: bool,
    },
}

/// Where the right side's text comes from.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum NewSide {
    /// The head's text, read from git.
    Git(String),
    /// The checkout's project buffer, whose bytes hash to the head's blob.
    Checkout,
    /// Nothing: the pull request deletes the file.
    Deleted,
}

/// A note in a file's header: a short label, and the reason in its tooltip.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Warning {
    pub(crate) label: SharedString,
    pub(crate) detail: SharedString,
}

/// The header label of a file Lo couldn't show.
const NOT_SHOWN: &str = "Not shown";

impl FilePlan {
    pub(crate) fn diff(old_text: Option<String>, new: NewSide) -> Self {
        Self {
            content: Content::Diff { old_text, new },
            warning: None,
            badge: None,
            rows: None,
            hunks: None,
            collapsed: None,
            images: None,
        }
    }

    pub(crate) fn placeholder(text: impl Into<String>, deleted: bool) -> Self {
        Self {
            content: Content::Placeholder {
                text: text.into(),
                deleted,
            },
            warning: None,
            badge: None,
            rows: None,
            hunks: None,
            collapsed: None,
            images: None,
        }
    }

    /// Shows only `rows` of the head, as they are.
    pub(crate) fn with_rows(self, rows: Vec<Range<u32>>) -> Self {
        Self {
            rows: Some(rows),
            ..self
        }
    }

    /// Shows the file only if Zed's diff of it lies inside GitHub's `hunks`.
    pub(crate) fn inside(self, hunks: Vec<HunkRows>) -> Self {
        Self {
            hunks: Some(hunks),
            ..self
        }
    }

    /// A placeholder that says why the file can't be shown, in its text and its header.
    pub(crate) fn failed(error: &anyhow::Error) -> Self {
        Self {
            warning: Some(Warning {
                label: NOT_SHOWN.into(),
                detail: format!("Lo couldn't show this file: {error:#}").into(),
            }),
            ..Self::placeholder(format!("(Lo couldn't show this file: {error:#})"), false)
        }
    }
}

/// Works out what `file` shows, given its `linguist-generated` attribute. Reads git, and with a
/// `checkout`, hashes the file there. A file too large to load at once gets a placeholder, unless
/// `load_large` asks for it anyway. Never fails: a file it can't read gets a placeholder that says
/// why. Blocks while git runs.
fn plan_file(
    repo: &Repo,
    file: &ChangedFile,
    generated: Option<bool>,
    checkout: Option<&Path>,
    load_large: bool,
) -> FilePlan {
    let plan = try_plan_file(repo, file, generated, checkout, load_large)
        .unwrap_or_else(|error| FilePlan::failed(&error));
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
    generated: Option<bool>,
    checkout: Option<&Path>,
    load_large: bool,
) -> anyhow::Result<FilePlan> {
    let deleted = file.new.is_none();
    let image = is_image(file);
    // A binary file that isn't an image is never read, so its size doesn't matter.
    let loads = !file.binary || image;
    let collapsed = initial_collapse(&FileEntry::from_local(file, generated).file, loads);
    let collapsed = match collapsed {
        Some(Collapsed::Large { generated, why }) if !load_large => {
            let text = format!("(not loaded: {why})");
            return Ok(FilePlan {
                collapsed: Some(Collapsed::Large { generated, why }),
                ..FilePlan::placeholder(text, deleted)
            });
        }
        // Loaded on request: only being generated is left to say.
        Some(Collapsed::Large { generated, .. }) => generated.map(Collapsed::Generated),
        collapsed => collapsed,
    };
    // Even on request, nothing past the limits is read.
    let too_large = |why: String| {
        Ok(FilePlan {
            collapsed: Some(Collapsed::TooLarge { why: why.clone() }),
            ..FilePlan::placeholder(format!("(too large to show: {why})"), deleted)
        })
    };
    if loads && let Some(why) = too_large_to_show(file.size(), &[]) {
        return too_large(why);
    }

    if file.binary {
        if image && let Some(plan) = image_plan(repo, file)? {
            return Ok(FilePlan { collapsed, ..plan });
        }
        let size = |version: &Option<FileVersion>| version.as_ref().and_then(|v| v.size);
        return Ok(FilePlan {
            collapsed,
            ..FilePlan::placeholder(binary_text(size(&file.old), size(&file.new)), deleted)
        });
    }

    let old_bytes = file
        .old
        .as_ref()
        .map(|old| read_side(repo, old))
        .transpose()
        .context("reading the base's version")?;
    if let Some(why) = too_large_to_show(None, &[old_bytes.as_deref().unwrap_or_default()]) {
        return too_large(why);
    }
    let Some(new) = &file.new else {
        let images = image
            .then(|| images_of(file, old_bytes.as_deref(), None))
            .flatten();
        return Ok(FilePlan {
            collapsed,
            images,
            ..FilePlan::diff(old_bytes.map(text), NewSide::Deleted)
        });
    };
    let new_bytes = read_side(repo, new).context("reading the head's version")?;
    if let Some(why) = too_large_to_show(None, &[&new_bytes]) {
        return too_large(why);
    }
    // An SVG file is text, compared as text, and shows as images too.
    let images = image
        .then(|| images_of(file, old_bytes.as_deref(), Some(&new_bytes)))
        .flatten();
    let old_text = old_bytes.clone().map(text);
    let new_text = text(new_bytes.clone());
    // Both sides' line endings are normalized, and bytes that aren't UTF-8 shown as U+FFFD, so a
    // change to those alone would otherwise look like no change at all.
    let badge = old_bytes
        .as_ref()
        .filter(|old| **old != new_bytes && old_text.as_ref() == Some(&new_text))
        .map(|old| {
            SharedString::from(if normalized(old) == normalized(&new_bytes) {
                "line endings only"
            } else {
                "non-UTF-8 bytes only"
            })
        });
    let from_git = |warning: Option<Warning>| FilePlan {
        warning,
        badge: badge.clone(),
        collapsed: collapsed.clone(),
        images: images.clone(),
        ..FilePlan::diff(old_text.clone(), NewSide::Git(new_text.clone()))
    };
    // A submodule is never a file in the checkout, and an image needs no language server.
    let Some(checkout) = checkout.filter(|_| new.mode != FileMode::Submodule && images.is_none())
    else {
        return Ok(from_git(None));
    };
    Ok(match repo.hash_checkout_file(checkout, &new.path) {
        Ok(Some(id)) if id == new.oid => FilePlan {
            badge: badge.clone(),
            collapsed: collapsed.clone(),
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
                "Lo couldn't hash the checkout's copy of this file ({error:#}), so this shows \
                 the head's text from git, without language servers."
            )
            .into(),
        })),
    })
}

/// Whether `file` would show as an image, by its name: PNG, JPEG, GIF, WebP or SVG.
fn is_image(file: &ChangedFile) -> bool {
    [file.old_path(), file.new_path()]
        .into_iter()
        .flatten()
        .all(|path| {
            let path = String::from_utf8_lossy(path);
            path.rsplit_once('.').is_some_and(|(_, extension)| {
                ["png", "jpg", "jpeg", "gif", "webp", "svg"]
                    .contains(&extension.to_ascii_lowercase().as_str())
            })
        })
}

/// A changed binary image: a line for each side saying its format, dimensions and size, compared
/// so they show as a change, and the images under them. `None` when either side isn't an image
/// after all, such as a Git LFS pointer.
fn image_plan(repo: &Repo, file: &ChangedFile) -> anyhow::Result<Option<FilePlan>> {
    let read = |version: &Option<FileVersion>| {
        version
            .as_ref()
            .map(|version| read_side(repo, version))
            .transpose()
    };
    let old_bytes = read(&file.old).context("reading the base's version")?;
    let new_bytes = read(&file.new).context("reading the head's version")?;
    let Some(images) = images_of(file, old_bytes.as_deref(), new_bytes.as_deref()) else {
        return Ok(None);
    };
    let old_text = images.old.as_ref().map(ImageSide::summary);
    let new = match &images.new {
        Some(image) => NewSide::Git(image.summary()),
        None => NewSide::Deleted,
    };
    Ok(Some(FilePlan {
        images: Some(images),
        ..FilePlan::diff(old_text, new)
    }))
}

/// The two sides of `file` as images, from their bytes. `None` when a side that's there isn't an
/// image.
fn images_of(file: &ChangedFile, old: Option<&[u8]>, new: Option<&[u8]>) -> Option<Images> {
    let side = |version: &Option<FileVersion>, bytes: Option<&[u8]>| match (version, bytes) {
        (Some(version), Some(bytes)) => {
            let path = String::from_utf8_lossy(&version.path);
            ImageSide::new(&path, bytes.to_vec()).map(Some)
        }
        _ => Some(None),
    };
    Some(Images {
        old: side(&file.old, old)?,
        new: side(&file.new, new)?,
    })
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
pub(crate) struct Target<'a> {
    /// The repository's name, which starts each file's full path.
    pub(crate) repo_name: &'a str,
    pub(crate) project: &'a Entity<Project>,
    pub(crate) checkout: Option<&'a Checkout>,
    pub(crate) languages: &'a Arc<LanguageRegistry>,
    pub(crate) editor: &'a Entity<SplittableEditor>,
    /// Whether changes to whitespace alone are hidden.
    pub(crate) hide_whitespace: bool,
}

/// What a file's header says about the pull request's change to it, besides its plan's badge and
/// warning.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FileHeader {
    /// Added, deleted, renamed or modified.
    pub(crate) status: FileStatus,
    /// The path a renamed file had, which the header shows before its new one.
    pub(crate) renamed_from: Option<SharedString>,
}

impl FileHeader {
    pub(crate) fn of(file: &ChangedFile) -> Self {
        let renamed_from = match file.change {
            Change::Renamed { .. } => file
                .old_path()
                .map(|path| String::from_utf8_lossy(path).into_owned().into()),
            _ => None,
        };
        Self {
            status: status(file),
            renamed_from,
        }
    }
}

/// A file added to the diff.
pub(crate) struct Shown {
    pub(crate) buffer_id: BufferId,
    pub(crate) warning: Option<Warning>,
}

impl Target<'_> {
    /// Adds one changed file to the diff, at `position` in the tab's order, or replaces the one at
    /// its path, keeping whether that one was folded. `path` is its path as shown: a name that
    /// isn't UTF-8 shows with U+FFFD, and its badge says so. A file that starts collapsed starts
    /// folded, its header saying why.
    pub(crate) async fn add_file(
        &self,
        position: usize,
        path: &str,
        deleted: bool,
        plan: FilePlan,
        header: FileHeader,
        cx: &mut AsyncWindowContext,
    ) -> anyhow::Result<Shown> {
        let shown_path = path.to_string();
        let path = RelPath::from_unix_str(path)?.into_arc();
        let FilePlan {
            content,
            warning,
            badge,
            rows,
            hunks,
            collapsed,
            images,
        } = plan;
        let (buffer, base, head) = match content {
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
                let snapshot = buffer.read_with(cx, |buffer, _| buffer.snapshot());
                let head = cx
                    .background_spawn(async move { Arc::<str>::from(snapshot.text()) })
                    .await;
                (buffer, Some(old_text), head)
            }
            Content::Diff {
                old_text,
                new: NewSide::Git(text),
            } => {
                // The head's text is kept, not copied out of the buffer again.
                let head: Arc<str> = cx.background_spawn(async move { text.into() }).await;
                let buffer = self.memory_buffer(head.clone(), &path, false, cx).await?;
                (buffer, Some(old_text), head)
            }
            Content::Diff {
                old_text,
                new: NewSide::Deleted,
            } => (
                self.memory_buffer("".into(), &path, true, cx).await?,
                Some(old_text),
                "".into(),
            ),
            Content::Placeholder { text, .. } => (
                self.memory_buffer(text.into(), &path, deleted, cx).await?,
                None,
                "".into(),
            ),
        };

        let snapshot = buffer.read_with(cx, |buffer, _| buffer.snapshot());
        let base: Option<Option<Arc<str>>> = cx
            .background_spawn(async move { base.map(|old| old.map(Arc::from)) })
            .await;
        let mut collapsed = collapsed;
        // What the head is compared with: the base, or the base as `git diff -w` compares it.
        let compared = match &base {
            Some(Some(old)) => {
                let compared: Arc<str> = if self.hide_whitespace && **old != *head {
                    cx.background_spawn({
                        let (old, head) = (old.clone(), head.clone());
                        async move { without_whitespace_changes(&old, &head).into() }
                    })
                    .await
                } else {
                    old.clone()
                };
                if collapsed.is_none() && images.is_none() && !deleted {
                    if **old == *head {
                        collapsed = Some(Collapsed::NoChanges);
                    } else if *compared == *head {
                        collapsed = Some(Collapsed::WhitespaceOnly);
                    }
                }
                Some(Some(compared))
            }
            other => other.clone(),
        };
        let diff = match compared {
            // A placeholder: nothing to compare, so no hunks.
            None => cx.new(|cx| {
                BufferDiff::new_unchanged(
                    &snapshot,
                    snapshot.language().cloned(),
                    Some(self.languages.clone()),
                    cx,
                )
            }),
            Some(compared) => {
                let diff = cx.new(|cx| {
                    BufferDiff::new(
                        &snapshot.text,
                        snapshot.language().cloned(),
                        Some(self.languages.clone()),
                        cx,
                    )
                });
                diff.update(cx, |diff, cx| {
                    diff.set_base_text(compared, snapshot.text.clone(), cx)
                })
                .await;
                diff
            }
        };
        if let Some(hunks) = &hunks {
            diff.read_with(cx, |diff, cx| {
                api_diff::check_hunks(&diff.snapshot(cx), &snapshot, hunks)
            })
            .map_err(OutsideGithubsHunks)?;
        }
        cx.update(|_, cx| follow_language(&buffer, &diff, cx))?;

        let buffer_id = buffer.read_with(cx, |buffer, _| buffer.remote_id());
        let mut file = ShownFile::new(
            path_key(position, path),
            buffer.clone(),
            buffer_id,
            diff.clone(),
            base,
        );
        let hunks_only = rows.is_some();
        file.rows = rows;
        file.set_images(images);
        file.collapsed = collapsed.clone();
        file.notes = badge;
        file.warning = warning.clone();
        file.renamed_from = header.renamed_from;
        file.whitespace_hidden = self.hide_whitespace;
        // A file that replaces another keeps whether it was folded and shown whole, and the
        // cursor, if it was in it.
        let replaced = cx.update(|_, cx| {
            let rhs = self.editor.read(cx).rhs_editor().read(cx);
            shown_files::read(self.editor, cx, |files| {
                let replaced = files.get(&shown_path)?;
                Some((rhs.is_buffer_folded(replaced.buffer_id, cx), replaced.whole))
            })
            .flatten()
        })?;
        if let Some((_, whole)) = replaced {
            file.whole = whole;
        }
        let cursor_in_it = replaced.is_some()
            && cx.update(|_, cx| {
                crate::diff_cursor::file_at_cursor(self.editor, cx)
                    .is_some_and(|(_, path)| path == shown_path)
            })?;
        let (ranges, context) = cx.update(|_, cx| shown_files::initial_ranges(&file, cx))?;
        let key = file.key.clone();
        self.editor.update_in(cx, |editor, window, cx| {
            editor.update_excerpts_for_path(key.clone(), buffer, ranges, context, diff, cx);
            if editor.diff_view_style() == DiffViewStyle::Split {
                editor.split(window, cx);
            }
            // The rows between the ones asked for aren't the file's, so they never show, even
            // on a left side made when the diff splits again.
            if hunks_only {
                editor.disable_expand_excerpt_buttons(cx);
            }
            // Otherwise the cursor would be left in the buffer that's gone.
            if cursor_in_it {
                let rhs = editor.rhs_editor().clone();
                let location = rhs.read(cx).buffer().read(cx).location_for_path(&key, cx);
                if let Some(location) = location {
                    rhs.update(cx, |rhs, cx| {
                        rhs.change_selections(
                            SelectionEffects::no_scroll(),
                            window,
                            cx,
                            |selections| selections.select_ranges([location..location]),
                        )
                    });
                }
            }
        })?;
        self.editor.update(cx, |editor, cx| {
            editor.rhs_editor().update(cx, |editor, _| {
                if let Some(headers) = editor.addon_mut::<FileHeaders>() {
                    headers.statuses.insert(buffer_id, header.status);
                }
            })
        });
        let fold = match replaced {
            Some((folded, _)) => folded,
            None => collapsed.is_some(),
        };
        cx.update(|window, cx| shown_files::add(self.editor, shown_path, file, fold, window, cx))?;
        Ok(Shown { buffer_id, warning })
    }

    /// An in-memory buffer holding `text`: the head's text from git, or a placeholder. Language
    /// servers never see it.
    async fn memory_buffer(
        &self,
        text: Arc<str>,
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
        // A loaded file can be tens of megabytes: its rope is built off the main thread.
        let text = cx.background_spawn(async move { Rope::from(&*text) }).await;
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

/// What a changed file's header says about the change besides its status and a rename, from what
/// git listed: a name that isn't UTF-8, a submodule, a change of type or mode.
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
/// renamed or modified) rather than its status in the checkout, which is always clean, and the
/// badges the diff's decorators give it (see [`crate::diff_decorations`]).
#[derive(Default)]
pub(crate) struct FileHeaders {
    pub(crate) statuses: HashMap<BufferId, FileStatus>,
    /// Each file's badges, in the order they show.
    badges: HashMap<BufferId, Vec<Badge>>,
}

impl FileHeaders {
    /// `buffer`'s file's badges.
    pub(crate) fn badges(&self, buffer: BufferId) -> &[Badge] {
        self.badges.get(&buffer).map_or(&[], Vec::as_slice)
    }

    /// Gives `buffer`'s file `badges`, in place of the ones it had.
    pub(crate) fn set_badges(&mut self, buffer: BufferId, badges: Vec<Badge>) {
        if badges.is_empty() {
            self.badges.remove(&buffer);
        } else {
            self.badges.insert(buffer, badges);
        }
    }

    /// Forgets what `buffer`'s header shows, once another buffer replaced it.
    pub(crate) fn forget(&mut self, buffer: BufferId) {
        self.statuses.remove(&buffer);
        self.badges.remove(&buffer);
    }

    /// The label of `buffer`'s file's first badge in `style`.
    #[cfg(test)]
    pub(crate) fn badge_label(&self, buffer: BufferId, style: BadgeStyle) -> Option<String> {
        (self.badges(buffer).iter())
            .find(|badge| badge.style == style)
            .map(|badge| badge.label.to_string())
    }

    /// Every file's badges.
    #[cfg(test)]
    pub(crate) fn all_badges(&self) -> impl Iterator<Item = &Badge> {
        self.badges.values().flatten()
    }
}

impl Addon for FileHeaders {
    fn to_any(&self) -> &dyn std::any::Any {
        self
    }

    fn to_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
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
        let id = buffer.remote_id();
        let badges = self.badges(id);
        if badges.is_empty() {
            return None;
        }
        Some(
            h_flex()
                .flex_shrink_0()
                .gap_2()
                .children(
                    (badges.iter().enumerate())
                        .map(|(index, badge)| render_badge(badge, (id, index), cx)),
                )
                .into_any_element(),
        )
    }
}

/// `badge`, the `n`th of a buffer's.
fn render_badge(badge: &Badge, (buffer, n): (BufferId, usize), cx: &App) -> AnyElement {
    let id = ElementId::NamedInteger(format!("file-badge-{n}").into(), buffer.to_proto());
    let tooltip = |element: Stateful<Div>| match &badge.detail {
        Some(detail) => element.tooltip(Tooltip::text(detail.clone())),
        None => element,
    };
    let label = || Label::new(badge.label.clone()).size(LabelSize::Small);
    match badge.style {
        BadgeStyle::Outlined => tooltip(
            h_flex()
                .id(id)
                .px_1()
                .rounded_sm()
                .border_1()
                .border_color(cx.theme().colors().border_variant)
                .child(label().color(Color::Muted)),
        )
        .into_any_element(),
        BadgeStyle::Note => match badge.detail {
            None => label().color(Color::Muted).into_any_element(),
            Some(_) => {
                tooltip(h_flex().id(id).child(label().color(Color::Muted))).into_any_element()
            }
        },
        BadgeStyle::Warning => tooltip(
            h_flex()
                .id(id)
                .gap_1()
                .child(
                    Icon::new(IconName::Warning)
                        .size(IconSize::Small)
                        .color(Color::Warning),
                )
                .child(label().color(Color::Warning)),
        )
        .into_any_element(),
        // Right before the header's own path, the new one.
        BadgeStyle::RenamedFrom => h_flex()
            .gap_1()
            .child(
                Label::new(badge.label.clone())
                    .buffer_font(cx)
                    .color(Color::Muted),
            )
            .child(Label::new("→").color(Color::Muted))
            .into_any_element(),
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
pub(crate) mod tests {
    use super::*;

    use repo_cache::{Git, LineCounts};
    use review::files::Generated;
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
            None
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
    fn copying_says_how_far_git_has_got() {
        let fetching = |stage: &str, done, total| {
            CopyProgress::Fetching(Progress {
                stage: stage.into(),
                done,
                total,
            })
            .text()
        };
        assert_eq!(
            fetching("Receiving objects", 555, Some(1234)),
            "Copying it locally: receiving objects, 44% of 1234…"
        );
        assert_eq!(
            fetching("Enumerating objects", 12, None),
            "Copying it locally: enumerating objects, 12…"
        );
        assert_eq!(
            CopyProgress::Listing.text(),
            "Copying it locally: listing the changed files…"
        );
    }

    #[test]
    fn only_a_fetch_that_may_work_later_is_tried_again() {
        let fetch = |failure| {
            anyhow::Error::new(repo_cache::Error::Fetch {
                what: "pull request #1".into(),
                failure,
                message: "fatal: unable to access".into(),
            })
            .context("reading the files")
        };
        use repo_cache::FetchFailure;
        assert_eq!(retry(&fetch(FetchFailure::Network)), Some(Retry::Always));
        assert_eq!(
            retry(&fetch(FetchFailure::RateLimited)),
            Some(Retry::Always)
        );
        assert_eq!(retry(&fetch(FetchFailure::Other)), Some(Retry::Few));
        assert_eq!(retry(&fetch(FetchFailure::Auth)), None);
        assert_eq!(retry(&fetch(FetchFailure::RepoNotFound)), None);
        assert_eq!(
            retry(&anyhow::Error::new(repo_cache::Error::DiskFull)),
            None
        );
        assert_eq!(
            retry(&anyhow::anyhow!(
                "sharkdp/fd#2212 shares no history with its base branch master"
            )),
            None
        );
    }

    #[test]
    fn each_wait_doubles_up_to_five_minutes() {
        let waits: Vec<u64> =
            std::iter::successors(Some(FIRST_RETRY), |wait| Some(next_wait(*wait)))
                .take(9)
                .map(|wait| wait.as_secs())
                .collect();
        assert_eq!(waits, [5, 10, 20, 40, 80, 160, 300, 300, 300]);
    }

    #[test]
    fn a_head_github_no_longer_has_gets_a_banner() {
        let link: PrLink = "https://github.com/sharkdp/fd/pull/2212".parse().unwrap();
        let oid = |c: char| -> Oid { c.to_string().repeat(40).parse().unwrap() };
        let banner = |head_source| head_banner(&link, &oid('a'), &head_source);

        assert_eq!(banner(HeadSource::PullRef), None);
        // GitHub moves refs/pull/<n>/head a moment after a push, and Lo shows the API's head.
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
        let placeholder = FilePlan::placeholder("(binary file changed: 1 B → 2 B)", false);

        assert_eq!(
            swap(&shown_from_git, from_checkout.clone()),
            Swap::ToCheckout(Box::new(from_checkout.clone()))
        );
        assert_eq!(swap(&shown_from_git, differs), Swap::Warn(warning));
        assert_eq!(swap(&shown_from_git, shown_from_git.clone()), Swap::Keep);
        // A placeholder or a deleted file never comes from the checkout.
        assert_eq!(swap(&placeholder, from_checkout.clone()), Swap::Keep);
        let deleted = FilePlan::diff(Some("text".into()), NewSide::Deleted);
        assert_eq!(swap(&deleted, from_checkout), Swap::Keep);
    }

    #[test]
    fn a_change_only_to_bytes_that_arent_utf8_gets_a_badge() {
        let copied = copy(
            &[Entry::File("a.txt", b"caf\xe9\n")],
            &[Entry::File("a.txt", b"caf\xe8\n")],
            false,
        );
        assert_eq!(
            copied.plans(),
            vec![(
                "a.txt".into(),
                FilePlan {
                    badge: Some("non-UTF-8 bytes only".into()),
                    ..FilePlan::diff(Some("caf\u{fffd}\n".into()), from_git("caf\u{fffd}\n"))
                }
            )]
        );
    }

    #[test]
    fn line_endings_normalize_to_newlines() {
        assert_eq!(normalized(b"a\r\nb\rc\n"), b"a\nb\nc\n");
        assert_eq!(normalized(b"a\nb"), b"a\nb");
        assert_eq!(normalized(b"a\r\r\n"), b"a\n\n");
    }

    /// A pull request copied locally from a fixture, and the whole of it, with a checkout when
    /// asked for.
    pub(crate) struct Copied {
        pub(crate) _fixture: fixtures::Fixture,
        pub(crate) _cache: TempDir,
        pub(crate) local: LocalPullRequest,
        pub(crate) comparison: ShownComparison,
        pub(crate) checkout: Option<PathBuf>,
    }

    pub(crate) fn copy(base: &[Entry], head: &[Entry], checkout: bool) -> Copied {
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
        let local = LocalPullRequest {
            repo,
            owner: "owner".into(),
            name: "repo".into(),
            number: 1,
            base,
            head: fetched.head,
            head_source: fetched.head_source,
        };
        let comparison = ShownComparison::full(&local).unwrap();
        let checkout = checkout.then(|| local.repo.checkout(1, &local.head).unwrap());
        Copied {
            _fixture: fixture,
            _cache: cache_dir,
            local,
            comparison,
            checkout,
        }
    }

    impl Copied {
        /// Each file's path, as shown, and plan.
        fn plans(&self) -> Vec<(String, FilePlan)> {
            self.plans_loading(false)
        }

        /// Each file's path and plan, loading large files when `load_large` says.
        fn plans_loading(&self, load_large: bool) -> Vec<(String, FilePlan)> {
            let comparison = &self.comparison;
            comparison
                .files()
                .iter()
                .map(|file| (&file.change, file.generated))
                .map(|(file, generated)| {
                    let plan = plan_file(
                        comparison.repo(),
                        file,
                        generated,
                        self.checkout.as_deref(),
                        load_large,
                    );
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
    fn a_file_with_more_than_5000_changed_lines_isnt_loaded_until_asked() {
        let big: String = (0..5_001).map(|i| format!("line {i}\n")).collect();
        let copied = copy(&[], &[Entry::File("big.txt", big.as_bytes())], false);

        let [(_, plan)] = &copied.plans()[..] else {
            panic!("expected one file");
        };

        let why = "5001 lines added and 0 deleted, more than the 5000 Lo loads at once";
        assert_eq!(
            plan.content,
            Content::Placeholder {
                text: format!("(not loaded: {why})"),
                deleted: false,
            }
        );
        assert_eq!(
            plan.collapsed,
            Some(Collapsed::Large {
                generated: None,
                why: why.into()
            })
        );
        assert_eq!(plan.warning, None);

        let [(_, loaded)] = &copied.plans_loading(true)[..] else {
            panic!("expected one file");
        };
        assert_eq!(
            loaded.content,
            Content::Diff {
                old_text: None,
                new: from_git(&big)
            }
        );
        assert_eq!(loaded.collapsed, None);
    }

    #[test]
    fn a_file_larger_than_a_mebibyte_isnt_loaded_until_asked() {
        // Few lines, so the line count alone wouldn't stop it.
        let minified = format!("{}\n", "x".repeat(999)).repeat(2_100);
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
            "(not loaded: 2.0 MiB, more than the 1 MiB Lo loads at once)"
        );
        assert!(plan.collapsed.as_ref().unwrap().is_large());
        let [(_, loaded)] = &copied.plans_loading(true)[..] else {
            panic!("expected one file");
        };
        assert!(matches!(loaded.content, Content::Diff { .. }));
    }

    #[test]
    fn generated_files_and_lockfiles_start_collapsed_and_load() {
        let lock = "# lock\n";
        let copied = copy(
            &[Entry::File("schema.pb.go", b"package a\n")],
            &[
                Entry::File(
                    ".gitattributes",
                    b"*.pb.go linguist-generated\nyarn.lock -linguist-generated\n",
                ),
                Entry::File("Cargo.lock", lock.as_bytes()),
                Entry::File("schema.pb.go", b"package b\n"),
                Entry::File("yarn.lock", lock.as_bytes()),
            ],
            false,
        );

        let collapsed: Vec<_> = copied
            .plans()
            .into_iter()
            .map(|(path, plan)| {
                assert!(matches!(plan.content, Content::Diff { .. }), "{path}");
                (path, plan.collapsed)
            })
            .collect();
        assert_eq!(
            collapsed,
            [
                (".gitattributes".into(), None),
                (
                    "Cargo.lock".into(),
                    Some(Collapsed::Generated(Generated::Lockfile))
                ),
                (
                    "schema.pb.go".into(),
                    Some(Collapsed::Generated(Generated::Attribute))
                ),
                // `-linguist-generated` keeps a lockfile expanded.
                ("yarn.lock".into(), None),
            ]
        );
    }

    #[test]
    fn a_deleted_file_is_generated_as_the_base_says() {
        let copied = copy(
            &[
                Entry::File(".gitattributes", b"old/** linguist-generated\n"),
                Entry::File("old/gen.js", b"gen()\n"),
                Entry::File("app.js", b"app()\n"),
            ],
            &[Entry::File("app.js", b"app(1)\n")],
            false,
        );
        let generated: Vec<_> = copied
            .comparison
            .files()
            .iter()
            .map(|file| (file.change.display_path().into_owned(), file.generated))
            .collect();
        assert_eq!(
            generated,
            [
                (".gitattributes".to_string(), None),
                ("app.js".to_string(), None),
                ("old/gen.js".to_string(), Some(true)),
            ]
        );
    }

    #[test]
    fn a_large_lockfile_loads_on_request_and_stays_marked_a_lockfile() {
        let big: String = (0..5_001).map(|i| format!("pkg-{i}\n")).collect();
        let copied = copy(&[], &[Entry::File("Cargo.lock", big.as_bytes())], false);

        let [(_, plan)] = &copied.plans()[..] else {
            panic!("expected one file");
        };
        assert_eq!(
            plan.collapsed
                .as_ref()
                .map(|collapsed| collapsed.label("⌥A")),
            Some("lockfile, large: load with ⌥A".into())
        );
        let [(_, loaded)] = &copied.plans_loading(true)[..] else {
            panic!("expected one file");
        };
        assert_eq!(
            loaded.collapsed,
            Some(Collapsed::Generated(Generated::Lockfile))
        );
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
        let mut file = copied.comparison.files()[0].change.clone();
        let missing = "f".repeat(40);
        file.new.as_mut().unwrap().oid = missing.parse().unwrap();

        let plan = plan_file(copied.comparison.repo(), &file, None, None, false);

        let Content::Placeholder { text, .. } = &plan.content else {
            panic!("{plan:?}");
        };
        assert!(
            text.starts_with("(Lo couldn't show this file: reading the head's version"),
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
    fn binary_files_say_they_changed() {
        let copied = copy(
            &[
                Entry::File("data.bin", b"\0\x01base"),
                Entry::File("gone.bin", b"\0\x01gone"),
            ],
            &[
                Entry::File("data.bin", b"\0\x01head!"),
                Entry::File("new.bin", b"\0"),
            ],
            false,
        );
        assert_eq!(
            copied.plans(),
            vec![
                (
                    "data.bin".into(),
                    FilePlan::placeholder("(binary file changed: 6 B → 7 B)", false)
                ),
                (
                    "gone.bin".into(),
                    FilePlan::placeholder("(binary file deleted: 6 B)", true)
                ),
                (
                    "new.bin".into(),
                    FilePlan::placeholder("(binary file added: 1 B)", false)
                ),
            ]
        );
    }

    #[test]
    fn images_show_before_and_after_under_a_line_for_each() {
        let png = crate::file_display::tests::png();
        let summary = |bytes: &[u8]| format!("PNG image, 3 × 2, {} B", bytes.len());
        let only_plan = |base: &[Entry], head: &[Entry]| {
            let copied = copy(base, head, false);
            let [(_, plan)] = &copied.plans()[..] else {
                panic!("expected one file");
            };
            plan.clone()
        };

        let changed = [png.as_slice(), b"\0"].concat();
        let modified = only_plan(
            &[Entry::File("logo.png", &png)],
            &[Entry::File("logo.png", &changed)],
        );
        assert_eq!(
            modified.content,
            Content::Diff {
                old_text: Some(summary(&png)),
                new: from_git(&summary(&changed)),
            }
        );
        let images = modified.images.unwrap();
        assert_eq!(images.old, ImageSide::new("a.png", png.clone()));
        assert_eq!(images.new.unwrap().dimensions, Some((3, 2)));

        let added = only_plan(&[], &[Entry::File("new.png", &png)]);
        assert_eq!(
            added.content,
            Content::Diff {
                old_text: None,
                new: from_git(&summary(&png))
            }
        );
        assert_eq!(added.images.unwrap().old, None);

        let deleted = only_plan(&[Entry::File("gone.png", &png)], &[]);
        assert_eq!(
            deleted.content,
            Content::Diff {
                old_text: Some(summary(&png)),
                new: NewSide::Deleted
            }
        );
        assert_eq!(deleted.images.unwrap().new, None);

        // A text file named like an image, such as a Git LFS pointer, is compared as text.
        let pointer = "version https://git-lfs.github.com/spec/v1\n";
        let lfs = only_plan(
            &[Entry::File("lfs.png", pointer.as_bytes())],
            &[Entry::File(
                "lfs.png",
                format!("{pointer}size 1\n").as_bytes(),
            )],
        );
        assert!(matches!(
            lfs.content,
            Content::Diff {
                new: NewSide::Git(_),
                ..
            }
        ));
        assert_eq!(lfs.images, None);
    }

    #[test]
    fn an_svg_file_shows_its_text_and_its_images() {
        let svg = |fill: &str| {
            format!(
                "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"4\" height=\"4\">\n\
                 <rect width=\"4\" height=\"4\" fill=\"{fill}\"/>\n</svg>\n"
            )
        };
        let (old, new) = (svg("red"), svg("blue"));
        for checkout in [false, true] {
            let copied = copy(
                &[Entry::File("icon.svg", old.as_bytes())],
                &[Entry::File("icon.svg", new.as_bytes())],
                checkout,
            );
            let [(_, plan)] = &copied.plans()[..] else {
                panic!("expected one file");
            };
            // From git even with a checkout: an image needs no language server.
            assert_eq!(
                plan.content,
                Content::Diff {
                    old_text: Some(old.clone()),
                    new: from_git(&new)
                }
            );
            let images = plan.images.as_ref().unwrap();
            assert_eq!(images.old.as_ref().unwrap().format, gpui::ImageFormat::Svg);
            assert_eq!(
                images.new.as_ref().unwrap().bytes.as_deref(),
                Some(new.as_bytes())
            );
        }
    }

    #[test]
    fn a_rename_shows_its_old_path_in_the_header() {
        let source: String = (0..20).map(|i| format!("pub fn f{i}() {{}}\n")).collect();
        let edited = format!("{source}pub fn added() {{}}\n");
        let copied = copy(
            &[
                Entry::File("old/a.rs", source.as_bytes()),
                Entry::File("old/b.rs", source.as_bytes()),
            ],
            &[
                Entry::File("new/a.rs", edited.as_bytes()),
                Entry::File("new/b.rs", source.replacen("f1()", "g1()", 1).as_bytes()),
            ],
            false,
        );

        let headers: Vec<_> = (copied.comparison.files().iter())
            .map(|file| FileHeader::of(&file.change))
            .collect();
        assert_eq!(
            headers
                .iter()
                .map(|header| header.renamed_from.as_deref())
                .collect::<Vec<_>>(),
            [Some("old/a.rs"), Some("old/b.rs")]
        );
        assert!(
            headers
                .iter()
                .all(|header| code(header.status) == StatusCode::Renamed)
        );
        // With its changes, and no badge repeating the old path.
        let plans = copied.plans();
        assert_eq!(
            plans[0].1,
            FilePlan::diff(Some(source.clone()), from_git(&edited))
        );
        assert_eq!(plans[1].1.badge, None);

        // Without a rename, no old path.
        let modified = copy(
            &[Entry::File("a.rs", b"a\n")],
            &[Entry::File("a.rs", b"b\n")],
            false,
        );
        assert_eq!(
            FileHeader::of(&modified.comparison.files()[0].change).renamed_from,
            None
        );
    }

    /// Pull requests in real repositories, served over `file://` as GitHub serves them.
    pub(crate) mod fixtures {
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

            fn git_dir(&self) -> std::path::PathBuf {
                self.dir.path().join("owner/repo.git")
            }

            /// Writes a commit holding exactly `entries`, on top of `parent`, and returns its ID.
            pub fn commit(&self, entries: &[Entry], parent: Option<&str>) -> String {
                commit(
                    &self.git_dir(),
                    &self.dir.path().join("index"),
                    entries,
                    parent,
                )
            }

            /// Points `reference`, such as `refs/heads/main`, at `commit`.
            pub fn set_ref(&self, reference: &str, commit: &str) {
                run(
                    &self.git_dir(),
                    &["update-ref", reference, commit],
                    None,
                    None,
                );
            }
        }

        /// A bare repository `owner/repo` with no commits.
        pub fn empty() -> Fixture {
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
            Fixture { dir }
        }

        /// A pull request from a base commit holding `base` to a head commit holding `head`.
        pub fn pull_request(base: &[Entry], head: &[Entry]) -> Fixture {
            let fixture = empty();
            let base = fixture.commit(base, None);
            let head = fixture.commit(head, Some(&base));
            fixture.set_ref("refs/heads/main", &base);
            fixture.set_ref("refs/pull/1/head", &head);
            fixture
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
                .env("GIT_AUTHOR_NAME", "Lo test")
                .env("GIT_AUTHOR_EMAIL", "test@lo.invalid")
                .env("GIT_COMMITTER_NAME", "Lo test")
                .env("GIT_COMMITTER_EMAIL", "test@lo.invalid")
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
