//! Marking files viewed with `e` (design §6 "Viewed marks"): GitHub's write, this Mac's record of
//! where each file was viewed, and the toasts that say why a mark couldn't be set or cleared.

use std::time::SystemTime;

use github::viewed::ViewedRecords;
use github::{GithubClient, GithubError, PullRef};
use gpui::{
    App, AsyncApp, Context, Div, ElementId, EntityId, InteractiveElement as _, WeakEntity, Window,
    actions, prelude::FluentBuilder as _,
};
use review::revisions::ViewedState;
use workspace::notifications::NotificationId;
use workspace::{Toast, Workspace};

use super::keys::reading;
use super::{OlderCopy, PrTab, reopen};
use crate::viewed_marks::{Toggle, write_failed};

actions!(
    review_ui,
    [
        /// Marks the current file viewed and moves to the next unviewed file, or clears the mark
        /// of a viewed file.
        ToggleViewed,
    ]
);

/// The key that marks files viewed.
pub(super) fn bind_keys(cx: &mut App) {
    cx.bind_keys(reading("e", ToggleViewed));
}

/// The tab runs the viewed mark's action wherever its key works, while the diff, whose current
/// file it marks, shows.
pub(super) fn on_actions(root: Div, tab: &PrTab, cx: &Context<PrTab>) -> Div {
    root.when(tab.middle.shows_diff(), |root| {
        root.on_action(cx.listener(PrTab::toggle_viewed))
    })
}

impl PrTab {
    /// `e`: marks the current file viewed and moves to the next unviewed file, or clears a viewed
    /// file's mark. The new mark shows at once, and goes back to GitHub's if the write fails.
    fn toggle_viewed(&mut self, _: &ToggleViewed, window: &mut Window, cx: &mut Context<Self>) {
        let Some((_, path)) = self.shown_file(cx) else {
            self.toast(
                "The diff is still loading, so there's no file to mark viewed yet.".into(),
                cx,
            );
            return;
        };
        // GitHub's view didn't load, so neither would a write: the local copy shows, read-only.
        // One older than GitHub's newest head gets the newer push's refusal below.
        let why = match self.showing_copy {
            Some(OlderCopy::Unreachable) => Some(format!(
                "Lo can't reach GitHub, so it can't mark {path} viewed until it's back"
            )),
            Some(OlderCopy::NotLoaded) => Some(format!(
                "GitHub's view of this pull request didn't load, so Lo can't mark {path} \
                 viewed until it does"
            )),
            Some(OlderCopy::NewerNotCopied) | None => None,
        };
        if let Some(why) = why {
            self.toast(
                format!("{why}. Until then, this is the local copy, read-only."),
                cx,
            );
            return;
        }
        let Some((poller, pull)) = self.watched.clone() else {
            self.toast(
                "Sign in to mark files viewed: this pull request opened without an account.".into(),
                cx,
            );
            return;
        };
        // Every write would fail; marking ahead of GitHub would move past files whose marks then
        // go back.
        if self.poll.status.offline.is_some() {
            self.toast(
                format!("Lo can't reach GitHub, so it can't mark {path} viewed until it's back."),
                cx,
            );
            return;
        }
        if self.poll.status.signed_out {
            let login = poller.read(cx).login().to_string();
            self.toast(
                format!(
                    "{login} is signed out, so Lo can't mark {path} viewed. Sign in again \
                     from Accounts… in the Lo menu."
                ),
                cx,
            );
            return;
        }
        let Some(node_id) = self.node_id.clone() else {
            self.toast(
                format!(
                    "Couldn't mark {path} viewed: GitHub hasn't sent this pull request's details \
                     yet. Try again in a moment."
                ),
                cx,
            );
            return;
        };
        let Some(github_path) = self.github_path(&path) else {
            self.file_toast(
                &path,
                format!(
                    "GitHub doesn't list {path} in this pull request, so Lo can't mark it \
                     viewed. GitHub and the local copy see this file's change differently."
                ),
                cx,
            );
            return;
        };
        // GitHub marks a file as of the pull request's latest head (`markFileAsViewed` takes no
        // commit), so a file is marked only while that's the head on screen. A push inside the
        // poll interval can still slip through.
        let marks = self.marks.state(&github_path) != Some(ViewedState::Viewed);
        if marks && self.newer_head().is_some() {
            self.newer_push_toast(&path, cx);
            return;
        }
        let toggle = self.marks.toggle(&github_path);
        self.files.set_viewed(&self.marks.shown());
        if toggle.mark {
            // Only to a file the diff shows; the others are still loading.
            let unviewed: Vec<String> = self
                .files
                .unviewed_after(&path)
                .map(|entry| entry.path().to_string())
                .collect();
            match unviewed
                .iter()
                .find(|next| self.location_in_diff(next, cx).is_some())
            {
                Some(next) => {
                    self.files.select_path(next);
                    self.selection_moved(window, cx);
                }
                None => {
                    if let Some(next) = unviewed.first() {
                        self.toast(
                            format!(
                                "{next}, the next unviewed file, is still loading, so the diff \
                                 stays on {path}."
                            ),
                            cx,
                        );
                    }
                }
            }
        }
        let (mark, github_path) = (toggle.mark, toggle.path.clone());
        let written = ViewedWrite {
            record: match (toggle.mark, &self.head) {
                (true, Some(head)) => RecordChange::Record(head.clone()),
                (true, None) => RecordChange::Keep,
                (false, _) => RecordChange::Forget,
            },
            toggle,
            pressed_at: SystemTime::now(),
            records: self.records.clone(),
            login: poller.read(cx).login().to_string(),
            pull: pull.clone(),
            workspace: self.workspace.clone(),
            toast: toast_id(cx.entity_id(), Some(&path)),
        };
        let write = poller.update(cx, |poller, cx| {
            poller.write(
                pull,
                move |client: GithubClient| async move {
                    if mark {
                        client.mark_file_as_viewed(&node_id, &github_path).await
                    } else {
                        client.unmark_file_as_viewed(&node_id, &github_path).await
                    }
                },
                cx,
            )
        });
        cx.spawn(async move |this, cx| {
            let result = write.await;
            this.update(cx, |this, cx| {
                this.viewed_written(&written, result.is_ok(), cx)
            })
            .ok();
            // The tab may have closed, which the write, this Mac's record and the toast outlive.
            written.finish(result, cx).await;
        })
        .detach();
        cx.notify();
    }

    /// A toggle's write finished: the file shows the new mark, or GitHub's again if the write
    /// failed, and on success the tab knows where this Mac records the file was viewed.
    fn viewed_written(&mut self, written: &ViewedWrite, succeeded: bool, cx: &mut Context<Self>) {
        self.marks
            .finish(&written.toggle, succeeded, SystemTime::now());
        self.files.set_viewed(&self.marks.shown());
        if succeeded {
            let path = written.toggle.path.clone();
            match &written.record {
                RecordChange::Record(head) => {
                    self.viewed_at.insert(path, head.clone());
                }
                RecordChange::Forget => {
                    self.viewed_at.remove(&path);
                }
                RecordChange::Keep => {}
            }
        }
        cx.notify();
    }

    /// Shows a toast about the tab, which replaces the tab's last one.
    fn toast(&self, message: String, cx: &mut Context<Self>) {
        let id = toast_id(cx.entity_id(), None);
        show_toast(&self.workspace, id, message, None, cx);
    }

    /// Shows a toast about `path`, which replaces only the last one about it.
    fn file_toast(&self, path: &str, message: String, cx: &mut Context<Self>) {
        let id = toast_id(cx.entity_id(), Some(path));
        show_toast(&self.workspace, id, message, None, cx);
    }

    /// Says `e` didn't mark `path` because GitHub has a newer push, and offers to load it.
    fn newer_push_toast(&self, path: &str, cx: &mut Context<Self>) {
        let (workspace, tab) = (self.workspace.clone(), cx.entity().downgrade());
        let load = move |window: &mut Window, cx: &mut App| {
            let (Some(workspace), Some(tab)) = (workspace.upgrade(), tab.upgrade()) else {
                return;
            };
            workspace.update(cx, |workspace, cx| reopen(workspace, &tab, window, cx));
        };
        show_toast(
            &self.workspace,
            toast_id(cx.entity_id(), None),
            format!(
                "There's a newer push to this pull request than the one on screen, so Lo \
                 didn't mark {path} viewed: GitHub would mark it as of that push. Load it to mark \
                 files viewed."
            ),
            Some(("Load the newer push", Box::new(load))),
            cx,
        );
    }
}

/// The toast that says why a viewed mark couldn't be set or cleared.
struct ViewedMarkToast;

/// The ID of a viewed-mark toast from the tab `tab`, about the file at `path` or about the tab.
/// Each file and each tab has its own, so one toast doesn't replace another's.
fn toast_id(tab: EntityId, path: Option<&str>) -> NotificationId {
    let id = match path {
        Some(path) => format!("{}:{path}", tab.as_u64()),
        None => tab.as_u64().to_string(),
    };
    NotificationId::composite::<ViewedMarkToast>(ElementId::Name(id.into()))
}

/// A toggle's write to GitHub, with what's needed once it finishes, whether or not its tab is
/// still open.
struct ViewedWrite {
    toggle: Toggle,
    /// What this Mac records about the file once GitHub took the write.
    record: RecordChange,
    /// When `e` was pressed, which the record keeps.
    pressed_at: SystemTime,
    records: Option<ViewedRecords>,
    login: String,
    pull: PullRef,
    workspace: WeakEntity<Workspace>,
    /// The toast that says why the write failed, the file's own.
    toast: NotificationId,
}

/// How a successful write changes this Mac's record of where a file was viewed.
enum RecordChange {
    /// Marked viewed with this head on screen.
    Record(String),
    /// Cleared.
    Forget,
    /// Marked, but the head on screen isn't known.
    Keep,
}

impl ViewedWrite {
    /// The write finished: on success this Mac records or forgets where the file was viewed; on
    /// failure a toast says why.
    async fn finish(self, result: Result<(), GithubError>, cx: &mut AsyncApp) {
        let Self {
            toggle,
            record,
            pressed_at,
            records,
            login,
            pull,
            workspace,
            toast,
        } = self;
        if let Err(error) = result {
            log::warn!("{pull}: {}: {error}", toggle.path);
            let message = write_failed(&toggle, &login, &error);
            cx.update(|cx| show_toast(&workspace, toast, message, None, cx));
            return;
        }
        let Some(records) = records else {
            return;
        };
        let path = &toggle.path;
        let written = match record {
            RecordChange::Record(head) => {
                records.record(&login, &pull, path, &head, pressed_at).await
            }
            RecordChange::Forget => records.forget(&login, &pull, path).await,
            RecordChange::Keep => Ok(()),
        };
        if let Err(error) = written {
            log::error!("{pull}: recording where {path} was viewed: {error:#}");
        }
    }
}

/// A toast's button: its label and what it does.
type ToastButton = (&'static str, Box<dyn Fn(&mut Window, &mut App)>);

/// Shows a toast in `workspace` about a viewed mark, with `button` if there's one.
fn show_toast(
    workspace: &WeakEntity<Workspace>,
    id: NotificationId,
    message: String,
    button: Option<ToastButton>,
    cx: &mut App,
) {
    #[cfg(test)]
    cx.default_global::<ToastLog>().0.push((
        id.clone(),
        message.clone(),
        button.as_ref().map(|(label, _)| label.to_string()),
    ));
    let mut toast = Toast::new(id, message);
    if let Some((label, on_click)) = button {
        toast = toast.on_click(label, on_click);
    }
    workspace
        .update(cx, |workspace, cx| workspace.show_toast(toast, cx))
        .ok();
}

/// The toasts the tabs showed, for tests: each one's ID, text and button.
#[cfg(test)]
#[derive(Default)]
pub(super) struct ToastLog(pub(super) Vec<(NotificationId, String, Option<String>)>);

#[cfg(test)]
impl gpui::Global for ToastLog {}
