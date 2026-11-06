//! The window's title: the active pull request's, or "rozed" when none is open (design §10
//! "Window").
//!
//! Zed's workspace titles the window from its project's folders, which say "empty project" in
//! rozed. rozed forces Zed's title to read "rozed" (the app's forced settings), so the workspace
//! never sets it again, and sets it here whenever the active tab or its title changes.

use gpui::{App, Window};
use workspace::Workspace;

use crate::pr_tab::PrTab;

/// The title with no pull request open.
pub const NO_PULL_REQUEST: &str = "rozed";

/// The title for a pull request: `owner/repo#n — title`, or `owner/repo#n` until its title is
/// known.
pub fn for_pull_request(repo: &str, number: u64, title: Option<&str>) -> String {
    match title {
        Some(title) => format!("{repo}#{number} — {title}"),
        None => format!("{repo}#{number}"),
    }
}

/// Titles `window` after the active tab of `workspace`.
pub(crate) fn update(workspace: &Workspace, window: &mut Window, cx: &App) {
    let title = workspace
        .active_item_as::<PrTab>(cx)
        .map(|tab| tab.read(cx).window_title())
        .unwrap_or_else(|| NO_PULL_REQUEST.to_string());
    window.set_window_title(&title);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pull_request_is_named_by_its_repository_number_and_title() {
        assert_eq!(
            for_pull_request(
                "sharkdp/fd",
                2212,
                Some("Honor explicit use-jemalloc requests")
            ),
            "sharkdp/fd#2212 — Honor explicit use-jemalloc requests"
        );
        assert_eq!(
            for_pull_request("rozed-dev/playground", 2, None),
            "rozed-dev/playground#2"
        );
    }
}
