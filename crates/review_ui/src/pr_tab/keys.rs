//! The tab's keys: the contexts they work in, and each part's own (design §10 "Keys"). Each part
//! binds its keys in its `bind_keys` and runs their actions in its `on_actions`, and adds both to
//! the lists here, one line each, in alphabetical order.
//!
//! The tab's own context says what its middle shows, `middle=diff` or `middle=view`
//! ([`tab_context`]). Keys that act on the diff hold only while it shows, so they don't reach a
//! hidden diff, and a view in its place can bind the same keys.

use gpui::{Action, App, Context, Div, KeyBinding, KeyContext};

use super::{Middle, PrTab, left, middle, right, viewed};

pub(crate) const KEY_CONTEXT: &str = "PrTab";
pub(crate) const FILE_LIST_CONTEXT: &str = "PrFileList";
/// The diff's editors, inside the tab while it shows the diff, where `tab` would otherwise indent:
/// full editors of many files, not an editor that's typed in, such as a comment's, which edits one
/// buffer.
pub(crate) const DIFF_CONTEXT: &str =
    "PrTab && middle == diff > Editor && mode == full && multibuffer";
/// The file list while the tab shows the diff, for keys that act on the diff from the list.
pub(crate) const DIFF_FILE_LIST_CONTEXT: &str = "PrTab && middle == diff > PrFileList";
/// The tab outside any text box in it, such as a comment box: keys that type a character work
/// here, so a text box still types them. The diff's own editors are the only full-size editors of
/// many files.
pub(crate) const READING_CONTEXT: &str = "PrTab && !(Editor && (mode != full || !multibuffer))";
/// [`READING_CONTEXT`] while the tab shows the diff: where keys for reading the diff work.
pub(crate) const DIFF_READING_CONTEXT: &str =
    "PrTab && middle == diff && !(Editor && (mode != full || !multibuffer))";

/// The tab's root context: [`KEY_CONTEXT`], and what its middle shows.
pub(crate) fn tab_context(middle: &Middle) -> KeyContext {
    let mut context = KeyContext::default();
    context.add(KEY_CONTEXT);
    let shows = if middle.shows_diff() { "diff" } else { "view" };
    context.set("middle", shows);
    context
}

/// Each part's `bind_keys`.
const BIND_KEYS: &[fn(&mut App)] = &[
    left::bind_keys,
    middle::bind_keys,
    right::bind_keys,
    viewed::bind_keys,
];

/// A part's `on_actions`, which registers its handlers on the tab's root. It's given the tab, so a
/// part can leave out the handlers that act on something the tab doesn't show.
type OnActions = fn(Div, &PrTab, &Context<PrTab>) -> Div;

/// Each part's `on_actions`.
const ON_ACTIONS: &[OnActions] = &[
    left::on_actions,
    middle::on_actions,
    right::on_actions,
    viewed::on_actions,
];

/// The tab's keys. Panels toggle with Zed's keys for its docks.
pub(crate) fn bind_keys(cx: &mut App) {
    for bind_keys in BIND_KEYS {
        bind_keys(cx);
    }
}

/// The tab runs each part's actions, wherever in it their keys work.
pub(super) fn on_actions(root: Div, tab: &PrTab, cx: &Context<PrTab>) -> Div {
    ON_ACTIONS
        .iter()
        .fold(root, |root, on_actions| on_actions(root, tab, cx))
}

/// Binds `keystrokes` to `action`, which reads the diff, wherever the diff is read: in the diff and
/// the file list, and on the tab around them, while the diff shows, but not in a text box in the
/// tab, which types them. Every key for reading the diff goes through here, so where they work can
/// change in this one place.
pub(crate) fn reading<A: Action + Clone>(keystrokes: &'static str, action: A) -> Vec<KeyBinding> {
    vec![KeyBinding::new(
        keystrokes,
        action,
        Some(DIFF_READING_CONTEXT),
    )]
}

/// Binds `keystrokes` to `action`, which acts on the diff, in the diff and in the file list, while
/// the diff shows. An option key types a character where there's text to type, so these work
/// only there.
pub(crate) fn diff_and_file_list<A: Action + Clone>(
    keystrokes: &'static str,
    action: A,
) -> Vec<KeyBinding> {
    vec![
        KeyBinding::new(keystrokes, action.clone(), Some(DIFF_CONTEXT)),
        KeyBinding::new(keystrokes, action, Some(DIFF_FILE_LIST_CONTEXT)),
    ]
}
