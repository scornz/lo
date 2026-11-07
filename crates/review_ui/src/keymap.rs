//! Lo's key bindings: Zed's default macOS keymap, and vim's when vim is on, minus every binding
//! that would open, create or write files, run a shell, or take a key Lo binds.
//!
//! Lo is read-only (design §1). Its buffers are read-only too, but Zed's open, new and save
//! actions work on the workspace, not a buffer: cmd-O opens any file on disk, cmd-N creates one,
//! and Save As on a read-only buffer writes a copy. So Lo never binds them
//! ([`FileActions`]).

use gpui::{App, KeyBinding};
use settings::{KeybindSource, KeymapFile, KeymapFileLoadResult, SettingsAssets};
use vim_mode_setting::{HelixModeSetting, VimModeSetting};

use crate::read_only::FileActions;
use crate::vim_mode::refusals;

/// Zed's actions that Lo leaves unbound because their keys are Lo's. Zed binds `cmd-enter` and
/// `cmd-shift-enter` to these in every full-size editor, the diff's included, where they'd beat
/// Lo's keys for the tab, which bind at the tab's depth.
const TAKE_LOS_KEYS: &[&str] = &["editor::NewlineBelow", "editor::NewlineAbove"];

/// Binds Zed's default macOS keymap, with vim's when vim is on, without what
/// [`without_dropped`] drops. Vim's goes between Zed's keymap and its macOS overrides, as Zed binds it.
pub fn load(cx: &mut App) {
    let vim = VimModeSetting::is_enabled(cx) || HelixModeSetting::is_enabled(cx);
    let mut bindings = bindings_in(settings::DEFAULT_KEYMAP_PATH, KeybindSource::Default, cx);
    if vim {
        bindings.extend(vim_bindings(cx));
    }
    bindings.extend(bindings_in(
        settings::SPECIFIC_OVERRIDES_KEYMAP_PATH,
        KeybindSource::Default,
        cx,
    ));
    cx.bind_keys(bindings);
}

/// Vim's keymap, as [`load`] binds it when vim is on.
pub(crate) fn vim_bindings(cx: &App) -> Vec<KeyBinding> {
    bindings_in(settings::VIM_KEYMAP_PATH, KeybindSource::Vim, cx)
}

/// The keymap at `path` in Zed's assets, from `source`, without what [`without_dropped`] drops. Bindings for actions from Zed crates Lo doesn't include (the agent,
/// terminal, debugger and so on) are skipped.
fn bindings_in(path: &str, source: KeybindSource, cx: &App) -> Vec<KeyBinding> {
    let content = util::asset_str::<SettingsAssets>(path);
    let bindings = match KeymapFile::load(&content, cx) {
        KeymapFileLoadResult::Success { key_bindings } => key_bindings,
        KeymapFileLoadResult::SomeFailedToLoad {
            key_bindings,
            error_message,
        } => {
            log::debug!("skipped bindings in {path} for actions Lo doesn't have: {error_message}");
            key_bindings
        }
        KeymapFileLoadResult::JsonParseFailure { error } => {
            log::error!("can't parse {path}: {error:#}");
            return Vec::new();
        }
    };
    let mut bindings: Vec<KeyBinding> = without_dropped(bindings).collect();
    for binding in &mut bindings {
        binding.set_meta(source.meta());
    }
    bindings
}

/// Drops the bindings for [`FileActions`], for closing with a save (vim's `ZZ`), for vim's shell
/// keys (`!`) and for [`TAKE_LOS_KEYS`].
fn without_dropped(bindings: Vec<KeyBinding>) -> impl Iterator<Item = KeyBinding> {
    let file_actions = FileActions::names();
    bindings.into_iter().filter(move |binding| {
        let name = binding.action().name();
        if file_actions.contains(&name) || refusals::saves_on_close(binding.action()) {
            log::debug!("not binding {name}: it opens, creates or writes files");
            false
        } else if refusals::SHELL_ACTIONS.contains(&name) {
            log::debug!("not binding {name}: it runs a shell");
            false
        } else if TAKE_LOS_KEYS.contains(&name) {
            log::debug!("not binding {name}: its keys are Lo's");
            false
        } else {
            true
        }
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

    /// Zed's bindings in the keymap at `path`, unfiltered.
    fn zeds(path: &str, cx: &App) -> Vec<KeyBinding> {
        let content = util::asset_str::<SettingsAssets>(path);
        match KeymapFile::load(&content, cx) {
            KeymapFileLoadResult::Success { key_bindings }
            | KeymapFileLoadResult::SomeFailedToLoad { key_bindings, .. } => key_bindings,
            KeymapFileLoadResult::JsonParseFailure { error } => panic!("{error:#}"),
        }
    }

    /// Asserts that `bindings` bind each key to its action.
    fn assert_binds(bindings: &[KeyBinding], path: &str, keys: &[(&'static str, &str)]) {
        for (key, action) in keys {
            let keystrokes: Vec<Keystroke> = key
                .split(' ')
                .map(|key| Keystroke::parse(key).unwrap())
                .collect();
            assert!(
                bindings.iter().any(|binding| {
                    binding.action().name() == *action
                        && binding.match_keystrokes(&keystrokes) == Some(false)
                }),
                "{path} no longer binds {key} to {action}"
            );
        }
    }

    #[gpui::test]
    fn zeds_keymaps_bind_actions_that_lo_drops(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let path = settings::DEFAULT_KEYMAP_PATH;
            // Without the filter, these keys would open, create and save files.
            let file_actions = [
                ("cmd-o", "workspace::Open"),
                ("cmd-n", "workspace::NewFile"),
                ("cmd-s", "workspace::Save"),
                ("cmd-shift-s", "workspace::SaveAs"),
                ("cmd-alt-s", "workspace::SaveAll"),
                ("cmd-shift-n", "workspace::NewWindow"),
            ];
            assert_binds(&zeds(path, cx), path, &file_actions);
            // And these would take Lo's keys in the diff.
            let newlines = [
                ("cmd-enter", "editor::NewlineBelow"),
                ("cmd-shift-enter", "editor::NewlineAbove"),
            ];
            assert_binds(&zeds(path, cx), path, &newlines);
            let path = settings::VIM_KEYMAP_PATH;
            let vim_drops = [
                ("ctrl-w n", "workspace::NewFileSplitHorizontal"),
                ("g f", "editor::OpenSelectedFilename"),
                ("!", "vim::PushShellCommand"),
                ("!", "vim::ShellCommand"),
            ];
            assert_binds(&zeds(path, cx), path, &vim_drops);
            let vims = zeds(path, cx);
            let zz = Keystroke::parse("shift-z").unwrap();
            assert!(
                vims.iter()
                    .any(|binding| refusals::saves_on_close(binding.action())
                        && binding.match_keystrokes(&[zz.clone(), zz.clone()]) == Some(false)),
                "{path} no longer binds ZZ to close saving"
            );
        });
    }

    #[gpui::test]
    fn no_key_takes_los_keys_from_the_diff(cx: &mut TestAppContext) {
        init(cx);
        cx.update(|cx| {
            let keymap = cx.key_bindings();
            let keymap = keymap.borrow();
            let bound: Vec<&str> = keymap
                .bindings()
                .map(|binding| binding.action().name())
                .filter(|name| TAKE_LOS_KEYS.contains(name))
                .collect();
            assert!(bound.is_empty(), "bound to a key: {bound:?}");
        });
    }

    /// The vim:: actions bound after loading with `vim_mode` set to `on`.
    fn vim_actions_bound(on: bool, cx: &mut TestAppContext) -> Vec<&'static str> {
        cx.update(|cx| {
            let mut store = settings::SettingsStore::test(cx);
            let result = store.set_user_settings(&format!(r#"{{ "vim_mode": {on} }}"#), cx);
            assert_eq!(result.parse_status, settings::ParseStatus::Success);
            cx.set_global(store);
            cx.clear_key_bindings();
            load(cx);
            let keymap = cx.key_bindings();
            let keymap = keymap.borrow();
            keymap
                .bindings()
                .map(|binding| binding.action().name())
                .filter(|name| name.starts_with("vim::"))
                .collect()
        })
    }

    #[gpui::test]
    fn vims_keys_load_only_when_vim_is_on(cx: &mut TestAppContext) {
        assert_eq!(vim_actions_bound(false, cx), [""; 0]);
        let bound = vim_actions_bound(true, cx);
        for action in ["vim::Down", "vim::NextWordEnd", "vim::ToggleVisual"] {
            assert!(bound.contains(&action), "{action} isn't bound");
        }
        // With vim's, still no key opens, creates or writes files.
        cx.update(|cx| {
            let keymap = cx.key_bindings();
            let keymap = keymap.borrow();
            let file_actions = FileActions::names();
            assert!(
                !keymap
                    .bindings()
                    .any(|binding| file_actions.contains(&binding.action().name()))
            );
            // Nor runs a shell (vim's `!`), nor closes saving first (vim's `ZZ`).
            assert!(!keymap.bindings().any(|binding| {
                refusals::SHELL_ACTIONS.contains(&binding.action().name())
                    || refusals::saves_on_close(binding.action())
            }));
            // And vim's come from vim.
            assert!(keymap.bindings().any(|binding| {
                binding.action().name() == "vim::Down"
                    && binding.meta() == Some(KeybindSource::Vim.meta())
            }));
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
