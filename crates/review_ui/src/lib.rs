//! The PR tab: a pull request's header, its files and its diff, in one tab of rozed's window
//! (design §5, §10 "Window").

pub mod account;
pub mod file_list;
pub mod github_api;
pub mod github_layer;
pub mod local_copies;
pub mod local_project;
pub mod pr_diff;
pub mod pr_link;
pub mod pr_tab;
pub mod tabs;
pub mod trust;

use github::accounts::Accounts;
use github::db::RozedDb;
use gpui::{App, Entity, Task};
use workspace::Workspace;

pub use pr_diff::Trust;
pub use pr_link::PrLink;
pub use pr_tab::{PrTab, open};

/// Sets up the PR tab: its keys, the GitHub layer from `accounts` (the loaded accounts and their
/// database, or `None` without one), and following which tab is active in every workspace.
pub fn init(accounts: Task<Option<(Entity<Accounts>, RozedDb)>>, cx: &mut App) {
    github_layer::init(accounts, cx);
    init_workspaces(cx);
}

/// [`init`] without the GitHub layer, for tests: pull requests open without signing in.
pub fn init_workspaces(cx: &mut App) {
    pr_tab::bind_keys(cx);
    cx.observe_new(|_: &mut Workspace, window, cx| {
        let Some(window) = window else {
            return;
        };
        let this = cx.entity();
        cx.subscribe_in(&this, window, |workspace, _, event, _, cx| {
            if let workspace::Event::ActiveItemChanged = event {
                pr_tab::sync_active(workspace, cx);
            }
        })
        .detach();
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
