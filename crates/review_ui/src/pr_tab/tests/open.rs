use super::*;

#[test]
fn tab_titles_fit_the_repository_number_and_title() {
    assert_eq!(tab_title("fd", 2212, None), "fd#2212");
    assert_eq!(
        tab_title("playground", 2, Some("Add a greeting")),
        "playground#2 Add a greeting"
    );
    assert_eq!(
        tab_title("fd", 2212, Some("Honor explicit use-jemalloc requests now")),
        "fd#2212 Honor explicit use-jemalloc req…"
    );
}

#[gpui::test]
async fn a_pull_request_github_doesnt_have_is_neither_polled_nor_recent(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    let cache = playground_seen_two_hours_ago(&github, &git, cx).await;

    // A typo: there's no #999.
    let tab = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/999", cx);

    let Status::Failed(message) = status(&tab, cx) else {
        panic!("the tab opened: {:?}", status(&tab, cx));
    };
    assert!(message.contains("GitHub has no pull request"), "{message}");
    github.requests();
    cx.executor().advance_clock(Duration::from_secs(10 * 60));
    cx.run_until_parked();
    assert_eq!(github.requests(), Vec::<String>::new());
    assert_eq!(cache.recent(10).unwrap(), Vec::new());
}

#[gpui::test]
async fn a_tab_loading_in_another_pane_doesnt_take_the_open_pull_request(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    playground_seen_two_hours_ago(&github, &git, cx).await;
    let other = git.pull_request("rozed-dev/playground", 3, &[], &[("c.rs", "c\n")]);
    github.add_pull(other);

    // #2 is active in the left pane, and #3 waits in a split to its right.
    let left = workspace.read_with(cx, |workspace, _| workspace.active_pane().clone());
    let two = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);
    let three = open_unloaded(&workspace, "github.com/rozed-dev/playground/pull/3", cx);
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.split_and_move(left.clone(), workspace::SplitDirection::Right, window, cx);
    });
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.activate_item(&two, true, true, window, cx);
    });
    cx.run_until_parked();
    let poller = cx.update(|_, cx| {
        let layer = github_layer::loaded(cx).unwrap();
        layer.update(cx, |layer, cx| layer.poller("rozed-bot", cx))
    });
    let open = |cx: &mut VisualTestContext| {
        poller.read_with(cx, |poller, _| poller.open().map(|pull| pull.number))
    };
    assert_eq!(open(cx), Some(2));

    // #3 loads in the background.
    assert_eq!(status(&two, cx), Status::Ready);
    three.update_in(cx, |tab, window, cx| tab.load(window, cx));
    cx.run_until_parked();

    assert_eq!(status(&three, cx), Status::Ready);
    assert_eq!(open(cx), Some(2));
    // Switching to it makes it the open one.
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.activate_item(&three, true, true, window, cx);
    });
    cx.run_until_parked();
    assert_eq!(open(cx), Some(3));
}

#[gpui::test]
async fn a_new_tab_shows_its_link_until_github_answers(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let tab = open_unloaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);

    assert_eq!(tabs(&workspace, cx), vec![tab.clone()]);
    assert_eq!(active_tab(&workspace, cx), Some(tab.clone()));
    tab.read_with(cx, |tab, cx| {
        assert_eq!(tab.tab_content_text(0, cx), "fd#2212");
        assert!(matches!(tab.status(), Status::Loading(_)));
        assert!(tab.is_file_list_shown());
        assert!(tab.is_threads_panel_shown());
    });
}

#[gpui::test]
async fn the_window_is_titled_after_the_active_pull_request(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    assert_eq!(cx.window_title().as_deref(), Some("Lo"));
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    public_pull(&github, &git, "sharkdp/fd", 2212);
    public_pull(&github, &git, "sharkdp/bat", 3000);
    anonymous(&github, cx);

    // Until GitHub answers, the title is the link's.
    let fd = open_unloaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);
    assert_eq!(cx.window_title().as_deref(), Some("sharkdp/fd#2212"));
    fd.update_in(cx, |tab, window, cx| tab.load(window, cx));
    cx.run_until_parked();
    assert_eq!(
        cx.window_title().as_deref(),
        Some("sharkdp/fd#2212 — Pull request 2212")
    );

    // It follows the active tab.
    let bat = open_loaded(&workspace, "github.com/sharkdp/bat/pull/3000", cx);
    assert_eq!(
        cx.window_title().as_deref(),
        Some("sharkdp/bat#3000 — Pull request 3000")
    );
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.activate_item(&fd, true, true, window, cx);
    });
    cx.run_until_parked();
    assert_eq!(
        cx.window_title().as_deref(),
        Some("sharkdp/fd#2212 — Pull request 2212")
    );

    // And the active pull request's title.
    fd.update(cx, |tab, cx| {
        tab.set_details(
            &details("Honor explicit use-jemalloc requests", "VIEWED"),
            cx,
        )
    });
    cx.run_until_parked();
    assert_eq!(
        cx.window_title().as_deref(),
        Some("sharkdp/fd#2212 — Honor explicit use-jemalloc requests")
    );

    // With none open, it's Lo's.
    close(&workspace, fd, cx);
    close(&workspace, bat, cx);
    assert_eq!(cx.window_title().as_deref(), Some("Lo"));
}

#[gpui::test]
async fn with_an_empty_pane_active_the_window_keeps_the_last_pull_requests_title(
    cx: &mut TestAppContext,
) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    public_pull(&github, &git, "sharkdp/fd", 2212);
    anonymous(&github, cx);
    let fd = open_loaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);
    let titled = Some("sharkdp/fd#2212 — Pull request 2212");
    assert_eq!(cx.window_title().as_deref(), titled);

    // A split leaves an empty pane active.
    workspace.update_in(cx, |workspace, window, cx| {
        let pane = workspace.active_pane().clone();
        workspace.split_pane(pane, workspace::SplitDirection::Right, window, cx);
    });
    cx.run_until_parked();
    assert!(workspace.read_with(cx, |workspace, cx| workspace.active_item(cx).is_none()));
    assert_eq!(cx.window_title().as_deref(), titled);

    // It follows that pull request's title, without the characters that turn text around.
    fd.update(cx, |tab, cx| {
        tab.set_details(&details("Fix \u{202E}gpj.exe", "VIEWED"), cx)
    });
    cx.run_until_parked();
    assert_eq!(
        cx.window_title().as_deref(),
        Some("sharkdp/fd#2212 — Fix gpj.exe")
    );
    assert_eq!(
        fd.read_with(cx, |tab, cx| tab.tab_content_text(0, cx)),
        "fd#2212 Fix gpj.exe"
    );
}

#[gpui::test]
async fn with_los_title_format_a_new_folder_doesnt_retitle_the_window(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    // As Lo's forced settings have it.
    cx.update(|_, cx| {
        cx.update_global::<settings::SettingsStore, _>(|store, cx| {
            let format =
                serde_json::json!({ "window_title_format": window_title::NO_PULL_REQUEST });
            store.set_server_settings(&format.to_string(), cx).unwrap();
        })
    });
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    public_pull(&github, &git, "sharkdp/fd", 2212);
    anonymous(&github, cx);
    open_loaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);
    let titled = Some("sharkdp/fd#2212 — Pull request 2212");
    assert_eq!(cx.window_title().as_deref(), titled);

    // A trusted checkout adds a folder to the project.
    let project = workspace.read_with(cx, |workspace, _| workspace.project().clone());
    project
        .update(cx, |project, cx| {
            project.find_or_create_worktree("/elsewhere", true, cx)
        })
        .await
        .unwrap();
    cx.run_until_parked();

    assert_eq!(worktrees(&workspace, cx), 1);
    assert_eq!(cx.window_title().as_deref(), titled);
}

#[gpui::test]
async fn the_pollers_details_fill_the_header_and_viewed_marks_once(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    playground_seen_two_hours_ago(&github, &git, cx).await;
    let other = git.pull_request("rozed-dev/playground", 3, &[], &[("c.rs", "c\n")]);
    github.add_pull(other);
    // A changed title makes the workspace report the active item changed, which comes back
    // to the tabs; the same details again must not, or the two would loop.
    let active_item_changes = std::rc::Rc::new(std::cell::Cell::new(0));
    let _subscription = cx.update(|_, cx| {
        let changes = active_item_changes.clone();
        cx.subscribe(&workspace, move |_, event, _| {
            if let workspace::Event::ActiveItemChanged = event {
                changes.set(changes.get() + 1);
            }
        })
    });
    let marks = |tab: &Entity<PrTab>, cx: &mut VisualTestContext| {
        tab.read_with(cx, |tab, _| {
            tab.files()
                .rows()
                .map(|entry| (entry.path().to_string(), entry.viewed))
                .collect::<Vec<_>>()
        })
    };

    let two = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);
    let poller = cx.update(|_, cx| {
        let layer = github_layer::loaded(cx).unwrap();
        layer.update(cx, |layer, cx| layer.poller("rozed-bot", cx))
    });
    let open = |cx: &mut VisualTestContext| {
        poller.read_with(cx, |poller, _| poller.open().map(|pull| pull.number))
    };

    // The poller's details: the title and the account's viewed marks.
    assert_eq!(status(&two, cx), Status::Ready);
    assert_eq!(open(cx), Some(2));
    assert_eq!(
        two.read_with(cx, |tab, cx| tab.tab_content_text(0, cx)),
        "playground#2 Add b"
    );
    assert_eq!(
        marks(&two, cx),
        [
            ("a.rs".to_string(), Some(ViewedState::Viewed)),
            ("b.rs".to_string(), Some(ViewedState::Unviewed))
        ]
    );

    // The open pull request follows the active tab.
    let three = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/3", cx);
    assert_eq!(status(&three, cx), Status::Ready);
    assert_eq!(open(cx), Some(3));
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.activate_item(&two, true, true, window, cx);
    });
    cx.run_until_parked();
    assert_eq!(open(cx), Some(2));

    // Once settled, the tabs hearing again which is active changes nothing.
    let settled = active_item_changes.get();
    workspace.update(cx, |workspace, cx| sync_active(workspace, cx));
    cx.run_until_parked();
    assert_eq!(active_item_changes.get(), settled);
    assert_eq!(open(cx), Some(2));

    // A file marked viewed elsewhere changes the marks, and only them.
    two.update(cx, |tab, cx| {
        tab.set_details(&details("Add b", "DISMISSED"), cx)
    });
    cx.run_until_parked();
    assert_eq!(
        marks(&two, cx)[0],
        ("a.rs".to_string(), Some(ViewedState::Dismissed))
    );
}

#[gpui::test]
async fn reopening_a_pull_request_switches_to_its_tab(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let fd = open_unloaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);
    let playground = open_unloaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);
    assert_eq!(tabs(&workspace, cx), vec![fd.clone(), playground.clone()]);
    assert_eq!(active_tab(&workspace, cx), Some(playground.clone()));

    let reopened = open_unloaded(
        &workspace,
        "https://github.com/SharkDP/fd/pull/2212/files",
        cx,
    );

    assert_eq!(reopened, fd);
    assert_eq!(tabs(&workspace, cx), vec![fd.clone(), playground.clone()]);
    assert_eq!(active_tab(&workspace, cx), Some(fd.clone()));

    // Once GitHub gives the repository's current name, a link with that name finds the tab.
    playground.update(cx, |tab, cx| {
        let mut pull_request = fd_2212();
        pull_request.base.repo.full_name = "rozed-dev/renamed".into();
        tab.set_pull_request(&pull_request, SystemTime::now(), cx);
    });
    let reopened = open_unloaded(&workspace, "github.com/rozed-dev/renamed/pull/2", cx);
    assert_eq!(reopened, playground);
    assert_eq!(tabs(&workspace, cx).len(), 2);
    assert_eq!(active_tab(&workspace, cx), Some(playground));
}

/// `rozed-dev/renamed#2`, once called `rozed-dev/playground`, which `rozed-bot` can see.
async fn renamed_repository(github: &FakeGithub, git: &GitServer, cx: &mut TestAppContext) {
    let mut pull = git.pull_request("rozed-dev/renamed", 2, &[], &[("a.rs", "a\n")]);
    pull.old_names = vec!["rozed-dev/playground".into()];
    github.add_pull(pull);
    github.let_see("gho_BOT", "rozed-dev/renamed");
    sign_in(github, &[("rozed-bot", "gho_BOT")], cx).await;
}

#[gpui::test]
async fn a_renamed_repositorys_old_name_finds_the_tab_opened_by_its_new_one(
    cx: &mut TestAppContext,
) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    renamed_repository(&github, &git, cx).await;

    let tab = open_loaded(&workspace, "github.com/rozed-dev/renamed/pull/2", cx);
    assert_eq!(status(&tab, cx), Status::Ready);
    // The old name opens a tab until GitHub names the repository, and then it's the same
    // pull request, so that tab goes and the first comes to the front.
    open_loaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);
    assert_eq!(tabs(&workspace, cx), vec![tab.clone()]);
    assert_eq!(active_tab(&workspace, cx), Some(tab.clone()));

    // From then on, the old name finds the tab at once.
    github.requests();
    let reopened = open_loaded(&workspace, "github.com/Rozed-Dev/Playground/pull/2", cx);
    assert_eq!(reopened, tab);
    assert_eq!(github.requests(), Vec::<String>::new());

    // The poller watches the pull request once, by its name now.
    let poller = cx.update(|_, cx| {
        let layer = github_layer::loaded(cx).unwrap();
        layer.update(cx, |layer, cx| layer.poller("rozed-bot", cx))
    });
    let open = poller.read_with(cx, |poller, _| poller.open().cloned());
    let open = open.unwrap();
    assert_eq!(
        (
            open.repo.owner.as_str(),
            open.repo.name.as_str(),
            open.number
        ),
        ("rozed-dev", "renamed", 2)
    );
}

#[gpui::test]
async fn two_names_opened_before_either_loads_end_in_one_tab(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    renamed_repository(&github, &git, cx).await;

    let old = open_unloaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);
    let new = open_unloaded(&workspace, "github.com/rozed-dev/renamed/pull/2", cx);
    for tab in [&old, &new] {
        tab.update_in(cx, |tab, window, cx| tab.load(window, cx));
    }
    cx.run_until_parked();

    let left = tabs(&workspace, cx);
    assert_eq!(left.len(), 1);
    assert_eq!(status(&left[0], cx), Status::Ready);
    for link in [
        "github.com/rozed-dev/playground/pull/2",
        "github.com/rozed-dev/renamed/pull/2",
    ] {
        assert_eq!(open_unloaded(&workspace, link, cx), left[0]);
    }
}

/// Whether the account's poller polls pull request `number` of `rozed-dev/playground`, as
/// it does every pull request it watches, within three minutes in the foreground.
fn polled(number: u64, github: &FakeGithub, cx: &mut VisualTestContext) -> bool {
    github.requests();
    cx.executor().advance_clock(Duration::from_secs(3 * 60));
    cx.run_until_parked();
    let path = format!("/repos/rozed-dev/playground/pulls/{number} ");
    github
        .requests()
        .iter()
        .any(|request| request.contains(&path))
}

#[gpui::test]
async fn a_tab_stays_watched_when_it_moves_and_isnt_once_it_closes(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    playground_seen_two_hours_ago(&github, &git, cx).await;
    let other = git.pull_request("rozed-dev/playground", 3, &[], &[("c.rs", "c\n")]);
    github.add_pull(other);
    let left = workspace.read_with(cx, |workspace, _| workspace.active_pane().clone());
    let three = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/3", cx);
    let two = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);
    let poller = cx.update(|_, cx| {
        let layer = github_layer::loaded(cx).unwrap();
        layer.update(cx, |layer, cx| layer.poller("rozed-bot", cx))
    });
    assert!(poller.read_with(cx, |poller, _| poller.is_foreground()));
    assert!(polled(2, &github, cx));

    // #2 moves into a split, and #3, left behind, is the active tab.
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.split_and_move(left.clone(), workspace::SplitDirection::Right, window, cx);
        workspace.activate_item(&three, true, true, window, cx);
    });
    cx.run_until_parked();
    assert_ne!(
        workspace.read_with(cx, |workspace, _| workspace.pane_for(&two)),
        Some(left)
    );
    assert!(polled(2, &github, cx));
    assert!(polled(3, &github, cx));

    close(&workspace, two, cx);
    assert!(!polled(2, &github, cx));
    assert!(polled(3, &github, cx));
}

#[gpui::test]
async fn a_trusted_checkout_goes_with_the_last_tab_that_uses_it(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let project = workspace.read_with(cx, |workspace, _| workspace.project().clone());
    let languages = project.read_with(cx, |project, _| project.languages().clone());
    languages.add(language::rust_lang());
    let mut servers = languages.register_fake_lsp(
        "Rust",
        language::FakeLspAdapter {
            name: "rust-analyzer",
            ..Default::default()
        },
    );
    // A checkout, which Lo trusts and opens as a folder of the project, with
    // rust-analyzer running.
    cx.update(|_, cx| crate::trust::trust_checkout(&project, Path::new("/elsewhere"), cx))
        .unwrap();
    let (worktree, _) = project
        .update(cx, |project, cx| {
            project.find_or_create_worktree("/elsewhere", true, cx)
        })
        .await
        .unwrap();
    let checkout = worktree.read_with(cx, |worktree, _| worktree.id());
    drop(worktree);
    cx.run_until_parked();
    let (_buffer, _lsp) = project
        .update(cx, |project, cx| {
            project.open_local_buffer_with_lsp("/elsewhere/src/main.rs", cx)
        })
        .await
        .unwrap();
    cx.run_until_parked();
    servers.try_recv().expect("rust-analyzer started");
    let running = |cx: &mut VisualTestContext| {
        project.read_with(cx, |project, cx| {
            project
                .lsp_store()
                .read(cx)
                .language_server_statuses()
                .count()
        })
    };
    assert_eq!(running(cx), 1);

    // Two tabs use it, as when a second link to the same head opened before the first
    // loaded.
    let first = open_unloaded(&workspace, "github.com/acme/app/pull/1", cx);
    let second = open_unloaded(&workspace, "github.com/acme/app/pull/2", cx);
    for tab in [&first, &second] {
        tab.update(cx, |tab, _| tab.uses_checkout(checkout));
    }

    close(&workspace, first, cx);
    assert_eq!(worktrees(&workspace, cx), 1);
    assert_eq!(running(cx), 1);

    close(&workspace, second, cx);
    assert_eq!(worktrees(&workspace, cx), 0);
    assert_eq!(running(cx), 0);
}

#[gpui::test]
async fn the_header_says_how_polling_goes(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    playground_seen_two_hours_ago(&github, &git, cx).await;
    let note = |tab: &Entity<PrTab>, cx: &mut VisualTestContext| {
        tab.read_with(cx, |tab, _| tab.poll_note().map(|note| note.text))
    };
    // The fake GitHub has no GraphQL API, so the poller's first poll for details fails.
    let tab = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);
    assert_eq!(status(&tab, cx), Status::Ready);
    assert_eq!(
        note(&tab, cx).as_deref(),
        Some("Lo couldn't update this pull request: Not Found")
    );

    github.set_offline(true);
    cx.executor().advance_clock(Duration::from_secs(30));
    cx.run_until_parked();
    let offline = note(&tab, cx).unwrap();
    assert!(
        offline.starts_with("Offline since just now. These details are from 2 hours ago."),
        "{offline}"
    );

    github.set_offline(false);
    let accounts = cx.update(|_, cx| {
        let layer = github_layer::loaded(cx).unwrap();
        layer.read(cx).accounts().clone()
    });
    accounts.update(cx, |accounts, cx| {
        accounts.mark_signed_out("rozed-bot", github::accounts::SignedOut::Refused, cx)
    });
    cx.executor().advance_clock(Duration::from_secs(5 * 60));
    cx.run_until_parked();
    assert_eq!(
        note(&tab, cx).as_deref(),
        Some(
            "Signed out: GitHub refused rozed-bot's token. Sign in again from Accounts… in \
             the Lo menu."
        )
    );
}

#[gpui::test]
async fn cmd_o_opens_a_link_in_a_tab_or_switches_to_it(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let fd = open_unloaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);
    let playground = open_unloaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);
    let has_box = |cx: &mut VisualTestContext| {
        workspace.read_with(cx, |workspace, cx| {
            workspace.active_modal::<crate::OpenBox>(cx).is_some()
        })
    };

    cx.simulate_keystrokes("cmd-o");
    assert!(has_box(cx));
    cx.simulate_input("not a link");
    cx.simulate_keystrokes("enter");
    let error = workspace.read_with(cx, |workspace, cx| {
        let open_box = workspace.active_modal::<crate::OpenBox>(cx).unwrap();
        open_box.read(cx).error().cloned()
    });
    assert_eq!(
        error.as_deref(),
        Some("Can't open that: it isn't a link to github.com.")
    );
    cx.simulate_keystrokes("escape");
    assert!(!has_box(cx));

    cx.simulate_keystrokes("cmd-o");
    cx.simulate_input("https://github.com/sharkdp/fd/pull/2212");
    cx.simulate_keystrokes("enter");

    assert!(!has_box(cx));
    assert_eq!(tabs(&workspace, cx), vec![fd.clone(), playground.clone()]);
    assert_eq!(active_tab(&workspace, cx), Some(fd.clone()));

    // A link that isn't open opens and loads in a new tab.
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    public_pull(&github, &git, "sharkdp/bat", 3000);
    anonymous(&github, cx);
    cx.simulate_keystrokes("cmd-o");
    cx.simulate_input("https://github.com/sharkdp/bat/pull/3000/files");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    let all = tabs(&workspace, cx);
    assert_eq!(all.len(), 3);
    // Zed adds a tab after the active one, fd's.
    let bat = all[1].clone();
    assert_eq!(bat.read_with(cx, |tab, _| tab.link().repo.clone()), "bat");
    assert_eq!(active_tab(&workspace, cx), Some(bat.clone()));
    assert_eq!(status(&bat, cx), Status::Ready);
    assert_eq!(
        bat.read_with(cx, |tab, cx| tab.tab_content_text(0, cx)),
        "bat#3000 Pull request 3000"
    );
}

#[gpui::test]
async fn a_tab_that_failed_tries_again_when_its_link_is_opened_again(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    public_pull(&github, &git, "sharkdp/fd", 2212);
    anonymous(&github, cx);
    github.answer(
        "",
        "/repos/sharkdp/fd/pulls/2212",
        Answer::new(502, "Bad gateway"),
    );
    let tab = open_loaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);
    assert!(matches!(status(&tab, cx), Status::Failed(_)));

    // GitHub's back, and the user opens the link again, by another spelling.
    github.forget_answer("", "/repos/sharkdp/fd/pulls/2212");
    let reopened = open_loaded(&workspace, "github.com/SharkDP/fd/pull/2212/files", cx);

    assert_eq!(reopened, tab);
    assert_eq!(tabs(&workspace, cx), vec![tab.clone()]);
    assert_eq!(status(&tab, cx), Status::Ready);
    tab.read_with(cx, |tab, _| assert_eq!(tab.files().len(), 3));

    // A tab that opened only switches.
    github.requests();
    open_loaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);
    assert_eq!(github.requests(), Vec::<String>::new());
}
