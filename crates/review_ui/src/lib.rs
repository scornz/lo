//! The PR tab: a pull request's header, its files and its diff, in one tab of Lo's window
//! (design §5, §10 "Window").

pub mod account;
pub mod account_picker;
pub mod api_diff;
pub mod diff_cursor;
mod file_display;
pub mod file_list;
pub mod github_api;
pub mod github_layer;
mod image_preview;
mod keymap;
pub mod local_copies;
pub mod local_project;
pub mod open_box;
pub mod poll_state;
pub mod pr_banner;
pub mod pr_diff;
pub mod pr_link;
pub mod pr_tab;
mod preview_pool;
mod read_only;
pub mod recent_prs;
mod shown_files;
pub mod tabs;
#[cfg(test)]
mod test_support;
pub mod trust;
pub mod viewed_marks;
pub mod window_title;

use github::accounts::Accounts;
use github::db::LoDb;
use gpui::{App, Context, Entity, Task, Window};
use workspace::Workspace;

pub use image_preview::{HELPER_FLAG as PREVIEW_HELPER_FLAG, serve_helper as serve_preview_helper};
pub use open_box::{OpenBox, OpenPullRequest};
pub use pr_diff::Trust;
pub use pr_link::PrLink;
pub use pr_tab::{PrTab, PrTabEvent, open};

/// Sets up the PR tab: Zed's keys and its own, `cmd-o`, the GitHub layer from `accounts` (the loaded accounts and
/// their database, or `None` without one), and Lo's changes to every workspace.
pub fn init(accounts: Task<Option<(Entity<Accounts>, LoDb)>>, cx: &mut App) {
    github_layer::init(accounts, cx);
    init_workspaces(cx);
}

/// [`init`] without the GitHub layer, for tests: pull requests open without signing in. Call it
/// after the command palette is set up.
pub fn init_workspaces(cx: &mut App) {
    // Of two bindings for a key in the same place, the later one wins: Lo's.
    keymap::load(cx);
    bind_keys(cx);
    read_only::hide_from_palette(cx);
    cx.observe_new(|workspace: &mut Workspace, window, cx| {
        let Some(window) = window else {
            return;
        };
        read_only::configure_workspace(workspace, cx);
        // After the workspace titles the window for the first time.
        cx.defer_in(window, |workspace, window, cx| {
            window_title::update(workspace, window, cx)
        });
        workspace.register_action(|workspace, _: &OpenPullRequest, window, cx| {
            OpenBox::toggle(workspace, window, cx)
        });
        let this = cx.entity();
        cx.subscribe_in(&this, window, workspace_event).detach();
        // In the background, every account's polling slows down.
        cx.observe_window_activation(window, |_, window, cx| {
            let foreground = window.is_window_active();
            if let Some(layer) = github_layer::loaded(cx) {
                layer.update(cx, |layer, cx| layer.set_foreground(foreground, cx));
            }
        })
        .detach();
    })
    .detach();
}

/// Lo's own keys, which go over Zed's. Each module binds its own, and CLAUDE.md's "Keys" table lists
/// them.
fn bind_keys(cx: &mut App) {
    pr_tab::bind_keys(cx);
    account_picker::bind_keys(cx);
    recent_prs::bind_keys(cx);
    open_box::bind_keys(cx);
}

/// Keeps the window's tabs, panes and title as Lo has them when the workspace changes.
fn workspace_event(
    workspace: &mut Workspace,
    _: &Entity<Workspace>,
    event: &workspace::Event,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    match event {
        // Also when the active tab's title changes, which the workspace reports so.
        workspace::Event::ActiveItemChanged => {
            pr_tab::sync_active(workspace, cx);
            window_title::update(workspace, window, cx);
            // A tab's question about its account waits until it's the active tab.
            account_picker::active_item_changed(cx);
        }
        workspace::Event::PaneAdded(pane) => read_only::configure_pane(pane, cx),
        workspace::Event::ItemAdded { item } => {
            read_only::item_added(item.as_ref(), cx);
            if item.downcast::<PrTab>().is_some() {
                recent_prs::pull_request_opened(workspace, window, cx);
            }
        }
        // With no pull request left, the window lists the recent ones.
        workspace::Event::ItemRemoved { .. } => recent_prs::tab_closed(workspace, window, cx),
        _ => {}
    }
}
