use super::*;

#[gpui::test]
async fn offline_what_lo_saved_shows_with_its_age(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    let cache = playground_seen_two_hours_ago(&github, &git, cx).await;
    github.set_offline(true);

    let tab = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);

    let Status::Failed(message) = status(&tab, cx) else {
        panic!("expected the tab to fail offline: {:?}", status(&tab, cx));
    };
    assert!(message.contains("couldn't reach GitHub"), "{message}");
    assert_eq!(
        tab.read_with(cx, |tab, cx| tab.tab_content_text(0, cx)),
        "playground#2 Add b"
    );
    assert_eq!(
        banners(&tab, cx),
        ["Showing what Lo saved for rozed-bot 2 hours ago."]
    );
    // There's no local copy, so no diff.
    tab.read_with(cx, |tab, _| {
        assert!(tab.diff().is_none());
        assert_eq!(tab.files().len(), 0);
    });
    // Offline, GitHub can't confirm the pull request, so nothing records it as opened.
    assert_eq!(cache.recent(10).unwrap(), Vec::new());

    // It keeps checking, and once GitHub answers again it loads, and the banner goes.
    github.set_offline(false);
    cx.executor().advance_clock(Duration::from_secs(5));
    cx.run_until_parked();
    assert_eq!(status(&tab, cx), Status::Ready);
    assert_eq!(banners(&tab, cx), Vec::<String>::new());
    let recent: Vec<_> = cache
        .recent(10)
        .unwrap()
        .into_iter()
        .map(|recent| (recent.pull.to_string(), recent.account))
        .collect();
    assert_eq!(
        recent,
        [(
            "rozed-dev/playground#2".to_string(),
            "rozed-bot".to_string()
        )]
    );
}

/// `rozed-dev/playground#2`, opened once online, which copied it, and closed; with what
/// Lo saved about it for `rozed-bot` two hours ago, `saved` from the pull request on the
/// fake GitHub. Then offline, GitHub's git server included.
async fn playground_copied_then_offline(
    workspace: &Entity<Workspace>,
    github: &FakeGithub,
    git: &GitServer,
    saved: impl FnOnce(&FakePull) -> PullRequestData,
    cx: &mut VisualTestContext,
) -> FakePull {
    let mut pull = git.pull_request(
        "rozed-dev/playground",
        2,
        &[("a.rs", "a\n"), ("b.rs", "b\n")],
        &[("a.rs", "a2\n"), ("b.rs", "b2\n")],
    );
    pull.title = "Add b".into();
    github.add_pull(pull.clone());
    github.let_see("gho_BOT", "rozed-dev/playground");
    let (_, cache) = sign_in(github, &[("rozed-bot", "gho_BOT")], cx).await;
    let tab = open_loaded(workspace, "github.com/rozed-dev/playground/pull/2", cx);
    assert_eq!(status(&tab, cx), Status::Ready);
    close(workspace, tab, cx);

    let two_hours_ago = SystemTime::now() - Duration::from_secs(2 * 3_600 + 30);
    cache
        .put(
            "rozed-bot",
            &Target::Pull(PullRef::new("rozed-dev", "playground", 2)),
            &saved(&pull),
            None,
            two_hours_ago,
        )
        .await
        .unwrap();
    github.set_offline(true);
    git.hide("rozed-dev/playground");
    pull
}

/// What Lo saved about `pull`: its head and base as GitHub gave them.
fn saved_as_given(pull: &FakePull) -> PullRequestData {
    details_based("Add b", "VIEWED", &pull.head, &pull.base_branch, &pull.base)
}

/// The text of each file's old side in `tab`'s diff, sorted.
fn old_texts(tab: &Entity<PrTab>, cx: &mut VisualTestContext) -> Vec<String> {
    tab.read_with(cx, |tab, cx| {
        let editor = tab.diff().unwrap().read(cx).rhs_editor().clone();
        let multi_buffer = editor.read(cx).buffer().read(cx);
        let mut texts: Vec<String> = multi_buffer
            .all_buffers()
            .iter()
            .filter_map(|buffer| {
                let diff = multi_buffer.diff_for(buffer.read(cx).remote_id())?;
                diff.read(cx).base_text_string(cx)
            })
            .collect();
        texts.sort();
        texts
    })
}

#[gpui::test]
async fn offline_a_pull_request_the_local_copy_has_shows_its_diff_from_the_copy(
    cx: &mut TestAppContext,
) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    let pull = playground_copied_then_offline(&workspace, &github, &git, saved_as_given, cx).await;

    let tab = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);

    let Status::Offline(message) = status(&tab, cx) else {
        panic!("expected the local copy offline: {:?}", status(&tab, cx));
    };
    assert!(message.starts_with("Offline: "), "{message}");
    assert!(message.contains("couldn't reach GitHub"), "{message}");
    assert!(
        message.ends_with("until GitHub answers again; Lo keeps trying."),
        "{message}"
    );
    assert_eq!(
        banners(&tab, cx),
        ["Showing what Lo saved for rozed-bot 2 hours ago."]
    );
    tab.read_with(cx, |tab, cx| {
        assert!(tab.diff().is_some());
        let files: Vec<_> = tab
            .files()
            .rows()
            .map(|entry| (entry.path().to_string(), entry.viewed))
            .collect();
        assert_eq!(
            files,
            [
                ("a.rs".to_string(), Some(ViewedState::Viewed)),
                ("b.rs".to_string(), Some(ViewedState::Unviewed))
            ]
        );
        assert_eq!(
            tab.revisions().unwrap().label(&pull.head).as_deref(),
            Some("r1")
        );
        assert_eq!(tab.tab_content_text(0, cx), "playground#2 Add b");
    });
    assert_eq!(
        cx.window_title().as_deref(),
        Some("rozed-dev/playground#2 — Add b")
    );
    let diff_text = tab.read_with(cx, |tab, cx| {
        let editor = tab.diff().unwrap().read(cx).rhs_editor().clone();
        editor.read(cx).buffer().read(cx).snapshot(cx).text()
    });
    assert!(diff_text.contains("a2"), "{diff_text}");
    assert!(diff_text.contains("b2"), "{diff_text}");
    // Against the saved base, which the copy has.
    assert_eq!(old_texts(&tab, cx), ["a\n", "b\n"]);

    // While GitHub can't be reached, it keeps checking, less often each time, and stays as
    // it is.
    github.requests();
    cx.executor().advance_clock(Duration::from_secs(5 + 10));
    cx.run_until_parked();
    assert_eq!(
        github.requests(),
        ["GET /rate_limit as nobody", "GET /rate_limit as nobody"]
    );
    assert!(matches!(status(&tab, cx), Status::Offline(_)));

    // Back online, it loads from GitHub by itself: the local copy fetches again, and the
    // offline status and the saved details' banner go.
    github.set_offline(false);
    git.restore("rozed-dev/playground");
    cx.executor().advance_clock(Duration::from_secs(20));
    cx.run_until_parked();
    let requests = github.requests();
    assert!(
        requests.contains(&"GET /repos/rozed-dev/playground/pulls/2 as gho_BOT".to_string()),
        "{requests:#?}"
    );
    assert_eq!(status(&tab, cx), Status::Ready);
    assert_eq!(banners(&tab, cx), Vec::<String>::new());
    tab.read_with(cx, |tab, _| {
        assert_eq!(tab.acting(), Some(&Acting::Account("rozed-bot".into())));
        assert!(tab.diff().is_some());
    });
    assert_eq!(old_texts(&tab, cx), ["a\n", "b\n"]);
}

#[gpui::test]
async fn offline_a_head_the_local_copy_never_fetched_shows_only_the_saved_details(
    cx: &mut TestAppContext,
) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    // GitHub had a newer head when Lo last saved the details.
    let newer = "e".repeat(40);
    playground_copied_then_offline(
        &workspace,
        &github,
        &git,
        |pull| details_based("Add b", "VIEWED", &newer, &pull.base_branch, &pull.base),
        cx,
    )
    .await;

    let tab = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);

    let Status::Failed(message) = status(&tab, cx) else {
        panic!("expected only the saved details: {:?}", status(&tab, cx));
    };
    assert!(message.contains("couldn't reach GitHub"), "{message}");
    assert_eq!(
        banners(&tab, cx),
        ["Showing what Lo saved for rozed-bot 2 hours ago."]
    );
    tab.read_with(cx, |tab, cx| {
        assert!(tab.diff().is_none());
        assert_eq!(tab.tab_content_text(0, cx), "playground#2 Add b");
    });
}

#[gpui::test]
async fn offline_a_newer_tip_of_the_same_base_branch_uses_the_one_fetched(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    // main moved on after the copy fetched it.
    let moved_on = "e".repeat(40);
    playground_copied_then_offline(
        &workspace,
        &github,
        &git,
        |pull| details_based("Add b", "VIEWED", &pull.head, &pull.base_branch, &moved_on),
        cx,
    )
    .await;

    let tab = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);

    assert!(matches!(status(&tab, cx), Status::Offline(_)));
    assert_eq!(old_texts(&tab, cx), ["a\n", "b\n"]);
}

#[gpui::test]
async fn offline_a_base_branch_the_copy_never_fetched_shows_only_the_saved_details(
    cx: &mut TestAppContext,
) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    // GitHub retargeted the pull request onto another branch after the copy fetched it, so
    // the base the copy has would show other changes.
    playground_copied_then_offline(
        &workspace,
        &github,
        &git,
        |pull| {
            details_based(
                "Add b",
                "VIEWED",
                &pull.head,
                "stack-a",
                "b046244d9569964088e952243e5e557312322488",
            )
        },
        cx,
    )
    .await;

    let tab = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);

    let Status::Failed(message) = status(&tab, cx) else {
        panic!("expected only the saved details: {:?}", status(&tab, cx));
    };
    assert!(message.contains("couldn't reach GitHub"), "{message}");
    tab.read_with(cx, |tab, cx| {
        assert!(tab.diff().is_none());
        assert_eq!(tab.tab_content_text(0, cx), "playground#2 Add b");
    });
}

#[gpui::test]
async fn offline_the_copy_shows_though_its_account_was_signed_out(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    playground_copied_then_offline(&workspace, &github, &git, saved_as_given, cx).await;
    let accounts = cx.update(|_, cx| {
        let layer = github_layer::loaded(cx).unwrap();
        layer.read(cx).accounts().clone()
    });
    accounts.update(cx, |accounts, cx| {
        accounts.mark_signed_out("rozed-bot", github::accounts::SignedOut::Refused, cx)
    });

    // With no account signed in, it asks GitHub without one, and gets no answer.
    let tab = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);

    assert!(
        matches!(status(&tab, cx), Status::Offline(_)),
        "{:?}",
        status(&tab, cx)
    );
    tab.read_with(cx, |tab, _| assert!(tab.diff().is_some()));
}

#[gpui::test]
async fn a_slow_answer_shows_the_copy_and_keeps_trying_without_saying_offline(
    cx: &mut TestAppContext,
) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    playground_copied_then_offline(&workspace, &github, &git, saved_as_given, cx).await;
    // GitHub can be reached, but nothing answers in time, as on a connection that died while
    // the Mac slept.
    github.set_offline(false);
    github.set_timing_out(true);

    let tab = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);

    let Status::Offline(message) = status(&tab, cx) else {
        panic!("expected the local copy: {:?}", status(&tab, cx));
    };
    assert!(message.starts_with("GitHub didn't answer: "), "{message}");
    assert!(message.contains("operation timed out"), "{message}");
    assert!(message.ends_with("Lo keeps trying."), "{message}");
    tab.read_with(cx, |tab, _| assert!(tab.diff().is_some()));

    // Once it answers in time, the tab loads by itself.
    github.set_timing_out(false);
    git.restore("rozed-dev/playground");
    cx.executor().advance_clock(Duration::from_secs(5));
    cx.run_until_parked();
    assert_eq!(status(&tab, cx), Status::Ready);
}

#[gpui::test]
async fn one_accounts_failed_connection_is_waited_for_rather_than_opening_as_another(
    cx: &mut TestAppContext,
) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    let pull = git.pull_request("rozed-dev/playground", 2, &[], &[("a.rs", "a\n")]);
    github.add_pull(pull);
    github.let_see("gho_BOT", "rozed-dev/playground");
    sign_in(
        &github,
        &[("home", "gho_HOME"), ("rozed-bot", "gho_BOT")],
        cx,
    )
    .await;
    // home's requests can't connect, and home might see the repository too.
    github.cut_off("gho_HOME");

    let tab = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);

    let Status::Failed(message) = status(&tab, cx) else {
        panic!("the tab opened: {:?}", status(&tab, cx));
    };
    assert!(
        message.starts_with("home's check got no answer"),
        "{message}"
    );
    assert!(message.ends_with("Lo keeps trying GitHub."), "{message}");
    assert_eq!(acting(&tab, cx), None);
    let requests = github.requests();
    assert!(
        !requests.iter().any(|request| request.contains("/pulls/")),
        "{requests:#?}"
    );

    // Once home gets an answer, which is that it can't see it, the tab opens as rozed-bot.
    github.reconnect("gho_HOME");
    cx.executor().advance_clock(Duration::from_secs(5));
    cx.run_until_parked();
    assert_eq!(status(&tab, cx), Status::Ready);
    assert_eq!(acting(&tab, cx), Some(Acting::Account("rozed-bot".into())));
}

#[gpui::test]
async fn offline_the_newest_saved_details_show_whichever_account_saved_them(
    cx: &mut TestAppContext,
) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    let pull = git.pull_request("rozed-dev/playground", 2, &[], &[("a.rs", "a\n")]);
    github.add_pull(pull);
    let (accounts, cache) = sign_in(
        &github,
        &[("home", "gho_HOME"), ("rozed-bot", "gho_BOT")],
        cx,
    )
    .await;
    accounts
        .update(cx, |accounts, cx| {
            accounts.remember_account_for_owner("rozed-dev", "rozed-bot", cx)
        })
        .await
        .unwrap();
    // rozed-bot, the owner's account, saved them weeks ago, and home an hour ago.
    let target = Target::Pull(PullRef::new("rozed-dev", "playground", 2));
    for (login, title, ago) in [
        ("rozed-bot", "Old title", 21 * 86_400),
        ("home", "New title", 3_600 + 30),
    ] {
        let at = SystemTime::now() - Duration::from_secs(ago);
        cache
            .put(login, &target, &details(title, "VIEWED"), None, at)
            .await
            .unwrap();
    }
    github.set_offline(true);

    let tab = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);

    assert_eq!(
        tab.read_with(cx, |tab, _| tab.header().title.clone())
            .as_deref(),
        Some("New title")
    );
    assert_eq!(
        banners(&tab, cx),
        ["Showing what Lo saved for home an hour ago."]
    );
}

#[test]
fn only_an_unreachable_github_counts_as_offline() {
    let network =
        |kind| anyhow::Error::new(GithubError::Network(std::io::Error::from(kind).into()));
    let unreachable = || network(std::io::ErrorKind::ConnectionRefused);
    assert!(is_offline(&unreachable()));
    assert!(is_offline(
        &unreachable().context("asking GitHub for a/b#1")
    ));
    // A slow answer isn't offline.
    assert!(!is_offline(&network(std::io::ErrorKind::TimedOut)));
    assert!(!is_offline(&anyhow::anyhow!(
        "GitHub has no pull request a/b#1, or bot can't see it"
    )));
    assert!(!is_offline(&anyhow::Error::new(
        GithubError::Unauthorized {
            message: "Bad credentials".into()
        }
    )));
}

fn paths(tab: &Entity<PrTab>, cx: &mut VisualTestContext) -> Vec<String> {
    tab.read_with(cx, |tab, _| {
        tab.files()
            .rows()
            .map(|entry| entry.path().to_string())
            .collect()
    })
}

#[gpui::test]
async fn offline_the_saved_base_is_used_though_the_copy_fetched_another_since(
    cx: &mut TestAppContext,
) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    // The pull request's head sits on a stack's lower layer, stack-a, and merges into main.
    let (mut pull, lower) = git.stacked_pull_request(
        "rozed-dev/playground",
        2,
        &[("a.rs", "a\n")],
        &[("a.rs", "a\n"), ("lower.rs", "lower\n")],
        &[("a.rs", "a2\n"), ("lower.rs", "lower\n")],
    );
    pull.title = "Add b".into();
    github.add_pull(pull.clone());
    github.let_see("gho_BOT", "rozed-dev/playground");
    let (_, cache) = sign_in(&github, &[("rozed-bot", "gho_BOT")], cx).await;
    let link = "github.com/rozed-dev/playground/pull/2";

    // Copied against main, which shows the lower layer's file too.
    let tab = open_loaded(&workspace, link, cx);
    assert_eq!(paths(&tab, cx), ["a.rs", "lower.rs"]);
    close(&workspace, tab, cx);
    // Then against stack-a, after a retarget, which doesn't.
    github.retarget("rozed-dev/playground", 2, "stack-a", &lower);
    let tab = open_loaded(&workspace, link, cx);
    assert_eq!(paths(&tab, cx), ["a.rs"]);
    close(&workspace, tab, cx);

    // What Lo saved says main, at the tip the copy fetched first.
    let two_hours_ago = SystemTime::now() - Duration::from_secs(2 * 3_600 + 30);
    cache
        .put(
            "rozed-bot",
            &Target::Pull(PullRef::new("rozed-dev", "playground", 2)),
            &details_based("Add b", "VIEWED", &pull.head, "main", &pull.base),
            None,
            two_hours_ago,
        )
        .await
        .unwrap();
    github.set_offline(true);
    git.hide("rozed-dev/playground");

    let tab = open_loaded(&workspace, link, cx);

    assert!(matches!(status(&tab, cx), Status::Offline(_)));
    assert_eq!(paths(&tab, cx), ["a.rs", "lower.rs"]);
}

#[gpui::test]
async fn when_github_answers_but_loading_fails_the_copy_stays_and_it_keeps_trying(
    cx: &mut TestAppContext,
) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    playground_copied_then_offline(&workspace, &github, &git, saved_as_given, cx).await;
    let tab = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);
    assert!(matches!(status(&tab, cx), Status::Offline(_)));

    // GitHub answers again, but with an error for the repository.
    github.set_offline(false);
    github.answer(
        "gho_BOT",
        "/repos/rozed-dev/playground",
        Answer::new(502, "Bad gateway"),
    );
    cx.executor().advance_clock(Duration::from_secs(5));
    cx.run_until_parked();

    let Status::Offline(message) = status(&tab, cx) else {
        panic!("expected the local copy: {:?}", status(&tab, cx));
    };
    assert!(
        message.starts_with("Lo couldn't load the pull request from GitHub: "),
        "{message}"
    );
    assert!(message.contains("502"), "{message}");
    tab.read_with(cx, |tab, _| assert!(tab.diff().is_some()));
    assert_eq!(old_texts(&tab, cx), ["a\n", "b\n"]);

    // It keeps checking, and the waits keep growing: not again within 5 seconds.
    github.requests();
    cx.executor().advance_clock(Duration::from_secs(5));
    cx.run_until_parked();
    assert_eq!(github.requests(), Vec::<String>::new());

    // Once it can, it loads.
    github.forget_answer("gho_BOT", "/repos/rozed-dev/playground");
    git.restore("rozed-dev/playground");
    cx.executor().advance_clock(Duration::from_secs(5));
    cx.run_until_parked();
    assert_eq!(status(&tab, cx), Status::Ready);
}

#[gpui::test]
async fn a_proxy_that_starts_refusing_is_reported_rather_than_waited_out(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    playground_copied_then_offline(&workspace, &github, &git, saved_as_given, cx).await;
    let tab = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);
    assert!(matches!(status(&tab, cx), Status::Offline(_)));

    // The network's back, behind a proxy that wants a password: waiting won't fix that, so
    // the next check loads, and the tab says why it still shows the copy.
    github.set_offline(false);
    github.set_proxy_refusing(true);
    cx.executor().advance_clock(Duration::from_secs(5));
    cx.run_until_parked();

    let Status::Offline(message) = status(&tab, cx) else {
        panic!("expected the local copy: {:?}", status(&tab, cx));
    };
    assert!(
        message.starts_with("Lo couldn't load the pull request from GitHub: "),
        "{message}"
    );
    assert!(
        message.contains("proxy authentication required"),
        "{message}"
    );
    tab.read_with(cx, |tab, _| assert!(tab.diff().is_some()));
}

#[gpui::test]
async fn the_check_that_github_answers_gives_up_after_a_while(cx: &mut TestAppContext) {
    let github = FakeGithub::default();
    github.hold("/rate_limit");
    let http: std::sync::Arc<dyn http_client::HttpClient> = std::sync::Arc::new(github.clone());
    let answered = cx
        .executor()
        .spawn(github_api::github_answers(http, cx.executor()));
    cx.run_until_parked();

    cx.executor()
        .advance_clock(github_api::ANSWER_TIMEOUT + Duration::from_secs(1));
    cx.run_until_parked();

    assert!(!answered.await);
}

#[gpui::test]
async fn a_refusal_while_the_copy_shows_gives_githubs_link_and_waits_for_the_user(
    cx: &mut TestAppContext,
) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    playground_copied_then_offline(&workspace, &github, &git, saved_as_given, cx).await;
    let tab = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);
    assert!(matches!(status(&tab, cx), Status::Offline(_)));

    // Back online, the organization wants single sign-on, which only the user can give.
    github.set_offline(false);
    git.restore("rozed-dev/playground");
    github.answer("gho_BOT", "/repos/rozed-dev/playground", needs_sso());
    cx.executor().advance_clock(Duration::from_secs(5));
    cx.run_until_parked();

    let Status::Offline(message) = status(&tab, cx) else {
        panic!("expected the local copy: {:?}", status(&tab, cx));
    };
    assert!(
        message.starts_with("Lo couldn't load the pull request from GitHub: "),
        "{message}"
    );
    assert!(
        message.ends_with("This is the local copy's diff, read-only."),
        "{message}"
    );
    assert!(!message.contains(".."), "{message}");
    assert_eq!(
        help_links(&tab, cx),
        ["https://github.com/orgs/acme/sso?authorization_request=A1"]
    );
    assert_eq!(action(&tab, cx), Some(TabAction::TryAgain));
    // Nothing's tried again by itself.
    github.requests();
    cx.executor().advance_clock(Duration::from_secs(10 * 60));
    cx.run_until_parked();
    assert_eq!(github.requests(), Vec::<String>::new());

    // Once authorized, the button loads it.
    github.forget_answer("gho_BOT", "/repos/rozed-dev/playground");
    tab.update_in(cx, |tab, window, cx| tab.load_again(window, cx));
    cx.run_until_parked();
    assert_eq!(status(&tab, cx), Status::Ready);
}

#[gpui::test]
async fn a_refusal_waiting_wont_fix_fails_a_fresh_tab_with_a_button_to_try_again(
    cx: &mut TestAppContext,
) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    playground_copied_then_offline(&workspace, &github, &git, saved_as_given, cx).await;
    // A proxy that wants a password, or a captive portal's certificate: not offline.
    github.set_offline(false);
    github.set_proxy_refusing(true);

    let tab = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);

    let Status::Failed(message) = status(&tab, cx) else {
        panic!("expected the tab to fail: {:?}", status(&tab, cx));
    };
    assert!(
        message.contains("proxy authentication required"),
        "{message}"
    );
    assert!(!message.contains("keeps trying"), "{message}");
    assert_eq!(action(&tab, cx), Some(TabAction::TryAgain));
    github.requests();
    cx.executor().advance_clock(Duration::from_secs(10 * 60));
    cx.run_until_parked();
    assert_eq!(github.requests(), Vec::<String>::new());

    github.set_proxy_refusing(false);
    git.restore("rozed-dev/playground");
    tab.update_in(cx, |tab, window, cx| tab.load_again(window, cx));
    cx.run_until_parked();
    assert_eq!(status(&tab, cx), Status::Ready);
}

#[gpui::test]
async fn a_failed_load_keeps_the_copys_viewed_marks(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    let pull = git.pull_request(
        "rozed-dev/playground",
        2,
        &[("a.rs", "a\n"), ("b.rs", "b\n")],
        &[("a.rs", "a2\n"), ("b.rs", "b2\n")],
    );
    github.add_pull(pull.clone());
    github.let_see("gho_BOT", "rozed-dev/playground");
    let (_, cache) = sign_in(
        &github,
        &[("other", "gho_OTHER"), ("rozed-bot", "gho_BOT")],
        cx,
    )
    .await;
    let link = "github.com/rozed-dev/playground/pull/2";
    let tab = open_loaded(&workspace, link, cx);
    assert_eq!(status(&tab, cx), Status::Ready);
    close(&workspace, tab, cx);
    // What Lo saved last is another account's, with its viewed marks.
    let target = Target::Pull(PullRef::new("rozed-dev", "playground", 2));
    cache
        .put(
            "other",
            &target,
            &saved_as_given(&pull),
            None,
            two_hours_ago(),
        )
        .await
        .unwrap();
    github.set_offline(true);
    git.hide("rozed-dev/playground");
    let tab = open_loaded(&workspace, link, cx);
    let marks = |cx: &mut VisualTestContext| {
        tab.read_with(cx, |tab, _| {
            tab.files()
                .rows()
                .map(|entry| entry.viewed)
                .collect::<Vec<_>>()
        })
    };
    let viewed = vec![Some(ViewedState::Viewed), Some(ViewedState::Unviewed)];
    assert_eq!(marks(cx), viewed);

    // Back online, rozed-bot's request for the pull request fails, after the account is
    // chosen: the copy keeps the marks it shows.
    github.set_offline(false);
    github.answer(
        "gho_BOT",
        "/repos/rozed-dev/playground/pulls/2",
        Answer::new(502, "Bad gateway"),
    );
    cx.executor().advance_clock(Duration::from_secs(5));
    cx.run_until_parked();
    assert!(matches!(status(&tab, cx), Status::Offline(_)));
    assert_eq!(acting(&tab, cx), Some(Acting::Account("rozed-bot".into())));
    assert_eq!(marks(cx), viewed);
}

#[gpui::test]
async fn a_rows_tab_shows_its_viewed_marks_and_offline_e_says_why_it_cant_mark(
    cx: &mut TestAppContext,
) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    let pull = git.pull_request(
        "rozed-dev/playground",
        2,
        &[("a.rs", "a\n"), ("b.rs", "b\n")],
        &[("a.rs", "a2\n"), ("b.rs", "b2\n")],
    );
    github.add_pull(pull.clone());
    github.let_see("gho_BOT", "rozed-dev/playground");
    let (_, cache) = sign_in(&github, &[("rozed-bot", "gho_BOT")], cx).await;
    // What Lo saved for the account, with its viewed marks.
    let target = Target::Pull(PullRef::new("rozed-dev", "playground", 2));
    cache
        .put(
            "rozed-bot",
            &target,
            &saved_as_given(&pull),
            None,
            two_hours_ago(),
        )
        .await
        .unwrap();
    let open_row = |cx: &mut VisualTestContext| {
        let tab = workspace.update_in(cx, |workspace, window, cx| {
            open_as(
                workspace,
                link("github.com/rozed-dev/playground/pull/2"),
                Trust::Untrusted,
                "rozed-bot".into(),
                window,
                cx,
            )
        });
        cx.run_until_parked();
        tab
    };
    let viewed = [
        mark("a.rs", Some(ViewedState::Viewed)),
        mark("b.rs", Some(ViewedState::Unviewed)),
    ];

    // Opened from the recent list, the account's viewed marks show on its files.
    let tab = open_row(cx);
    assert_eq!(status(&tab, cx), Status::Ready);
    assert_eq!(marks(&tab, cx), viewed);
    close(&workspace, tab, cx);
    // Where this Mac recorded a.rs viewed.
    let records = cx.update(|_, cx| {
        let layer = github_layer::loaded(cx).unwrap();
        layer.read(cx).viewed_records().clone()
    });
    let pull_ref = PullRef::new("rozed-dev", "playground", 2);
    records
        .record(
            "rozed-bot",
            &pull_ref,
            "a.rs",
            &pull.head,
            SystemTime::now(),
        )
        .await
        .unwrap();

    // Offline, the local copy shows with the marks Lo saved, and `e` marks nothing and
    // says why.
    github.set_offline(true);
    let tab = open_row(cx);
    assert!(matches!(status(&tab, cx), Status::Offline(_)));
    assert_eq!(marks(&tab, cx), viewed);
    assert_eq!(
        tab.read_with(cx, |tab, _| tab.pull.viewed_at.get("a.rs").cloned()),
        Some(pull.head.clone())
    );
    tab.update_in(cx, |tab, window, cx| tab.focus_diff(window, cx));
    cx.simulate_keystrokes("e");
    assert_eq!(
        toasts(cx),
        [
            "Lo can't reach GitHub, so it can't mark a.rs viewed until it's back. Until \
             then, this is the local copy, read-only."
        ]
    );
    assert_eq!(marks(&tab, cx), viewed);
}

#[gpui::test]
async fn back_online_the_older_copy_stays_until_the_newer_push_is_copied(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    let repo = "rozed-dev/playground";
    let pull = git.pull_request(
        repo,
        2,
        &[("a.rs", "a\n"), ("b.rs", "b\n")],
        &[("a.rs", "a2\n"), ("b.rs", "b2\n")],
    );
    github.add_pull(pull.clone());
    github.let_see("gho_BOT", repo);
    let (_, cache) = sign_in(&github, &[("rozed-bot", "gho_BOT")], cx).await;
    let link = "github.com/rozed-dev/playground/pull/2";
    let tab = open_loaded(&workspace, link, cx);
    assert_eq!(status(&tab, cx), Status::Ready);
    close(&workspace, tab, cx);
    // What Lo saved at that head, with no file viewed yet.
    let target = Target::Pull(PullRef::new("rozed-dev", "playground", 2));
    let saved = details_based(
        "Add b",
        "UNVIEWED",
        &pull.head,
        &pull.base_branch,
        &pull.base,
    );
    cache
        .put("rozed-bot", &target, &saved, None, two_hours_ago())
        .await
        .unwrap();
    // A push the local copy hasn't fetched.
    let newer = git.push(repo, 2, &[("a.rs", "a3\n"), ("b.rs", "b3\n")]);

    // Offline, the copy of the saved head shows, and the reviewer reads b.rs.
    github.set_offline(true);
    git.hide(repo);
    let tab = open_loaded(&workspace, link, cx);
    assert!(matches!(status(&tab, cx), Status::Offline(_)));
    go_to(&tab, "b.rs", cx);
    let older_copy = tab.read_with(cx, |tab, _| tab.diff().cloned()).unwrap();
    let unviewed = [
        mark("a.rs", Some(ViewedState::Unviewed)),
        mark("b.rs", Some(ViewedState::Unviewed)),
    ];

    // Back online, GitHub names the newer push, but copying it fails: the older copy stays,
    // read-only, saying so, with a button to try again.
    github.set_head(repo, 2, &newer);
    github.set_offline(false);
    cx.executor().advance_clock(Duration::from_secs(5));
    cx.run_until_parked();
    let Status::Offline(text) = status(&tab, cx) else {
        panic!("{:?}", status(&tab, cx));
    };
    assert!(
        text.starts_with(&format!(
            "GitHub has {}, but Lo couldn't copy it: ",
            &newer[..7]
        )),
        "{text}"
    );
    assert!(
        text.ends_with(&format!(
            "This is the local copy of {}, read-only.",
            &pull.head[..7]
        )),
        "{text}"
    );
    assert!(!text.contains(".."), "{text}");
    assert_eq!(action(&tab, cx), Some(TabAction::TryAgain));
    assert_eq!(
        tab.read_with(cx, |tab, _| tab.diff().cloned()),
        Some(older_copy.clone())
    );
    assert!(tab.read_with(cx, |tab, _| tab.status().tries_again()));

    // GitHub would mark b.rs as of the newer push, which isn't on screen, so `e` doesn't.
    github.hold("/graphql");
    tab.update_in(cx, |tab, window, cx| tab.focus_diff(window, cx));
    cx.simulate_keystrokes("e");
    let toasts = toasts(cx);
    assert!(
        toasts
            .last()
            .is_some_and(|toast| toast.starts_with("There's a newer push")),
        "{toasts:#?}"
    );
    assert_eq!(marks(&tab, cx), unviewed);

    // Trying again copies the newer push, which replaces the older copy where the reviewer
    // was, and `e` marks b.rs.
    git.restore(repo);
    tab.update_in(cx, |tab, window, cx| tab.load_again(window, cx));
    cx.run_until_parked();
    assert_eq!(status(&tab, cx), Status::Ready);
    assert_ne!(
        tab.read_with(cx, |tab, _| tab.diff().cloned()),
        Some(older_copy)
    );
    assert_eq!(cursor_file(&tab, cx).as_deref(), Some("b.rs"));
    tab.update_in(cx, |tab, window, cx| tab.focus_diff(window, cx));
    cx.simulate_keystrokes("e");
    assert_eq!(marks(&tab, cx)[1], mark("b.rs", Some(ViewedState::Viewed)));
    github.let_go("/graphql");
}

#[gpui::test]
async fn a_copy_stopped_behind_an_older_copy_keeps_it_and_says_so(cx: &mut TestAppContext) {
    let (_, tab, _signed_in, cx) = three_files("copy_stopped_older", &[], &[], cx).await;
    let mut newer = fd_2212();
    newer.head.sha = NEWER.into();
    let (retry, _asked) = oneshot::channel();
    tab.update(cx, |tab, cx| {
        tab.set_pull_request(&newer, SystemTime::now(), cx);
        tab.copy_stopped("the disk is full.", 5, retry, cx);
    });
    assert_eq!(
        status(&tab, cx),
        Status::Offline(
            "GitHub has 3333333, but Lo couldn't copy it: the disk is full. This is the \
             local copy of 2222222, read-only."
                .into()
        )
    );

    // The banner's button tries the copy again.
    tab.update(cx, |tab, cx| tab.retry_copy(cx));
    assert!(matches!(status(&tab, cx), Status::Loading(_)));
    assert_eq!(tab.read_with(cx, |tab, _| tab.showing_copy), None);
}

#[gpui::test]
async fn after_github_listed_a_newer_head_its_older_copy_doesnt_mark_as_of_it(
    cx: &mut TestAppContext,
) {
    let (workspace, cx) = workspace(cx).await;
    let tab = open_unloaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);
    let signed_in = sign_in_bot(&tab, "e_after_the_switch", &[], cx).await;
    // A minute ago GitHub said the head was HEAD. It then listed the files at NEWER, and its
    // details say NEWER.
    let answered_at = SystemTime::now() - Duration::from_secs(60);
    tab.update(cx, |tab, cx| {
        tab.set_pull_request(&fd_2212(), answered_at, cx)
    });
    show_github_files(&tab, vec![github_file_at("a.rs", 3, NEWER)], cx);
    tab.update(cx, |tab, cx| {
        tab.set_details(&details_at(NEWER, &[("a.rs", "UNVIEWED")]), cx)
    });

    // The local copy of HEAD, which GitHub gave first, replaces GitHub's diff.
    let files = [("a.rs", lines(5, &[]), lines(5, &[3]))];
    let head: Oid = HEAD.parse().unwrap();
    let editor = test_diff_editor(&workspace, cx);
    let positions = tab.update(cx, |tab, cx| {
        tab.copied(
            local_entries(&[changed("a.rs", Change::Modified)]),
            &head,
            answered_at,
            cx,
        )
    });
    add_test_files(&editor, &files, &positions, cx).await;
    tab.update_in(cx, |tab, window, cx| {
        tab.switch_diff(editor.clone(), &head, window, cx)
    });
    cx.run_until_parked();
    assert_eq!(
        tab.read_with(cx, |tab, _| tab.pull.head.clone()),
        Some(HEAD.to_string())
    );

    // GitHub would mark a.rs as of NEWER, which isn't on screen.
    tab.update_in(cx, |tab, window, cx| tab.focus_diff(window, cx));
    cx.simulate_keystrokes("e");
    assert_eq!(signed_in.github.viewed_mutations(), []);
    assert!(toasts(cx)[0].starts_with("There's a newer push"));
}

#[test]
fn only_what_waiting_may_fix_is_tried_again_by_itself() {
    let network = |error: anyhow::Error| anyhow::Error::new(GithubError::Network(error));
    // A local copy's fetch that failed on the network.
    let fetch = anyhow::Error::new(repo_cache::Error::Fetch {
        what: "pull request #1".into(),
        failure: repo_cache::FetchFailure::Network,
        message: "Could not resolve host: github.com".into(),
    })
    .context("copying acme/app#1 locally");
    assert!(is_waitable(&fetch));
    // A proxy that couldn't open the tunnel to GitHub.
    let tunnel = network(anyhow::anyhow!("unsuccessful tunnel").context("client error (Connect)"));
    assert!(is_waitable(&tunnel));
    // A refusal for single sign-on, and a choice of account, are the user's to fix.
    let sso = Refusal {
        message: "sso".into(),
        links: Vec::new(),
        waitable: false,
    };
    assert!(!is_waitable(&sso.into()));
    let needed = anyhow::Error::new(AccountNeeded {
        message: "No account was chosen for acme".into(),
    });
    assert!(!is_waitable(&needed));
    assert_eq!(action_for(&needed), TabAction::ChooseAccount);
    let tls = network(anyhow::anyhow!("invalid peer certificate: UnknownIssuer"));
    assert!(!is_waitable(&tls));
    assert_eq!(action_for(&tls), TabAction::TryAgain);
    // A message that ends a sentence isn't given a second full stop.
    assert_eq!(
        sentence(&anyhow::anyhow!(
            "GitHub answered 502 for a/b#1: Bad gateway."
        )),
        "GitHub answered 502 for a/b#1: Bad gateway"
    );
}
