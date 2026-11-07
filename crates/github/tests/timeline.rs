//! Pull requests' timelines, read from recorded answers and mapped into Lo's timeline items:
//!
//! - `sharkdp/fd#2212`, read as rozed-bot three items a page: in full, then its newest page again.
//! - `cli/cli#13057`, merged, read as rozed-bot: two pages, the second holding again items pushed
//!   at the same time as the first page's oldest, then its newest page again.
//! - `rozed-dev/playground#4`, the review API checks' pull request; `rozed-dev/e2e` #14 (E1's full
//!   run) and #19 (E8, retargeted).

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use futures::executor::block_on;
use github::queries::timeline::{
    AutoMerge, BaseChange, Branch, Comment, Commit, Event, READ_IN_FULL_EVERY, Reference,
    ReferenceKind, Review, Reviewer,
};
use github::queries::{PullRequestTimeline, TimelineItem};
use github::replay::ReplayHttpClient;
use github::{GithubClient, PullRef, Token};

fn client(cassette: &str) -> (GithubClient, Arc<ReplayHttpClient>) {
    let http = ReplayHttpClient::cassette(cassette);
    (
        GithubClient::new(http.clone(), Token::new("gho_FAKE")),
        http,
    )
}

/// The `before` each request of a replaying client asked for: `None` for a newest page.
fn asked(http: &ReplayHttpClient) -> Vec<Option<String>> {
    (http.seen().iter())
        .map(|seen| {
            let body: serde_json::Value = serde_json::from_slice(&seen.body).unwrap();
            assert_eq!(body["operationName"], "Timeline");
            body["variables"]["before"].as_str().map(String::from)
        })
        .collect()
}

/// Any time: the recordings' reads don't depend on it, only on how far apart they are.
fn noon() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_794_000_000)
}

fn items(timeline: &PullRequestTimeline) -> Vec<&TimelineItem> {
    timeline.items.iter().map(|entry| &entry.item).collect()
}

fn other(typename: &str) -> TimelineItem {
    TimelineItem::Other {
        typename: Some(typename.into()),
    }
}

fn at(iso: &str) -> chrono::DateTime<chrono::Utc> {
    iso.parse().unwrap()
}

fn event(actor: &str, iso: &str) -> Event {
    Event {
        actor: Some(actor.into()),
        at: at(iso),
    }
}

#[test]
fn fd_2212s_timeline_is_read_in_full_then_by_its_newest_page() {
    let (client, http) = client("timeline_fd_2212_paged");
    let pull = PullRef::new("sharkdp", "fd", 2212);
    let first = block_on(client.timeline_paged(&pull, None, noon(), 3)).unwrap();
    // Newest first, three a page. Each page is asked for before the second item of the page
    // after it, which has a later time than the first, so it holds that first item again.
    let pages = asked(&http);
    assert_eq!(pages.len(), 4);
    assert_eq!(pages[0], None);
    assert!(pages[1..].iter().all(Option::is_some));
    assert_eq!(first.left_out, 0);
    assert_eq!(
        first.read_in_full_at,
        chrono::DateTime::<chrono::Utc>::from(noon())
    );
    assert_eq!(first.description.author.as_deref(), Some("voidsipp"));
    assert!(!first.description.body.is_empty());

    let first_push = "15635d66ba7ff3d2da0e50392fbcd37c90d83de5";
    let second_push = "22c70acc6992c0c51cb68527b87adf114d01acbc";
    let commit = |oid: &str, headline: &str, iso: &str| {
        TimelineItem::Commit(Commit {
            oid: oid.into(),
            headline: headline.into(),
            author_login: Some("voidsipp".into()),
            author_name: Some("voidsip".into()),
            committed_at: at(iso),
            checks: Some("SUCCESS".into()),
        })
    };
    let TimelineItem::Review(review) = items(&first)[1] else {
        panic!("{:?}", items(&first)[1]);
    };
    let TimelineItem::Comment(comment) = items(&first)[3] else {
        panic!("{:?}", items(&first)[3]);
    };
    assert_eq!(
        items(&first),
        [
            &commit(
                first_push,
                "Honor explicit use-jemalloc requests",
                "2026-11-03T07:06:46Z"
            ),
            &TimelineItem::Review(Review {
                id: "PRR_kwDOBWllys8AAAABVqux2w".into(),
                author: Some("tmccombs".into()),
                body: review.body.clone(),
                state: "CHANGES_REQUESTED".into(),
                created_at: at("2026-11-03T08:26:10Z"),
                submitted_at: Some(at("2026-11-03T09:01:32Z")),
                last_edited_at: None,
                url: "https://github.com/sharkdp/fd/pull/2212#pullrequestreview-5749060059".into(),
                hidden: false,
                commit: Some(first_push.into()),
                comments: 7,
            }),
            &commit(
                second_push,
                "Address release feature selection review",
                "2026-11-03T10:05:53Z"
            ),
            &TimelineItem::Comment(Comment {
                id: "IC_kwDOBWllys8AAAABiW8w-w".into(),
                author: Some("voidsipp".into()),
                body: comment.body.clone(),
                created_at: at("2026-11-03T10:05:55Z"),
                last_edited_at: None,
                url: "https://github.com/sharkdp/fd/pull/2212#issuecomment-6600732923".into(),
                hidden: None,
                viewer_can_update: false,
                viewer_can_delete: false,
            }),
            &other("ReferencedEvent"),
            &TimelineItem::CrossReference {
                event: event("scornz", "2026-11-06T14:25:39Z"),
                source: Some(Reference {
                    kind: ReferenceKind::PullRequest,
                    repository: "scornz/lo".into(),
                    number: 13,
                    title: "feat(repo_cache): keep a partial clone of each repository".into(),
                    url: "https://github.com/scornz/lo/pull/13".into(),
                }),
                will_close: false,
            },
            &other("ReferencedEvent"),
            &other("ReferencedEvent"),
        ]
    );

    // Read again within the hour, it asks for its newest page alone, which joins what it has.
    let later = noon() + Duration::from_secs(60);
    let again = block_on(client.timeline_paged(&pull, Some(&first), later, 3)).unwrap();
    assert_eq!(asked(&http).len(), 5);
    assert_eq!(asked(&http)[4], None);
    assert_eq!(again, first);

    // An hour after it was last read in full, it's read in full again.
    let hour_later = noon() + READ_IN_FULL_EVERY;
    let full = block_on(client.timeline_paged(&pull, Some(&again), hour_later, 3)).unwrap();
    assert_eq!(asked(&http).len(), 9);
    assert_eq!(full.items, first.items);
    assert_eq!(
        full.read_in_full_at,
        chrono::DateTime::<chrono::Utc>::from(hour_later)
    );
}

#[test]
fn cli_13057s_timeline_reads_each_item_once_where_items_share_a_time() {
    let (client, http) = client("timeline_cli_13057");
    let pull = PullRef::new("cli", "cli", 13057);
    let first = block_on(client.timeline(&pull, None, noon())).unwrap();
    let pages = asked(&http);
    assert_eq!(pages.len(), 2);

    // Every item once, as many as GitHub counts.
    assert_eq!(first.items.len(), 112);
    assert_eq!(first.left_out, 0);
    let cursors: HashSet<&str> = (first.items.iter())
        .map(|entry| entry.cursor.as_str())
        .collect();
    assert_eq!(cursors.len(), 112);

    // The 14 commits of a rebase share a time. The newest page held 7 of them, so the page before
    // it was read before the first item after them, which holds all 14 again: reading before the
    // newest page's first commit would have skipped one.
    let rebased = (first.items.iter())
        .position(|entry| {
            matches!(&entry.item, TimelineItem::Commit(commit) if commit.oid.starts_with("c0025738"))
        })
        .unwrap();
    let after_rebase = (first.items[rebased..].iter())
        .position(|entry| !matches!(entry.item, TimelineItem::Commit(_)))
        .unwrap();
    assert_eq!(after_rebase, 14);
    assert_eq!(
        pages[1].as_deref(),
        Some(first.items[rebased + after_rebase].cursor.as_str())
    );
    let newest_first = &first.items[first.items.len() - 100];
    assert!(
        matches!(&newest_first.item, TimelineItem::Commit(commit) if commit.oid.starts_with("c00bc6a9"))
    );

    // What the items say.
    let description = &first.description;
    assert_eq!(description.author.as_deref(), Some("BagToad"));
    assert_eq!(description.created_at, at("2026-03-30T00:42:33Z"));
    assert_eq!(description.last_edited_at, Some(at("2026-05-06T21:08:32Z")));
    let bagtoad = |iso| event("BagToad", iso);
    let all = items(&first);
    for expected in [
        TimelineItem::ForcePushed {
            event: bagtoad("2026-04-30T14:49:45Z"),
            branch: Branch::Head,
            before: Some("cea0e576f674d817a024e3bbe3b7233f44ac57af".into()),
            after: Some("6e76f11dec6067b7cb26b70875caf796145c3640".into()),
        },
        TimelineItem::Draft {
            event: bagtoad("2026-04-30T14:49:47Z"),
            draft: false,
        },
        // A reviewer GitHub doesn't show, and a person.
        TimelineItem::ReviewRequest {
            event: bagtoad("2026-04-30T14:49:48Z"),
            reviewer: None,
            added: true,
        },
        TimelineItem::ReviewRequest {
            event: bagtoad("2026-04-30T14:49:49Z"),
            reviewer: Some(Reviewer::Account("williammartin".into())),
            added: true,
        },
        TimelineItem::Merged {
            event: bagtoad("2026-06-04T00:27:11Z"),
            commit: Some("20147bf81fed534faf1b5c9be12638983b8e5e82".into()),
            into: "trunk".into(),
        },
        TimelineItem::Closed(bagtoad("2026-06-04T00:27:12Z")),
        other("HeadRefDeletedEvent"),
    ] {
        assert!(all.contains(&&expected), "{expected:?}");
    }
    let auto_merge: Vec<&AutoMerge> = (all.iter())
        .filter_map(|item| match item {
            TimelineItem::AutoMerge { change, .. } => Some(change),
            _ => None,
        })
        .collect();
    let disabled = AutoMerge::Disabled {
        reason: Some("Manually disabled by user".into()),
    };
    assert_eq!(
        auto_merge,
        [
            &AutoMerge::EnabledMerge,
            &disabled,
            &AutoMerge::EnabledSquash,
            &disabled,
            &AutoMerge::EnabledSquash
        ]
    );
    let reviews: Vec<(&str, &str, usize)> = (all.iter())
        .filter_map(|item| match item {
            TimelineItem::Review(review) => Some((
                review.author.as_deref().unwrap(),
                review.state.as_str(),
                review.comments,
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        reviews,
        [
            ("copilot-pull-request-reviewer", "COMMENTED", 6),
            ("BagToad", "COMMENTED", 1),
            ("camchenry", "COMMENTED", 1),
            ("babakks", "CHANGES_REQUESTED", 20),
            ("babakks", "CHANGES_REQUESTED", 3),
            ("babakks", "APPROVED", 0),
        ]
    );
    let references = (all.iter())
        .filter(|item| matches!(item, TimelineItem::CrossReference { .. }))
        .count();
    assert_eq!(references, 36);

    // Its newest page again joins what it has.
    let again = block_on(client.timeline(&pull, Some(&first), noon())).unwrap();
    assert_eq!(asked(&http), [pages[0].clone(), pages[1].clone(), None]);
    assert_eq!(again, first);
}

#[test]
fn the_playgrounds_review_checks_timeline_has_its_reviews_and_close() {
    let (client, _) = client("timeline_playground_4");
    let pull = PullRef::new("rozed-dev", "playground", 4);
    let timeline = block_on(client.timeline(&pull, None, noon())).unwrap();
    let all = items(&timeline);
    assert_eq!(all.len(), 12);
    let commits: Vec<&str> = (all.iter())
        .filter_map(|item| match item {
            TimelineItem::Commit(commit) => Some(&commit.oid[..7]),
            _ => None,
        })
        .collect();
    assert_eq!(commits, ["fc0f4b4", "2104a3c", "b405bfc"]);
    let reviews: Vec<&Review> = (all.iter())
        .filter_map(|item| match item {
            TimelineItem::Review(review) => Some(review),
            _ => None,
        })
        .collect();
    assert_eq!(reviews.len(), 8);
    for review in reviews {
        assert_eq!(review.author.as_deref(), Some("rozed-bot"));
        assert_eq!(review.state, "COMMENTED");
        assert!(review.submitted_at.is_some());
    }
    assert!(
        matches!(all[11], TimelineItem::Closed(event) if event.actor.as_deref() == Some("rozed-bot"))
    );
}

#[test]
fn e2es_timelines_have_their_force_push_and_base_change() {
    let (client, _) = client("timeline_e2e");
    let e1 =
        block_on(client.timeline(&PullRef::new("rozed-dev", "e2e", 14), None, noon())).unwrap();
    let pushed = (e1.items.iter())
        .find_map(|entry| match &entry.item {
            TimelineItem::ForcePushed {
                event,
                branch,
                after,
                ..
            } => Some((event, branch, after)),
            _ => None,
        })
        .unwrap();
    assert_eq!(pushed.0.actor.as_deref(), Some("rozed-bot"));
    assert_eq!(*pushed.1, Branch::Head);
    assert!(pushed.2.is_some());
    assert!(
        matches!(items(&e1).last(), Some(TimelineItem::Other { typename: Some(name) }) if name == "ReferencedEvent")
    );

    let e8 =
        block_on(client.timeline(&PullRef::new("rozed-dev", "e2e", 19), None, noon())).unwrap();
    let changed = (e8.items.iter())
        .find_map(|entry| match &entry.item {
            TimelineItem::BaseChanged {
                event,
                from,
                to,
                how,
            } => Some((event.actor.as_deref(), from.as_str(), to.as_str(), *how)),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        changed,
        (Some("rozed-bot"), "main", "e8/base", BaseChange::Changed)
    );
}
