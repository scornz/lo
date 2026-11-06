//! Keeps rozed's window read-only where Zed's workspace would offer to create or edit files: no
//! welcome page and no "+" menu in a pane, no new file on a double click in the tab bar or an
//! empty pane, nothing opened from files dropped on the window, and any editor the workspace
//! opens, such as the one a file header's "Open File" opens, is read-only.

use editor::Editor;
use gpui::{App, Context, Entity, ExternalPaths, InteractiveElement as _};
use workspace::item::ItemHandle;
use workspace::{NewFile, Pane, Workspace};

/// Sets up a new workspace and its panes.
pub(crate) fn configure_workspace(workspace: &mut Workspace, cx: &mut Context<Workspace>) {
    for pane in workspace.panes().to_vec() {
        configure_pane(&pane, cx);
    }
    workspace.register_action_renderer(|div, _, _, _| {
        div
            // A double click in the tab bar or an empty pane sends NewFile, which no key sends.
            .capture_action(|_: &NewFile, _, cx| cx.stop_propagation())
            // A pane opens files and folders dropped on it, as a worktree Zed trusts to run
            // language servers in. rozed never opens a path it didn't create, so a drag from
            // Finder ends as soon as it enters the window, before any pane can take the drop.
            .on_drag_move::<ExternalPaths>(|_, window, cx| {
                if cx.stop_active_drag(window) {
                    log::info!("not opening files dragged onto rozed's window");
                }
            })
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
