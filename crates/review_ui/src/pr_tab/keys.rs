//! The tab's keys: the contexts they work in, and each part's own (design §10 "Keys"). Each part
//! binds its keys in its `bind_keys` and runs their actions in its `on_actions`, and adds both to
//! the lists here, one line each, in alphabetical order.
//!
//! The tab's own context says what its middle shows, `middle=diff` or `middle=view`
//! ([`tab_context`]). Keys that act on the diff hold only while it shows, so they don't reach a
//! hidden diff, and a view in its place can bind the same keys.
//!
//! Vim's keys bind at the depth of the editor they work in, and before Lo's. So in the diff, Lo's
//! keys bind at the depth of its editors too ([`DIFF_CONTEXT`]), where Lo's win, being bound
//! later. [`OVER_VIM`] and [`UNDER_VIM`] list where Lo's keys and vim's meet.

use std::any::Any;

use editor::{Addon, Editor, SplittableEditor};
use gpui::{Action, App, Context, Div, Entity, KeyBinding, KeyContext};

use super::{Middle, PrTab, left, middle, right, viewed};

pub(crate) const KEY_CONTEXT: &str = "PrTab";
pub(crate) const FILE_LIST_CONTEXT: &str = "PrFileList";
/// What the diff's editors, both sides, add to their own context ([`mark_diff`]).
pub(crate) const PR_DIFF: &str = "PrDiff";
/// The diff's editors, inside the tab while it shows the diff, where Lo's keys bind at the
/// editors' depth, so they win over vim's there. Not while vim waits for the rest of a command:
/// after an operator such as `y` (`vim_mode == operator`), for the character after `f`, `t`, `m`,
/// `'` or a register's `"` (`waiting`, or `literal`), or with a count typed (`VimCount`). So `ye`,
/// `fe` and `3e` are vim's. Never in a text box, such as a comment box: an editor that isn't full
/// size never matches, and while an editor inside the diff has focus, Zed leaves the diff's own
/// context, `PrDiff` included, out of the stack.
pub(crate) const DIFF_CONTEXT: &str = "(PrTab && middle == diff > Editor && PrDiff) \
    && !(Editor && mode != full) \
    && vim_mode != operator && vim_mode != waiting && vim_mode != literal && !VimCount";
/// The file list while the tab shows the diff, for keys that act on the diff from the list.
pub(crate) const DIFF_FILE_LIST_CONTEXT: &str = "PrTab && middle == diff > PrFileList";
/// The tab outside any text box in it, such as a comment box: keys that type a character work
/// here, so a text box still types them. The diff's own editors are the only full-size editors of
/// many files.
pub(crate) const READING_CONTEXT: &str = "PrTab && !(Editor && (mode != full || !multibuffer))";
/// The tab outside any editor while it shows the diff, such as in the file list: where keys for
/// reading the diff work besides the diff itself ([`DIFF_CONTEXT`]). A text box types them.
pub(crate) const DIFF_READING_CONTEXT: &str = "PrTab && middle == diff && !Editor";

/// Lo's keys that win over vim's where both hold, hiding vim's meaning there: by keystrokes and
/// Lo's action. CLAUDE.md's "Keys" table says what each hides, and `key_clashes::with_vim` checks
/// the list on the tab's places.
#[cfg(test)]
pub(crate) const OVER_VIM: &[(&str, &str)] = &[
    // vim::NextWordEnd.
    ("e", "review_ui::ToggleViewed"),
    // workspace::FollowNextCollaborator, for both.
    ("] f", "review_ui::NextFile"),
    ("[ f", "review_ui::PreviousFile"),
    // editor::GoToHunk and editor::GoToPreviousHunk, which stay in the file.
    ("] c", "review_ui::NextChange"),
    ("[ c", "review_ui::PreviousChange"),
    // vim::Tab.
    ("tab", "review_ui::SwitchSide"),
];

/// Vim's keys that win over Lo's where both hold: by keystrokes and vim's action.
#[cfg(test)]
pub(crate) const UNDER_VIM: &[(&str, &str)] = &[
    // In insert mode, in a text box such as a comment box: a normal-mode command, over
    // pane::GoBack.
    ("ctrl-o", "vim::TemporaryNormal"),
];

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

/// Binds `keystrokes` to `action`, which reads the diff, wherever the diff is read: in the diff
/// ([`DIFF_CONTEXT`]), and elsewhere in the tab while the diff shows, such as the file list
/// ([`DIFF_READING_CONTEXT`]), but not in a text box in the tab, which types them. Every key for
/// reading the diff goes through here, so where they work can change in this one place.
pub(crate) fn reading<A: Action + Clone>(keystrokes: &'static str, action: A) -> Vec<KeyBinding> {
    vec![
        KeyBinding::new(keystrokes, action.clone(), Some(DIFF_CONTEXT)),
        KeyBinding::new(keystrokes, action, Some(DIFF_READING_CONTEXT)),
    ]
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

/// Adds [`PR_DIFF`] to an editor's context.
struct DiffKeyContext;

impl Addon for DiffKeyContext {
    fn extend_key_context(&self, context: &mut KeyContext, _: &App) {
        context.add(PR_DIFF);
    }

    fn to_any(&self) -> &dyn Any {
        self
    }
}

/// Whether `editor` is one of the diff's ([`mark_diff`]).
pub(crate) fn is_diff(editor: &Editor) -> bool {
    editor.addon::<DiffKeyContext>().is_some()
}

/// Gives `diff`'s editors, both sides, the diff's context, [`PR_DIFF`]. Call it whenever the
/// diff may have gone side by side, which makes a new left-hand editor.
pub(crate) fn mark_diff(diff: &Entity<SplittableEditor>, cx: &mut App) {
    let editors = {
        let diff = diff.read(cx);
        [Some(diff.rhs_editor().clone()), diff.lhs_editor().cloned()]
    };
    for editor in editors.into_iter().flatten() {
        if editor.read(cx).addon::<DiffKeyContext>().is_none() {
            editor.update(cx, |editor, cx| {
                editor.register_addon(DiffKeyContext);
                cx.notify();
            });
        }
    }
}
