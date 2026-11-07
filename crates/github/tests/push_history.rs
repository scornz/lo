//! Pull requests' push histories, read from recorded answers and mapped into revisions and their
//! bases:
//!
//! - `rozed-dev/e2e` #14 and #16, E1's revisions (`script/e2e/README.md`): #14 is closed after the
//!   full run, r1-r6 with GitHub's own rebase between r4 and r5; #16 is open at r3. rozed-bot
//!   reviewed r2 of each.
//! - `sharkdp/fd` #2212, open, from the fork `voidsipp/fd`; and #2202, merged with a merge commit
//!   from a fork since deleted. Read as rozed-bot, read only.
//!
//! Each cassette holds two reads, the second asking about each page of pushes with its ETag.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use futures::executor::block_on;
use github::activity::{BranchActivity, Direction};
use github::cassette::{self, Interaction, RecordedRequest, RecordedResponse};
use github::push_history::PushHistory;
use github::replay::ReplayHttpClient;
use github::revision_marks::SeenHead;
use github::{GithubClient, PullRef, RepoRef, Token};
use review::revisions::{
    BaseTipSource, Comparison, PushedAt, Revisions, Source, base_tip_at, build_revisions,
    default_comparison,
};

const BOT: &str = "rozed-bot";

fn client(http: Arc<ReplayHttpClient>) -> GithubClient {
    GithubClient::new(http, Token::new("gho_FAKE"))
}

/// The cassette's first read, and the replaying client, ready for its second.
fn read(cassette: &str, pull: &PullRef) -> (PushHistory, GithubClient, Arc<ReplayHttpClient>) {
    let http = ReplayHttpClient::cassette(cassette);
    let client = client(http.clone());
    let history = block_on(client.push_history(pull, BOT, None)).unwrap();
    (history, client, http)
}

fn time(iso: &str) -> SystemTime {
    iso.parse::<DateTime<Utc>>().unwrap().into()
}

/// The full SHA starting with `short` among `shas`.
fn full<'a>(shas: impl IntoIterator<Item = &'a String>, short: &str) -> String {
    let found: Vec<&String> = shas
        .into_iter()
        .filter(|sha| sha.starts_with(short))
        .collect();
    assert_eq!(found.len(), 1, "{short}");
    found[0].clone()
}

fn short(revisions: &Revisions) -> Vec<String> {
    revisions
        .iter()
        .map(|revision| revision.head[..7].to_string())
        .collect()
}

fn revisions(history: &PushHistory, seen: &[SeenHead]) -> Revisions {
    build_revisions(
        &history.push_records(seen),
        &[],
        &history.head,
        history.opened_at(),
    )
}

/// For `base_tip_at` without a local copy to ask: no base tip holds a revision's head.
fn no_tip_holds(_tip: &str, _head: &str) -> bool {
    false
}

fn e2e(number: i64) -> PullRef {
    PullRef::new("rozed-dev", "e2e", number)
}

#[test]
fn e1s_full_run_has_six_revisions_with_githubs_rebase_as_a_force_push() {
    let (history, _, _) = read("push_history_e2e_14", &e2e(14));

    let revisions = revisions(&history, &[]);

    assert_eq!(
        short(&revisions),
        [
            "857b788", "2e2d4be", "864ede1", "28b21f2", "7513db3", "adb7208"
        ]
    );
    let forced: Vec<Option<bool>> = revisions.iter().map(|r| r.forced).collect();
    assert_eq!(
        forced,
        [
            None,
            Some(false),
            Some(false),
            Some(false),
            Some(true),
            Some(false)
        ]
    );
    let [r1, r2, r3, r4, r5, r6] = revisions.as_slice() else {
        panic!("six revisions");
    };
    // r1 created the branch a second before the pull request opened.
    assert_eq!(r1.pushed_at, PushedAt::Exact(time("2026-11-06T19:27:50Z")));
    assert_eq!(history.opened_at(), time("2026-11-06T19:27:51Z"));
    assert_eq!(r2.sources, [Source::Activity, Source::Review].into());
    assert_eq!(r3.sources, [Source::Activity].into());
    // The rebase shows in both the activity API and the timeline, which names r4 as the head it
    // replaced.
    assert_eq!(r4.sources, [Source::Activity, Source::ForcePush].into());
    assert_eq!(r5.sources, [Source::Activity, Source::ForcePush].into());
    assert_eq!(r5.previous.as_deref(), Some(r4.head.as_str()));
    assert_eq!(r5.pushed_at, PushedAt::Exact(time("2026-11-06T19:28:47Z")));
    assert_eq!(r6.sources, [Source::Activity, Source::Head].into());
    assert_eq!(
        (history.head_force_pushes[0].at, history.closed_at.is_some()),
        ("2026-11-06T19:28:48Z".parse().unwrap(), true)
    );
    // rozed-bot's last review was on r2, so the pull request opens at r2 → r6.
    assert_eq!(history.last_review(), Some(r2.head.as_str()));
    assert_eq!(
        default_comparison(&revisions, history.last_review(), None),
        Comparison::Between {
            from: r2.head.clone(),
            to: r6.head.clone(),
        }
    );
}

#[test]
fn e1s_bases_follow_main_as_it_moved() {
    let (history, _, _) = read("push_history_e2e_14", &e2e(14));
    let revisions = revisions(&history, &[]);
    let main = &history.base_activity[0];
    assert_eq!(
        (main.branch.as_str(), main.direction),
        ("main", Direction::Desc)
    );
    let tips: Vec<String> = main.entries().iter().map(|e| e.after.clone()).collect();
    let main_now = full(&tips, "a45754b");
    let base = history.base_history(Some(&main_now));

    let bases: Vec<(String, BaseTipSource)> = revisions
        .iter()
        .map(|revision| {
            let tip = base_tip_at(&base, revision.pushed_at, &revision.head, no_tip_holds).unwrap();
            (tip.commit[..7].to_string(), tip.source)
        })
        .collect();

    // r1-r4 were pushed onto 99a9e80; #15 then moved main to a45754b, and GitHub rebased r4 onto
    // it as r5.
    let activity = BaseTipSource::Activity;
    assert_eq!(
        bases,
        [
            ("99a9e80".to_string(), activity),
            ("99a9e80".to_string(), activity),
            ("99a9e80".to_string(), activity),
            ("99a9e80".to_string(), activity),
            ("a45754b".to_string(), activity),
            ("a45754b".to_string(), activity),
        ]
    );
}

#[test]
fn e1_open_at_r3_opens_from_your_review_on_r2() {
    let (history, _, _) = read("push_history_e2e_16", &e2e(16));

    let revisions = revisions(&history, &[]);

    assert_eq!(short(&revisions), ["60005cf", "b88cc8b", "34427a8"]);
    assert_eq!(history.closed_at, None);
    assert_eq!(history.head_force_pushes, []);
    let r2 = &revisions.as_slice()[1];
    assert_eq!(history.last_review(), Some(r2.head.as_str()));
    assert_eq!(
        default_comparison(&revisions, history.last_review(), None),
        Comparison::Between {
            from: r2.head.clone(),
            to: history.head.clone(),
        }
    );
    // Seen here at r3 too: nothing's new.
    assert_eq!(
        default_comparison(&revisions, history.last_review(), Some(&history.head)),
        Comparison::Full {
            nothing_new_since: Some(history.head.clone()),
        }
    );
}

#[test]
fn reading_again_asks_about_each_page_with_its_etag() {
    for (cassette, pull) in [
        ("push_history_e2e_14", e2e(14)),
        ("push_history_fd_2212", PullRef::new("sharkdp", "fd", 2212)),
        ("push_history_fd_2202", PullRef::new("sharkdp", "fd", 2202)),
    ] {
        let (first, client, http) = read(cassette, &pull);
        let before = http.seen().len();

        let again = block_on(client.push_history(&pull, BOT, Some(&first))).unwrap();

        assert_eq!(again, first, "{cassette}");
        let rest = http.seen().split_off(before);
        let activity: Vec<_> = rest
            .iter()
            .filter(|seen| seen.url.contains("/activity"))
            .collect();
        assert!(!activity.is_empty(), "{cassette}");
        for seen in activity {
            assert!(
                seen.header("if-none-match").is_some(),
                "{cassette}: {} without its ETag",
                seen.url
            );
        }
    }
}

#[test]
fn a_pull_request_from_a_fork_reads_the_forks_branch() {
    let pull = PullRef::new("sharkdp", "fd", 2212);
    let (history, _, http) = read("push_history_fd_2212", &pull);

    let head_repo = history.head_repo.as_ref().unwrap();
    assert_eq!(RepoRef::from(head_repo), RepoRef::new("voidsipp", "fd"));
    assert!(http.seen()[1].url.starts_with(
        "https://api.github.com/repos/voidsipp/fd/activity?ref=refs%2Fheads%2Ffix%2F1822"
    ));
    let revisions = revisions(&history, &[]);
    assert_eq!(short(&revisions), ["15635d6", "22c70ac"]);
    // The maintainer reviewed r1. The base repository's check suites for a fork's pull request
    // weren't started by a push, so they name no heads.
    assert_eq!(
        revisions.as_slice()[0].sources,
        [Source::Activity, Source::Review].into()
    );
    assert!(
        history
            .check_suites
            .iter()
            .all(|suite| suite.next.is_none())
    );
    // rozed-bot never reviewed it.
    assert_eq!(history.last_review(), None);
    assert_eq!(
        default_comparison(&revisions, None, None),
        Comparison::Full {
            nothing_new_since: None
        }
    );

    // master's activity, newest first, stopped at the push before the pull request's first
    // revision.
    let master = &history.base_activity[0];
    let entries = master.entries();
    assert_eq!(master.pages.len(), 1);
    assert!(master.pages[0].next.is_some() && !master.truncated);
    let base = history.base_history(Some(&entries.last().unwrap().after));
    let r1 = &revisions.as_slice()[0];
    let tip = base_tip_at(&base, r1.pushed_at, &r1.head, no_tip_holds).unwrap();
    assert_eq!(
        (&tip.commit[..7], tip.source),
        ("cc4e40f", BaseTipSource::Activity)
    );
}

#[test]
fn a_merged_pull_request_whose_fork_is_gone_has_no_pushes_but_its_reviews_and_check_suites() {
    let pull = PullRef::new("sharkdp", "fd", 2202);
    let (history, _, http) = read("push_history_fd_2202", &pull);

    assert_eq!(history.head_repo, None);
    assert!(history.head_activity.unavailable.is_some());
    assert!(
        http.seen()
            .iter()
            .all(|seen| !seen.url.contains("windows-arm64"))
    );
    let revisions = revisions(&history, &[]);
    assert_eq!(short(&revisions), ["4feccf0", "31f36c8"]);
    // Without the fork's pushes, the base repository's check suites, which no push started,
    // still say each was the head by the time they were made: a day before the first review.
    assert!(!history.complete_log());
    let r1 = &revisions.as_slice()[0];
    assert_eq!(r1.pushed_at, PushedAt::By(time("2026-10-28T03:56:10Z")));
    assert_eq!(r1.sources, [Source::Review, Source::CheckSuite].into());

    // Merged with a merge commit: every revision's base is its first parent.
    let merge = history.merge_commit.as_ref().unwrap();
    assert!(history.merged);
    assert_eq!(merge.parents.len(), 2);
    let base = history.base_history(None);
    for revision in revisions.iter() {
        let tip = base_tip_at(&base, revision.pushed_at, &revision.head, no_tip_holds).unwrap();
        assert_eq!(tip.commit, merge.parents[0]);
        assert_eq!(tip.source, BaseTipSource::MergeParent);
    }
}

#[test]
fn heads_the_poller_saw_are_revisions_too() {
    let (history, _, _) = read("push_history_fd_2202", &PullRef::new("sharkdp", "fd", 2202));
    // A push between the two reviewed heads that only rozed saw.
    let seen = [SeenHead {
        head: "5eed000000000000000000000000000000000000".into(),
        first_seen: time("2026-10-29T06:20:00Z"),
    }];

    let revisions = revisions(&history, &seen);

    assert_eq!(short(&revisions), ["4feccf0", "5eed000", "31f36c8"]);
    assert_eq!(revisions.as_slice()[1].sources, [Source::Poller].into());
}

// Paging, on answers made up from the recorded ones.

/// GraphQL's answer for e2e#14, as recorded.
fn recorded_revisions() -> Interaction {
    let (_, interactions) =
        cassette::read_cassette(&cassette::fixtures_dir().join("push_history_e2e_14")).unwrap();
    interactions.into_iter().next().unwrap().1
}

/// An activity entry for a push at `minute` past 10:00 on 2026-11-06.
fn entry(id: i64, before: &str, after: &str, minute: u64, kind: &str) -> serde_json::Value {
    let at = DateTime::<Utc>::from(time("2026-11-06T10:00:00Z") + Duration::from_secs(minute * 60));
    serde_json::json!({
        "id": id,
        "node_id": format!("RA_{id}"),
        "before": before,
        "after": after,
        "ref": "refs/heads/branch",
        "timestamp": at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "activity_type": kind,
        "actor": {"login": "someone"},
    })
}

fn sha(n: usize) -> String {
    format!("{n:040x}")
}

/// `count` normal pushes, one a minute, after the branch is created from `sha(1)`.
fn pushes(count: usize) -> Vec<serde_json::Value> {
    let mut entries = vec![entry(1, &"0".repeat(40), &sha(1), 0, "branch_creation")];
    for n in 2..=count {
        entries.push(entry(n as i64, &sha(n - 1), &sha(n), n as u64, "push"));
    }
    entries
}

fn page(url: &str, etag: &str, next: Option<&str>, entries: &[serde_json::Value]) -> Interaction {
    let mut headers = std::collections::BTreeMap::new();
    headers.insert("etag".to_string(), etag.to_string());
    if let Some(next) = next {
        headers.insert("link".to_string(), format!("<{next}>; rel=\"next\""));
    }
    Interaction {
        request: RecordedRequest {
            method: "GET".into(),
            url: url.into(),
            ..Default::default()
        },
        response: RecordedResponse {
            status: 200,
            headers,
            body: Some(serde_json::Value::Array(entries.to_vec())),
            body_text: None,
        },
    }
}

fn not_modified(url: &str, etag: &str) -> Interaction {
    Interaction {
        request: RecordedRequest {
            method: "GET".into(),
            url: url.into(),
            if_none_match: Some(etag.into()),
            ..Default::default()
        },
        response: RecordedResponse {
            status: 304,
            headers: [("etag".to_string(), etag.to_string())].into(),
            body: None,
            body_text: None,
        },
    }
}

const FIRST: &str = "https://api.github.com/repos/o/r/activity?ref=refs%2Fheads%2Fbranch&direction=asc&per_page=100";
const SECOND: &str = "https://api.github.com/repositories/1/activity?ref=refs%2Fheads%2Fbranch&direction=asc&per_page=100&after=CURSOR";

fn branch_activity(
    client: &GithubClient,
    direction: Direction,
    known: Option<&BranchActivity>,
    enough: impl Fn(&github::activity::ActivityEntry) -> bool,
) -> BranchActivity {
    block_on(client.branch_activity(
        &RepoRef::new("o", "r"),
        "branch",
        direction,
        known,
        10,
        enough,
    ))
    .unwrap()
}

fn requests(http: &ReplayHttpClient) -> Vec<(String, bool)> {
    http.seen()
        .into_iter()
        .map(|seen| {
            let conditional = seen.header("if-none-match").is_some();
            (seen.url, conditional)
        })
        .collect()
}

#[test]
fn reads_every_page_of_pushes_and_asks_about_each_again_with_its_etag() {
    let entries = pushes(150);
    let http = Arc::new(
        ReplayHttpClient::from_interactions(
            "paged_activity",
            [
                page(FIRST, "\"p1\"", Some(SECOND), &entries[..100]),
                page(SECOND, "\"p2\"", None, &entries[100..]),
                not_modified(FIRST, "\"p1\""),
                not_modified(SECOND, "\"p2\""),
            ],
        )
        .unwrap(),
    );
    let client = client(http.clone());

    let first = branch_activity(&client, Direction::Asc, None, |_| false);
    assert_eq!(first.entries().len(), 150);
    assert_eq!(first.entries()[149].after, sha(150));
    assert_eq!(first.pages[0].next.as_deref(), Some(SECOND));
    let again = branch_activity(&client, Direction::Asc, Some(&first), |_| false);

    assert_eq!(again, first);
    assert_eq!(
        requests(&http),
        [
            (FIRST.to_string(), false),
            (SECOND.to_string(), false),
            (FIRST.to_string(), true),
            (SECOND.to_string(), true),
        ]
    );
}

#[test]
fn a_full_last_page_is_read_in_full_again_for_a_page_after_it() {
    let entries = pushes(101);
    let mut grown = page(FIRST, "\"p1\"", Some(SECOND), &entries[..100]);
    // GitHub's answer to the same URL, now with a next page.
    grown.request.url = FIRST.into();
    let http = Arc::new(
        ReplayHttpClient::from_interactions(
            "full_last_page",
            [
                page(FIRST, "\"p1\"", None, &entries[..100]),
                grown,
                page(SECOND, "\"p2\"", None, &entries[100..]),
            ],
        )
        .unwrap(),
    );
    let client = client(http.clone());
    let first = branch_activity(&client, Direction::Asc, None, |_| false);
    assert_eq!(first.entries().len(), 100);

    let again = branch_activity(&client, Direction::Asc, Some(&first), |_| false);

    assert_eq!(again.entries().len(), 101);
    // Asked without its ETag, which would have answered 304 without the new Link.
    assert_eq!(requests(&http)[1], (FIRST.to_string(), false));
}

#[test]
fn newest_first_reading_stops_at_enough_and_an_unchanged_first_page_ends_it() {
    let first_url = "https://api.github.com/repos/o/r/activity?ref=refs%2Fheads%2Fbranch&direction=desc&per_page=100";
    let mut newest_first = pushes(250);
    newest_first.reverse();
    let next = |n: usize| format!("{first_url}&after=C{n}");
    let http = Arc::new(
        ReplayHttpClient::from_interactions(
            "desc_activity",
            [
                page(first_url, "\"d1\"", Some(&next(2)), &newest_first[..100]),
                page(&next(2), "\"d2\"", Some(&next(3)), &newest_first[100..200]),
                not_modified(first_url, "\"d1\""),
            ],
        )
        .unwrap(),
    );
    let client = client(http.clone());
    // Enough once a push at or before minute 120 is read: the second page has it.
    let since = time("2026-11-06T12:00:00Z");
    let enough =
        |entry: &github::activity::ActivityEntry| SystemTime::from(entry.timestamp) <= since;

    let first = branch_activity(&client, Direction::Desc, None, enough);

    assert_eq!(first.pages.len(), 2);
    assert!(!first.truncated);
    let entries = first.entries();
    // Oldest first, whichever way it was read.
    assert_eq!(entries.first().unwrap().after, sha(51));
    assert_eq!(entries.last().unwrap().after, sha(250));
    let again = branch_activity(&client, Direction::Desc, Some(&first), enough);
    assert_eq!(again, first);
    assert_eq!(requests(&http).len(), 3);
}

#[test]
fn reading_stops_at_the_most_pages() {
    let entries = pushes(250);
    let url = |n: usize| format!("{FIRST}&after=C{n}");
    let http = Arc::new(
        ReplayHttpClient::from_interactions(
            "capped_activity",
            [
                page(FIRST, "\"p1\"", Some(&url(2)), &entries[..100]),
                page(&url(2), "\"p2\"", Some(&url(3)), &entries[100..200]),
            ],
        )
        .unwrap(),
    );
    let client = client(http.clone());

    let activity = block_on(client.branch_activity(
        &RepoRef::new("o", "r"),
        "branch",
        Direction::Asc,
        None,
        2,
        |_| false,
    ))
    .unwrap();

    assert!(activity.truncated);
    assert_eq!(activity.entries().len(), 200);
}

#[test]
fn a_repository_that_is_gone_has_no_pushes_to_show() {
    let mut gone = page(FIRST, "", None, &[]);
    gone.response.status = 404;
    gone.response.headers.clear();
    gone.response.body = Some(serde_json::json!({
        "message": "Not Found",
        "documentation_url": "https://docs.github.com/rest/repos/repos#list-repository-activities",
        "status": "404"
    }));
    let http = Arc::new(ReplayHttpClient::from_interactions("gone", [gone]).unwrap());

    let activity = branch_activity(&client(http), Direction::Asc, None, |_| false);

    assert_eq!(activity.unavailable.as_deref(), Some("Not Found"));
    assert!(activity.entries().is_empty());
}

/// e2e#14's answer, read `first` a page, with the force-push event and the review on separate
/// pages: the timeline's second page brings a made-up force-push.
fn paged_revisions() -> Vec<Interaction> {
    let recorded = recorded_revisions();
    let mut first = recorded.clone();
    first.request.variables.as_mut().unwrap()["first"] = 1.into();
    let mut first_pull =
        first.response.body.as_ref().unwrap()["data"]["repository"]["pullRequest"].clone();
    first_pull["timelineItems"]["filteredCount"] = 2.into();
    first_pull["timelineItems"]["pageInfo"] =
        serde_json::json!({"hasNextPage": true, "endCursor": "T1"});
    first_pull["reviews"]["pageInfo"] =
        serde_json::json!({"hasNextPage": false, "endCursor": "R1"});
    first.response.body.as_mut().unwrap()["data"]["repository"]["pullRequest"] = first_pull.clone();

    let mut second = first.clone();
    let variables = second.request.variables.as_mut().unwrap();
    variables["timelineAfter"] = "T1".into();
    variables["reviewsAfter"] = "R1".into();
    let mut second_pull = first_pull;
    second_pull["timelineItems"]["pageInfo"] =
        serde_json::json!({"hasNextPage": false, "endCursor": "T2"});
    second_pull["timelineItems"]["nodes"] = serde_json::json!([{
        "__typename": "HeadRefForcePushedEvent",
        "createdAt": "2026-11-06T19:30:00Z",
        "beforeCommit": {"oid": sha(1)},
        "afterCommit": {"oid": sha(2)},
    }]);
    // The reviews were all read on the first page: what comes back for them now is ignored.
    second_pull["reviews"]["nodes"] = serde_json::json!([{
        "submittedAt": "2026-11-06T19:31:00Z",
        "commit": {"oid": sha(2)},
        "author": {"__typename": "User", "login": "someone"},
    }]);
    second_pull["reviews"]["pageInfo"] =
        serde_json::json!({"hasNextPage": false, "endCursor": null});
    second.response.body.as_mut().unwrap()["data"]["repository"]["pullRequest"] = second_pull;
    vec![first, second]
}

#[test]
fn reads_every_page_of_force_pushes_and_reviews() {
    let http = Arc::new(
        ReplayHttpClient::from_interactions("paged_revisions", paged_revisions()).unwrap(),
    );

    let pull_request =
        block_on(client(http.clone()).pull_request_revisions_paged("rozed-dev", "e2e", 14, BOT, 1))
            .unwrap();

    assert_eq!(pull_request.timeline_items.nodes.unwrap().len(), 2);
    assert_eq!(pull_request.reviews.unwrap().nodes.unwrap().len(), 1);
    assert_eq!(http.seen().len(), 2);
}

#[test]
fn the_fields_a_push_history_watches_are_in_the_check() {
    use github::poll::CachedRead as _;

    let client = client(ReplayHttpClient::cassette("graphql_pull_request_check_1"));
    let check = block_on(client.pull_request_check("rozed-dev", "playground", 1)).unwrap();
    let json = serde_json::to_value(&check).unwrap();

    for pointer in PushHistory::WATCHED {
        assert!(
            json.pointer(pointer).is_some_and(|value| !value.is_null()),
            "{pointer} isn't in the check"
        );
    }
}

// A long-lived head branch, on answers made up from e2e#14's.

/// The activity API's first page of e2e#14's head branch, newest first.
const HEAD_PUSHES: &str = "https://api.github.com/repos/rozed-dev/e2e/activity?ref=refs%2Fheads%2Fe1%2Frevisions-3&direction=desc&per_page=100";

/// When push `n` of [`pushes`] was made.
fn pushed_at(n: usize) -> SystemTime {
    time("2026-11-06T10:00:00Z") + Duration::from_secs(n as u64 * 60)
}

/// e2e#14 as an open pull request whose head branch had `before` pushes when it opened, half a
/// minute after the last of them, and `after` pushes since, each a minute apart: GraphQL's answer,
/// the head branch's pushes newest first, a page at a time, and `main`'s as recorded.
fn long_lived(before: usize, after: usize) -> (Vec<Interaction>, SystemTime) {
    let opened = pushed_at(before) + Duration::from_secs(30);
    let mut graphql = recorded_revisions();
    let pull = &mut graphql.response.body.as_mut().unwrap()["data"]["repository"]["pullRequest"];
    pull["createdAt"] = DateTime::<Utc>::from(opened)
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        .into();
    pull["closed"] = false.into();
    pull["closedAt"] = serde_json::Value::Null;
    pull["headRefOid"] = sha(before + after).into();
    pull["timelineItems"]["filteredCount"] = 0.into();
    pull["timelineItems"]["nodes"] = serde_json::json!([]);
    pull["reviews"]["totalCount"] = 0.into();
    pull["reviews"]["nodes"] = serde_json::json!([]);
    pull["viewerLastReview"]["nodes"] = serde_json::json!([]);
    pull["commits"]["nodes"] = serde_json::json!([]);

    let mut newest_first = pushes(before + after);
    newest_first.reverse();
    let url = |page: usize| match page {
        0 => HEAD_PUSHES.to_string(),
        page => format!("{HEAD_PUSHES}&after=C{page}"),
    };
    let pages: Vec<&[serde_json::Value]> = newest_first.chunks(100).collect();
    let mut interactions = vec![graphql];
    for (index, entries) in pages.iter().enumerate() {
        let next = (index + 1 < pages.len()).then(|| url(index + 1));
        let etag = format!("\"h{index}\"");
        interactions.push(page(&url(index), &etag, next.as_deref(), entries));
    }
    let (_, recorded) =
        cassette::read_cassette(&cassette::fixtures_dir().join("push_history_e2e_14")).unwrap();
    interactions.extend(
        (recorded.into_iter().map(|(_, interaction)| interaction))
            .filter(|interaction| interaction.request.url.contains("heads%2Fmain"))
            .take(1),
    );
    (interactions, opened)
}

#[test]
fn a_long_lived_head_branch_is_read_back_to_when_the_pull_request_opened() {
    let (interactions, opened) = long_lived(1500, 5);
    let http = Arc::new(ReplayHttpClient::from_interactions("long_lived", interactions).unwrap());

    let history = block_on(client(http.clone()).push_history(&e2e(14), BOT, None)).unwrap();

    // One page holds the five pushes since it opened, and the one it opened with.
    let head_pages = http
        .seen()
        .iter()
        .filter(|seen| seen.url.contains("revisions-3"))
        .count();
    assert_eq!(head_pages, 1);
    assert!(!history.head_activity.truncated);
    let revisions = revisions(&history, &[]);
    let heads: Vec<String> = revisions.iter().map(|r| r.head.clone()).collect();
    assert_eq!(heads, (1500..=1505).map(sha).collect::<Vec<_>>());
    assert_eq!(
        revisions.as_slice()[0].pushed_at,
        PushedAt::Exact(pushed_at(1500))
    );
    assert!(pushed_at(1500) < opened);
}

#[test]
fn a_head_branch_with_more_pushes_since_it_opened_than_rozed_reads_is_truncated() {
    let (interactions, opened) = long_lived(3, 1100);
    let http = Arc::new(ReplayHttpClient::from_interactions("truncated", interactions).unwrap());

    let history = block_on(client(http.clone()).push_history(&e2e(14), BOT, None)).unwrap();

    assert!(history.head_activity.truncated);
    let head_pages = http
        .seen()
        .iter()
        .filter(|seen| seen.url.contains("revisions-3"))
        .count();
    assert_eq!(head_pages, github::push_history::MAX_HEAD_PAGES);
    // Only the latest thousand pushes, all since it opened: nothing from before stands in for
    // its first revision.
    let revisions = revisions(&history, &[]);
    assert_eq!(revisions.len(), 1000);
    assert_eq!(revisions.as_slice()[0].head, sha(104));
    assert!(revisions.iter().all(|revision| match revision.pushed_at {
        PushedAt::Exact(at) => at > opened,
        _ => false,
    }));
}

/// fd#2202's answers, merged as `edit` says instead of with a merge commit.
fn merged_otherwise(edit: impl Fn(&mut serde_json::Value)) -> PushHistory {
    let (_, interactions) =
        cassette::read_cassette(&cassette::fixtures_dir().join("push_history_fd_2202")).unwrap();
    let interactions = interactions.into_iter().map(|(_, mut interaction)| {
        if interaction.request.operation.is_some() {
            let pull = &mut interaction.response.body.as_mut().unwrap()["data"]["repository"]
                ["pullRequest"];
            edit(&mut pull["mergeCommit"]);
        }
        interaction
    });
    let http = Arc::new(ReplayHttpClient::from_interactions("merged", interactions).unwrap());
    block_on(client(http).push_history(&PullRef::new("sharkdp", "fd", 2202), BOT, None)).unwrap()
}

#[test]
fn merges_without_a_merge_commit_take_master_as_it_was() {
    let bases = |history: &PushHistory| -> Vec<(String, BaseTipSource)> {
        let base = history.base_history(Some(&sha(9)));
        revisions(history, &[])
            .iter()
            .map(|revision| {
                let tip =
                    base_tip_at(&base, revision.pushed_at, &revision.head, no_tip_holds).unwrap();
                (tip.commit[..7].to_string(), tip.source)
            })
            .collect()
    };
    // `master`'s push that merged it went from 5e10274 to the merge commit, 8b14e8d.
    let squashed = merged_otherwise(|commit| {
        commit["parents"]["nodes"] =
            serde_json::json!([{"oid": "5e10274e350b5c20407dcfa6d42be6971242b377"}]);
    });
    let before = ("5e10274".to_string(), BaseTipSource::BeforeMerge);
    assert_eq!(bases(&squashed), [before.clone(), before]);

    // Fast-forwarded: its merge commit is its head, and `master` now holds every revision.
    // `master`'s pushes read reach back to when they were pushed, so each base is exact.
    let mut fast_forwarded = merged_otherwise(|commit| {
        commit["oid"] = "31f36c873e1065755830f947c05db158348d586d".into();
        commit["parents"]["nodes"] =
            serde_json::json!([{"oid": "4feccf0ae8840cad866647eaf76127bab756ccfd"}]);
    });
    assert_eq!(
        bases(&fast_forwarded),
        [
            ("d76b4e3".to_string(), BaseTipSource::Activity),
            ("5e10274".to_string(), BaseTipSource::Activity),
        ]
    );
    // Without them, today's tip would make every revision's diff empty: it's uncertain.
    fast_forwarded.base_activity.clear();
    let uncertain = (sha(9)[..7].to_string(), BaseTipSource::Uncertain);
    assert_eq!(bases(&fast_forwarded), [uncertain.clone(), uncertain]);
}

/// e2e#14's recorded answers, with `main`'s pushes answered by `answers` in turn: for the read
/// without an ETag, then for the one with it.
fn with_main_answers(first: Interaction, again: Option<Interaction>) -> Vec<Interaction> {
    let (_, recorded) =
        cassette::read_cassette(&cassette::fixtures_dir().join("push_history_e2e_14")).unwrap();
    let mut interactions = Vec::new();
    for (_, interaction) in recorded {
        let main = interaction.request.url.contains("heads%2Fmain");
        match (main, interaction.request.if_none_match.is_some()) {
            (true, false) => interactions.push(first.clone()),
            (true, true) => interactions.extend(again.clone()),
            (false, _) => interactions.push(interaction),
        }
    }
    interactions
}

/// The recorded first read of `main`'s pushes, answered `status` with `body` instead.
fn main_answered(status: u16, headers: &[(&str, &str)], body: serde_json::Value) -> Interaction {
    let (_, recorded) =
        cassette::read_cassette(&cassette::fixtures_dir().join("push_history_e2e_14")).unwrap();
    let mut interaction = (recorded.into_iter().map(|(_, interaction)| interaction))
        .find(|interaction| interaction.request.url.contains("heads%2Fmain"))
        .unwrap();
    interaction.response = RecordedResponse {
        status,
        headers: (headers.iter())
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect(),
        body: Some(body),
        body_text: None,
    };
    interaction
}

#[test]
fn a_branch_whose_pushes_fail_is_read_again_and_the_rest_is_kept() {
    use github::poll::CachedRead as _;

    let bad_gateway = main_answered(502, &[], serde_json::json!({"message": "Bad Gateway"}));
    let (_, recorded) =
        cassette::read_cassette(&cassette::fixtures_dir().join("push_history_e2e_14")).unwrap();
    let main = (recorded.into_iter().map(|(_, interaction)| interaction))
        .find(|interaction| interaction.request.url.contains("heads%2Fmain"))
        .unwrap();
    let mut interactions = with_main_answers(bad_gateway.clone(), None);
    // Read again, `main` answers.
    interactions.push(main.clone());
    let http = Arc::new(ReplayHttpClient::from_interactions("failed", interactions).unwrap());
    let client = client(http);

    let failed = block_on(client.push_history(&e2e(14), BOT, None)).unwrap();

    // GraphQL's answer and the head branch's pushes are all there, so are the revisions.
    assert!(!failed.is_complete());
    let main_activity = &failed.base_activity[0];
    assert_eq!(
        main_activity.failed.as_deref(),
        Some("GitHub answered 502: Bad Gateway")
    );
    assert!(main_activity.entries().is_empty() && main_activity.unavailable.is_none());
    assert_eq!(revisions(&failed, &[]).len(), 6);
    assert!(failed.last_review().is_some());

    let again = block_on(client.push_history(&e2e(14), BOT, Some(&failed))).unwrap();
    assert!(again.is_complete());
    assert_eq!(again.base_activity[0].entries().len(), 5);

    // A read that fails after one that worked keeps the pushes it read.
    let interactions = with_main_answers(main, Some(bad_gateway_for_etag(&again)));
    let http = Arc::new(ReplayHttpClient::from_interactions("failed_later", interactions).unwrap());
    let client = self::client(http);
    let first = block_on(client.push_history(&e2e(14), BOT, None)).unwrap();
    let later = block_on(client.push_history(&e2e(14), BOT, Some(&first))).unwrap();
    assert!(!later.is_complete());
    assert_eq!(later.base_activity[0].entries().len(), 5);
    assert!(later.base_activity[0].failed.is_some());
}

/// A 502 for the conditional read of `main`'s first page, as `known` read it.
fn bad_gateway_for_etag(known: &PushHistory) -> Interaction {
    let page = &known.base_activity[0].pages[0];
    let mut interaction = main_answered(502, &[], serde_json::json!({"message": "Bad Gateway"}));
    interaction.request.if_none_match = page.etag.clone();
    interaction
}

#[test]
fn a_base_branch_behind_single_sign_on_is_unavailable_with_its_link() {
    let sso = main_answered(
        403,
        &[(
            "x-github-sso",
            "required; url=https://github.com/orgs/acme/sso?authorization_request=REDACTED",
        )],
        serde_json::json!({"message": "Resource protected by organization SAML enforcement."}),
    );
    let http =
        Arc::new(ReplayHttpClient::from_interactions("sso", with_main_answers(sso, None)).unwrap());

    let history = block_on(client(http).push_history(&e2e(14), BOT, None)).unwrap();

    let main = &history.base_activity[0];
    assert_eq!(
        main.unavailable.as_deref(),
        Some("Resource protected by organization SAML enforcement.")
    );
    assert_eq!(
        main.help_url.as_deref(),
        Some("https://github.com/orgs/acme/sso?authorization_request=REDACTED")
    );
    assert!(main.failed.is_none());
}

// Edits of e2e#14's answers.

/// e2e#14's first read, each answer as `edit` leaves it, with `extra` answers added.
fn e2e_14_with(
    edit: impl Fn(&mut Interaction),
    extra: Vec<Interaction>,
) -> (PushHistory, Arc<ReplayHttpClient>) {
    let (_, recorded) =
        cassette::read_cassette(&cassette::fixtures_dir().join("push_history_e2e_14")).unwrap();
    let mut interactions: Vec<Interaction> = (recorded.into_iter())
        .map(|(_, interaction)| interaction)
        .filter(|interaction| interaction.request.if_none_match.is_none())
        .take(3)
        .collect();
    interactions.iter_mut().for_each(edit);
    interactions.extend(extra);
    let http =
        Arc::new(ReplayHttpClient::from_interactions("e2e_14_edited", interactions).unwrap());
    let history = block_on(client(http.clone()).push_history(&e2e(14), BOT, None)).unwrap();
    (history, http)
}

fn graphql_pull(interaction: &mut Interaction) -> Option<&mut serde_json::Value> {
    interaction.request.operation.as_ref()?;
    Some(&mut interaction.response.body.as_mut().unwrap()["data"]["repository"]["pullRequest"])
}

fn head_pushes(interaction: &mut Interaction) -> Option<&mut Vec<serde_json::Value>> {
    if !interaction.request.url.contains("revisions-3") {
        return None;
    }
    interaction.response.body.as_mut().unwrap().as_array_mut()
}

const R4: &str = "28b21f232a7ed2ac8682a792045dcbb49a80183f";
const R5: &str = "7513db38608b5bdda9fb488daf00b31c846c1dc1";

#[test]
fn e1s_full_run_has_a_complete_log() {
    let (history, _, _) = read("push_history_e2e_14", &e2e(14));
    assert!(history.complete_log());
    assert_eq!(history.unavailable_heads(), [] as [String; 0]);
}

#[test]
fn a_force_push_whose_head_github_lost_is_named_by_the_issue_timeline() {
    // The fork is gone and GitHub lost r5: only the issue timeline names it.
    let timeline = Interaction {
        request: RecordedRequest {
            method: "GET".into(),
            url: "https://api.github.com/repos/rozed-dev/e2e/issues/14/timeline?per_page=100"
                .into(),
            ..Default::default()
        },
        response: RecordedResponse {
            status: 200,
            headers: Default::default(),
            body: Some(serde_json::json!([
                {"event": "commented", "created_at": "2026-11-06T19:28:00Z"},
                {"event": "head_ref_force_pushed", "commit_id": R5,
                 "created_at": "2026-11-06T19:28:48Z"},
            ])),
            body_text: None,
        },
    };
    let (history, _) = e2e_14_with(
        |interaction| {
            if let Some(pull) = graphql_pull(interaction) {
                pull["headRepository"] = serde_json::Value::Null;
                pull["timelineItems"]["nodes"][0]["afterCommit"] = serde_json::Value::Null;
            }
        },
        vec![timeline],
    );

    assert_eq!(history.unavailable_heads(), [R5.to_string()]);
    let mut revisions = revisions(&history, &[]);
    assert_eq!(
        short(&revisions),
        ["2e2d4be", "28b21f2", "7513db3", "adb7208"]
    );
    assert_eq!(revisions.get(R5).unwrap().forced, Some(true));
    // The caller shows it as no longer available.
    for head in history.unavailable_heads() {
        revisions.mark_unavailable(head);
    }
    assert!(!revisions.get(R5).unwrap().available);
}

#[test]
fn a_review_on_a_commit_the_complete_log_never_had_as_its_head_is_marked() {
    // r5's first commit, which came in r5's push, was never the head.
    let (history, _) = e2e_14_with(
        |interaction| {
            if let Some(pull) = graphql_pull(interaction) {
                let commit = pull["commits"]["nodes"][0]["commit"]["oid"].clone();
                pull["reviews"]["nodes"][0]["commit"]["oid"] = commit;
            }
        },
        Vec::new(),
    );

    assert!(history.complete_log());
    let revisions = revisions(&history, &[]);
    let reviewed = history.reviews[0].commit.as_deref().unwrap();
    assert_eq!(
        revisions.get(reviewed).unwrap().sources,
        [Source::ReviewedCommit].into()
    );
    assert!(!revisions.get(R4).unwrap().sources.contains(&Source::Review));
}

#[test]
fn pushes_after_the_pull_request_closed_belong_to_whatever_reused_the_branch() {
    let (history, _) = e2e_14_with(
        |interaction| {
            if let Some(pushes) = head_pushes(interaction) {
                let mut later = pushes[0].clone();
                later["id"] = 1.into();
                later["before"] = pushes[0]["after"].clone();
                later["after"] = sha(7).into();
                later["timestamp"] = "2026-11-07T09:00:00Z".into();
                pushes.insert(0, later);
            }
        },
        Vec::new(),
    );

    let revisions = revisions(&history, &[]);
    assert!(revisions.get(&sha(7)).is_none());
    assert_eq!(revisions.len(), 6);
}

#[test]
fn check_suites_on_another_branch_name_no_heads() {
    let (history, _) = e2e_14_with(
        |interaction| {
            if let Some(pull) = graphql_pull(interaction) {
                pull["headRepository"] = serde_json::Value::Null;
                let commit = pull["commits"]["nodes"][0]["commit"]["oid"].clone();
                pull["commits"]["nodes"][0]["commit"]["checkSuites"]["nodes"] = serde_json::json!([
                    {"createdAt": "2026-11-06T19:28:50Z", "branch": {"name": "backport"},
                     "push": {"previousSha": sha(3), "nextSha": commit}},
                    {"createdAt": "2026-11-06T19:28:51Z", "branch": {"name": "backport"},
                     "push": null},
                ]);
            }
        },
        Vec::new(),
    );

    let commit = history.check_suites[0].commit.clone();
    assert_eq!(history.check_suites.len(), 2);
    let records = history.push_records(&[]);
    assert!(!records.iter().any(|record| matches!(
        record,
        review::revisions::PushRecord::CheckSuite { next, .. } if *next == commit
    )));
}

#[test]
fn every_branch_the_pull_request_targeted_is_read_back_to_its_first_revision() {
    let old_base = "https://api.github.com/repos/rozed-dev/e2e/activity?ref=refs%2Fheads%2Fe1%2Fold-base&direction=desc&per_page=100";
    let second = format!("{old_base}&after=C1");
    // r1 was pushed at 19:27:50. The old base's first page is all later pushes, so its second
    // page is read too, which reaches back far enough; a third would never be asked for.
    let old_pages = vec![
        page(
            old_base,
            "\"o1\"",
            Some(&second),
            &[serde_json::json!({
                "id": 3, "before": sha(2), "after": sha(3), "activity_type": "push",
                "timestamp": "2026-11-06T19:28:00Z", "actor": null
            })],
        ),
        page(
            &second,
            "\"o2\"",
            Some(&format!("{old_base}&after=C2")),
            &[serde_json::json!({
                "id": 2, "before": sha(1), "after": sha(2), "activity_type": "push",
                "timestamp": "2026-11-06T19:20:00Z", "actor": null
            })],
        ),
    ];
    let (history, http) = e2e_14_with(
        |interaction| {
            if let Some(pull) = graphql_pull(interaction) {
                pull["timelineItems"]["nodes"]
                    .as_array_mut()
                    .unwrap()
                    .push(serde_json::json!({
                        "__typename": "BaseRefChangedEvent",
                        "createdAt": "2026-11-06T19:28:30Z",
                        "previousRefName": "e1/old-base",
                        "currentRefName": "main",
                    }));
            }
        },
        old_pages,
    );

    let branches: Vec<&str> = history
        .base_activity
        .iter()
        .map(|a| a.branch.as_str())
        .collect();
    assert_eq!(branches, ["main", "e1/old-base"]);
    assert_eq!(history.base_activity[1].pages.len(), 2);
    assert!(!http.seen().iter().any(|seen| seen.url.contains("after=C2")));
    // r1 targeted the old base, at its tip then.
    let base = history.base_history(None);
    let r1 = revisions(&history, &[]).as_slice()[0].clone();
    assert_eq!(
        base_tip_at(&base, r1.pushed_at, &r1.head, no_tip_holds)
            .unwrap()
            .commit,
        sha(2)
    );
}
