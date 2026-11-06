//! The window's title: the active pull request's, or "rozed" when none is open (design §10
//! "Window"). With an empty pane active, as after a split, it's the pull request last active.
//!
//! Zed's workspace titles the window from its project's folders, which say "empty project" in
//! rozed. rozed forces Zed's title to read "rozed" (the app's forced settings), so the workspace
//! never sets it again, and sets it here whenever the active tab or a pull request's title
//! changes.

use gpui::{App, Entity, SharedString, SystemWindowTabController, Window};
use workspace::Workspace;

use crate::pr_tab::PrTab;

/// The title with no pull request open.
pub const NO_PULL_REQUEST: &str = "rozed";

/// The title for a pull request: `owner/repo#n — title`, or `owner/repo#n` until its title is
/// known.
pub fn for_pull_request(repo: &str, number: u64, title: Option<&str>) -> String {
    match title {
        Some(title) => format!("{repo}#{number} — {}", without_bidi(title)),
        None => format!("{repo}#{number}"),
    }
}

/// `text` without the characters that turn the direction of the text after them around until
/// they're closed (embeddings, overrides and isolates), which a pull request's author could use
/// to make its title read as something else. The marks right-to-left text places numbers and
/// punctuation with (LRM, RLM, ALM) stay: they change no more than their neighbours.
pub fn without_bidi(text: &str) -> String {
    text.chars()
        .filter(|c| !matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'))
        .collect()
}

/// Titles `window` after the active tab of `workspace`, or with an empty pane active, after the
/// pull request last active.
pub(crate) fn update(workspace: &Workspace, window: &mut Window, cx: &mut App) {
    let title = shown_pull_request(workspace, cx)
        .map(|tab| tab.read(cx).window_title())
        .unwrap_or_else(|| NO_PULL_REQUEST.to_string());
    window.set_window_title(&title);
    // A native window tab, where the system shows them, has the same title.
    SystemWindowTabController::update_tab_title(
        cx,
        window.window_handle().window_id(),
        SharedString::from(title),
    );
}

/// The pull request the window is about: the active tab's, or with an empty pane active, the one
/// whose tab was active last.
pub(crate) fn shown_pull_request(workspace: &Workspace, cx: &App) -> Option<Entity<PrTab>> {
    let pane_is_empty = workspace.active_pane().read(cx).active_item().is_none();
    match workspace.active_item_as::<PrTab>(cx) {
        Some(tab) => Some(tab),
        None if pane_is_empty => last_active_pull_request(workspace, cx),
        None => None,
    }
}

/// The pull request whose tab was active last, in any pane. (Zed's `recent_active_item_by_type`
/// misses the first item ever activated, whose activation is numbered 0.)
fn last_active_pull_request(workspace: &Workspace, cx: &App) -> Option<Entity<PrTab>> {
    workspace
        .panes()
        .iter()
        .flat_map(|pane| {
            let pane = pane.read(cx);
            pane.activation_history()
                .iter()
                .filter_map(|entry| {
                    let item = pane
                        .items()
                        .find(|item| item.item_id() == entry.entity_id)?;
                    Some((entry.timestamp, item.downcast::<PrTab>()?))
                })
                .collect::<Vec<_>>()
        })
        .max_by_key(|(activated, _)| *activated)
        .map(|(_, tab)| tab)
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

    #[test]
    fn titles_lose_the_characters_that_turn_text_around() {
        // U+202E turns what follows right to left, so "Fix gpj.exe" would read "Fix exe.jpg".
        assert_eq!(
            for_pull_request(
                "a/b",
                1,
                Some("Fix \u{202E}gpj.exe\u{202C} \u{2067}x\u{2069}")
            ),
            "a/b#1 — Fix gpj.exe x"
        );
        // The marks that place numbers in right-to-left text stay.
        assert_eq!(
            without_bidi("שלום \u{200F}1\u{061C}"),
            "שלום \u{200F}1\u{061C}"
        );
    }
}
