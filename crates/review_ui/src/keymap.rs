//! Lo's key bindings: Zed's default macOS keymap, minus every binding that would open, create
//! or write files.
//!
//! Lo is read-only (design §1). Its buffers are read-only too, but Zed's open, new and save
//! actions work on the workspace, not a buffer: cmd-O opens any file on disk, cmd-N creates one,
//! and Save As on a read-only buffer writes a copy. So Lo never binds them
//! ([`FileActions`]).

use gpui::{App, KeyBinding};
use settings::{KeybindSource, KeymapFile, KeymapFileLoadResult, SettingsAssets};

use crate::read_only::FileActions;

/// Binds Zed's default macOS keymap, without [`FileActions`].
pub fn load(cx: &mut App) {
    let bindings = default_bindings(cx);
    cx.bind_keys(bindings);
}

/// Zed's default macOS keymap, without [`FileActions`]. Bindings for actions from Zed crates Lo
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
                    "skipped bindings in {path} for actions Lo doesn't have: {error_message}"
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

/// Drops the bindings for [`FileActions`].
fn without_file_actions(bindings: Vec<KeyBinding>) -> impl Iterator<Item = KeyBinding> {
    let file_actions = FileActions::names();
    bindings.into_iter().filter(move |binding| {
        let name = binding.action().name();
        let keep = !file_actions.contains(&name);
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
            let file_actions = FileActions::names();
            let bound: Vec<&str> = keymap
                .bindings()
                .map(|binding| binding.action().name())
                .filter(|name| file_actions.contains(name))
                .collect();
            assert!(bound.is_empty(), "bound to a key: {bound:?}");
        });
    }

    #[gpui::test]
    fn zeds_keymap_binds_file_actions_that_lo_drops(cx: &mut TestAppContext) {
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
