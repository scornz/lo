//! rozed's key bindings: Zed's default macOS keymap, minus every binding that would open, create
//! or write files.
//!
//! rozed is read-only (design §1). Its buffers are read-only too, but Zed's open, new and save
//! actions work on the workspace, not a buffer: cmd-O opens any file on disk, cmd-N creates one,
//! and Save As on a read-only buffer writes a copy. So rozed never binds them.

use gpui::{App, KeyBinding};
use settings::{KeybindSource, KeymapFile, KeymapFileLoadResult, SettingsAssets};

/// Actions rozed never binds to a key, because they open, create or write files.
///
/// Only actions compiled into rozed are listed: Zed's keymap loader skips a binding whose action
/// doesn't exist, so the project panel's file actions, for example, can't be bound. Add an
/// action's crate, and its file actions go here.
pub const FILE_ACTIONS: &[&str] = &[
    // Opening files and folders from anywhere on disk, or a new window to open them in.
    "workspace::Open",
    "workspace::OpenFiles",
    "workspace::AddFolderToProject",
    "workspace::NewWindow",
    "projects::OpenRecent",
    "projects::OpenRemote",
    "welcome::OpenRecentProject",
    // Opens whatever path the text under the cursor names (Vim's `gf`).
    "editor::OpenSelectedFilename",
    // Opening settings, keymap and task files, rozed's own or the repository's `.zed/`, to edit.
    "zed::OpenSettings",
    "zed::OpenSettingsFile",
    "zed::OpenProjectSettings",
    "zed::OpenProjectTasks",
    "zed::OpenWorktreeSetupTasks",
    "zed::OpenKeymap",
    "zed::OpenKeymapFile",
    "zed::OpenServerSettings",
    // Creating files.
    "workspace::NewFile",
    "workspace::NewFileSplit",
    "workspace::NewFileSplitVertical",
    "workspace::NewFileSplitHorizontal",
    // Writing files, or making a read-only one writable.
    "workspace::Save",
    "workspace::SaveAs",
    "workspace::SaveAll",
    "workspace::SaveWithoutFormat",
    "workspace::FormatAndSave",
    "workspace::ToggleReadOnlyFile",
    // Handing a file to the app macOS picks for it, which may run it.
    "workspace::OpenWithSystem",
    // Changing the repository: its files, index, stashes, commits, branches or remotes.
    "git::ToggleStaged",
    "git::StageRange",
    "git::StageAndNext",
    "git::UnstageAndNext",
    "git::Restore",
    "git::RestoreAndNext",
    "git::RestoreFile",
    "git::StageFile",
    "git::UnstageFile",
    "git::StageSection",
    "git::UnstageSection",
    "git::StageAll",
    "git::UnstageAll",
    "git::StashAll",
    "git::StashTracked",
    "git::StashStaged",
    "git::StashPop",
    "git::StashApply",
    "git::RestoreTrackedFiles",
    "git::TrashUntrackedFiles",
    "git::Uncommit",
    "git::Push",
    "git::PushTo",
    "git::ForcePush",
    "git::Pull",
    "git::PullRebase",
    "git::Fetch",
    "git::FetchFrom",
    "git::Commit",
    "git::Amend",
    "git::Init",
    "git::Clone",
    "git::AddToGitignore",
    "git::AddToGitInfoExclude",
    "git::CreateTagAtHead",
    "git::RenameBranch",
    "git::CheckoutBranch",
    "git::Switch",
    "git::Branch",
    "git::CreateRemote",
    "git::Worktree",
];

/// Binds Zed's default macOS keymap, without [`FILE_ACTIONS`].
pub fn load(cx: &mut App) {
    let bindings = default_bindings(cx);
    cx.bind_keys(bindings);
}

/// Zed's default macOS keymap, without [`FILE_ACTIONS`]. Bindings for actions from Zed crates rozed
/// doesn't include (the agent, terminal, debugger and so on) are skipped.
fn default_bindings(cx: &App) -> Vec<KeyBinding> {
    let mut all = Vec::new();
    for path in [
        settings::DEFAULT_KEYMAP_PATH,
        settings::SPECIFIC_OVERRIDES_KEYMAP_PATH,
    ] {
        let content = util::asset_str::<SettingsAssets>(path);
        let bindings = match KeymapFile::load(&content, cx) {
            KeymapFileLoadResult::Success { key_bindings } => key_bindings,
            KeymapFileLoadResult::SomeFailedToLoad {
                key_bindings,
                error_message,
            } => {
                log::debug!(
                    "skipped bindings in {path} for actions rozed doesn't have: {error_message}"
                );
                key_bindings
            }
            KeymapFileLoadResult::JsonParseFailure { error } => {
                log::error!("can't parse {path}: {error:#}");
                continue;
            }
        };
        all.extend(without_file_actions(bindings));
    }
    for binding in &mut all {
        binding.set_meta(KeybindSource::Default.meta());
    }
    all
}

/// Drops the bindings for [`FILE_ACTIONS`].
fn without_file_actions(bindings: Vec<KeyBinding>) -> impl Iterator<Item = KeyBinding> {
    bindings.into_iter().filter(|binding| {
        let name = binding.action().name();
        let keep = !FILE_ACTIONS.contains(&name);
        if !keep {
            log::debug!("not binding {name}: it opens, creates or writes files");
        }
        keep
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use gpui::{Keystroke, TestAppContext};

    fn init(cx: &mut TestAppContext) {
        cx.update(load);
    }

    #[gpui::test]
    fn no_key_opens_creates_or_writes_files(cx: &mut TestAppContext) {
        init(cx);
        cx.update(|cx| {
            let keymap = cx.key_bindings();
            let keymap = keymap.borrow();
            let bound: Vec<&str> = keymap
                .bindings()
                .map(|binding| binding.action().name())
                .filter(|name| FILE_ACTIONS.contains(name))
                .collect();
            assert!(bound.is_empty(), "bound to a key: {bound:?}");
        });
    }

    #[gpui::test]
    fn zeds_keymap_binds_file_actions_that_rozed_drops(cx: &mut TestAppContext) {
        // Without the filter, these keys would open, create and save files.
        cx.update(|cx| {
            let content = util::asset_str::<SettingsAssets>(settings::DEFAULT_KEYMAP_PATH);
            let key_bindings = match KeymapFile::load(&content, cx) {
                KeymapFileLoadResult::Success { key_bindings }
                | KeymapFileLoadResult::SomeFailedToLoad { key_bindings, .. } => key_bindings,
                KeymapFileLoadResult::JsonParseFailure { error } => panic!("{error:#}"),
            };
            for (key, action) in [
                ("cmd-o", "workspace::Open"),
                ("cmd-n", "workspace::NewFile"),
                ("cmd-s", "workspace::Save"),
                ("cmd-shift-s", "workspace::SaveAs"),
                ("cmd-alt-s", "workspace::SaveAll"),
                ("cmd-shift-n", "workspace::NewWindow"),
            ] {
                let keystroke = Keystroke::parse(key).unwrap();
                assert!(
                    key_bindings.iter().any(|binding| {
                        binding.action().name() == action
                            && binding.match_keystrokes(std::slice::from_ref(&keystroke))
                                == Some(false)
                    }),
                    "Zed's keymap no longer binds {key} to {action}"
                );
            }
        });
    }

    #[gpui::test]
    fn every_file_action_exists(cx: &mut TestAppContext) {
        // A name Zed renamed or removed would no longer match its binding, which would come back.
        cx.update(|cx| {
            let known = cx.all_action_names();
            let missing: Vec<&str> = FILE_ACTIONS
                .iter()
                .copied()
                .filter(|name| !known.contains(name))
                .collect();
            assert!(missing.is_empty(), "not actions in rozed: {missing:?}");
        });
    }

    #[gpui::test]
    fn other_bindings_still_load(cx: &mut TestAppContext) {
        init(cx);
        cx.update(|cx| {
            let keymap = cx.key_bindings();
            let keymap = keymap.borrow();
            for action in ["editor::Copy", "workspace::CloseWindow", "zed::Quit"] {
                assert!(
                    keymap.bindings().any(|b| b.action().name() == action),
                    "{action} isn't bound"
                );
            }
        });
    }
}
