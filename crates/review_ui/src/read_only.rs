//! Keeps Lo's window read-only where Zed's workspace would offer to open, create or edit files:
//! no welcome page and no "+" menu in a pane, none of [`FileActions`] from anywhere, nothing
//! opened from files dropped on the window, and any editor the workspace opens, such as the one a
//! file header's "Open File" opens, is read-only.

use std::any::TypeId;

use command_palette_hooks::CommandPaletteFilter;
use editor::Editor;
use gpui::{Action, App, Context, Div, Entity, ExternalPaths, InteractiveElement as _};
use workspace::item::ItemHandle;
use workspace::{Pane, Workspace};

/// Lists the actions that open, create or write files, or open a window, so the same list is
/// captured, hidden from the command palette and left out of the keymap.
macro_rules! file_actions {
    ($($action:path),* $(,)?) => {
        /// The actions Lo never runs, because they open, create or write files, or open a
        /// window (design §1 "Read-only", §10 "One window").
        ///
        /// No key is bound to them ([`crate::keymap`]), the command palette doesn't list them,
        /// and the window captures them before any handler, so neither a menu, a click (a double
        /// click in the tab bar or an empty pane sends `workspace::NewFile`) nor anything else
        /// that dispatches one runs it. Only actions compiled into Lo can be listed; add an
        /// action's crate, and its file actions go here.
        pub struct FileActions;

        impl FileActions {
            /// Their names, as the keymap and the command palette name them.
            pub fn names() -> Vec<&'static str> {
                vec![$(<$action as Action>::name_for_type()),*]
            }

            fn type_ids() -> Vec<TypeId> {
                vec![$(TypeId::of::<$action>()),*]
            }

            /// Stops each of them when it's dispatched anywhere in the workspace.
            fn capture(div: Div) -> Div {
                div$(.capture_action(|_: &$action, _, cx| {
                    log::info!(
                        "not running {}: it opens, creates or writes files",
                        <$action as Action>::name_for_type()
                    );
                    cx.stop_propagation();
                }))*
            }
        }
    };
}

file_actions![
    // Opening files and folders from anywhere on disk, or a new window to open them in.
    workspace::Open,
    workspace::OpenFiles,
    workspace::AddFolderToProject,
    workspace::NewWindow,
    zed_actions::OpenRecent,
    zed_actions::OpenRemote,
    workspace::welcome::OpenRecentProject,
    // Opens whatever path the text under the cursor names (Vim's `gf`).
    editor::actions::OpenSelectedFilename,
    // Opening settings, keymap and task files, Lo's own or the repository's `.zed/`, to edit.
    zed_actions::OpenSettings,
    zed_actions::OpenSettingsFile,
    zed_actions::OpenProjectSettings,
    zed_actions::OpenProjectTasks,
    zed_actions::OpenWorktreeSetupTasks,
    zed_actions::OpenKeymap,
    zed_actions::OpenKeymapFile,
    zed_actions::OpenServerSettings,
    // Creating files.
    workspace::NewFile,
    workspace::NewFileSplit,
    workspace::NewFileSplitVertical,
    workspace::NewFileSplitHorizontal,
    // Writing files, or making a read-only one writable.
    workspace::Save,
    workspace::SaveAs,
    workspace::SaveAll,
    workspace::SaveWithoutFormat,
    workspace::FormatAndSave,
    workspace::ToggleReadOnlyFile,
    // Vim's switches write Lo's settings file, which Lo doesn't read yet.
    vim::ToggleVimMode,
    vim::ToggleHelixMode,
    // Handing a file to the app macOS picks for it, which may run it.
    zed_actions::workspace::OpenWithSystem,
    // Changing the repository: its files, index, stashes, commits, branches or remotes.
    git::ToggleStaged,
    git::StageRange,
    git::StageAndNext,
    git::UnstageAndNext,
    git::Restore,
    git::RestoreAndNext,
    git::RestoreFile,
    git::StageFile,
    git::UnstageFile,
    git::StageSection,
    git::UnstageSection,
    git::StageAll,
    git::UnstageAll,
    git::StashAll,
    git::StashTracked,
    git::StashStaged,
    git::StashPop,
    git::StashApply,
    git::RestoreTrackedFiles,
    git::TrashUntrackedFiles,
    git::Uncommit,
    git::Push,
    git::PushTo,
    git::ForcePush,
    git::Pull,
    git::PullRebase,
    git::Fetch,
    git::FetchFrom,
    git::Commit,
    git::Amend,
    git::Init,
    git::Clone,
    git::AddToGitignore,
    git::AddToGitInfoExclude,
    git::CreateTagAtHead,
    git::RenameBranch,
    zed_actions::git::CheckoutBranch,
    zed_actions::git::Switch,
    zed_actions::git::Branch,
    zed_actions::git::CreateRemote,
    zed_actions::git::Worktree,
];

/// Hides [`FileActions`] from the command palette. Call it after the palette is set up.
pub(crate) fn hide_from_palette(cx: &mut App) {
    CommandPaletteFilter::update_global(cx, |filter, _| {
        filter.hide_action_types(&FileActions::type_ids());
    });
}

/// Sets up a new workspace and its panes.
pub(crate) fn configure_workspace(workspace: &mut Workspace, cx: &mut Context<Workspace>) {
    for pane in workspace.panes().to_vec() {
        configure_pane(&pane, cx);
    }
    workspace.register_action_renderer(|div, _, _, _| {
        FileActions::capture(div)
            // A pane opens files and folders dropped on it, as a worktree Zed trusts to run
            // language servers in. Lo never opens a path it didn't create, so a drag from
            // Finder ends as soon as it enters the window, before any pane can take the drop.
            .on_drag_move::<ExternalPaths>(|_, window, cx| {
                if cx.stop_active_drag(window) {
                    log::info!("not opening files dragged onto Lo's window");
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
