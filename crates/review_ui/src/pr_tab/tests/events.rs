use super::*;

use std::cell::RefCell;
use std::rc::Rc;

/// The events `tab` sends from now on.
fn record_events(tab: &Entity<PrTab>, cx: &mut VisualTestContext) -> Rc<RefCell<Vec<PrTabEvent>>> {
    let events = Rc::new(RefCell::new(Vec::new()));
    let log = events.clone();
    cx.update(|_, cx| {
        cx.subscribe(tab, move |_, event: &PrTabEvent, _| {
            log.borrow_mut().push(event.clone())
        })
        .detach()
    });
    events
}

/// The events sent since the last call, leaving out [`PrTabEvent::Changed`], which only the
/// window's title follows.
fn taken(events: &Rc<RefCell<Vec<PrTabEvent>>>) -> Vec<PrTabEvent> {
    events
        .borrow_mut()
        .drain(..)
        .filter(|event| *event != PrTabEvent::Changed)
        .collect()
}

fn with_head(head: char) -> PullRequest {
    let mut pull_request = fd_2212();
    pull_request.head.sha = head.to_string().repeat(40);
    pull_request
}

/// Features hear when GitHub gives a new head and when the diff shows another comparison, and
/// only then.
#[gpui::test]
async fn features_hear_of_a_new_head_and_a_new_comparison(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let tab = open_unloaded(&workspace, PULL, cx);
    let events = record_events(&tab, cx);

    let answered = SystemTime::now();
    tab.update(cx, |tab, cx| {
        tab.set_pull_request(&with_head('2'), answered, cx)
    });
    assert_eq!(taken(&events), [PrTabEvent::HeadChanged]);
    // The same head again isn't news, nor is an older answer naming another.
    tab.update(cx, |tab, cx| {
        tab.set_pull_request(&with_head('2'), answered + Duration::from_secs(1), cx);
        tab.set_pull_request(&with_head('3'), answered - Duration::from_secs(1), cx);
    });
    assert_eq!(taken(&events), []);
    tab.update(cx, |tab, cx| {
        tab.set_pull_request(&with_head('4'), answered + Duration::from_secs(2), cx)
    });
    assert_eq!(taken(&events), [PrTabEvent::HeadChanged]);

    let files = [("a.rs", lines(5, &[]), lines(5, &[3]))];
    show_test_diff(&tab, &workspace, &files, cx).await;
    assert_eq!(taken(&events), [PrTabEvent::ComparisonChanged]);
}

/// A tab can show another pull request in place of its own and back, as a stack's tab does for
/// each layer: everything it knows about each goes with it, and features hear of each swap.
#[gpui::test]
async fn swapping_the_pull_request_swaps_what_the_tab_knows_about_it(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let tab = open_unloaded(&workspace, PULL, cx);
    tab.update(cx, |tab, cx| {
        tab.set_pull_request(&fd_2212(), SystemTime::now(), cx)
    });
    let files = [
        ("a.rs", lines(5, &[]), lines(5, &[3])),
        ("b.rs", lines(5, &[]), lines(5, &[3])),
    ];
    list_test_files(&tab, &files, cx);
    let events = record_events(&tab, cx);

    let first = tab.read_with(cx, |tab, _| tab.pull().id());
    tab.update_in(cx, |tab, window, cx| {
        let other = ShownPull::new(link("https://github.com/owner/repo/pull/2"), cx);
        let first = tab.show_pull(other, window, cx);
        tab.keep_pull(first);
    });
    cx.run_until_parked();
    assert_eq!(
        taken(&events),
        [PrTabEvent::PullChanged, PrTabEvent::ComparisonChanged]
    );
    assert_eq!(cx.window_title().as_deref(), Some("owner/repo#2"));
    tab.read_with(cx, |tab, _| {
        assert_eq!(tab.pull().number(), 2);
        assert_eq!(tab.link().number, 2);
        assert_eq!(tab.header().title, None);
        assert_eq!(tab.files().len(), 0);
        assert!(tab.revisions().is_none());
        assert_eq!(tab.pull().latest_head(), None);
        let kept = &tab.kept_pulls()[0];
        assert_eq!(kept.number(), 1);
        assert_eq!(kept.latest_head(), Some("2".repeat(40).as_str()));
    });

    assert!(tab.update_in(cx, |tab, window, cx| tab.show_kept_pull(first, window, cx)));
    cx.run_until_parked();
    assert_eq!(
        taken(&events),
        [PrTabEvent::PullChanged, PrTabEvent::ComparisonChanged]
    );
    tab.read_with(cx, |tab, _| {
        assert_eq!(tab.pull().number(), 1);
        assert_eq!(
            tab.header().title.as_deref(),
            Some("Honor explicit use-jemalloc requests")
        );
        assert_eq!(tab.files().len(), 2);
        assert!(tab.revisions().is_some());
    });
    assert_eq!(
        cx.window_title().as_deref(),
        Some("sharkdp/fd#1 — Honor explicit use-jemalloc requests")
    );
}

/// When the tab shows another pull request whose details its poller has, features hear of the new
/// pull request before they hear of its head.
#[gpui::test]
async fn features_hear_of_a_new_pull_request_before_its_head(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    let tab = open_unloaded(&workspace, "github.com/rozed-dev/playground/pull/2", cx);
    let signed_in = sign_in_bot(&tab, "new_pull_before_its_head", &[], cx).await;
    let poller = signed_in._poller.clone();
    tab.update(cx, |tab, cx| tab.set_active(true, cx));
    let other = PullRef::new("rozed-dev", "playground", 3);
    signed_in
        .cache
        .put(
            BOT,
            &Target::Pull(other.clone()),
            &details_headed("Add c", "UNVIEWED", &"e".repeat(40)),
            None,
            SystemTime::now(),
        )
        .await
        .unwrap();
    // The other pull request is kept out of sight, watched, as a stack's other layer is.
    let kept = tab.update(cx, |tab, cx| {
        let pull = ShownPull::new(link("github.com/rozed-dev/playground/pull/3"), cx);
        let id = pull.id();
        tab.keep_pull(pull);
        tab.with_pull(id, cx, |tab, cx| tab.watch(poller, other, None, cx));
        id
    });
    cx.run_until_parked();
    let events = record_events(&tab, cx);

    tab.update_in(cx, |tab, window, cx| tab.show_kept_pull(kept, window, cx));
    cx.run_until_parked();
    assert_eq!(
        taken(&events),
        [
            PrTabEvent::PullChanged,
            PrTabEvent::HeadChanged,
            PrTabEvent::ComparisonChanged
        ]
    );
    tab.read_with(cx, |tab, _| {
        assert_eq!(tab.header().title.as_deref(), Some("Add c"));
        assert_eq!(tab.pull().latest_head(), Some("e".repeat(40).as_str()));
    });
}
