//! Keeps rozed's window read-only where Zed's workspace would offer to create or edit files: no
//! welcome page and no "+" menu in a pane, no new file on a double click in the tab bar or an
//! empty pane, and any editor the workspace opens, such as the one a file header's "Open File"
//! opens, is read-only.

use editor::Editor;
use gpui::{App, Context, Entity, InteractiveElement as _};
use workspace::item::ItemHandle;
use workspace::{NewFile, Pane, Workspace};

/// Sets up a new workspace and its panes.
pub(crate) fn configure_workspace(workspace: &mut Workspace, cx: &mut Context<Workspace>) {
    for pane in workspace.panes().to_vec() {
        configure_pane(&pane, cx);
    }
    // A double click in the tab bar or an empty pane sends NewFile, which no key sends.
    workspace.register_action_renderer(|div, _, _, _| {
        div.capture_action(|_: &NewFile, _, cx| cx.stop_propagation())
    });
}

/// Hides Zed's welcome page and the tab bar's "+" and split menus.
pub(crate) fn configure_pane(pane: &Entity<Pane>, cx: &mut App) {
    pane.update(cx, |pane, cx| {
        pane.set_should_display_welcome_page(false);
        pane.set_render_tab_bar_buttons(cx, |_, _, _| (None, None));
    });
}

/// Makes an editor the workspace opened read-only.
pub(crate) fn item_added(item: &dyn ItemHandle, cx: &mut App) {
    if let Some(editor) = item.downcast::<Editor>() {
        editor.update(cx, |editor, _| editor.set_read_only(true));
    }
}
