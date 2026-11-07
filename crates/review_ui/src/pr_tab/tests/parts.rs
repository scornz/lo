use super::*;

/// Whether `handle` has focus, or one of its children does.
fn has_focus(handle: &FocusHandle, cx: &mut VisualTestContext) -> bool {
    cx.update(|window, cx| handle.contains_focused(window, cx))
}

/// Views in the tab's parts take focus, keep the diff's keys off the hidden diff, and give focus
/// back when they hide or go.
#[gpui::test]
async fn views_in_the_tabs_parts_take_focus_and_give_it_back(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let tab = open_unloaded(&workspace, "github.com/owner/repo/pull/1", cx);
    let tab_focus = tab.read_with(cx, |tab, cx| tab.focus_handle(cx));
    tab.update_in(cx, |tab, window, cx| {
        window.focus(&tab.focus_handle(cx), cx)
    });

    // Before there's a diff, a view in its place takes focus, and the diff's place gives it to
    // the tab.
    let view = cx.new(Probe::new);
    let view_focus = view.read_with(cx, |view, _| view.focus.clone());
    tab.update_in(cx, |tab, window, cx| {
        tab.show_middle(Middle::View(PartView::new(&view, cx)), window, cx)
    });
    cx.run_until_parked();
    assert!(has_focus(&view_focus, cx));
    assert!(tab.read_with(cx, |tab, _| tab.middle().shows(view.entity_id())));
    tab.update_in(cx, |tab, window, cx| {
        tab.show_middle(Middle::Diff, window, cx)
    });
    cx.run_until_parked();
    assert!(cx.update(|window, _| tab_focus.is_focused(window)));

    // With the diff, the diff's keys don't reach it while a view shows in its place.
    let files = [
        ("a.rs", lines(5, &[]), lines(5, &[3])),
        ("b.rs", lines(5, &[]), lines(5, &[3])),
    ];
    let diff = show_test_diff(&tab, &workspace, &files, cx).await;
    let diff_focus = diff.read_with(cx, |diff, cx| diff.focus_handle(cx));
    tab.update_in(cx, |tab, window, cx| tab.focus_diff(window, cx));
    tab.update_in(cx, |tab, window, cx| {
        tab.show_middle(Middle::View(PartView::new(&view, cx)), window, cx)
    });
    cx.run_until_parked();
    assert!(has_focus(&view_focus, cx));
    cx.simulate_keystrokes("e ] f");
    assert_eq!(toasts(cx), Vec::<String>::new());
    assert_eq!(cursor_file(&tab, cx).as_deref(), Some("a.rs"));
    tab.update_in(cx, |tab, window, cx| {
        tab.show_middle(Middle::Diff, window, cx)
    });
    cx.run_until_parked();
    assert!(has_focus(&diff_focus, cx));
    cx.simulate_keystrokes("] f");
    assert_eq!(cursor_file(&tab, cx).as_deref(), Some("b.rs"));

    // The stack tree shows and takes focus, then hides and gives it back, as it does when it goes.
    let tree = cx.new(Probe::new);
    let tree_focus = tree.read_with(cx, |tree, _| tree.focus.clone());
    tab.update_in(cx, |tab, window, cx| {
        tab.set_stack_tree(Some(PartView::new(&tree, cx)), window, cx);
        tab.toggle_stack_tree(window, cx);
    });
    cx.run_until_parked();
    assert!(tab.read_with(cx, |tab, _| tab.is_stack_tree_shown()));
    assert!(has_focus(&tree_focus, cx));
    tab.update_in(cx, |tab, window, cx| tab.toggle_stack_tree(window, cx));
    cx.run_until_parked();
    assert!(!tab.read_with(cx, |tab, _| tab.is_stack_tree_shown()));
    assert!(has_focus(&diff_focus, cx));
    tab.update_in(cx, |tab, window, cx| tab.toggle_stack_tree(window, cx));
    cx.run_until_parked();
    assert!(has_focus(&tree_focus, cx));
    tab.update_in(cx, |tab, window, cx| tab.set_stack_tree(None, window, cx));
    cx.run_until_parked();
    assert!(has_focus(&diff_focus, cx));

    // The threads panel gives focus back when its key hides it, and when it goes.
    let panel = cx.new(Probe::new);
    let panel_focus = panel.read_with(cx, |panel, _| panel.focus.clone());
    tab.update_in(cx, |tab, window, cx| {
        tab.set_threads_panel(Some(PartView::new(&panel, cx)), window, cx)
    });
    cx.run_until_parked();
    cx.update(|window, cx| window.focus(&panel_focus, cx));
    cx.run_until_parked();
    assert!(has_focus(&panel_focus, cx));
    cx.simulate_keystrokes("cmd-r");
    assert!(!tab.read_with(cx, |tab, _| tab.is_threads_panel_shown()));
    assert!(has_focus(&diff_focus, cx));
    cx.simulate_keystrokes("cmd-r");
    cx.update(|window, cx| window.focus(&panel_focus, cx));
    cx.run_until_parked();
    tab.update_in(cx, |tab, window, cx| {
        tab.set_threads_panel(None, window, cx)
    });
    cx.run_until_parked();
    assert!(has_focus(&diff_focus, cx));
}
