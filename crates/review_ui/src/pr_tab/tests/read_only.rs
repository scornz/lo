use super::*;

/// Drags `path` from Finder and drops it at the middle of the window.
fn drop_file(path: &str, cx: &mut VisualTestContext) {
    let position = gpui::point(px(500.), px(400.));
    cx.simulate_event(gpui::FileDropEvent::Entered {
        position,
        paths: gpui::ExternalPaths([std::path::PathBuf::from(path)].into_iter().collect()),
    });
    cx.simulate_event(gpui::FileDropEvent::Pending { position });
    cx.simulate_event(gpui::FileDropEvent::Submit { position });
    cx.run_until_parked();
}

#[gpui::test]
async fn a_file_dropped_on_an_empty_pane_opens_nothing(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;

    drop_file("/elsewhere/src/main.rs", cx);
    drop_file("/elsewhere", cx);

    workspace.read_with(cx, |workspace, cx| {
        assert_eq!(workspace.items(cx).count(), 0)
    });
    assert_eq!(worktrees(&workspace, cx), 0);
}

#[gpui::test]
async fn a_file_dropped_on_a_pull_requests_tab_opens_nothing(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let tab = open_unloaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);

    drop_file("/elsewhere/src/main.rs", cx);

    assert_eq!(tabs(&workspace, cx), vec![tab.clone()]);
    workspace.read_with(cx, |workspace, cx| {
        assert_eq!(workspace.items(cx).count(), 1)
    });
    assert_eq!(worktrees(&workspace, cx), 0);

    // Even if a drop reaches the tab, the tab keeps it from the pane.
    let dropped = gpui::ExternalPaths(
        [std::path::PathBuf::from("/elsewhere/src/main.rs")]
            .into_iter()
            .collect(),
    );
    let pane = workspace.read_with(cx, |workspace, _| workspace.active_pane().clone());
    let swallowed = cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            tab.update(cx, |tab, cx| tab.handle_drop(pane, &dropped, window, cx))
        })
    });
    assert!(swallowed);
}

#[gpui::test]
async fn no_file_action_runs_or_shows_in_the_command_palette(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    // Where the command palette sends an action: what had focus, here an empty pane, which a
    // double click in the tab bar also sends NewFile to.
    workspace.update_in(cx, |workspace, window, cx| {
        window.focus(&workspace.active_pane().focus_handle(cx), cx)
    });
    cx.run_until_parked();

    let names = crate::read_only::FileActions::names();
    for name in [
        "workspace::NewFile",
        "workspace::NewFileSplit",
        "workspace::NewFileSplitVertical",
        "workspace::NewFileSplitHorizontal",
        "workspace::NewWindow",
        "workspace::SaveAs",
        "workspace::Open",
    ] {
        assert!(names.contains(&name), "{name}");
    }
    for name in names {
        // Two take an argument: which recent project, and which way to split.
        let data = match name {
            "welcome::OpenRecentProject" => Some(serde_json::json!(0)),
            "workspace::NewFileSplit" => Some(serde_json::json!("right")),
            _ => None,
        };
        let action = cx.update(|_, cx| cx.build_action(name, data)).unwrap();
        let hidden = cx.update(|_, cx| {
            command_palette_hooks::CommandPaletteFilter::try_global(cx)
                .is_some_and(|filter| filter.is_hidden(&*action))
        });
        assert!(hidden, "the command palette lists {name}");
        cx.update(|window, cx| window.dispatch_action(action.boxed_clone(), cx));
        cx.run_until_parked();
        assert_eq!(
            workspace.read_with(cx, |workspace, cx| workspace.items(cx).count()),
            0,
            "{name} opened something"
        );
        assert_eq!(cx.update(|_, cx| cx.windows().len()), 1, "{name}");
        assert_eq!(worktrees(&workspace, cx), 0, "{name}");
    }
}

#[gpui::test]
async fn the_window_opens_editors_read_only(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    // A trusted checkout is a folder of the project, and a file header's Open File opens one
    // of its files in an ordinary editor.
    let project = workspace.read_with(cx, |workspace, _| workspace.project().clone());
    let (worktree, _) = project
        .update(cx, |project, cx| {
            project.find_or_create_worktree("/elsewhere", true, cx)
        })
        .await
        .unwrap();
    cx.run_until_parked();
    let worktree_id = worktree.read_with(cx, |worktree, _| worktree.id());
    let path = RelPath::from_unix_str("src/main.rs").unwrap().into_arc();
    let item = workspace
        .update_in(cx, |workspace, window, cx| {
            workspace.open_path((worktree_id, path), None, true, window, cx)
        })
        .await
        .unwrap();

    let editor = item.downcast::<Editor>().unwrap();
    assert_eq!(
        workspace.read_with(cx, |workspace, cx| workspace.items(cx).count()),
        1
    );
    assert!(editor.read_with(cx, |editor, cx| editor.read_only(cx)));
}
