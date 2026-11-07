use super::*;

#[gpui::test]
fn the_tabs_typing_keys_type_in_a_text_box_inside_it(cx: &mut TestAppContext) {
    init(cx);
    cx.update(|cx| {
        let keymap = cx.key_bindings();
        let keymap = keymap.borrow();
        let stack = |contexts: &[&str]| -> Vec<gpui::KeyContext> {
            contexts
                .iter()
                .map(|context| gpui::KeyContext::parse(context).unwrap())
                .collect()
        };
        let tab = ["Workspace", "Pane", "PrTab"];
        let diff = stack(&[&tab[..], &["Editor mode=full multibuffer"]].concat());
        let file_list = stack(&[&tab[..], &["PrFileList"]].concat());
        // A comment box in the diff, as milestone 8 adds, or any other text box in the tab.
        let text_box = stack(
            &[
                &tab[..],
                &["Editor mode=full multibuffer", "Editor mode=auto_height"],
            ]
            .concat(),
        );
        let line = stack(&[&tab[..], &["Editor mode=single_line"]].concat());
        let lo = |input: &'static str, contexts: &[gpui::KeyContext]| {
            let keystrokes: Vec<gpui::Keystroke> = input
                .split(' ')
                .map(|key| gpui::Keystroke::parse(key).unwrap())
                .collect();
            let (bindings, pending) = keymap.bindings_for_input(&keystrokes, contexts);
            let actions: Vec<&str> = bindings
                .iter()
                .map(|binding| binding.action().name())
                .filter(|name| name.starts_with("review_ui::"))
                .collect();
            (actions, pending)
        };

        for (keys, action) in [
            ("e", "review_ui::ToggleViewed"),
            ("] f", "review_ui::NextFile"),
            ("[ f", "review_ui::PreviousFile"),
            ("] c", "review_ui::NextChange"),
            ("[ c", "review_ui::PreviousChange"),
            ("alt-o", "review_ui::ToggleFileOrder"),
        ] {
            assert_eq!(lo(keys, &diff).0, [action], "{keys} in the diff");
            assert_eq!(lo(keys, &file_list).0, [action], "{keys} in the file list");
            for contexts in [&text_box, &line] {
                assert_eq!(lo(keys, contexts).0, [""; 0], "{keys} in a text box");
            }
        }
        // A bracket types at once, not waiting for a second key.
        for prefix in ["]", "["] {
            assert!(lo(prefix, &diff).1, "{prefix} waits in the diff");
            assert!(!lo(prefix, &text_box).1, "{prefix} types in a text box");
        }
    });
}

#[gpui::test]
async fn panels_open_and_close_with_their_keys(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let tab = open_unloaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);
    let files = [
        changed("b.rs", Change::Modified),
        changed("a.json", Change::Modified),
    ];
    let head: Oid = "2".repeat(40).parse().unwrap();
    tab.update(cx, |tab, cx| {
        tab.copied(
            files
                .iter()
                .map(|file| FileEntry::from_local(file, None))
                .collect(),
            &head,
            SystemTime::now(),
            cx,
        )
    });
    let shown = |cx: &mut VisualTestContext| {
        tab.read_with(cx, |tab, _| {
            (tab.is_file_list_shown(), tab.is_threads_panel_shown())
        })
    };
    assert_eq!(shown(cx), (true, true));

    cx.simulate_keystrokes("cmd-b");
    assert_eq!(shown(cx), (false, true));
    cx.simulate_keystrokes("cmd-alt-b");
    assert_eq!(shown(cx), (false, false));
    cx.simulate_keystrokes("cmd-r");
    assert_eq!(shown(cx), (false, true));

    // Showing the file list focuses it, where j and k move and o switches the order.
    cx.simulate_keystrokes("cmd-b");
    assert_eq!(shown(cx), (true, true));
    let in_file_list = |cx: &mut VisualTestContext| {
        let focus = tab.read_with(cx, |tab, _| tab.file_list_focus_handle().clone());
        cx.update(|window, cx| focus.contains_focused(window, cx))
    };
    assert!(in_file_list(cx));
    let selected = |cx: &mut VisualTestContext| {
        tab.read_with(cx, |tab, _| {
            (
                tab.files().order(),
                tab.files().selected().map(|entry| entry.path().to_string()),
            )
        })
    };
    cx.simulate_keystrokes("j");
    assert_eq!(selected(cx), (FileOrder::Review, Some("b.rs".into())));
    cx.simulate_keystrokes("j");
    assert_eq!(selected(cx), (FileOrder::Review, Some("a.json".into())));
    cx.simulate_keystrokes("o");
    assert_eq!(selected(cx), (FileOrder::Path, Some("a.json".into())));
    cx.simulate_keystrokes("j k");
    assert_eq!(selected(cx), (FileOrder::Path, Some("a.json".into())));
    cx.simulate_keystrokes("alt-o");
    assert_eq!(selected(cx).0, FileOrder::Review);

    // Escape goes back to the diff, leaving the list shown, and cmd-b then hides it.
    cx.simulate_keystrokes("escape");
    assert!(!in_file_list(cx));
    cx.simulate_keystrokes("cmd-b");
    assert_eq!(shown(cx), (false, true));
}

#[gpui::test]
fn los_keys_win_over_zeds_and_leave_text_fields_alone(cx: &mut TestAppContext) {
    init(cx);
    cx.update(|cx| {
        let keymap = cx.key_bindings();
        let keymap = keymap.borrow();
        let first = |key: &'static str, contexts: &[&str]| -> Option<&'static str> {
            let stack: Vec<gpui::KeyContext> = contexts
                .iter()
                .map(|context| gpui::KeyContext::parse(context).unwrap())
                .collect();
            let keystroke = gpui::Keystroke::parse(key).unwrap();
            let (bindings, _) = keymap.bindings_for_input(&[keystroke], &stack);
            bindings.first().map(|binding| binding.action().name())
        };
        let diff = ["Workspace", "Pane", "PrTab", "Editor mode=full multibuffer"];
        let file_list = ["Workspace", "Pane", "PrTab", "PrFileList"];
        let comment = ["Workspace", "Pane", "PrTab", "Editor mode=auto_height"];
        // A comment box can be a full editor too, of one buffer.
        let full_comment = ["Workspace", "Pane", "PrTab", "Editor mode=full"];
        let elsewhere = ["Workspace", "Pane", "Editor mode=full multibuffer"];

        // Zed's keys are bound, and Lo's, bound after, win where both apply.
        assert_eq!(first("tab", &elsewhere), Some("editor::Tab"));
        assert_eq!(first("tab", &diff), Some("review_ui::SwitchSide"));
        assert_eq!(first("tab", &comment), Some("editor::Tab"));
        assert_eq!(first("tab", &full_comment), Some("editor::Tab"));
        for (key, action) in [
            ("alt-v", "review_ui::ToggleDiffLayout"),
            ("alt-w", "review_ui::ToggleWhitespace"),
            ("alt-a", "review_ui::ExpandFile"),
        ] {
            assert_eq!(first(key, &diff), Some(action), "{key}");
            assert_eq!(first(key, &file_list), Some(action), "{key}");
            // Where text is typed, option and a letter types a character.
            assert_eq!(first(key, &comment), None, "{key}");
            assert_eq!(first(key, &full_comment), None, "{key}");
        }
    });
}

/// Each of Lo's keys does one thing where it works: of two bindings with the same keystrokes and
/// context, the one bound later would quietly win.
#[gpui::test]
fn no_two_of_los_bindings_share_a_keystroke_and_a_context(cx: &mut TestAppContext) {
    cx.update(|cx| {
        // The test app starts with no keymap, so these are Lo's bindings alone.
        crate::bind_keys(cx);
        let keymap = cx.key_bindings();
        let keymap = keymap.borrow();
        let mut bound = HashMap::new();
        for binding in keymap.bindings() {
            let keys: Vec<String> = binding
                .keystrokes()
                .iter()
                .map(|keystroke| keystroke.inner().unparse(gpui::OperatingSystem::Mac))
                .collect();
            let context = binding
                .predicate()
                .map(|predicate| predicate.to_string())
                .unwrap_or_default();
            let action = binding.action().name();
            if let Some(earlier) = bound.insert((keys.join(" "), context.clone()), action) {
                panic!(
                    "`{}` in `{context}` is bound to both {earlier} and {action}",
                    keys.join(" ")
                );
            }
        }
        assert!(bound.len() > 30, "only {} bindings", bound.len());
    });
}
