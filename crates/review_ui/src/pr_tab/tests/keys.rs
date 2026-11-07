use super::*;

use crate::key_clashes::Place;

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
        let tab = ["Workspace", "Pane", "PrTab middle=diff"];
        let diff = "Editor mode=full multibuffer PrDiff";
        let vim_diff = "Editor mode=full multibuffer PrDiff VimControl vim_mode=normal";
        let diffs = [
            stack(&[&tab[..], &[diff]].concat()),
            stack(&[&tab[..], &[vim_diff]].concat()),
        ];
        let file_list = stack(&[&tab[..], &["PrFileList"]].concat());
        // A comment box in the diff, as milestone 8 adds. While it has focus, Zed leaves the
        // diff's own context, `PrDiff` and vim's included, out of the stack. Without that, the
        // box's size alone keeps Lo's keys out.
        let text_boxes = [
            stack(
                &[
                    &tab[..],
                    &["Editor mode=full multibuffer", "Editor mode=auto_height"],
                ]
                .concat(),
            ),
            stack(&[&tab[..], &[diff, "Editor mode=auto_height"]].concat()),
            stack(
                &[
                    &tab[..],
                    &[
                        "Editor mode=full multibuffer",
                        "Editor mode=auto_height VimControl vim_mode=normal",
                    ],
                ]
                .concat(),
            ),
        ];
        // Any other text box in the tab.
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
            for diff in &diffs {
                assert_eq!(lo(keys, diff).0, [action], "{keys} in the diff");
            }
            assert_eq!(lo(keys, &file_list).0, [action], "{keys} in the file list");
            for contexts in text_boxes.iter().chain([&line]) {
                assert_eq!(lo(keys, contexts).0, [""; 0], "{keys} in a text box");
            }
        }
        // A bracket types at once, not waiting for a second key.
        for prefix in ["]", "["] {
            assert!(lo(prefix, &diffs[0]).1, "{prefix} waits in the diff");
            assert!(
                !lo(prefix, &text_boxes[0]).1,
                "{prefix} types in a text box"
            );
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
        let tab = "PrTab middle=diff";
        let diff = [
            "Workspace",
            "Pane",
            tab,
            "Editor mode=full multibuffer PrDiff",
        ];
        let file_list = ["Workspace", "Pane", tab, "PrFileList"];
        let comment = ["Workspace", "Pane", tab, "Editor mode=auto_height"];
        // A comment box can be a full editor too, of one buffer.
        let full_comment = ["Workspace", "Pane", tab, "Editor mode=full"];
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

/// Each of Lo's keys does one thing wherever it's typed: on the context stack of every place in the
/// window that takes keys, read from a window showing it, no two of Lo's bindings hold with the
/// same keystrokes, or with one's starting the other's (see [`crate::key_clashes`]). Every binding
/// holds in one of the places, so a binding in a new context needs a place here. Where one of
/// Lo's and one of vim's fight, `keys::OVER_VIM` or `keys::UNDER_VIM` says which wins. The `lo`
/// crate checks its Accounts modal the same way.
#[gpui::test]
async fn no_two_of_los_bindings_fight_where_keys_are_typed(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let mut places = Vec::new();
    // The focused element's context stack, which ends in `last`.
    let focused = |name: &str, last: &str, cx: &mut VisualTestContext| {
        cx.run_until_parked();
        let stack = cx.update(|window, _| window.context_stack());
        assert!(
            stack.last().is_some_and(|context| context.contains(last)),
            "{name}: {stack:?}"
        );
        Place {
            name: name.into(),
            stack,
        }
    };

    workspace.update_in(cx, |workspace, window, cx| {
        crate::recent_prs::show(workspace, window, cx)
    });
    places.push(focused("the recent list", "RecentPullRequests", cx));

    workspace.update_in(cx, |workspace, window, cx| {
        crate::OpenBox::toggle(workspace, window, cx)
    });
    places.push(focused("the open box", "Editor", cx));
    cx.dispatch_action(menu::Cancel);

    let tab = open_unloaded(&workspace, "github.com/owner/repo/pull/1", cx);
    tab.update_in(cx, |tab, window, cx| {
        window.focus(&tab.focus_handle(cx), cx)
    });
    places.push(focused("a tab without a diff", "PrTab", cx));
    tab.update_in(cx, |tab, window, cx| {
        window.focus(tab.file_list_focus_handle(), cx)
    });
    places.push(focused("the file list", "PrFileList", cx));

    let files = [("a.rs", lines(5, &[]), lines(5, &[3]))];
    let diff = show_test_diff(&tab, &workspace, &files, cx).await;
    tab.update_in(cx, |tab, window, cx| tab.focus_diff(window, cx));
    places.push(focused("the diff", "Editor", cx));
    let diff_stack = places.last().unwrap().stack.clone();
    // Vim's other modes in the diff, and while it waits for the rest of a command.
    for (name, keys) in [
        ("the diff in vim's visual mode", "v"),
        ("the diff in vim's insert mode", "i"),
        ("the diff after vim's `y`", "y"),
        ("the diff after vim's `f`", "f"),
        ("the diff with a count", "3"),
    ] {
        cx.simulate_keystrokes(keys);
        places.push(focused(name, "Editor", cx));
        cx.simulate_keystrokes("escape");
    }
    cx.update(|window, cx| {
        let left = diff.read(cx).lhs_editor().unwrap().focus_handle(cx);
        window.focus(&left, cx)
    });
    places.push(focused("the diff's left side", "Editor", cx));
    // A view in the diff's place, such as the timeline.
    let view = cx.new(Probe::new);
    tab.update_in(cx, |tab, window, cx| {
        tab.show_middle(Middle::View(PartView::new(&view, cx)), window, cx)
    });
    places.push(focused("a view in the diff's place", "Probe", cx));
    tab.update_in(cx, |tab, window, cx| {
        tab.show_middle(Middle::Diff, window, cx)
    });
    // A text box in the diff, such as a comment box, with vim's keys, and a line of text in the
    // tab. While the box has focus, Zed leaves the diff's own context out of the stack, `PrDiff`
    // and vim's included.
    for (name, editor) in [
        (
            "a text box in the diff",
            cx.update(|window, cx| {
                cx.new(|cx| crate::vim_mode::modal(Editor::auto_height(1, 10, window, cx)))
            }),
        ),
        (
            "a line of text in the tab",
            cx.update(|window, cx| cx.new(|cx| Editor::single_line(window, cx))),
        ),
    ] {
        let mut stack = diff_stack.clone();
        stack.pop();
        if name.ends_with("in the diff") {
            stack.push(gpui::KeyContext::parse("Editor mode=full multibuffer").unwrap());
        }
        stack.push(
            cx.update(|window, cx| editor.update(cx, |editor, cx| editor.key_context(window, cx))),
        );
        places.push(Place {
            name: name.into(),
            stack,
        });
    }

    // Two accounts can see acme/app, so opening it asks which to use.
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    let pull = git.pull_request("acme/app", 1, &[], &[("a.rs", "a\n")]);
    github.add_pull(pull);
    github.let_see("gho_HOME", "acme/app");
    github.let_see("gho_WORK", "acme/app");
    sign_in(&github, &[("home", "gho_HOME"), ("work", "gho_WORK")], cx).await;
    open_loaded(&workspace, "github.com/acme/app/pull/1", cx);
    places.push(focused("the account picker", "AccountPicker", cx));

    cx.update(|_, cx| {
        // Lo's bindings alone.
        cx.clear_key_bindings();
        crate::bind_keys(cx);
        let lo: Vec<gpui::KeyBinding> = cx.key_bindings().borrow().bindings().cloned().collect();
        assert!(lo.len() > 30);
        let unplaced = crate::key_clashes::unplaced(&lo, &places);
        assert!(unplaced.is_empty(), "{unplaced:#?}");
        let clashes = crate::key_clashes::clashes(&lo, &places);
        assert!(clashes.is_empty(), "{clashes:#?}");
        // And with vim's.
        let vim = crate::key_clashes::vim_bindings(cx);
        assert!(vim.len() > 300);
        let fights = crate::key_clashes::with_vim(
            &lo,
            &vim,
            &places,
            crate::pr_tab::keys::OVER_VIM,
            crate::pr_tab::keys::UNDER_VIM,
        );
        assert!(fights.is_empty(), "{fights:#?}");
    });
}
