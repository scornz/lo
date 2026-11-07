//! The tab's keys: the contexts they work in, and each part's own (design §10 "Keys"). Each part
//! binds its keys in its `bind_keys` and runs their actions in its `on_actions`.

use gpui::{App, Context, Div};

use super::{PrTab, left, middle, right, viewed};

pub(super) const KEY_CONTEXT: &str = "PrTab";
pub(super) const FILE_LIST_CONTEXT: &str = "PrFileList";
/// The diff's editors, inside the tab, where `tab` would otherwise indent: full editors of many
/// files, not an editor that's typed in, such as a comment's, which edits one buffer.
pub(super) const DIFF_CONTEXT: &str = "PrTab > Editor && mode == full && multibuffer";
/// The tab outside any text box in it, such as a comment box: keys that type a character work
/// here, so a text box still types them. The diff's own editors are the only full-size editors of
/// many files.
pub(super) const READING_CONTEXT: &str = "PrTab && !(Editor && (mode != full || !multibuffer))";

/// The tab's keys. Panels toggle with Zed's keys for its docks.
pub(crate) fn bind_keys(cx: &mut App) {
    middle::bind_keys(cx);
    left::bind_keys(cx);
    right::bind_keys(cx);
    viewed::bind_keys(cx);
}

/// The tab runs each part's actions, wherever in it their keys work.
pub(super) fn on_actions(tab: Div, cx: &Context<PrTab>) -> Div {
    let tab = left::on_actions(tab, cx);
    let tab = middle::on_actions(tab, cx);
    let tab = right::on_actions(tab, cx);
    viewed::on_actions(tab, cx)
}
