use super::*;

#[gpui::test]
async fn a_public_pull_request_loads_without_signing_in(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    let pull = public_pull(&github, &git, "sharkdp/fd", 2212);
    anonymous(&github, cx);

    let tab = open_loaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);

    assert_eq!(status(&tab, cx), Status::Ready);
    tab.read_with(cx, |tab, cx| {
        assert_eq!(tab.acting(), Some(&Acting::Anonymous));
        assert_eq!(tab.header().title.as_deref(), Some("Pull request 2212"));
        assert_eq!(tab.header().repo.as_deref(), Some("sharkdp/fd"));
        let paths: Vec<_> = tab.files().rows().map(FileEntry::path).collect();
        assert_eq!(paths, ["src/main.rs", "tests/main.rs", "Cargo.lock"]);
        let revisions = tab.revisions().unwrap();
        assert_eq!(revisions.label(&pull.head).as_deref(), Some("r1"));
        assert!(tab.diff().is_some());
        assert_eq!(tab.tab_content_text(0, cx), "fd#2212 Pull request 2212");
    });
    // GitHub's diff, if it showed, and the local copy's are of the same head.
    assert_eq!(banners(&tab, cx), Vec::<String>::new());
    assert_eq!(
        github.requests(),
        [
            "GET /repos/sharkdp/fd/pulls/2212 as nobody",
            "GET /repos/sharkdp/fd/pulls/2212/files?per_page=100 as nobody"
        ]
    );
}

#[gpui::test]
async fn a_private_pull_request_loads_with_the_account_that_can_see_it(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    let pull = git.pull_request(
        "rozed-dev/playground",
        2,
        &[("a.rs", "a\n")],
        &[("a.rs", "b\n")],
    );
    github.add_pull(pull);
    github.let_see("gho_BOT", "rozed-dev/playground");
    sign_in(
        &github,
        &[("other", "gho_OTHER"), ("rozed-bot", "gho_BOT")],
        cx,
    )
    .await;

    let tab = open_loaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);

    assert_eq!(status(&tab, cx), Status::Ready);
    tab.read_with(cx, |tab, _| {
        assert_eq!(tab.acting(), Some(&Acting::Account("rozed-bot".into())));
        assert_eq!(tab.acting_text(), "as rozed-bot");
        let paths: Vec<_> = tab.files().rows().map(FileEntry::path).collect();
        assert_eq!(paths, ["a.rs"]);
    });
    let requests = github.requests();
    assert!(
        requests.contains(&"GET /repos/rozed-dev/playground/pulls/2 as gho_BOT".to_string()),
        "{requests:#?}"
    );
    assert!(
        !requests
            .iter()
            .any(|request| request.ends_with("as nobody")),
        "{requests:#?}"
    );
}

/// What the account picker asks about, an owner or one repository, and the accounts it names,
/// if it's open.
fn picker(
    workspace: &Entity<Workspace>,
    cx: &mut VisualTestContext,
) -> Option<(String, Vec<String>)> {
    workspace.read_with(cx, |workspace, cx| {
        let picker = workspace.active_modal::<account_picker::AccountPicker>(cx)?;
        let question = picker.read(cx).question();
        Some((question.subject(), question.logins.clone()))
    })
}

fn logins(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| name.to_string()).collect()
}

/// Starts loading `tabs`, one after another.
fn load(tabs: &[&Entity<PrTab>], cx: &mut VisualTestContext) {
    for tab in tabs {
        tab.update_in(cx, |tab, window, cx| tab.load(window, cx));
        cx.run_until_parked();
    }
}

/// Makes `tab` the active tab, as clicking it does.
fn activate(workspace: &Entity<Workspace>, tab: &Entity<PrTab>, cx: &mut VisualTestContext) {
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.activate_item(tab, true, true, window, cx)
    });
    cx.run_until_parked();
}

/// Pull requests on the fake GitHub, each `(repo, number, tokens that can see it)`, and the
/// accounts `(login, token)` signed in.
async fn pulls_seen_by(
    github: &FakeGithub,
    git: &GitServer,
    pulls: &[(&str, u64, &[&str])],
    accounts: &[(&str, &str)],
    cx: &mut TestAppContext,
) -> Entity<github::accounts::Accounts> {
    for (repo, number, tokens) in pulls {
        let pull = git.pull_request(repo, *number, &[], &[("a.rs", "a\n")]);
        github.add_pull(pull);
        for token in *tokens {
            github.let_see(token, repo);
        }
    }
    sign_in(github, accounts, cx).await.0
}

/// `acme/app#1` and `acme/api#2` on the fake GitHub, which `home` and `work` can both see.
async fn two_accounts_see_acme(
    github: &FakeGithub,
    git: &GitServer,
    cx: &mut TestAppContext,
) -> Entity<github::accounts::Accounts> {
    for (repo, number) in [("acme/app", 1), ("acme/api", 2)] {
        let pull = git.pull_request(repo, number, &[], &[("a.rs", "a\n")]);
        github.add_pull(pull);
        github.let_see("gho_HOME", repo);
        github.let_see("gho_WORK", repo);
    }
    let (accounts, _) = sign_in(github, &[("home", "gho_HOME"), ("work", "gho_WORK")], cx).await;
    accounts
}

#[gpui::test]
async fn with_several_accounts_it_asks_once_per_owner_and_remembers(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    let accounts = two_accounts_see_acme(&github, &git, cx).await;

    // The picker names each account, and the tab waits for the answer.
    let tab = open_loaded(&workspace, "github.com/acme/app/pull/1", cx);
    assert_eq!(
        picker(&workspace, cx),
        Some((
            "acme".to_string(),
            vec!["home".to_string(), "work".to_string()]
        ))
    );
    assert!(matches!(status(&tab, cx), Status::Loading(_)));
    assert_eq!(tab.read_with(cx, |tab, _| tab.acting().cloned()), None);
    github.requests();

    cx.simulate_keystrokes("j enter");
    cx.run_until_parked();

    assert_eq!(picker(&workspace, cx), None);
    assert_eq!(status(&tab, cx), Status::Ready);
    assert_eq!(tab.read_with(cx, |tab, _| tab.acting_text()), "as work");
    let requests = github.requests();
    assert!(
        requests.contains(&"GET /repos/acme/app/pulls/1 as gho_WORK".to_string()),
        "{requests:#?}"
    );
    assert_eq!(
        accounts.read_with(cx, |accounts, _| accounts
            .remembered_account("acme")
            .unwrap()),
        Some("work".to_string())
    );

    // Another of the owner's repositories opens with it, without asking.
    let api = open_loaded(&workspace, "github.com/acme/api/pull/2", cx);
    assert_eq!(picker(&workspace, cx), None);
    assert_eq!(status(&api, cx), Status::Ready);
    assert_eq!(api.read_with(cx, |tab, _| tab.acting_text()), "as work");
}

#[gpui::test]
async fn closing_the_account_picker_opens_nothing_until_the_link_is_opened_again(
    cx: &mut TestAppContext,
) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    let accounts = two_accounts_see_acme(&github, &git, cx).await;

    let tab = open_loaded(&workspace, "github.com/acme/app/pull/1", cx);
    github.requests();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    assert_eq!(picker(&workspace, cx), None);
    let Status::Failed(message) = status(&tab, cx) else {
        panic!("the tab opened: {:?}", status(&tab, cx));
    };
    assert_eq!(message.as_ref(), "No account was chosen for acme.");
    assert_eq!(
        tab.read_with(cx, |tab, _| tab.pull.action),
        Some(TabAction::ChooseAccount)
    );
    // It doesn't ask again by itself.
    cx.executor().advance_clock(Duration::from_secs(10 * 60));
    cx.run_until_parked();
    assert_eq!(picker(&workspace, cx), None);
    assert_eq!(github.requests(), Vec::<String>::new());
    assert_eq!(
        accounts.read_with(cx, |accounts, _| accounts
            .remembered_account("acme")
            .unwrap()),
        None
    );

    // Opening it again asks again.
    let reopened = open_loaded(&workspace, "github.com/acme/app/pull/1", cx);
    assert_eq!(reopened, tab);
    assert_eq!(
        picker(&workspace, cx).map(|(owner, _)| owner),
        Some("acme".to_string())
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(status(&tab, cx), Status::Ready);
    assert_eq!(tab.read_with(cx, |tab, _| tab.acting_text()), "as home");
}

#[gpui::test]
async fn two_tabs_waiting_on_the_same_owner_share_one_answer(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    two_accounts_see_acme(&github, &git, cx).await;

    let app = open_unloaded(&workspace, "github.com/acme/app/pull/1", cx);
    let api = open_unloaded(&workspace, "github.com/acme/api/pull/2", cx);
    for tab in [&app, &api] {
        tab.update_in(cx, |tab, window, cx| tab.load(window, cx));
    }
    cx.run_until_parked();
    assert!(picker(&workspace, cx).is_some());

    cx.simulate_keystrokes("down enter");
    cx.run_until_parked();

    assert_eq!(picker(&workspace, cx), None);
    for tab in [&app, &api] {
        assert_eq!(status(tab, cx), Status::Ready);
        assert_eq!(tab.read_with(cx, |tab, _| tab.acting_text()), "as work");
    }
}

#[gpui::test]
async fn closing_the_only_tab_waiting_on_the_picker_closes_it(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    two_accounts_see_acme(&github, &git, cx).await;
    let tab = open_loaded(&workspace, "github.com/acme/app/pull/1", cx);
    assert!(picker(&workspace, cx).is_some());

    close(&workspace, tab, cx);

    assert_eq!(picker(&workspace, cx), None);
}

/// With the picker listing `home` and `work`, `work` is removed, or signed out: it drops out,
/// and the picker asks again with `home` alone, rather than open as an account that can't.
async fn an_account_dropping_out_of_the_picker(signed_out: bool, cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    let accounts = two_accounts_see_acme(&github, &git, cx).await;
    let tab = open_loaded(&workspace, "github.com/acme/app/pull/1", cx);
    assert_eq!(
        picker(&workspace, cx),
        Some(("acme".to_string(), logins(&["home", "work"])))
    );
    if signed_out {
        accounts.update(cx, |accounts, cx| {
            accounts.mark_signed_out("work", github::accounts::SignedOut::Refused, cx)
        });
    } else {
        accounts
            .update(cx, |accounts, cx| accounts.remove("work", cx))
            .await
            .unwrap();
    }
    cx.run_until_parked();

    assert_eq!(
        picker(&workspace, cx),
        Some(("acme".to_string(), logins(&["home"])))
    );
    assert_eq!(acting(&tab, cx), None);
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(acting(&tab, cx), Some(Acting::Account("home".into())));
    assert_eq!(
        accounts.read_with(cx, |accounts, _| accounts
            .remembered_account("acme")
            .unwrap()),
        Some("home".to_string())
    );
}

#[gpui::test]
async fn an_account_removed_while_the_picker_lists_it_drops_out(cx: &mut TestAppContext) {
    an_account_dropping_out_of_the_picker(false, cx).await;
}

#[gpui::test]
async fn an_account_signed_out_while_the_picker_lists_it_drops_out(cx: &mut TestAppContext) {
    an_account_dropping_out_of_the_picker(true, cx).await;
}

#[gpui::test]
async fn an_answer_goes_only_to_the_tabs_whose_accounts_include_it(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    // home and work can see acme/app; home and personal can see acme/secret.
    let accounts = pulls_seen_by(
        &github,
        &git,
        &[
            ("acme/app", 1, &["gho_HOME", "gho_WORK"]),
            ("acme/secret", 2, &["gho_HOME", "gho_PERSONAL"]),
        ],
        &[
            ("home", "gho_HOME"),
            ("personal", "gho_PERSONAL"),
            ("work", "gho_WORK"),
        ],
        cx,
    )
    .await;
    let app = open_unloaded(&workspace, "github.com/acme/app/pull/1", cx);
    let secret = open_unloaded(&workspace, "github.com/acme/secret/pull/2", cx);
    activate(&workspace, &app, cx);
    load(&[&app, &secret], cx);

    // Both checks are done before the answer, and the picker lists the active tab's
    // accounts, acme/app's.
    let requests = github.requests();
    assert!(
        requests.contains(&"GET /repos/acme/secret as gho_PERSONAL".to_string()),
        "{requests:#?}"
    );
    assert_eq!(
        picker(&workspace, cx),
        Some(("acme".to_string(), logins(&["home", "work"])))
    );
    for tab in [&app, &secret] {
        assert!(matches!(status(tab, cx), Status::Loading(_)));
        assert_eq!(acting(tab, cx), None);
    }

    cx.simulate_keystrokes("j enter");
    cx.run_until_parked();

    // acme/app opens as work. work can't see acme/secret, so it's asked about with its own
    // accounts, for itself alone, once it's the active tab, and isn't opened as work.
    assert_eq!(status(&app, cx), Status::Ready);
    assert_eq!(acting(&app, cx), Some(Acting::Account("work".into())));
    assert_eq!(picker(&workspace, cx), None);
    assert_eq!(
        status(&secret, cx),
        Status::Loading("Waiting to ask which account Lo uses for acme/secret…".into())
    );
    activate(&workspace, &secret, cx);
    assert_eq!(
        picker(&workspace, cx),
        Some(("acme/secret".to_string(), logins(&["home", "personal"])))
    );
    assert!(matches!(status(&secret, cx), Status::Loading(_)));
    assert_eq!(acting(&secret, cx), None);
    let requests = github.requests();
    assert!(
        !requests
            .iter()
            .any(|request| request.contains("/repos/acme/secret/pulls/")),
        "{requests:#?}"
    );

    cx.simulate_keystrokes("j enter");
    cx.run_until_parked();

    assert_eq!(status(&secret, cx), Status::Ready);
    assert_eq!(
        acting(&secret, cx),
        Some(Acting::Account("personal".into()))
    );
    // acme keeps work, and acme/secret has its own.
    accounts.read_with(cx, |accounts, _| {
        assert_eq!(
            accounts.remembered("acme", "secret").unwrap(),
            github::accounts::Remembered {
                repo: Some("personal".into()),
                owner: Some("work".into()),
            }
        );
    });
}

#[gpui::test]
async fn when_the_owners_account_cant_see_a_repository_it_asks_for_that_repository_alone(
    cx: &mut TestAppContext,
) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    let accounts = pulls_seen_by(
        &github,
        &git,
        &[
            ("acme/app", 1, &["gho_HOME", "gho_WORK"]),
            ("acme/secret", 2, &["gho_HOME", "gho_PERSONAL"]),
        ],
        &[
            ("home", "gho_HOME"),
            ("personal", "gho_PERSONAL"),
            ("work", "gho_WORK"),
        ],
        cx,
    )
    .await;
    accounts
        .update(cx, |accounts, cx| {
            accounts.remember_account_for_owner("acme", "work", cx)
        })
        .await
        .unwrap();

    let secret = open_loaded(&workspace, "github.com/acme/secret/pull/2", cx);
    assert_eq!(
        picker(&workspace, cx),
        Some(("acme/secret".to_string(), logins(&["home", "personal"])))
    );
    let owners_account = workspace.read_with(cx, |workspace, cx| {
        let picker = workspace
            .active_modal::<account_picker::AccountPicker>(cx)
            .unwrap();
        picker.read(cx).question().owners_account.clone()
    });
    assert_eq!(owners_account.as_deref(), Some("work"));
    assert_eq!(
        status(&secret, cx),
        Status::Loading("Choose the account Lo uses for acme/secret…".into())
    );

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(acting(&secret, cx), Some(Acting::Account("home".into())));
    assert_eq!(
        accounts.read_with(cx, |accounts, _| accounts
            .remembered_account("acme")
            .unwrap()),
        Some("work".to_string())
    );

    // The owner's other repositories still open with work, and acme/secret with home, without
    // asking.
    let app = open_loaded(&workspace, "github.com/acme/app/pull/1", cx);
    assert_eq!(picker(&workspace, cx), None);
    assert_eq!(acting(&app, cx), Some(Acting::Account("work".into())));
    close(&workspace, secret, cx);
    let secret = open_loaded(&workspace, "github.com/acme/secret/pull/2", cx);
    assert_eq!(picker(&workspace, cx), None);
    assert_eq!(status(&secret, cx), Status::Ready);
    assert_eq!(acting(&secret, cx), Some(Acting::Account("home".into())));
}

#[gpui::test]
async fn a_transferred_repository_uses_and_keeps_its_current_owners_account(
    cx: &mut TestAppContext,
) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    // alice/tool was transferred to BigOrg.
    let mut pull = git.pull_request("BigOrg/tool", 1, &[], &[("a.rs", "a\n")]);
    pull.old_names = vec!["alice/tool".into()];
    github.add_pull(pull);
    for token in ["gho_HOME", "gho_WORK"] {
        github.let_see(token, "BigOrg/tool");
    }
    let (accounts, _) = sign_in(&github, &[("home", "gho_HOME"), ("work", "gho_WORK")], cx).await;
    accounts
        .update(cx, |accounts, cx| {
            accounts.remember_account_for_owner("alice", "home", cx)
        })
        .await
        .unwrap();

    // An old link asks about the owner now, not alice, whose account is home.
    let tab = open_loaded(&workspace, "github.com/alice/tool/pull/1", cx);
    assert_eq!(
        picker(&workspace, cx),
        Some(("BigOrg".to_string(), logins(&["home", "work"])))
    );
    cx.simulate_keystrokes("j enter");
    cx.run_until_parked();
    assert_eq!(acting(&tab, cx), Some(Acting::Account("work".into())));
    accounts.read_with(cx, |accounts, _| {
        assert_eq!(
            accounts.remembered_account("bigorg").unwrap().as_deref(),
            Some("work")
        );
        assert_eq!(
            accounts.remembered_account("alice").unwrap().as_deref(),
            Some("home")
        );
    });

    // Opened by its old name again, it uses BigOrg's choice.
    close(&workspace, tab, cx);
    let tab = open_loaded(&workspace, "github.com/alice/tool/pull/1", cx);
    assert_eq!(picker(&workspace, cx), None);
    assert_eq!(acting(&tab, cx), Some(Acting::Account("work".into())));
}

#[gpui::test]
async fn another_dialog_or_a_click_elsewhere_leaves_the_picker_and_its_tab_waiting(
    cx: &mut TestAppContext,
) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    two_accounts_see_acme(&github, &git, cx).await;
    let tab = open_loaded(&workspace, "github.com/acme/app/pull/1", cx);
    let asking = Some(("acme".to_string(), logins(&["home", "work"])));
    assert_eq!(picker(&workspace, cx), asking);

    // cmd-o's box doesn't replace it.
    cx.simulate_keystrokes("cmd-o");
    cx.run_until_parked();
    assert_eq!(picker(&workspace, cx), asking);
    // Nor does a click outside it.
    cx.simulate_click(gpui::point(px(10.), px(600.)), gpui::Modifiers::none());
    cx.run_until_parked();
    assert_eq!(picker(&workspace, cx), asking);
    assert!(matches!(status(&tab, cx), Status::Loading(_)));

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(status(&tab, cx), Status::Ready);
    assert_eq!(acting(&tab, cx), Some(Acting::Account("home".into())));
}

#[gpui::test]
async fn a_question_waits_while_another_dialog_is_open(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    two_accounts_see_acme(&github, &git, cx).await;
    let tab = open_unloaded(&workspace, "github.com/acme/app/pull/1", cx);
    let open_box = |cx: &mut VisualTestContext| {
        workspace.read_with(cx, |workspace, cx| {
            workspace
                .active_modal::<crate::OpenBox>(cx)
                .map(|open_box| open_box.read(cx).input().read(cx).text(cx))
        })
    };
    cx.simulate_keystrokes("cmd-o");
    cx.simulate_input("github.com/acme");

    // The tab's question waits for the box, which keeps what's typed.
    load(&[&tab], cx);
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    assert_eq!(open_box(cx).as_deref(), Some("github.com/acme"));
    assert_eq!(picker(&workspace, cx), None);
    assert!(matches!(status(&tab, cx), Status::Loading(_)));

    cx.simulate_keystrokes("escape");
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    assert_eq!(
        picker(&workspace, cx),
        Some(("acme".to_string(), logins(&["home", "work"])))
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(status(&tab, cx), Status::Ready);
}

#[gpui::test]
async fn a_question_waits_until_its_tab_is_active(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    let both: &[&str] = &["gho_HOME", "gho_WORK"];
    pulls_seen_by(
        &github,
        &git,
        &[("acme/app", 1, both), ("beta/x", 2, both)],
        &[("home", "gho_HOME"), ("work", "gho_WORK")],
        cx,
    )
    .await;
    let acme = open_unloaded(&workspace, "github.com/acme/app/pull/1", cx);
    let beta = open_unloaded(&workspace, "github.com/beta/x/pull/2", cx);
    // beta is the active tab, and acme loads in the background, as a tab coming back online
    // does.
    load(&[&acme, &beta], cx);

    // The picker asks about the active tab's owner. acme's question waits, without the
    // picker over beta, and its tab doesn't fail.
    assert_eq!(
        picker(&workspace, cx)
            .map(|(subject, _)| subject)
            .as_deref(),
        Some("beta")
    );
    assert_eq!(
        status(&acme, cx),
        Status::Loading("Waiting to ask which account Lo uses for acme…".into())
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(status(&beta, cx), Status::Ready);
    assert_eq!(picker(&workspace, cx), None);
    assert!(matches!(status(&acme, cx), Status::Loading(_)));

    // Once acme's tab is active, the picker asks about acme.
    activate(&workspace, &acme, cx);
    assert_eq!(
        picker(&workspace, cx)
            .map(|(subject, _)| subject)
            .as_deref(),
        Some("acme")
    );
    cx.simulate_keystrokes("j enter");
    cx.run_until_parked();
    assert_eq!(status(&acme, cx), Status::Ready);
    assert_eq!(acting(&acme, cx), Some(Acting::Account("work".into())));
    assert_eq!(acting(&beta, cx), Some(Acting::Account("home".into())));
}

#[gpui::test]
async fn a_check_that_finishes_after_the_answer_uses_it(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    let accounts = two_accounts_see_acme(&github, &git, cx).await;
    let app = open_unloaded(&workspace, "github.com/acme/app/pull/1", cx);
    let api = open_unloaded(&workspace, "github.com/acme/api/pull/2", cx);
    activate(&workspace, &app, cx);
    // acme/api's check waits in the accounts' queues until after the answer.
    github.hold("/repos/acme/api");
    load(&[&app, &api], cx);
    assert_eq!(
        picker(&workspace, cx),
        Some(("acme".to_string(), logins(&["home", "work"])))
    );

    cx.simulate_keystrokes("j enter");
    cx.run_until_parked();
    assert_eq!(picker(&workspace, cx), None);
    assert_eq!(
        accounts.read_with(cx, |accounts, _| accounts
            .remembered_account("acme")
            .unwrap()),
        Some("work".to_string())
    );

    github.let_go("/repos/acme/api");
    cx.run_until_parked();
    assert_eq!(picker(&workspace, cx), None);
    for tab in [&app, &api] {
        assert_eq!(status(tab, cx), Status::Ready);
        assert_eq!(acting(tab, cx), Some(Acting::Account("work".into())));
    }
}

#[gpui::test]
async fn a_poller_started_while_lo_is_in_the_background_starts_slow(cx: &mut TestAppContext) {
    let (_workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    sign_in(&github, &[("home", "gho_HOME")], cx).await;
    let layer = cx.update(|_, cx| github_layer::ready(cx));
    let layer = layer.await.unwrap();

    layer.update(cx, |layer, cx| layer.set_foreground(false, cx));
    let poller = layer.update(cx, |layer, cx| layer.poller("home", cx));

    assert!(!poller.read_with(cx, |poller, _| poller.is_foreground()));
}

#[gpui::test]
async fn when_no_account_can_check_it_the_tab_says_why_with_githubs_link(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    // A public repository, which would open without signing in.
    public_pull(&github, &git, "acme/app", 7);
    github.answer("gho_WORK", "/repos/acme/app", needs_sso());
    sign_in(&github, &[("work", "gho_WORK")], cx).await;

    let tab = open_loaded(&workspace, "github.com/acme/app/pull/7", cx);

    let Status::Failed(message) = status(&tab, cx) else {
        panic!("the tab opened: {:?}", status(&tab, cx));
    };
    assert!(
        message.contains("work: Resource protected by organization SAML enforcement."),
        "{message}"
    );
    assert_eq!(
        help_links(&tab, cx),
        ["https://github.com/orgs/acme/sso?authorization_request=A1"]
    );
    let requests = github.requests();
    assert!(
        !requests
            .iter()
            .any(|request| request.ends_with("as nobody")),
        "{requests:#?}"
    );
    tab.read_with(cx, |tab, _| {
        assert_eq!(tab.acting(), None);
        assert_eq!(tab.title(), "Not opened");
    });
}

#[gpui::test]
async fn a_remembered_account_that_cant_check_is_never_swapped_for_another(
    cx: &mut TestAppContext,
) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    let pull = git.pull_request("acme/api", 12, &[("a.rs", "a\n")], &[("a.rs", "b\n")]);
    github.add_pull(pull);
    github.let_see("gho_WORK", "acme/api");
    github.let_see("gho_HOME", "acme/api");
    let (accounts, _) = sign_in(&github, &[("home", "gho_HOME"), ("work", "gho_WORK")], cx).await;
    accounts
        .update(cx, |accounts, cx| {
            accounts.remember_account_for_owner("acme", "work", cx)
        })
        .await
        .unwrap();
    github.answer("gho_WORK", "/repos/acme/api", needs_sso());

    let tab = open_loaded(&workspace, "github.com/acme/api/pull/12", cx);

    let Status::Failed(message) = status(&tab, cx) else {
        panic!("the tab opened: {:?}", status(&tab, cx));
    };
    assert!(
        message.starts_with("work, the account Lo uses for acme, couldn't check acme/api"),
        "{message}"
    );
    assert_eq!(
        help_links(&tab, cx),
        ["https://github.com/orgs/acme/sso?authorization_request=A1"]
    );
    let requests = github.requests();
    assert!(
        !requests.iter().any(|request| request.contains("/pulls/")),
        "{requests:#?}"
    );

    // Signed out, it isn't swapped either.
    accounts.update(cx, |accounts, cx| {
        accounts.mark_signed_out("work", github::accounts::SignedOut::Refused, cx)
    });
    let reopened = open_loaded(&workspace, "github.com/acme/api/pull/12", cx);
    assert_eq!(reopened, tab);
    let Status::Failed(message) = status(&tab, cx) else {
        panic!("the tab opened: {:?}", status(&tab, cx));
    };
    assert!(message.contains("work is signed out"), "{message}");
    assert_eq!(help_links(&tab, cx), Vec::<String>::new());
}

#[gpui::test]
async fn a_tab_opening_while_the_picker_asks_doesnt_take_its_keys(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    two_accounts_see_acme(&github, &git, cx).await;
    let app = open_loaded(&workspace, "github.com/acme/app/pull/1", cx);
    assert!(picker(&workspace, cx).is_some());

    // A link opens another tab, which becomes the active one.
    open_unloaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);

    let focused = workspace.read_with(cx, |workspace, cx| {
        let picker = workspace
            .active_modal::<account_picker::AccountPicker>(cx)
            .unwrap();
        picker.read(cx).focus_handle(cx)
    });
    assert!(cx.update(|window, cx| focused.contains_focused(window, cx)));
    cx.simulate_keystrokes("j enter");
    cx.run_until_parked();
    assert_eq!(acting(&app, cx), Some(Acting::Account("work".into())));
}

#[gpui::test]
async fn the_picker_set_aside_for_another_dialog_asks_again_once_it_closes(
    cx: &mut TestAppContext,
) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    two_accounts_see_acme(&github, &git, cx).await;
    let tab = open_loaded(&workspace, "github.com/acme/app/pull/1", cx);
    assert!(picker(&workspace, cx).is_some());

    // As Accounts… does before it opens its dialog.
    workspace.update_in(cx, |workspace, window, cx| {
        assert!(account_picker::set_aside(workspace, cx));
        crate::OpenBox::toggle(workspace, window, cx);
    });
    cx.run_until_parked();
    assert_eq!(picker(&workspace, cx), None);
    assert!(workspace.read_with(cx, |workspace, cx| {
        workspace.active_modal::<crate::OpenBox>(cx).is_some()
    }));
    assert_eq!(
        status(&tab, cx),
        Status::Loading("Waiting to ask which account Lo uses for acme…".into())
    );

    cx.simulate_keystrokes("escape");
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    assert_eq!(
        picker(&workspace, cx),
        Some(("acme".to_string(), logins(&["home", "work"])))
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(acting(&tab, cx), Some(Acting::Account("home".into())));
}

#[gpui::test]
async fn a_load_lo_starts_never_asks_which_account_and_escape_sticks(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    let pull = git.pull_request("acme/app", 1, &[("a.rs", "a\n")], &[("a.rs", "a2\n")]);
    github.add_pull(pull.clone());
    for token in ["gho_HOME", "gho_WORK"] {
        github.let_see(token, "acme/app");
    }
    // Copied once while only home was signed in, so it opened without asking.
    let (accounts, cache) = sign_in(&github, &[("home", "gho_HOME")], cx).await;
    let link = "github.com/acme/app/pull/1";
    let tab = open_loaded(&workspace, link, cx);
    assert_eq!(status(&tab, cx), Status::Ready);
    close(&workspace, tab, cx);
    let target = Target::Pull(PullRef::new("acme", "app", 1));
    let mut saved = details_based("Add a", "VIEWED", &pull.head, &pull.base_branch, &pull.base);
    saved.url = "https://github.com/acme/app/pull/1".into();
    saved.number = 1;
    cache
        .put("home", &target, &saved, None, two_hours_ago())
        .await
        .unwrap();
    sign_in_as(&accounts, "work", "gho_WORK", cx).await;
    github.set_offline(true);
    git.hide("acme/app");
    let tab = open_loaded(&workspace, link, cx);
    assert!(matches!(status(&tab, cx), Status::Offline(_)));

    // Back online, Lo loads it again by itself, but doesn't put the picker up.
    github.set_offline(false);
    git.restore("acme/app");
    cx.executor().advance_clock(Duration::from_secs(5));
    cx.run_until_parked();
    assert_eq!(picker(&workspace, cx), None);
    assert_eq!(
        status(&tab, cx),
        Status::Offline(
            "Choose which account Lo uses for acme. This is the local copy's diff, \
             read-only."
                .into()
        )
    );
    assert_eq!(action(&tab, cx), Some(TabAction::ChooseAccount));
    cx.executor().advance_clock(Duration::from_secs(10 * 60));
    cx.run_until_parked();
    assert_eq!(picker(&workspace, cx), None);

    // The button asks. Escape leaves the copy, and nothing asks again by itself.
    tab.update_in(cx, |tab, window, cx| tab.load_again(window, cx));
    cx.run_until_parked();
    assert_eq!(
        picker(&workspace, cx),
        Some(("acme".to_string(), logins(&["home", "work"])))
    );
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(
        status(&tab, cx),
        Status::Offline(
            "No account was chosen for acme. This is the local copy's diff, read-only.".into()
        )
    );
    cx.executor().advance_clock(Duration::from_secs(10 * 60));
    cx.run_until_parked();
    assert_eq!(picker(&workspace, cx), None);
    tab.read_with(cx, |tab, _| assert!(tab.diff().is_some()));
}

/// A dialog standing in for Accounts…, which lives in the app.
struct AccountsDialog {
    focus_handle: FocusHandle,
}

impl EventEmitter<gpui::DismissEvent> for AccountsDialog {}

impl workspace::ModalView for AccountsDialog {}

impl Focusable for AccountsDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for AccountsDialog {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().track_focus(&self.focus_handle).child("Accounts")
    }
}

#[gpui::test]
async fn an_account_signed_in_again_in_accounts_is_offered_again(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    let accounts = two_accounts_see_acme(&github, &git, cx).await;
    let tab = open_loaded(&workspace, "github.com/acme/app/pull/1", cx);
    accounts.update(cx, |accounts, cx| {
        accounts.mark_signed_out("work", github::accounts::SignedOut::Refused, cx)
    });
    cx.run_until_parked();
    assert_eq!(
        picker(&workspace, cx),
        Some(("acme".to_string(), logins(&["home"])))
    );

    // Accounts… sets the picker aside and opens its dialog, where work signs in again.
    workspace.update_in(cx, |workspace, window, cx| {
        assert!(account_picker::set_aside(workspace, cx));
        workspace.toggle_modal(window, cx, |_, cx| AccountsDialog {
            focus_handle: cx.focus_handle(),
        });
    });
    sign_in_as(&accounts, "work", "gho_WORK", cx).await;
    assert_eq!(picker(&workspace, cx), None);
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.hide_modal(window, cx);
    });
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();

    assert_eq!(
        picker(&workspace, cx),
        Some(("acme".to_string(), logins(&["home", "work"])))
    );
    cx.simulate_keystrokes("j enter");
    cx.run_until_parked();
    assert_eq!(acting(&tab, cx), Some(Acting::Account("work".into())));
}

#[gpui::test]
async fn a_tab_beside_an_empty_pane_is_asked_about(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    two_accounts_see_acme(&github, &git, cx).await;
    let app = open_unloaded(&workspace, "github.com/acme/app/pull/1", cx);
    workspace.update_in(cx, |workspace, window, cx| {
        let pane = workspace.active_pane().clone();
        workspace.split_pane(pane, workspace::SplitDirection::Right, window, cx);
    });
    cx.run_until_parked();

    load(&[&app], cx);

    assert_eq!(
        picker(&workspace, cx),
        Some(("acme".to_string(), logins(&["home", "work"])))
    );
}

#[gpui::test]
async fn a_rows_account_is_only_for_the_load_it_started(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let git = GitServer::start(cx);
    let accounts = two_accounts_see_acme(&github, &git, cx).await;
    accounts
        .update(cx, |accounts, cx| {
            accounts.remember_account_for_owner("acme", "work", cx)
        })
        .await
        .unwrap();
    // A row opened as home while offline.
    github.set_offline(true);
    let tab = workspace.update_in(cx, |workspace, window, cx| {
        open_as(
            workspace,
            link("github.com/acme/app/pull/1"),
            Trust::Untrusted,
            "home".into(),
            window,
            cx,
        )
    });
    cx.run_until_parked();
    assert!(matches!(status(&tab, cx), Status::Failed(_)));

    // Lo's own load once GitHub answers uses acme's account.
    github.set_offline(false);
    cx.executor().advance_clock(Duration::from_secs(5));
    cx.run_until_parked();
    assert_eq!(status(&tab, cx), Status::Ready);
    assert_eq!(acting(&tab, cx), Some(Acting::Account("work".into())));
}

#[gpui::test]
async fn a_tab_that_changes_account_stops_hearing_the_old_accounts_poller(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let tab = open_unloaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);
    let bot = sign_in_bot(&tab, "changes_account", &[], cx).await;
    let head: Oid = HEAD.parse().unwrap();
    tab.update(cx, |tab, cx| {
        tab.copied(
            local_entries(&[changed("a.rs", Change::Modified)]),
            &head,
            SystemTime::now(),
            cx,
        );
        tab.set_details(&details_with(&[("a.rs", "UNVIEWED")]), cx);
        tab.set_active(true, cx);
    });
    // Another account's saved marks aren't written into the bot's records.
    tab.update(cx, |tab, cx| {
        let saved = Saved {
            login: "other".into(),
            details: details_with(&[("a.rs", "VIEWED")]),
            fetched_at: SystemTime::now(),
        };
        tab.show_saved(&saved, cx);
    });
    cx.run_until_parked();
    assert_eq!(bot.recorded(), HashMap::new());
    tab.update(cx, |tab, cx| {
        tab.set_details(&details_with(&[("a.rs", "VIEWED")]), cx)
    });
    let (bots, pull) = (bot._poller.clone(), bot.pull.clone());
    let open = |cx: &mut VisualTestContext| bots.read_with(cx, |bots, _| bots.open().cloned());
    assert_eq!(open(cx), Some(pull.clone()));
    assert_eq!(marks(&tab, cx), [mark("a.rs", Some(ViewedState::Viewed))]);

    // The tab loads again as another account, which watches with its own poller.
    let other = cx.new(|cx| {
        Poller::new(
            bot.accounts.clone(),
            "other",
            bot.cache.clone(),
            github::poll::PollConfig::default(),
            cx,
        )
    });
    tab.update(cx, |tab, cx| {
        tab.set_acting(Acting::Account("other".into()), cx);
        tab.watch(other.clone(), pull.clone(), Some(bot.records.clone()), cx);
    });
    cx.run_until_parked();

    // The bot's poller stops watching it, and the bot's marks go.
    assert_eq!(open(cx), None);
    assert_eq!(marks(&tab, cx), [mark("a.rs", None)]);
    // What the bot's poller says doesn't reach the tab.
    bots.update(cx, |_, cx| {
        cx.emit(PollEvent::Details {
            pull: pull.clone(),
            details: std::sync::Arc::new(details_with(&[("a.rs", "VIEWED")])),
        })
    });
    cx.run_until_parked();
    assert_eq!(marks(&tab, cx), [mark("a.rs", None)]);
}
