use super::*;

#[gpui::test]
async fn the_file_keys_move_between_files_in_review_order(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let tab = open_unloaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);
    show_test_diff(
        &tab,
        &workspace,
        &[
            ("Cargo.lock", lines(5, &[]), lines(5, &[2])),
            ("src/main.rs", lines(5, &[]), lines(5, &[3])),
            ("tests/main.rs", lines(5, &[]), lines(5, &[4])),
        ],
        cx,
    )
    .await;
    // The list shows path order; the keys still follow the diff's order, review order.
    cx.simulate_keystrokes("alt-o");

    // Until something is selected, the first file in review order is the current one.
    cx.simulate_keystrokes("] f");
    assert_eq!(selected(&tab, cx).as_deref(), Some("tests/main.rs"));
    cx.simulate_keystrokes("] f");
    assert_eq!(selected(&tab, cx).as_deref(), Some("Cargo.lock"));
    cx.simulate_keystrokes("] f");
    assert_eq!(selected(&tab, cx).as_deref(), Some("Cargo.lock"));
    cx.simulate_keystrokes("[ f");
    assert_eq!(selected(&tab, cx).as_deref(), Some("tests/main.rs"));
    cx.simulate_keystrokes("[ f [ f");
    assert_eq!(selected(&tab, cx).as_deref(), Some("src/main.rs"));
}

#[gpui::test]
async fn the_change_keys_move_between_changes_across_files(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let tab = open_unloaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);
    let editor = show_test_diff(
        &tab,
        &workspace,
        &[
            ("src/a.rs", lines(30, &[]), lines(30, &[2, 25])),
            ("src/b.rs", lines(5, &[]), lines(5, &[3])),
            ("src/c.rs", lines(5, &[]), lines(5, &[4])),
        ],
        cx,
    )
    .await;
    let file = |cx: &mut VisualTestContext| cursor(&tab, &editor, cx).0;
    assert_eq!(file(cx).as_deref(), Some("src/a.rs"));

    // Both of a.rs's changes, then on into b.rs, which the list follows.
    cx.simulate_keystrokes("] c");
    let (in_file, first, _) = cursor(&tab, &editor, cx);
    assert_eq!(in_file.as_deref(), Some("src/a.rs"));
    cx.simulate_keystrokes("] c");
    let (in_file, second, _) = cursor(&tab, &editor, cx);
    assert_eq!(in_file.as_deref(), Some("src/a.rs"));
    assert!(second > first, "{first} then {second}");
    cx.simulate_keystrokes("] c");
    let (in_file, _, listed) = cursor(&tab, &editor, cx);
    assert_eq!(in_file.as_deref(), Some("src/b.rs"));
    assert_eq!(listed.as_deref(), Some("src/b.rs"));

    // The last change stays put.
    cx.simulate_keystrokes("] c");
    let last = cursor(&tab, &editor, cx);
    assert_eq!(last.0.as_deref(), Some("src/c.rs"));
    cx.simulate_keystrokes("] c");
    assert_eq!(cursor(&tab, &editor, cx), last);

    // And back, across files.
    cx.simulate_keystrokes("[ c [ c");
    let (in_file, row, listed) = cursor(&tab, &editor, cx);
    assert_eq!((in_file.as_deref(), row), (Some("src/a.rs"), second));
    assert_eq!(listed.as_deref(), Some("src/a.rs"));

    // The first change stays put too.
    cx.simulate_keystrokes("[ c");
    assert_eq!(cursor(&tab, &editor, cx).1, first);
    cx.simulate_keystrokes("[ c");
    assert_eq!(
        cursor(&tab, &editor, cx),
        (Some("src/a.rs".into()), first, Some("src/a.rs".into()))
    );
}

#[gpui::test]
async fn the_change_keys_skip_folded_files(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let tab = open_unloaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);
    let editor = show_test_diff(
        &tab,
        &workspace,
        &[
            ("src/a.rs", lines(5, &[]), lines(5, &[3])),
            ("src/b.rs", lines(5, &[]), lines(5, &[2, 4])),
            ("src/c.rs", lines(5, &[]), lines(5, &[3])),
        ],
        cx,
    )
    .await;
    // b.rs is collapsed, as a generated file starts, or as its header's chevron folds it.
    let rhs = editor.read_with(cx, |editor, _| editor.rhs_editor().clone());
    rhs.update(cx, |editor, cx| {
        let multi_buffer = editor.buffer().read(cx);
        let snapshot = multi_buffer.snapshot(cx);
        let b = multi_buffer
            .all_buffers_iter()
            .map(|buffer| buffer.read(cx).remote_id())
            .find(|id| {
                snapshot
                    .path_for_buffer(*id)
                    .is_some_and(|key| key.path.as_unix_str() == "src/b.rs")
            })
            .unwrap();
        editor.fold_buffers([b], cx);
    });
    cx.run_until_parked();

    cx.simulate_keystrokes("] c");
    assert_eq!(cursor(&tab, &editor, cx).0.as_deref(), Some("src/a.rs"));
    cx.simulate_keystrokes("] c");
    let (in_file, _, listed) = cursor(&tab, &editor, cx);
    assert_eq!(in_file.as_deref(), Some("src/c.rs"));
    assert_eq!(listed.as_deref(), Some("src/c.rs"));
    cx.simulate_keystrokes("[ c");
    assert_eq!(cursor(&tab, &editor, cx).0.as_deref(), Some("src/a.rs"));
    // b.rs stays folded.
    assert!(rhs.read_with(cx, |editor, cx| editor.has_any_buffer_folded(cx)));
}

#[gpui::test]
async fn a_change_key_back_to_where_the_cursor_started_moves_the_list_with_it(
    cx: &mut TestAppContext,
) {
    // An added first file: its one change starts at its first row, where the cursor starts.
    let files = [
        ("a.rs", String::new(), lines(3, &[])),
        ("b.rs", lines(5, &[]), lines(5, &[3])),
    ];
    let (_, tab, github, editor, cx) = signed_in_diff(
        "back_to_the_start",
        &[],
        &files,
        &[("a.rs", "UNVIEWED"), ("b.rs", "UNVIEWED")],
        cx,
    )
    .await;
    assert_eq!(cursor(&tab, &editor, cx), (Some("a.rs".into()), 0, None));

    cx.simulate_keystrokes("] c");
    assert_eq!(cursor(&tab, &editor, cx).2.as_deref(), Some("b.rs"));
    cx.simulate_keystrokes("[ c");
    assert_eq!(
        cursor(&tab, &editor, cx),
        (Some("a.rs".into()), 0, Some("a.rs".into()))
    );

    // `e` marks the file read, not the one the list showed before.
    github.github.answer_viewed(WriteAnswer::Done);
    cx.simulate_keystrokes("e");
    assert_eq!(github.github.viewed_mutations()[0].1["path"], "a.rs");
}

#[gpui::test]
async fn the_diffs_cursor_starts_in_the_first_file_in_review_order(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let tab = open_unloaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);
    // Files arrive in git's order; the diff shows them in review order.
    let editor = show_test_diff(
        &tab,
        &workspace,
        &[
            ("Cargo.lock", lines(5, &[]), lines(5, &[2])),
            ("src/main.rs", lines(5, &[]), lines(5, &[3])),
            ("tests/main.rs", lines(5, &[]), lines(5, &[4])),
        ],
        cx,
    )
    .await;

    let (in_file, row, listed) = cursor(&tab, &editor, cx);
    assert_eq!((in_file.as_deref(), row), (Some("src/main.rs"), 0));
    // Where the tab put the cursor isn't a move, so the list still selects nothing.
    assert_eq!(listed, None);
    cx.simulate_keystrokes("] f");
    assert_eq!(
        cursor(&tab, &editor, cx).0.as_deref(),
        Some("tests/main.rs")
    );
}

/// Builds a diff of `files` out of sight, then shows it in `tab`, as the local copy's diff
/// replaces GitHub's.
async fn show_built_diff(
    tab: &Entity<PrTab>,
    workspace: &Entity<Workspace>,
    files: &[(&str, String, String)],
    cx: &mut VisualTestContext,
) -> Entity<SplittableEditor> {
    let positions = list_test_files(tab, files, cx);
    let editor = test_diff_editor(workspace, cx);
    add_test_files(&editor, files, &positions, cx).await;
    cx.run_until_parked();
    tab.update_in(cx, |tab, window, cx| {
        tab.show_diff(editor.clone(), DiffSource::LocalCopy, window, cx)
    });
    cx.run_until_parked();
    editor
}

#[gpui::test]
async fn a_diff_shown_with_its_files_in_starts_in_its_first_file(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let tab = open_unloaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);
    let editor = show_built_diff(
        &tab,
        &workspace,
        &[
            ("Cargo.lock", lines(5, &[]), lines(5, &[2])),
            ("src/main.rs", lines(5, &[]), lines(5, &[3])),
            ("tests/main.rs", lines(5, &[]), lines(5, &[4])),
        ],
        cx,
    )
    .await;

    // Not at the end, in the last file, where an editor's cursor starts.
    assert_eq!(
        cursor(&tab, &editor, cx),
        (Some("src/main.rs".into()), 0, None)
    );
}

#[gpui::test]
async fn the_file_keys_move_the_diffs_cursor_to_each_file(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let tab = open_unloaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);
    let editor = show_test_diff(
        &tab,
        &workspace,
        &[
            ("src/a.rs", lines(30, &[]), lines(30, &[2, 25])),
            ("src/b.rs", lines(5, &[]), lines(5, &[3])),
            ("src/c.rs", lines(5, &[]), lines(5, &[4])),
        ],
        cx,
    )
    .await;

    // From the cursor's file, even before the list selected anything.
    cx.simulate_keystrokes("] f");
    let (in_file, _, listed) = cursor(&tab, &editor, cx);
    assert_eq!(in_file.as_deref(), Some("src/b.rs"));
    assert_eq!(listed.as_deref(), Some("src/b.rs"));

    // From a change deep in a file, the previous file is the one before it.
    cx.simulate_keystrokes("] c [ f");
    assert_eq!(cursor(&tab, &editor, cx).0.as_deref(), Some("src/a.rs"));
    cx.simulate_keystrokes("] f ] f");
    assert_eq!(cursor(&tab, &editor, cx).0.as_deref(), Some("src/c.rs"));
}

#[gpui::test]
async fn the_file_list_keeps_its_selection_when_the_local_copy_reorders_it(
    cx: &mut TestAppContext,
) {
    let (workspace, cx) = workspace(cx).await;
    let tab = open_unloaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);
    let head: Oid = "2".repeat(40).parse().unwrap();
    let files = [
        changed("a.pb.go", Change::Modified),
        changed("b.rs", Change::Modified),
    ];
    let entries = |generated: Option<bool>| -> Vec<FileEntry> {
        files
            .iter()
            .map(|file| {
                let is_pb = file.display_path().ends_with(".pb.go");
                FileEntry::from_local(file, generated.filter(|_| is_pb))
            })
            .collect()
    };
    tab.update(cx, |tab, cx| {
        tab.copied(entries(None), &head, SystemTime::now(), cx);
        tab.pull.files.select_path("a.pb.go");
    });
    assert_eq!(
        tab.read_with(cx, |tab, _| tab.files().selected_row()),
        Some(0)
    );

    // .gitattributes marks a.pb.go generated, so it lists last, still selected.
    tab.update(cx, |tab, cx| {
        tab.copied(entries(Some(true)), &head, SystemTime::now(), cx)
    });
    tab.read_with(cx, |tab, _| {
        assert_eq!(tab.files().selected().map(FileEntry::path), Some("a.pb.go"));
        assert_eq!(tab.files().selected_row(), Some(1));
    });
}

/// The row of the diff's cursor.
fn cursor_row(tab: &Entity<PrTab>, cx: &mut VisualTestContext) -> u32 {
    let editor = tab.read_with(cx, |tab, cx| {
        tab.diff().unwrap().read(cx).rhs_editor().clone()
    });
    editor.update(cx, |editor, cx| {
        let snapshot = editor.display_snapshot(cx);
        editor
            .selections
            .newest::<language::Point>(&snapshot)
            .head()
            .row
    })
}

#[gpui::test]
async fn back_and_forward_retrace_jumps_between_files(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    let lines = |prefix: &str| {
        (0..30)
            .map(|n| format!("{prefix} {n}\n"))
            .collect::<String>()
    };
    let (a, b) = (lines("a"), lines("b"));
    let pull = git.pull_request(
        "sharkdp/fd",
        1,
        &[("a.rs", "a\n"), ("b.rs", "b\n")],
        &[("a.rs", &a), ("b.rs", &b)],
    );
    github.add_pull(pull);
    github.make_public("sharkdp/fd");
    anonymous(&github, cx);
    let tab = open_loaded(&workspace, "github.com/sharkdp/fd/pull/1", cx);
    assert_eq!(status(&tab, cx), Status::Ready);

    // In the file list, j goes to the first file and then the next, and enter goes back to
    // the diff.
    cx.simulate_keystrokes("cmd-b cmd-b j");
    let at_a = cursor_row(&tab, cx);
    cx.simulate_keystrokes("j enter");
    let at_b = cursor_row(&tab, cx);
    assert!(at_b > at_a, "{at_a} → {at_b}");

    cx.simulate_keystrokes("ctrl-o");
    assert_eq!(cursor_row(&tab, cx), at_a);
    cx.simulate_keystrokes("ctrl-i");
    assert_eq!(cursor_row(&tab, cx), at_b);
}
