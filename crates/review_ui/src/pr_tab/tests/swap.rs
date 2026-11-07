use super::*;

/// Shows a new pull request for `link` in `tab` and keeps the one it showed, as a stack's tab
/// does when it moves to another layer. Returns the kept one's ID.
fn swap_to(tab: &Entity<PrTab>, link: &str, cx: &mut VisualTestContext) -> PullId {
    let link = self::link(link);
    let kept = tab.update_in(cx, |tab, window, cx| {
        let other = ShownPull::new(link, cx);
        let shown = tab.show_pull(other, window, cx);
        let id = shown.id();
        tab.keep_pull(shown);
        id
    });
    cx.run_until_parked();
    kept
}

/// Reads the pull request `id` that `tab` keeps.
fn kept<R>(
    tab: &Entity<PrTab>,
    id: PullId,
    cx: &mut VisualTestContext,
    read: impl FnOnce(&ShownPull) -> R,
) -> R {
    tab.read_with(cx, |tab, _| {
        let pull = tab.kept_pulls().iter().find(|pull| pull.id() == id);
        read(pull.expect("the tab keeps it"))
    })
}

/// A pull request's load and GitHub's diff of it, still on their way when the tab shows another
/// pull request, land on the one they're for, which shows them once it shows again. The pull
/// request shown meanwhile gets none of it.
#[gpui::test]
async fn a_load_in_flight_lands_on_its_own_pull_request_after_a_swap(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let _git = GitServer::start(cx);
    super::github_diff::uncopyable_pull(&github, "acme/app", 1, 2);
    anonymous(&github, cx);
    github.hold("/repos/acme/app/pulls/1");
    github.hold("/repos/acme/app/pulls/1/files");

    let tab = open_loaded(&workspace, "github.com/acme/app/pull/1", cx);
    let first = swap_to(&tab, "github.com/acme/app/pull/2", cx);
    let untouched = |tab: &Entity<PrTab>, cx: &mut VisualTestContext| {
        tab.read_with(cx, |tab, _| {
            assert_eq!(tab.pull().number(), 2);
            assert_eq!(tab.header().title, None);
            assert_eq!(tab.files().len(), 0);
            assert!(tab.diff().is_none());
            assert_eq!(tab.pull().latest_head(), None);
            assert_eq!(
                tab.status(),
                &Status::Loading("Finding an account that can see it…".into())
            );
        });
        assert_eq!(banners(tab, cx), Vec::<String>::new());
    };

    // GitHub answers the first pull request's load, which goes on to read GitHub's diff, and
    // fails to copy it locally.
    github.let_go("/repos/acme/app/pulls/1");
    cx.run_until_parked();
    untouched(&tab, cx);
    kept(&tab, first, cx, |pull| {
        assert_eq!(pull.header().title.as_deref(), Some("Pull request 1"));
        assert_eq!(pull.latest_head(), Some("d".repeat(40).as_str()));
        assert!(pull.diff().is_none());
    });

    // GitHub's diff of it arrives.
    github.let_go("/repos/acme/app/pulls/1/files");
    cx.run_until_parked();
    untouched(&tab, cx);
    kept(&tab, first, cx, |pull| {
        assert_eq!(pull.files().len(), 2);
        assert!(pull.diff().is_some());
    });

    // Shown again, it shows all of it.
    assert!(tab.update_in(cx, |tab, window, cx| tab.show_kept_pull(first, window, cx)));
    cx.run_until_parked();
    tab.read_with(cx, |tab, _| {
        assert_eq!(tab.pull().number(), 1);
        assert_eq!(tab.diff_source(), Some(DiffSource::GitHub));
        assert_eq!(tab.files().len(), 2);
    });
    assert!(
        banners(&tab, cx)
            .iter()
            .any(|banner| banner.starts_with("Lo couldn't show the local copy's diff")),
        "{:#?}",
        banners(&tab, cx)
    );
}

/// A viewed mark written while the tab moves to another pull request lands on the pull request it
/// was marked in, even where the other has a file at the same path. `e` and `]f` then act on the
/// pull request shown, its diff and its marks.
#[gpui::test]
async fn a_viewed_write_in_flight_lands_on_its_own_pull_request_after_a_swap(
    cx: &mut TestAppContext,
) {
    let files = [
        ("a.rs", lines(5, &[]), lines(5, &[3])),
        ("b.rs", lines(5, &[]), lines(5, &[3])),
    ];
    let unviewed = [("a.rs", "UNVIEWED"), ("b.rs", "UNVIEWED")];
    let (workspace, tab, github, _, cx) =
        signed_in_diff("viewed_write_across_a_swap", &[], &files, &unviewed, cx).await;
    github.github.hold("graphql:MarkFileAsViewed");
    github.github.answer_viewed(WriteAnswer::Done);
    cx.simulate_keystrokes("e");
    cx.run_until_parked();
    assert_eq!(marks(&tab, cx)[0], mark("a.rs", Some(ViewedState::Viewed)));

    // Another pull request shows, with files at the same paths, watched by the same account.
    let first = swap_to(&tab, "github.com/rozed-dev/playground/pull/3", cx);
    let other = PullRef::new("rozed-dev", "playground", 3);
    let diff = show_test_diff(&tab, &workspace, &files, cx).await;
    tab.update(cx, |tab, cx| {
        tab.watch(
            github._poller.clone(),
            other.clone(),
            Some(github.records.clone()),
            cx,
        );
        tab.set_details(&details_with(&unviewed), cx);
    });
    cx.run_until_parked();

    // GitHub takes the first pull request's mark.
    github.github.let_go("graphql:MarkFileAsViewed");
    cx.run_until_parked();
    assert_eq!(
        marks(&tab, cx),
        [
            mark("a.rs", Some(ViewedState::Unviewed)),
            mark("b.rs", Some(ViewedState::Unviewed)),
        ]
    );
    assert_eq!(
        tab.read_with(cx, |tab, _| tab
            .pull()
            .viewed_at("a.rs")
            .map(str::to_string)),
        None
    );
    kept(&tab, first, cx, |pull| {
        assert_eq!(pull.viewed_at("a.rs"), Some(HEAD));
        let viewed: Vec<_> = (pull.files().rows())
            .map(|entry| (entry.path().to_string(), entry.viewed))
            .collect();
        assert_eq!(viewed[0], mark("a.rs", Some(ViewedState::Viewed)));
    });
    assert_eq!(
        github.recorded().get("a.rs").map(String::as_str),
        Some(HEAD)
    );
    assert!(
        github.records.heads(BOT, &other).unwrap().is_empty(),
        "the other pull request's records stay empty"
    );

    // The keys act on the pull request shown: `]f` moves its diff, and `e` marks its file.
    cx.update(|window, cx| window.focus(&diff.focus_handle(cx), cx));
    cx.simulate_keystrokes("] f");
    assert_eq!(cursor_file(&tab, cx).as_deref(), Some("b.rs"));
    github.github.answer_viewed(WriteAnswer::Done);
    cx.simulate_keystrokes("e");
    // After the poller's spacing between writes.
    cx.executor().advance_clock(Duration::from_secs(2));
    cx.run_until_parked();
    let mutations = github.github.viewed_mutations();
    assert_eq!(mutations.len(), 2);
    assert_eq!(mutations[1].1["path"], "b.rs");
    assert_eq!(
        marks(&tab, cx),
        [
            mark("a.rs", Some(ViewedState::Unviewed)),
            mark("b.rs", Some(ViewedState::Viewed)),
        ]
    );
    kept(&tab, first, cx, |pull| {
        let viewed: Vec<_> = (pull.files().rows())
            .map(|entry| (entry.path().to_string(), entry.viewed))
            .collect();
        assert_eq!(viewed[1], mark("b.rs", Some(ViewedState::Unviewed)));
    });
}

/// Closing the tab lets go of every pull request it holds: the poller stops watching the one it
/// shows and the ones it keeps.
#[gpui::test]
async fn closing_the_tab_lets_go_of_every_pull_request_it_holds(cx: &mut TestAppContext) {
    let files = [("a.rs", lines(5, &[]), lines(5, &[3]))];
    let (workspace, tab, github, _, cx) =
        signed_in_diff("closing_releases_every_pull", &[], &files, &[], cx).await;
    swap_to(&tab, "github.com/rozed-dev/playground/pull/3", cx);
    let other = PullRef::new("rozed-dev", "playground", 3);
    tab.update(cx, |tab, cx| {
        tab.watch(github._poller.clone(), other.clone(), None, cx)
    });
    let watched = |cx: &mut VisualTestContext| {
        github._poller.read_with(cx, |poller, _| {
            (poller.watches(&github.pull), poller.watches(&other))
        })
    };
    assert_eq!(watched(cx), (true, true));

    close(&workspace, tab, cx);
    assert_eq!(watched(cx), (false, false));
}
