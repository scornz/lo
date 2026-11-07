//! Vim's keys, from Zed's `vim` crate, on by default (design §10 "Keys"): in the diff, which is
//! read-only, and in text boxes such as comment boxes. Lo's own keys in the diff go over vim's
//! (`pr_tab::keys`), and Lo refuses what vim would do to the disk, a shell or the diff's text
//! ([`refusals`]).

pub(crate) mod refusals;

use editor::Editor;
use gpui::{App, UpdateGlobal as _};
use settings::SettingsStore;
use vim_mode_setting::{HelixModeSetting, VimModeSetting};

/// Sets vim up, and turns it on. `vim_mode: true` is a default, not a forced setting, so a user's
/// `"vim_mode": false` turns it off. Call it before the keymap loads, which binds vim's keys only
/// when vim is on.
pub(crate) fn init(cx: &mut App) {
    vim::init(cx);
    refusals::init(cx);
    // Vim sets its command palette interceptor each time it turns on, as it sees the setting
    // change; this sees it after vim does, and puts Lo's over it.
    let mut was_on = false;
    cx.observe_global::<SettingsStore>(move |cx| {
        let on = VimModeSetting::is_enabled(cx) || HelixModeSetting::is_enabled(cx);
        if on && !was_on {
            refusals::wrap_vims_interceptor(cx);
        }
        was_on = on;
    })
    .detach();
    let turn_on = |cx: &mut App| {
        SettingsStore::update_global(cx, |store, cx| {
            store.update_default_settings(cx, |defaults| defaults.vim_mode = Some(true))
        })
    };
    // Now, for the keymap, which loads next. And again once vim's observer, and Lo's, start
    // observing, after this update: they miss a change made while an earlier one in it still
    // waits to be announced, and vim sets its command palette up only when it sees the change.
    turn_on(cx);
    cx.defer(turn_on);
}

/// `editor`, a text box such as a comment box, taking vim's keys when vim is on. Zed gives vim
/// only to full-size editors, and decides when an editor is made, so call it inside the closure
/// that makes the editor, as in `cx.new(|cx| modal(Editor::auto_height(1, 10, window, cx)))`.
pub fn modal(mut editor: Editor) -> Editor {
    editor.set_use_modal_editing(true);
    editor
}

#[cfg(test)]
mod tests {
    use super::*;

    use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext};
    use settings::Settings as _;
    use vim_mode_setting::VimModeSetting;

    fn vim_mode(editor: &Entity<Editor>, cx: &mut VisualTestContext) -> Option<String> {
        cx.update(|window, cx| {
            let context = editor.update(cx, |editor, cx| editor.key_context(window, cx));
            context.get("vim_mode").map(|mode| mode.to_string())
        })
    }

    #[gpui::test]
    async fn vim_is_on_unless_the_users_settings_turn_it_off(cx: &mut TestAppContext) {
        let (_, cx) = crate::test_support::workspace(cx).await;
        assert!(cx.update(|_, cx| VimModeSetting::get_global(cx).0));
        cx.update(|_, cx| {
            SettingsStore::update_global(cx, |store, cx| {
                let result = store.set_user_settings(r#"{ "vim_mode": false }"#, cx);
                assert_eq!(result.parse_status, settings::ParseStatus::Success);
            })
        });
        assert!(!cx.update(|_, cx| VimModeSetting::get_global(cx).0));
    }

    #[gpui::test]
    async fn a_text_box_made_modal_takes_vims_keys(cx: &mut TestAppContext) {
        let (_, cx) = crate::test_support::workspace(cx).await;
        let modal_box =
            cx.update(|window, cx| cx.new(|cx| modal(Editor::auto_height(1, 10, window, cx))));
        let plain_box = cx.update(|window, cx| cx.new(|cx| Editor::auto_height(1, 10, window, cx)));
        assert_eq!(vim_mode(&modal_box, cx).as_deref(), Some("normal"));
        assert_eq!(vim_mode(&plain_box, cx), None);
    }
}
