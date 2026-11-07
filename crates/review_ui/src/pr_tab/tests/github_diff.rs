use super::*;

/// A public pull request of `count` files on the fake GitHub, which its git server doesn't
/// have, so its local copy fails and GitHub's diff stays.
fn uncopyable_pull(github: &FakeGithub, repo: &str, number: u64, count: usize) {
    let head = "d".repeat(40);
    let files = (0..count)
        .map(|index| {
            let path = format!("src/f{index:04}.rs");
            serde_json::json!({
                "filename": path,
                "status": "modified",
                "additions": 1,
                "deletions": 1,
                "changes": 2,
                "patch": "@@ -1 +1 @@\n-a\n+b",
                "blob_url": format!("https://github.com/{repo}/blob/{head}/{path}"),
                "contents_url": format!(
                    "https://api.github.com/repos/{repo}/contents/{path}?ref={head}"
                ),
            })
        })
        .collect();
    github.add_pull(FakePull {
        repo: repo.into(),
        old_names: Vec::new(),
        number,
        title: format!("Pull request {number}"),
        head,
        base_branch: "main".into(),
        base: "b".repeat(40),
        files,
        changed_files: None,
    });
    github.make_public(repo);
}

#[gpui::test]
async fn githubs_diff_is_read_again_after_a_failure_that_may_pass(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let _git = GitServer::start(cx);
    uncopyable_pull(&github, "acme/app", 1, 2);
    anonymous(&github, cx);
    let files = "/repos/acme/app/pulls/1/files?per_page=100";
    github.answer("", files, Answer::new(502, r#"{"message":"Server Error"}"#));

    let tab = open_loaded(&workspace, "github.com/acme/app/pull/1", cx);

    // Neither diff shows yet, and GitHub's is tried again.
    assert_eq!(tab.read_with(cx, |tab, _| tab.diff_source()), None);
    assert!(matches!(status(&tab, cx), Status::Loading(_)));
    let shown = banners(&tab, cx);
    assert!(
        shown.iter().any(|banner| banner.starts_with(
            "Lo couldn't read GitHub's diff, and tries again until the local copy's diff \
             is ready: GitHub answered 502"
        )),
        "{shown:#?}"
    );
    github.forget_answer("", files);
    cx.executor().advance_clock(Duration::from_secs(5));
    cx.run_until_parked();

    tab.read_with(cx, |tab, _| {
        assert_eq!(tab.diff_source(), Some(DiffSource::GitHub));
        assert_eq!(tab.status(), &Status::Ready);
    });
    let shown = banners(&tab, cx);
    assert!(
        !shown
            .iter()
            .any(|banner| banner.contains("GitHub's diff, and tries again")),
        "{shown:#?}"
    );
}

#[test]
fn githubs_diff_is_read_again_only_after_a_failure_that_may_pass() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
    let wait = Duration::from_secs(10);
    let api = |status| GithubError::Api {
        status,
        message: String::new(),
        documentation_url: None,
        errors: Vec::new(),
    };
    let network = GithubError::Network(anyhow::anyhow!("offline"));
    assert_eq!(github_retry_wait(&network, wait, now), Some(wait));
    assert_eq!(github_retry_wait(&api(502), wait, now), Some(wait));
    assert_eq!(github_retry_wait(&api(422), wait, now), None);
    let not_found = GithubError::NotFound {
        message: "Not Found".into(),
        documentation_url: None,
    };
    assert_eq!(github_retry_wait(&not_found, wait, now), None);
    // A used-up rate limit waits for its reset, or for as long as GitHub asked.
    let limited = |reset, retry_after| GithubError::RateLimited {
        message: String::new(),
        secondary: false,
        reset,
        retry_after,
        resource: Some("core".into()),
    };
    assert_eq!(
        github_retry_wait(&limited(Some(1_600), None), wait, now),
        Some(Duration::from_secs(600))
    );
    assert_eq!(
        github_retry_wait(&limited(None, Some(Duration::from_secs(60))), wait, now),
        Some(Duration::from_secs(60))
    );
    assert_eq!(
        github_retry_wait(&limited(None, None), wait, now),
        Some(wait)
    );
}

#[gpui::test]
async fn when_neither_diff_can_show_the_tab_says_why_the_copy_failed(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let _git = GitServer::start(cx);
    uncopyable_pull(&github, "acme/app", 1, 2);
    anonymous(&github, cx);
    let files = "/repos/acme/app/pulls/1/files?per_page=100";
    github.answer("", files, Answer::new(404, r#"{"message":"Not Found"}"#));

    let tab = open_loaded(&workspace, "github.com/acme/app/pull/1", cx);

    let Status::Failed(message) = status(&tab, cx) else {
        panic!("the tab opened: {:?}", status(&tab, cx));
    };
    assert!(
        message.contains("the repository doesn't exist"),
        "{message}"
    );
}

#[gpui::test]
async fn without_signing_in_githubs_diff_reads_at_most_three_pages(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let _git = GitServer::start(cx);
    uncopyable_pull(&github, "acme/big", 1, 450);
    anonymous(&github, cx);

    let tab = open_loaded(&workspace, "github.com/acme/big/pull/1", cx);

    let requests = github.requests();
    let pages: Vec<_> = requests
        .iter()
        .filter(|request| request.contains("/files"))
        .collect();
    assert_eq!(pages.len(), 3, "{requests:#?}");
    assert_eq!(tab.read_with(cx, |tab, _| tab.files().len()), 300);
    let banners = banners(&tab, cx);
    assert!(
        banners.contains(
            &"Without signing in, GitHub's diff shows the first 300 of this pull request's \
              450 files, to save GitHub's 60 requests an hour. The rest show once the local \
              copy is ready."
                .to_string()
        ),
        "{banners:#?}"
    );
}

#[gpui::test]
async fn without_signing_in_githubs_diff_isnt_read_while_few_requests_are_left(
    cx: &mut TestAppContext,
) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let _git = GitServer::start(cx);
    uncopyable_pull(&github, "acme/one", 1, 2);
    uncopyable_pull(&github, "acme/two", 2, 2);
    anonymous(&github, cx);
    github.limit_anonymous(13);

    // The first reads its one page, and the app's one client without an account keeps what
    // GitHub said about the limit.
    let one = open_loaded(&workspace, "github.com/acme/one/pull/1", cx);
    assert_eq!(
        one.read_with(cx, |tab, _| tab.diff_source()),
        Some(DiffSource::GitHub)
    );
    let left = cx.update(|_, cx| {
        let client = github_api::anonymous_client(cx);
        client.rate_limits().core().map(|core| core.remaining)
    });
    assert_eq!(left, Some(11));

    // With 10 left after the second's own request, its file list isn't read.
    github.requests();
    let two = open_loaded(&workspace, "github.com/acme/two/pull/2", cx);
    assert_eq!(github.requests(), ["GET /repos/acme/two/pulls/2 as nobody"]);
    assert_eq!(two.read_with(cx, |tab, _| tab.diff_source()), None);
    let banners = banners(&two, cx);
    assert!(
        banners.iter().any(|banner| banner
            .starts_with("Lo didn't read GitHub's diff, to keep the few requests GitHub allows")),
        "{banners:#?}"
    );
}

#[gpui::test]
async fn githubs_diff_says_when_github_lists_only_some_files(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let _git = GitServer::start(cx);
    uncopyable_pull(&github, "acme/huge", 1, 3);
    github.let_see("gho_BOT", "acme/huge");
    sign_in(&github, &[("rozed-bot", "gho_BOT")], cx).await;
    // GitHub lists no more than 3,000 of a pull request's files; this one has 3,100.
    github.set_changed_files("acme/huge", 1, 3_100);

    let tab = open_loaded(&workspace, "github.com/acme/huge/pull/1", cx);

    assert_eq!(
        tab.read_with(cx, |tab, _| tab.diff_source()),
        Some(DiffSource::GitHub)
    );
    let banners = banners(&tab, cx);
    assert!(
        banners.contains(
            &"GitHub lists only 3 of this pull request's 3,100 files, so GitHub's diff shows \
              those. The rest show once the local copy is ready."
                .to_string()
        ),
        "{banners:#?}"
    );
}

/// `count` lines, "`name` 1" to "`name` `count`", with line `changed` as "`name` `changed`
/// changed".
fn numbered(name: &str, count: u32, changed: Option<u32>) -> String {
    (1..=count)
        .map(|line| match changed {
            Some(changed) if changed == line => format!("{name} {line} changed\n"),
            _ => format!("{name} {line}\n"),
        })
        .collect()
}

#[gpui::test]
async fn githubs_diff_read_after_a_push_says_which_head_it_shows(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let _git = GitServer::start(cx);
    // The files are listed at dddddd…, but GitHub said the head was cccccc… a moment before.
    uncopyable_pull(&github, "acme/app", 1, 2);
    github.set_head("acme/app", 1, &"c".repeat(40));
    anonymous(&github, cx);

    let tab = open_loaded(&workspace, "github.com/acme/app/pull/1", cx);

    assert_eq!(
        tab.read_with(cx, |tab, _| tab.diff_source()),
        Some(DiffSource::GitHub)
    );
    let banners = banners(&tab, cx);
    assert!(
        banners.contains(
            &"This pull request moved on while Lo read it. GitHub's diff shows ddddddd, \
              and the local copy's diff shows ccccccc once it's ready."
                .to_string()
        ),
        "{banners:#?}"
    );
}

#[gpui::test]
async fn the_switch_says_when_the_local_copy_shows_another_head(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let tab = open_unloaded(&workspace, PULL, cx);
    let github_head = "f".repeat(40);
    show_github_files(&tab, vec![github_file_at("a.txt", 50, &github_head)], cx);
    assert_eq!(banners(&tab, cx), Vec::<String>::new());

    let _local_copy = show_local_copy(&tab, cx);

    let local_head = tab.read_with(cx, |tab, _| {
        assert_eq!(tab.diff_source(), Some(DiffSource::LocalCopy));
        tab.revisions().unwrap().latest().unwrap().head.clone()
    });
    assert_eq!(
        banners(&tab, cx),
        [format!(
            "This pull request moved on while it loaded. This diff shows {}; GitHub's diff \
             showed fffffff.",
            &local_head[..7]
        )]
    );
}

/// Shows the local copy's diff of a pull request that changes line 50 of a.txt and line 60 of
/// b.txt, each 100 lines long. The local copy lasts as long as what's returned.
fn show_local_copy(
    tab: &Entity<PrTab>,
    cx: &mut VisualTestContext,
) -> (pr_diff::tests::fixtures::Fixture, tempfile::TempDir) {
    let (a, b) = (numbered("a", 100, None), numbered("b", 100, None));
    let (new_a, new_b) = (numbered("a", 100, Some(50)), numbered("b", 100, Some(60)));
    let base = [
        Entry::File("a.txt", a.as_bytes()),
        Entry::File("b.txt", b.as_bytes()),
    ];
    let head = [
        Entry::File("a.txt", new_a.as_bytes()),
        Entry::File("b.txt", new_b.as_bytes()),
    ];
    let pr_diff::tests::Copied {
        _fixture: fixture,
        _cache: cache,
        local,
        comparison,
        ..
    } = pr_diff::tests::copy(&base, &head, false);
    let (local, comparison) = (Arc::new(local), Arc::new(comparison));
    tab.update_in(cx, |tab, window, cx| {
        let (workspace, link) = (tab.workspace().clone(), tab.link().clone());
        cx.spawn_in(window, async move |tab, cx| {
            let title = format!("{link}: Change a and b");
            pr_diff::show_local_copy(
                tab,
                workspace,
                link,
                title,
                local,
                comparison,
                SystemTime::now(),
                Trust::Untrusted,
                Instant::now(),
                cx,
            )
            .await
            .unwrap()
        })
        .detach();
    });
    cx.run_until_parked();
    (fixture, cache)
}

fn shown_text(diff: &Entity<SplittableEditor>, cx: &mut VisualTestContext) -> String {
    diff.read_with(cx, |diff, cx| diff.rhs_editor().read(cx).text(cx))
}

fn scrolled_to(
    diff: &Entity<SplittableEditor>,
    cx: &mut VisualTestContext,
) -> Option<DiffPosition> {
    cx.update(|_, cx| pr_diff::scroll_position(diff, cx))
}

#[gpui::test]
async fn githubs_diff_shows_first_then_the_local_copys_at_the_same_line(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let tab = open_unloaded(&workspace, PULL, cx);

    show_github_files(
        &tab,
        vec![github_file("a.txt", 50), github_file("b.txt", 60)],
        cx,
    );

    let github = tab.read_with(cx, |tab, _| {
        assert_eq!(tab.diff_source(), Some(DiffSource::GitHub));
        tab.diff().unwrap().clone()
    });
    let text = shown_text(&github, cx);
    // Only the hunks show: line 47 to line 53 of a.txt, and 57 to 63 of b.txt.
    assert!(
        text.contains("a 50 changed") && text.contains("b 60 changed"),
        "{text}"
    );
    assert!(text.contains("a 47\n") && text.contains("b 63"), "{text}");
    assert!(!text.contains("a 46\n") && !text.contains("b 64"), "{text}");
    // The rows between hunks never show.
    assert!(!text.contains('\u{E000}'), "{text:?}");
    let paths: Vec<_> = tab.read_with(cx, |tab, _| {
        tab.files()
            .rows()
            .map(|entry| entry.path().to_string())
            .collect()
    });
    assert_eq!(paths, ["a.txt", "b.txt"]);

    // Reading b.txt's change, line 60, at the top of the view.
    let position = DiffPosition {
        path: "b.txt".into(),
        row: 59,
        below_top: 0.,
    };
    cx.update(|window, cx| assert!(pr_diff::scroll_to(&github, &position, window, cx)));
    assert_eq!(scrolled_to(&github, cx), Some(position.clone()));

    let _local_copy = show_local_copy(&tab, cx);

    let local = tab.read_with(cx, |tab, _| {
        assert_eq!(tab.diff_source(), Some(DiffSource::LocalCopy));
        assert_eq!(tab.status(), &Status::Ready);
        tab.diff().unwrap().clone()
    });
    assert_ne!(local, github);
    let scrolled = scrolled_to(&local, cx).unwrap();
    assert_eq!((scrolled.path.as_str(), scrolled.row), ("b.txt", 59));
    assert_eq!(scrolled.below_top, 0.);
}

/// Shows every diff made from now on in one column, deleted lines above added ones.
fn unified(cx: &mut VisualTestContext) {
    cx.update(|_, cx| {
        cx.update_global::<settings::SettingsStore, _>(|store, cx| {
            store.update_user_settings(cx, |settings| {
                settings.editor.diff_view_style = Some(settings::DiffViewStyle::Unified)
            })
        })
    });
}

/// Scrolls `diff` so the display row `above` rows above `position`'s line is at the top.
fn scroll_above(
    diff: &Entity<SplittableEditor>,
    position: &DiffPosition,
    above: f64,
    cx: &mut VisualTestContext,
) {
    cx.update(|window, cx| assert!(pr_diff::scroll_to(diff, position, window, cx)));
    let editor = diff.read_with(cx, |diff, _| diff.rhs_editor().clone());
    editor.update_in(cx, |editor, window, cx| {
        let top = editor.scroll_position(cx).y;
        editor.set_scroll_position(gpui::point(0., top - above), window, cx);
    });
}

#[gpui::test]
async fn the_switch_keeps_a_deleted_line_at_the_top_in_one_column(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    unified(cx);
    let tab = open_unloaded(&workspace, PULL, cx);
    show_github_files(
        &tab,
        vec![github_file("a.txt", 50), github_file("b.txt", 60)],
        cx,
    );
    let github = tab.read_with(cx, |tab, _| tab.diff().unwrap().clone());
    assert!(!github.read_with(cx, |diff, _| diff.is_split()));

    // b.txt's deleted line 60, which only the base has, is at the top, above the line that
    // replaced it.
    let changed = DiffPosition {
        path: "b.txt".into(),
        row: 59,
        below_top: 0.,
    };
    scroll_above(&github, &changed, 1., cx);
    let at_top = DiffPosition {
        below_top: 1.,
        ..changed
    };
    assert_eq!(scrolled_to(&github, cx), Some(at_top.clone()));

    let _local_copy = show_local_copy(&tab, cx);

    let local = tab.read_with(cx, |tab, _| tab.diff().unwrap().clone());
    assert!(!local.read_with(cx, |diff, _| diff.is_split()));
    assert_eq!(scrolled_to(&local, cx), Some(at_top));
}

#[gpui::test]
async fn a_cursor_never_moved_starts_the_local_copys_diff_in_its_first_file(
    cx: &mut TestAppContext,
) {
    let (workspace, cx) = workspace(cx).await;
    let tab = open_unloaded(&workspace, PULL, cx);
    show_github_files(
        &tab,
        vec![github_file("a.txt", 50), github_file("b.txt", 60)],
        cx,
    );

    let _local_copy = show_local_copy(&tab, cx);

    let local = tab.read_with(cx, |tab, _| tab.diff().unwrap().clone());
    // At the diff's first row, not at its end in b.txt, where the local copy's editor's
    // cursor starts.
    let (in_file, row, _) = cursor(&tab, &local, cx);
    assert_eq!((in_file.as_deref(), row), (Some("a.txt"), 0));
}

#[gpui::test]
async fn the_switch_keeps_the_cursor_where_it_was(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let tab = open_unloaded(&workspace, PULL, cx);
    show_github_files(
        &tab,
        vec![github_file("a.txt", 50), github_file("b.txt", 60)],
        cx,
    );
    let github = tab.read_with(cx, |tab, _| tab.diff().unwrap().clone());
    // The cursor is on b.txt's line 61, two below the top of the view.
    let top = DiffPosition {
        path: "b.txt".into(),
        row: 58,
        below_top: 0.,
    };
    cx.update(|window, cx| assert!(pr_diff::scroll_to(&github, &top, window, cx)));
    let cursor = DiffCursor {
        path: "b.txt".into(),
        point: language::Point::new(60, 3),
    };
    cx.update(|window, cx| assert!(pr_diff::put_cursor(&github, &cursor, window, cx)));
    assert_eq!(
        cx.update(|_, cx| pr_diff::cursor_position(&github, cx)),
        Some(cursor.clone())
    );

    let _local_copy = show_local_copy(&tab, cx);

    let local = tab.read_with(cx, |tab, _| tab.diff().unwrap().clone());
    assert_eq!(
        cx.update(|_, cx| pr_diff::cursor_position(&local, cx)),
        Some(cursor)
    );
    // Moving the cursor moves it from there, and the view stays put.
    cx.simulate_keystrokes("down");
    let moved = cx
        .update(|_, cx| pr_diff::cursor_position(&local, cx))
        .unwrap();
    assert_eq!((moved.path.as_str(), moved.point.row), ("b.txt", 61));
    let scrolled = scrolled_to(&local, cx).unwrap();
    assert_eq!((scrolled.path.as_str(), scrolled.row), ("b.txt", 58));
}

/// A copy whose fetch failed with `failure`, as git reports it.
fn fetch_failed(failure: repo_cache::FetchFailure) -> anyhow::Error {
    anyhow::Error::new(repo_cache::Error::Fetch {
        what: "pull request #1".into(),
        failure,
        message: "fatal: unable to access 'https://github.com/owner/repo.git/'".into(),
    })
}

/// Copies `tab`'s pull request locally as the tab does, trying again after failures, with
/// each try answered by the next of `answers`: a failure, or `None` for a copy that works.
/// Returns the task, which ends when the copy works or fails for good, and the tries so far.
fn copy_with(
    tab: &Entity<PrTab>,
    answers: Vec<Option<repo_cache::FetchFailure>>,
    cx: &mut VisualTestContext,
) -> (
    Task<anyhow::Result<()>>,
    std::rc::Rc<std::cell::Cell<usize>>,
    pr_diff::tests::Copied,
) {
    let copied = pr_diff::tests::copy(&[Entry::File("a.txt", b"a\n")], &[], false);
    let local = std::cell::Cell::new(Some((copied.local.clone(), copied.comparison.clone())));
    let tries = std::rc::Rc::new(std::cell::Cell::new(0));
    let mut answers = answers.into_iter();
    let copy_once = {
        let tries = tries.clone();
        move |_progress, _cx: &mut AsyncWindowContext| {
            tries.set(tries.get() + 1);
            Task::ready(match answers.next().flatten() {
                Some(failure) => Err(fetch_failed(failure)),
                None => Ok(local.take().expect("a copy that works ends the tries")),
            })
        }
    };
    // Nothing has focus, so no cursor blinks while the clock runs on.
    cx.update(|window, cx| window.blur(cx));
    let task = tab.update_in(cx, |tab, window, cx| {
        let link = tab.link().clone();
        cx.spawn_in(window, async move |tab, cx| {
            pr_diff::copy_with_retries(&tab, &link, copy_once, cx)
                .await
                .map(|_| ())
        })
    });
    cx.run_until_parked();
    (task, tries, copied)
}

fn wait(seconds: u64, cx: &mut VisualTestContext) {
    cx.executor().advance_clock(Duration::from_secs(seconds));
    cx.run_until_parked();
}

#[gpui::test]
async fn a_copy_that_fails_on_the_network_is_tried_again_ever_later(cx: &mut TestAppContext) {
    use repo_cache::FetchFailure::{Network, RateLimited};
    let (workspace, cx) = workspace(cx).await;
    let tab = open_unloaded(&workspace, PULL, cx);
    show_github_files(&tab, vec![github_file("a.txt", 50)], cx);
    // As many failures as stop a copy that fails for an unknown reason, then a copy that
    // works.
    let mut answers = vec![Some(Network), Some(RateLimited)];
    answers.extend([Some(Network); 3]);
    answers.push(None);

    let (task, tries, _copied) = copy_with(&tab, answers, cx);

    let why = "can't fetch pull request #1: the network failed (fatal: unable to access \
               'https://github.com/owner/repo.git/')";
    assert_eq!(
        banners(&tab, cx),
        [format!(
            "Lo couldn't copy this pull request locally: {why}. Trying again in 5 s. \
             Until then, this is GitHub's diff: changed lines only."
        )]
    );
    // It counts down a second at a time.
    wait(1, cx);
    assert!(banners(&tab, cx)[0].contains("Trying again in 4 s."));
    wait(4, cx);
    assert_eq!(tries.get(), 2);
    // Each wait doubles ([`pr_diff::next_wait`] caps it at 5 minutes).
    for (try_, seconds) in [10, 20, 40, 80].into_iter().enumerate() {
        let banner = &banners(&tab, cx)[0];
        assert!(
            banner.contains(&format!("Trying again in {seconds} s.")),
            "{banner}"
        );
        wait(seconds, cx);
        assert_eq!(tries.get(), try_ + 3);
    }

    // The sixth try works, and the banner goes.
    assert!(banners(&tab, cx).is_empty());
    assert!(task.now_or_never().unwrap().is_ok());
}

#[gpui::test]
async fn a_copy_that_keeps_failing_for_an_unknown_reason_stops_with_a_retry_button(
    cx: &mut TestAppContext,
) {
    use repo_cache::FetchFailure::Other;
    let (workspace, cx) = workspace(cx).await;
    let tab = open_unloaded(&workspace, PULL, cx);
    show_github_files(&tab, vec![github_file("a.txt", 50)], cx);

    let (task, tries, _copied) = copy_with(&tab, vec![Some(Other); 5], cx);
    for seconds in [5, 10, 20, 40] {
        wait(seconds, cx);
    }

    assert_eq!(tries.get(), 5);
    let shown = tab.read_with(cx, |tab, cx| tab.banners().read(cx).messages().to_vec());
    assert_eq!(
        shown[0].text.as_ref(),
        "Lo stopped trying to copy this pull request locally after 5 tries: can't fetch \
         pull request #1: fatal: unable to access 'https://github.com/owner/repo.git/'. \
         Until then, this is GitHub's diff: changed lines only."
    );
    assert_eq!(
        tab.read_with(cx, |tab, _| tab.status().clone()),
        Status::Ready
    );
    // It waits for the button, however long.
    wait(60, cx);
    assert_eq!(tries.get(), 5);

    let retry = shown[0].action.clone().unwrap();
    assert_eq!(retry.label.as_ref(), "Retry");
    cx.update(|window, cx| (retry.on_click)(window, cx));
    cx.run_until_parked();

    assert_eq!(tries.get(), 6);
    assert!(banners(&tab, cx).is_empty());
    assert!(task.now_or_never().unwrap().is_ok());
}

#[gpui::test]
async fn opening_a_stopped_copy_again_starts_over_without_its_retry_button(
    cx: &mut TestAppContext,
) {
    let (workspace, cx) = workspace(cx).await;
    let tab = open_unloaded(&workspace, PULL, cx);
    let (retry, retried) = oneshot::channel();
    tab.update(cx, |tab, cx| tab.copy_stopped("git said no", 5, retry, cx));
    assert!(matches!(status(&tab, cx), Status::Failed(_)));
    assert_eq!(banners(&tab, cx).len(), 1);

    open_unloaded(&workspace, PULL, cx);

    assert!(matches!(status(&tab, cx), Status::Loading(_)));
    assert!(banners(&tab, cx).is_empty());
    // The old copy's button is gone with it.
    assert!(retried.now_or_never().unwrap().is_err());
}

#[gpui::test]
async fn a_copy_that_fails_for_good_isnt_tried_again(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let tab = open_unloaded(&workspace, PULL, cx);

    let (task, tries, _copied) = copy_with(&tab, vec![Some(repo_cache::FetchFailure::Auth)], cx);

    assert_eq!(tries.get(), 1);
    let error = task.now_or_never().unwrap().unwrap_err();
    assert!(
        format!("{error:#}").contains("sign in with one"),
        "{error:#}"
    );
    assert!(banners(&tab, cx).is_empty());
}

/// Whether each side of `diff` hides the buttons that expand excerpts: the right side, and
/// the left one if the diff is split.
fn expand_buttons_hidden(
    diff: &Entity<SplittableEditor>,
    cx: &mut VisualTestContext,
) -> (bool, Option<bool>) {
    diff.read_with(cx, |diff, cx| {
        let hidden = |editor: &Entity<Editor>| editor.read(cx).expand_excerpt_buttons_disabled();
        (hidden(diff.rhs_editor()), diff.lhs_editor().map(hidden))
    })
}

#[gpui::test]
async fn githubs_diff_cant_be_expanded_even_once_it_splits_again(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let tab = open_unloaded(&workspace, PULL, cx);
    show_github_files(&tab, vec![github_file("a.txt", 50)], cx);
    let diff = tab.read_with(cx, |tab, _| tab.diff().unwrap().clone());
    let before = shown_text(&diff, cx);
    assert_eq!(expand_buttons_hidden(&diff, cx), (true, Some(true)));

    // The left side is made anew when the diff splits again, as when it widens past
    // `minimum_split_diff_width` or the diff view style changes.
    for _ in 0..2 {
        diff.update_in(cx, |diff, window, cx| {
            diff.toggle_split(&editor::ToggleSplitDiff, window, cx)
        });
        cx.run_until_parked();
    }
    assert!(diff.read_with(cx, |diff, _| diff.is_split()));
    assert_eq!(expand_buttons_hidden(&diff, cx), (true, Some(true)));

    // Nor do the keys expand it, on either side.
    for side in [true, false] {
        let editor = diff.read_with(cx, |diff, _| {
            if side {
                diff.rhs_editor().clone()
            } else {
                diff.lhs_editor().unwrap().clone()
            }
        });
        cx.update(|window, cx| window.focus(&editor.focus_handle(cx), cx));
        cx.simulate_keystrokes("shift-enter");
        cx.dispatch_action(editor::actions::ExpandExcerptsUp::default());
        cx.dispatch_action(editor::actions::ExpandExcerptsDown::default());
        assert_eq!(shown_text(&diff, cx), before);
    }
}

#[gpui::test]
async fn a_file_whose_diff_doesnt_match_githubs_shows_githubs_patch(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let tab = open_unloaded(&workspace, PULL, cx);
    // 94c4db2's poll cassette, whose swapped entries Zed's diff pairs across GitHub's hunks.
    let patch = include_str!(
        "../../../tests/fixtures/github_patches/94c4db2-open-pull-requests.json.patch"
    );
    let swapped = PullFile {
        filename: "cassette.json".into(),
        additions: 50,
        deletions: 50,
        changes: 100,
        patch: Some(patch.into()),
        ..github_file("cassette.json", 10)
    };

    show_github_files(&tab, vec![github_file("a.txt", 50), swapped], cx);

    let diff = tab.read_with(cx, |tab, _| tab.diff().unwrap().clone());
    let text = shown_text(&diff, cx);
    assert!(text.contains("a 50 changed"), "{text}");
    assert!(text.contains(patch), "{text}");
    let warnings: Vec<_> = diff.read_with(cx, |diff, cx| {
        let editor = diff.rhs_editor().read(cx);
        let headers = editor.addon::<pr_diff::FileHeaders>().unwrap();
        headers
            .all_badges()
            .filter(|badge| badge.style == crate::diff_decorations::BadgeStyle::Warning)
            .map(|badge| badge.label.to_string())
            .collect()
    });
    assert_eq!(warnings, [api_diff::GITHUBS_PATCH]);
}

#[gpui::test]
async fn a_copy_that_fails_for_good_leaves_githubs_diff_with_a_banner(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let tab = open_unloaded(&workspace, PULL, cx);
    show_github_files(&tab, vec![github_file("a.txt", 50)], cx);

    let error = anyhow::anyhow!("the repository doesn't exist, or the account can't see it");
    tab.update(cx, |tab, cx| tab.load_failed(&error, cx));

    tab.read_with(cx, |tab, _| {
        assert_eq!(tab.status(), &Status::Ready);
        assert_eq!(tab.diff_source(), Some(DiffSource::GitHub));
    });
    assert_eq!(
        banners(&tab, cx),
        [
            "Lo couldn't show the local copy's diff, so this is GitHub's diff: changed lines \
          only. the repository doesn't exist, or the account can't see it"
        ]
    );

    // Without any diff, the tab says why it couldn't open.
    let other = open_unloaded(&workspace, "https://github.com/owner/repo/pull/2", cx);
    other.update(cx, |tab, cx| tab.load_failed(&error, cx));
    other.read_with(cx, |tab, _| {
        assert!(matches!(tab.status(), Status::Failed(_)))
    });
}

#[gpui::test]
async fn a_copy_that_fails_before_githubs_diff_arrives_leaves_it_with_a_banner(
    cx: &mut TestAppContext,
) {
    let (workspace, cx) = workspace(cx).await;
    let tab = open_unloaded(&workspace, PULL, cx);
    let error = anyhow::anyhow!("the repository doesn't exist, or the account can't see it");

    // GitHub's diff is on its way when the copy fails for good.
    tab.update_in(cx, |tab, window, cx| {
        let source = GithubDiff {
            link: link(PULL),
            title: format!("{PULL}: Change a"),
            repo_name: "repo".into(),
            head: None,
            listed_at: SystemTime::now(),
        };
        tab.show_github_files(source, vec![github_file("a.txt", 50)], window, cx);
        tab.load_failed(&error, cx);
    });
    assert!(matches!(status(&tab, cx), Status::Loading(_)));
    assert_eq!(banners(&tab, cx).len(), 1);
    cx.run_until_parked();

    tab.read_with(cx, |tab, _| {
        assert_eq!(tab.status(), &Status::Ready);
        assert_eq!(tab.diff_source(), Some(DiffSource::GitHub));
    });
    assert_eq!(
        banners(&tab, cx),
        [
            "Lo couldn't show the local copy's diff, so this is GitHub's diff: changed lines \
          only. the repository doesn't exist, or the account can't see it"
        ]
    );
}

#[gpui::test]
async fn a_public_pull_request_git_cant_copy_shows_githubs_diff(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let github = FakeGithub::default();
    let _git = GitServer::start(cx);
    uncopyable_pull(&github, "acme/app", 1, 2);
    anonymous(&github, cx);

    let tab = open_loaded(&workspace, "github.com/acme/app/pull/1", cx);

    tab.read_with(cx, |tab, _| {
        assert_eq!(tab.status(), &Status::Ready);
        assert_eq!(tab.diff_source(), Some(DiffSource::GitHub));
        assert_eq!(tab.files().len(), 2);
    });
    let banners = banners(&tab, cx);
    assert!(
        banners[0].starts_with("Lo couldn't show the local copy's diff"),
        "{banners:#?}"
    );
}

#[gpui::test]
async fn githubs_diff_never_replaces_the_local_copys(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let tab = open_unloaded(&workspace, PULL, cx);

    let _local_copy = show_local_copy(&tab, cx);
    let local = tab.read_with(cx, |tab, _| tab.diff().unwrap().clone());
    // Whole files: the local copy has the lines GitHub's patches leave out.
    let text = shown_text(&local, cx);
    assert!(text.contains("a 48\n"), "{text:?}");
    show_github_files(
        &tab,
        vec![github_file("a.txt", 50), github_file("b.txt", 60)],
        cx,
    );

    tab.read_with(cx, |tab, _| {
        assert_eq!(tab.diff_source(), Some(DiffSource::LocalCopy));
        assert_eq!(tab.diff(), Some(&local));
    });
}
