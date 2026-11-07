use super::*;

/// Whether the workspace shows the last viewed-mark toast a tab showed.
fn has_viewed_toast(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> bool {
    let Some((id, _, _)) = toast_log(cx).pop() else {
        return false;
    };
    workspace.read_with(cx, |workspace, _| workspace.has_notification(&id))
}

#[gpui::test]
async fn e_before_the_diff_shows_a_file_marks_nothing_and_says_why(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let tab = open_unloaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);
    let github = sign_in_bot(&tab, "e_before_the_diff", &[], cx).await;
    let head: Oid = HEAD.parse().unwrap();
    tab.update(cx, |tab, cx| {
        tab.copied(
            local_entries(&[changed("a.rs", Change::Modified)]),
            &head,
            SystemTime::now(),
            cx,
        );
        tab.set_details(&details_with(&[("a.rs", "UNVIEWED")]), cx);
    });

    // The list has the file, but the diff doesn't exist yet.
    cx.simulate_keystrokes("e");
    assert_eq!(
        toasts(cx),
        ["The diff is still loading, so there's no file to mark viewed yet."]
    );
    assert_eq!(marks(&tab, cx), [mark("a.rs", Some(ViewedState::Unviewed))]);
    assert_eq!(github.github.viewed_mutations(), []);

    // Nor once it exists, until a file is in it.
    let editor = test_diff_editor(&workspace, cx);
    tab.update_in(cx, |tab, window, cx| {
        tab.show_diff(editor.clone(), DiffSource::LocalCopy, window, cx)
    });
    cx.simulate_keystrokes("e");
    assert_eq!(toasts(cx).len(), 2);
    assert_eq!(github.github.viewed_mutations(), []);
}

#[gpui::test]
async fn e_acts_on_the_file_at_the_top_of_the_view_when_the_cursor_is_out_of_it(
    cx: &mut TestAppContext,
) {
    let files = [
        ("a.rs", String::new(), lines(200, &[])),
        ("b.rs", String::new(), lines(200, &[])),
    ];
    let (_, tab, github, editor, cx) = signed_in_diff(
        "e_at_the_top",
        &[],
        &files,
        &[("a.rs", "UNVIEWED"), ("b.rs", "UNVIEWED")],
        cx,
    )
    .await;
    // Scrolling doesn't move the cursor, which stays at a.rs's start.
    let rhs = editor.read_with(cx, |editor, _| editor.rhs_editor().clone());
    rhs.update_in(cx, |editor, window, cx| {
        editor.set_scroll_position(gpui::point(0., 260.), window, cx);
    });
    cx.run_until_parked();
    assert_eq!(cursor(&tab, &editor, cx).0.as_deref(), Some("a.rs"));
    // The list follows the view.
    assert_eq!(selected(&tab, cx).as_deref(), Some("b.rs"));

    github.github.answer_viewed(WriteAnswer::Done);
    cx.simulate_keystrokes("e");
    assert_eq!(github.github.viewed_mutations()[0].1["path"], "b.rs");
}

#[gpui::test]
async fn e_and_the_file_keys_move_only_to_files_in_the_diff(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let tab = open_unloaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);
    let github = sign_in_bot(&tab, "e_while_loading", &[], cx).await;
    let files = [
        ("a.rs", lines(5, &[]), lines(5, &[3])),
        ("b.rs", lines(5, &[]), lines(5, &[3])),
        ("c.rs", lines(5, &[]), lines(5, &[3])),
    ];
    let positions = list_test_files(&tab, &files, cx);
    tab.update(cx, |tab, cx| {
        tab.set_details(
            &details_with(&[
                ("a.rs", "UNVIEWED"),
                ("b.rs", "UNVIEWED"),
                ("c.rs", "UNVIEWED"),
            ]),
            cx,
        )
    });
    let editor = test_diff_editor(&workspace, cx);
    tab.update_in(cx, |tab, window, cx| {
        tab.show_diff(editor.clone(), DiffSource::LocalCopy, window, cx)
    });
    // Only a.rs has arrived.
    add_test_files(&editor, &files[..1], &positions[..1], cx).await;
    cx.run_until_parked();

    // There's no file in the diff to move to.
    cx.simulate_keystrokes("] f");
    assert_eq!(cursor(&tab, &editor, cx).0.as_deref(), Some("a.rs"));

    // e marks a.rs, and says why it stays: b.rs, the next unviewed file, isn't in yet.
    github.github.answer_viewed(WriteAnswer::Done);
    cx.simulate_keystrokes("e");
    assert_eq!(github.github.viewed_mutations()[0].1["path"], "a.rs");
    assert_eq!(cursor(&tab, &editor, cx).0.as_deref(), Some("a.rs"));
    assert_ne!(selected(&tab, cx).as_deref(), Some("b.rs"));
    assert_eq!(
        toasts(cx),
        ["b.rs, the next unviewed file, is still loading, so the diff stays on a.rs."]
    );

    // c.rs arrives before b.rs, and the keys skip b.rs while it isn't in.
    add_test_files(&editor, &files[2..], &positions[2..], cx).await;
    cx.run_until_parked();
    cx.simulate_keystrokes("] f");
    assert_eq!(cursor(&tab, &editor, cx).0.as_deref(), Some("c.rs"));
}

#[gpui::test]
async fn e_marks_the_file_viewed_and_moves_to_the_next_unviewed_one(cx: &mut TestAppContext) {
    let (workspace, tab, github, cx) = three_files(
        "e_marks",
        &[],
        &[
            ("a.rs", "UNVIEWED"),
            ("b.rs", "VIEWED"),
            ("c.rs", "DISMISSED"),
        ],
        cx,
    )
    .await;
    github.github.answer_viewed(WriteAnswer::Done);

    // GitHub's head is the head on screen, and the diff's cursor starts in the first file.
    cx.simulate_keystrokes("e");

    use ViewedState::*;
    assert_eq!(
        marks(&tab, cx),
        [
            mark("a.rs", Some(Viewed)),
            mark("b.rs", Some(Viewed)),
            mark("c.rs", Some(Dismissed))
        ]
    );
    // b.rs is viewed, and a file changed since viewed counts as unviewed.
    assert_eq!(selected(&tab, cx).as_deref(), Some("c.rs"));
    assert_eq!(
        github.github.viewed_mutations(),
        [(
            "MarkFileAsViewed".to_string(),
            serde_json::json!({ "pullRequestId": "PR_kwDOVk6v9M8AAAABNQwFBw", "path": "a.rs" })
        )]
    );
    // This Mac records the head on screen when the file was marked.
    assert_eq!(
        github.recorded(),
        HashMap::from([("a.rs".to_string(), HEAD.to_string())])
    );
    assert!(!has_viewed_toast(&workspace, cx));
}

/// GitHub's REST view of PR #2 with its head at `head`, as the poller's summary carries it.
fn summary_at(head: &str) -> github::PullSummary {
    serde_json::from_value(serde_json::json!({
        "number": 2,
        "title": "Add b",
        "state": "open",
        "updated_at": "2026-11-06T09:00:00Z",
        "head": { "ref": "stack-b", "sha": head },
        "base": { "ref": "stack-a", "sha": "b046244d9569964088e952243e5e557312322488" },
    }))
    .unwrap()
}

#[gpui::test]
async fn after_a_newer_push_e_wont_mark_a_file_but_still_clears_one(cx: &mut TestAppContext) {
    let marks_now = [
        ("a.rs", "UNVIEWED"),
        ("b.rs", "VIEWED"),
        ("c.rs", "UNVIEWED"),
    ];
    let (_, tab, github, cx) = three_files("e_after_a_push", &[], &marks_now, cx).await;
    // GitHub's details say the pull request moved on from the head on screen.
    tab.update(cx, |tab, cx| {
        tab.set_details(&details_at(NEWER, &marks_now), cx)
    });
    let before = marks(&tab, cx);

    // markFileAsViewed takes no commit, so GitHub would mark a.rs as of the newer push.
    cx.simulate_keystrokes("e");
    assert_eq!(marks(&tab, cx), before);
    assert_eq!(github.github.viewed_mutations(), []);
    assert_eq!(cursor_file(&tab, cx).as_deref(), Some("a.rs"));
    let toast = toast_log(cx).pop().unwrap();
    assert_eq!(
        toast.1,
        "There's a newer push to this pull request than the one on screen, so Lo didn't \
         mark a.rs viewed: GitHub would mark it as of that push. Load it to mark files viewed."
    );
    assert_eq!(toast.2.as_deref(), Some("Load the newer push"));

    // Clearing a mark is always allowed.
    go_to(&tab, "b.rs", cx);
    github.github.answer_viewed(WriteAnswer::Done);
    cx.simulate_keystrokes("e");
    assert_eq!(github.github.viewed_mutations()[0].0, "UnmarkFileAsViewed");
}

#[gpui::test]
async fn a_newer_push_in_githubs_summary_also_stops_e_marking(cx: &mut TestAppContext) {
    let (_, tab, github, cx) =
        three_files("e_after_a_summary", &[], &[("a.rs", "UNVIEWED")], cx).await;
    let pull = github.pull.clone();
    tab.update(cx, |tab, cx| {
        tab.poll_event(
            &PollEvent::Summary {
                pull,
                summary: std::sync::Arc::new(summary_at(NEWER)),
            },
            cx,
        )
    });

    cx.simulate_keystrokes("e");
    assert_eq!(github.github.viewed_mutations(), []);
    assert!(toasts(cx)[0].starts_with("There's a newer push"));

    // Older details, from before the summary, don't bring the old head back.
    let older = SystemTime::now() - Duration::from_secs(60);
    tab.update(cx, |tab, cx| {
        tab.take_details(&details_with(&[("a.rs", "UNVIEWED")]), older, Some(BOT), cx)
    });
    cx.simulate_keystrokes("e");
    assert_eq!(github.github.viewed_mutations(), []);
}

#[gpui::test]
async fn an_older_head_from_before_the_diff_loaded_doesnt_stop_e(cx: &mut TestAppContext) {
    let (_, tab, github, cx) = three_files("e_after_saved", &[], &[("a.rs", "UNVIEWED")], cx).await;
    // What Lo saved, from before this diff's head was pushed.
    let saved_at = SystemTime::now() - Duration::from_secs(3600);
    tab.update(cx, |tab, cx| {
        tab.latest_head = None;
        tab.take_details(
            &details_at(
                "1111111111111111111111111111111111111111",
                &[("a.rs", "UNVIEWED")],
            ),
            saved_at,
            Some(BOT),
            cx,
        )
    });

    github.github.answer_viewed(WriteAnswer::Done);
    cx.simulate_keystrokes("e");
    assert_eq!(github.github.viewed_mutations()[0].0, "MarkFileAsViewed");
}

#[gpui::test]
async fn loading_the_newer_push_reopens_the_tab_in_its_place(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let first = open_unloaded(&workspace, "https://github.com/sharkdp/fd/pull/1", cx);
    let tab = open_unloaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);
    open_unloaded(&workspace, "https://github.com/sharkdp/fd/pull/3", cx);

    let reopened = workspace
        .update_in(cx, |workspace, window, cx| {
            reopen_with(workspace, &tab, window, cx, |_, _, _| {})
        })
        .unwrap();
    cx.run_until_parked();

    let tabs = tabs(&workspace, cx);
    assert_eq!(tabs.len(), 3);
    assert_eq!(tabs[0], first);
    assert_eq!(tabs[1], reopened);
    assert_ne!(reopened, tab);
    assert_eq!(
        reopened.read_with(cx, |tab, _| tab.link().to_string()),
        "sharkdp/fd#2212"
    );
    assert_eq!(active_tab(&workspace, cx), Some(reopened));
}

#[gpui::test]
async fn e_wont_mark_a_file_github_doesnt_list(cx: &mut TestAppContext) {
    // GitHub lists every file of the pull request, and not c.rs.
    let (_, tab, github, cx) = three_files(
        "e_unlisted",
        &[],
        &[("a.rs", "UNVIEWED"), ("b.rs", "UNVIEWED")],
        cx,
    )
    .await;
    go_to(&tab, "c.rs", cx);

    cx.simulate_keystrokes("e");

    assert_eq!(github.github.viewed_mutations(), []);
    assert_eq!(marks(&tab, cx)[2], mark("c.rs", None));
    assert_eq!(
        toasts(cx),
        [
            "GitHub doesn't list c.rs in this pull request, so Lo can't mark it viewed. \
          GitHub and the local copy see this file's change differently."
        ]
    );
}

#[gpui::test]
async fn e_marks_a_renamed_file_at_the_path_github_lists(cx: &mut TestAppContext) {
    let files = [
        ("a.rs", lines(5, &[]), lines(5, &[3])),
        ("new.rs", lines(5, &[]), lines(5, &[3])),
    ];
    // The local copy sees a rename; GitHub lists the file by its old path.
    let (_, tab, github, _, cx) = signed_in_diff(
        "e_renamed",
        &[],
        &files,
        &[("a.rs", "UNVIEWED"), ("old.rs", "UNVIEWED")],
        cx,
    )
    .await;
    let mut renamed = changed("new.rs", Change::Renamed { similarity: 90 });
    renamed.old.as_mut().unwrap().path = "old.rs".into();
    let head: Oid = HEAD.parse().unwrap();
    tab.update(cx, |tab, cx| {
        tab.copied(
            local_entries(&[changed("a.rs", Change::Modified), renamed]),
            &head,
            SystemTime::now(),
            cx,
        );
    });
    assert_eq!(
        marks(&tab, cx)[1],
        mark("new.rs", Some(ViewedState::Unviewed))
    );
    go_to(&tab, "new.rs", cx);

    github.github.answer_viewed(WriteAnswer::Done);
    cx.simulate_keystrokes("e");
    cx.run_until_parked();

    assert_eq!(github.github.viewed_mutations()[0].1["path"], "old.rs");
    assert_eq!(
        marks(&tab, cx)[1],
        mark("new.rs", Some(ViewedState::Viewed))
    );
    assert_eq!(
        github.recorded(),
        HashMap::from([("old.rs".to_string(), HEAD.to_string())])
    );
}

#[gpui::test]
async fn writes_waiting_when_the_tab_closes_still_record_and_say_why_they_failed(
    cx: &mut TestAppContext,
) {
    let (workspace, tab, github, cx) = three_files(
        "e_then_close",
        &[],
        &[
            ("a.rs", "UNVIEWED"),
            ("b.rs", "UNVIEWED"),
            ("c.rs", "UNVIEWED"),
        ],
        cx,
    )
    .await;
    github.github.answer_viewed(WriteAnswer::Done);
    github.github.answer_viewed(WriteAnswer::Done);
    github.github.answer_viewed(WriteAnswer::Forbidden);

    // Writes are a second apart, so the second and third wait while the tab closes.
    cx.simulate_keystrokes("e");
    cx.simulate_keystrokes("e");
    cx.simulate_keystrokes("e");
    close(&workspace, tab, cx);
    assert!(tabs(&workspace, cx).is_empty());
    cx.executor().advance_clock(Duration::from_secs(3));
    cx.run_until_parked();

    assert_eq!(github.github.viewed_mutations().len(), 3);
    assert_eq!(
        github.recorded(),
        HashMap::from([
            ("a.rs".to_string(), HEAD.to_string()),
            ("b.rs".to_string(), HEAD.to_string()),
        ])
    );
    assert_eq!(
        toasts(cx),
        [
            "Couldn't mark c.rs viewed: rozed-bot isn't allowed to (Resource not accessible by \
          integration). It shows GitHub's mark again."
        ]
    );
}

#[gpui::test]
async fn marks_cleared_or_set_elsewhere_update_this_macs_records(cx: &mut TestAppContext) {
    const R1: &str = "1111111111111111111111111111111111111111";
    let (_, tab, github, cx) = three_files(
        "records_follow_github",
        &[("a.rs", R1), ("b.rs", R1)],
        &[("a.rs", "VIEWED"), ("b.rs", "VIEWED"), ("c.rs", "UNVIEWED")],
        cx,
    )
    .await;
    let viewed_at = |cx: &mut VisualTestContext| tab.read_with(cx, |tab, _| tab.viewed_at.clone());
    let both = HashMap::from([
        ("a.rs".to_string(), R1.to_string()),
        ("b.rs".to_string(), R1.to_string()),
    ]);
    // Marks set before the tab opened keep their records.
    assert_eq!(github.recorded(), both);

    // a.rs's mark cleared on the web, and b.rs changed by a push.
    tab.update(cx, |tab, cx| {
        tab.set_details(
            &details_with(&[
                ("a.rs", "UNVIEWED"),
                ("b.rs", "DISMISSED"),
                ("c.rs", "UNVIEWED"),
            ]),
            cx,
        )
    });
    cx.run_until_parked();
    let b_only = HashMap::from([("b.rs".to_string(), R1.to_string())]);
    assert_eq!(github.recorded(), b_only);
    assert_eq!(viewed_at(cx), b_only);

    // Both marked again on another Mac, at the head GitHub has then.
    tab.update(cx, |tab, cx| {
        tab.set_details(
            &details_with(&[("a.rs", "VIEWED"), ("b.rs", "VIEWED"), ("c.rs", "UNVIEWED")]),
            cx,
        )
    });
    cx.run_until_parked();
    let at_head = HashMap::from([
        ("a.rs".to_string(), HEAD.to_string()),
        ("b.rs".to_string(), HEAD.to_string()),
    ]);
    assert_eq!(github.recorded(), at_head);
    assert_eq!(viewed_at(cx), at_head);
}

#[gpui::test]
async fn each_failed_write_has_its_own_toast_and_the_selection_stays(cx: &mut TestAppContext) {
    let (workspace, tab, github, cx) = three_files(
        "e_fails_twice",
        &[],
        &[
            ("a.rs", "UNVIEWED"),
            ("b.rs", "UNVIEWED"),
            ("c.rs", "UNVIEWED"),
        ],
        cx,
    )
    .await;
    github.github.answer_viewed(WriteAnswer::Forbidden);
    github.github.answer_viewed(WriteAnswer::Forbidden);

    cx.simulate_keystrokes("e");
    cx.simulate_keystrokes("e");
    assert_eq!(selected(&tab, cx).as_deref(), Some("c.rs"));
    cx.executor().advance_clock(Duration::from_secs(2));
    cx.run_until_parked();

    // Both say what failed, one toast a file.
    let log = toast_log(cx);
    assert_eq!(log.len(), 2);
    assert!(
        log[0].1.starts_with("Couldn't mark a.rs viewed"),
        "{}",
        log[0].1
    );
    assert!(
        log[1].1.starts_with("Couldn't mark b.rs viewed"),
        "{}",
        log[1].1
    );
    assert_ne!(log[0].0, log[1].0);
    for (id, _, _) in &log {
        assert!(workspace.read_with(cx, |workspace, _| workspace.has_notification(id)));
    }
    // Only the marks went back: the failures don't move the selection.
    assert_eq!(selected(&tab, cx).as_deref(), Some("c.rs"));
    assert_eq!(
        marks(&tab, cx),
        [
            mark("a.rs", Some(ViewedState::Unviewed)),
            mark("b.rs", Some(ViewedState::Unviewed)),
            mark("c.rs", Some(ViewedState::Unviewed))
        ]
    );
}

#[gpui::test]
async fn while_github_cant_be_reached_e_marks_nothing_and_stays(cx: &mut TestAppContext) {
    let (_, tab, github, cx) = three_files("e_offline", &[], &[("a.rs", "UNVIEWED")], cx).await;
    tab.update(cx, |tab, _| {
        tab.poll.status.offline = Some(github::poll::Offline {
            since: SystemTime::now(),
            retry_at: SystemTime::now() + Duration::from_secs(30),
            error: "the network is down".into(),
        })
    });

    cx.simulate_keystrokes("e");

    assert_eq!(github.github.viewed_mutations(), []);
    assert_eq!(
        marks(&tab, cx)[0],
        mark("a.rs", Some(ViewedState::Unviewed))
    );
    assert_eq!(cursor_file(&tab, cx).as_deref(), Some("a.rs"));
    assert_eq!(
        toasts(cx),
        ["Lo can't reach GitHub, so it can't mark a.rs viewed until it's back."]
    );
}

#[gpui::test]
async fn marks_github_read_before_a_write_finished_dont_bring_the_old_mark_back(
    cx: &mut TestAppContext,
) {
    let (_, tab, github, cx) = three_files(
        "e_then_switch_tabs",
        &[],
        &[("a.rs", "UNVIEWED"), ("b.rs", "UNVIEWED")],
        cx,
    )
    .await;
    let before_the_write = SystemTime::now() - Duration::from_millis(1);
    github.github.answer_viewed(WriteAnswer::Done);
    cx.simulate_keystrokes("e");
    cx.run_until_parked();
    assert_eq!(marks(&tab, cx)[0], mark("a.rs", Some(ViewedState::Viewed)));

    // Switching back to the tab hands over the details the poller held from before.
    let stale = details_with(&[("a.rs", "UNVIEWED"), ("b.rs", "VIEWED")]);
    tab.update(cx, |tab, cx| {
        tab.take_details(&stale, before_the_write, Some(BOT), cx)
    });
    assert_eq!(
        marks(&tab, cx)[..2],
        [
            mark("a.rs", Some(ViewedState::Viewed)),
            mark("b.rs", Some(ViewedState::Viewed))
        ]
    );
    // And its record stays.
    assert_eq!(
        github.recorded(),
        HashMap::from([("a.rs".to_string(), HEAD.to_string())])
    );

    // Details fetched after the write stand.
    tab.update(cx, |tab, cx| tab.set_details(&stale, cx));
    assert_eq!(
        marks(&tab, cx)[0],
        mark("a.rs", Some(ViewedState::Unviewed))
    );
}

#[gpui::test]
async fn e_on_a_viewed_file_clears_its_mark(cx: &mut TestAppContext) {
    let (_, tab, github, cx) = three_files(
        "e_clears",
        &[("b.rs", "1111111111111111111111111111111111111111")],
        &[
            ("a.rs", "UNVIEWED"),
            ("b.rs", "VIEWED"),
            ("c.rs", "UNVIEWED"),
        ],
        cx,
    )
    .await;
    go_to(&tab, "b.rs", cx);
    github.github.answer_viewed(WriteAnswer::Done);

    cx.simulate_keystrokes("e");

    use ViewedState::*;
    assert_eq!(
        marks(&tab, cx),
        [
            mark("a.rs", Some(Unviewed)),
            mark("b.rs", Some(Unviewed)),
            mark("c.rs", Some(Unviewed))
        ]
    );
    // Clearing a mark stays on the file, and forgets where it was viewed.
    assert_eq!(selected(&tab, cx).as_deref(), Some("b.rs"));
    assert_eq!(github.github.viewed_mutations()[0].0, "UnmarkFileAsViewed");
    assert_eq!(github.recorded(), HashMap::new());

    // Pressed again, after the second's spacing between writes, it's marked again.
    github.github.answer_viewed(WriteAnswer::Done);
    cx.simulate_keystrokes("e");
    cx.executor().advance_clock(Duration::from_secs(2));
    cx.run_until_parked();
    assert_eq!(marks(&tab, cx)[1], mark("b.rs", Some(Viewed)));
    assert_eq!(github.github.viewed_mutations()[1].0, "MarkFileAsViewed");
    assert_eq!(
        github.recorded(),
        HashMap::from([("b.rs".to_string(), HEAD.to_string())])
    );
}

#[gpui::test]
async fn a_mark_that_github_refuses_reverts_and_a_toast_says_why(cx: &mut TestAppContext) {
    let (workspace, tab, github, cx) = three_files(
        "e_reverts",
        &[],
        &[
            ("a.rs", "UNVIEWED"),
            ("b.rs", "VIEWED"),
            ("c.rs", "UNVIEWED"),
        ],
        cx,
    )
    .await;
    use ViewedState::*;
    let before = marks(&tab, cx);

    for answer in [
        WriteAnswer::Forbidden,
        WriteAnswer::Offline,
        WriteAnswer::RateLimited,
    ] {
        workspace.update(cx, |workspace, cx| workspace.clear_all_notifications(cx));
        go_to(&tab, "a.rs", cx);
        github.github.answer_viewed(answer);

        cx.simulate_keystrokes("e");
        cx.executor().advance_clock(Duration::from_secs(2));
        cx.run_until_parked();

        assert_eq!(marks(&tab, cx), before, "{answer:?}");
        assert!(has_viewed_toast(&workspace, cx), "{answer:?}");
        assert_eq!(github.recorded(), HashMap::new(), "{answer:?}");
        // The selection still moved on; only the mark went back.
        assert_eq!(selected(&tab, cx).as_deref(), Some("c.rs"), "{answer:?}");
    }
    assert_eq!(github.github.viewed_mutations().len(), 3);
    assert_eq!(before[0], mark("a.rs", Some(Unviewed)));
}

#[gpui::test]
async fn the_new_mark_shows_while_the_write_waits(cx: &mut TestAppContext) {
    let (_, tab, github, cx) = three_files(
        "e_waits",
        &[],
        &[
            ("a.rs", "UNVIEWED"),
            ("b.rs", "UNVIEWED"),
            ("c.rs", "UNVIEWED"),
        ],
        cx,
    )
    .await;
    github.github.answer_viewed(WriteAnswer::Done);
    github.github.answer_viewed(WriteAnswer::Forbidden);

    // The second write waits a second after the first, so its mark shows before GitHub answers.
    cx.simulate_keystrokes("e");
    cx.simulate_keystrokes("e");

    use ViewedState::*;
    assert_eq!(
        marks(&tab, cx),
        [
            mark("a.rs", Some(Viewed)),
            mark("b.rs", Some(Viewed)),
            mark("c.rs", Some(Unviewed))
        ]
    );
    assert_eq!(github.github.viewed_mutations().len(), 1);

    cx.executor().advance_clock(Duration::from_secs(2));
    cx.run_until_parked();
    // GitHub took a.rs's mark and refused b.rs's, which goes back and says why.
    assert_eq!(
        marks(&tab, cx),
        [
            mark("a.rs", Some(Viewed)),
            mark("b.rs", Some(Unviewed)),
            mark("c.rs", Some(Unviewed))
        ]
    );
    assert_eq!(github.github.viewed_mutations().len(), 2);
    assert_eq!(
        toasts(cx),
        [
            "Couldn't mark b.rs viewed: rozed-bot isn't allowed to (Resource not accessible by \
          integration). It shows GitHub's mark again."
        ]
    );
    assert_eq!(
        github.recorded(),
        HashMap::from([("a.rs".to_string(), HEAD.to_string())])
    );
}

#[gpui::test]
async fn e_says_why_it_cant_mark_without_an_account_or_githubs_details(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let files = [("a.rs", lines(5, &[]), lines(5, &[3]))];
    let anonymous = open_unloaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);
    anonymous.update(cx, |tab, cx| tab.set_acting(Acting::Anonymous, cx));
    show_test_diff(&anonymous, &workspace, &files, cx).await;

    cx.simulate_keystrokes("e");
    assert_eq!(
        toasts(cx),
        ["Sign in to mark files viewed: this pull request opened without an account."]
    );
    assert!(has_viewed_toast(&workspace, cx));
    assert_eq!(marks(&anonymous, cx), [mark("a.rs", None)]);

    // Signed in, but GitHub hasn't sent the pull request's details, which name it for writes.
    let tab = open_unloaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);
    let github = sign_in_bot(&tab, "e_without_details", &[], cx).await;
    show_test_diff(&tab, &workspace, &files, cx).await;

    cx.simulate_keystrokes("e");
    assert_eq!(
        toasts(cx)[1..],
        [
            "Couldn't mark a.rs viewed: GitHub hasn't sent this pull request's details yet. Try \
          again in a moment."
        ]
    );
    assert!(has_viewed_toast(&workspace, cx));
    assert_eq!(marks(&tab, cx), [mark("a.rs", None)]);
    assert_eq!(github.github.viewed_mutations(), []);
    // Each tab's toast is its own, so the first still shows.
    let first = toast_log(cx)[0].0.clone();
    assert!(workspace.read_with(cx, |workspace, _| workspace.has_notification(&first)));
}

#[gpui::test]
async fn a_file_that_changed_since_it_was_viewed_says_so_when_opened(cx: &mut TestAppContext) {
    const VIEWED_AT: &str = "1111111111111111111111111111111111111111";
    let (_, tab, _, cx) = three_files(
        "changed_since_viewed",
        &[("b.rs", VIEWED_AT)],
        &[
            ("a.rs", "UNVIEWED"),
            ("b.rs", "DISMISSED"),
            ("c.rs", "VIEWED"),
        ],
        cx,
    )
    .await;
    let note = |cx: &mut VisualTestContext| {
        tab.update(cx, |tab, cx| tab.changed_since_viewed(cx))
            .map(|(_, note)| note)
    };
    assert_eq!(note(cx), None, "a.rs is current, and unviewed");

    // The list shows b.rs changed since viewed; opening it says since when.
    assert_eq!(
        marks(&tab, cx)[1],
        mark("b.rs", Some(ViewedState::Dismissed))
    );
    cx.simulate_keystrokes("] f");
    assert_eq!(
        note(cx).as_deref(),
        Some(
            "Changed since it was viewed. Lo can't show only what changed since then yet, \
             so this is the file's whole diff."
        ),
        "the head this Mac recorded isn't a revision the tab knows"
    );

    // With the revision this Mac recorded among the pull request's, it's named.
    tab.update(cx, |tab, _| {
        let at = |secs: u64| SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000 + secs);
        tab.revisions = Some(build_revisions(
            &[
                PushRecord::Polled {
                    head: VIEWED_AT.into(),
                    at: at(0),
                },
                PushRecord::Polled {
                    head: HEAD.into(),
                    at: at(60),
                },
            ],
            &[],
            HEAD,
            SystemTime::UNIX_EPOCH,
        ));
    });
    assert_eq!(
        note(cx).as_deref(),
        Some(
            "Changed since you viewed it in r1. Lo can't show only what changed since \
             then yet, so this is the file's whole diff."
        )
    );

    // A viewed file says nothing.
    cx.simulate_keystrokes("] f");
    assert_eq!(note(cx), None);
}
