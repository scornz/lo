//! Revisions and comparisons for whole pull requests, from push records as GitHub's APIs and Lo's
//! poller report them, modeled on real pull requests. SHAs are shortened to 8 characters.

use std::time::{Duration, SystemTime};

use review::revisions::{
    Comparison, FileComparison, PushRecord, PushedAt, Revisions, Source, ViewedState,
    build_revisions, default_comparison, viewed_file_comparison,
};

/// GitHub's SHA for "no commit", the `before` of a push that creates a branch.
const NO_COMMIT: &str = "0000000000000000000000000000000000000000";

/// A UTC time as GitHub writes it, such as `2026-10-19T09:30:40Z`.
fn time(iso: &str) -> SystemTime {
    let field = |from: usize, to: usize| iso[from..to].parse::<i64>().unwrap();
    let (year, month, day) = (field(0, 4), field(5, 7), field(8, 10));
    // Days since 1970-01-01, by Howard Hinnant's `days_from_civil`.
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    let seconds = days * 86_400 + field(11, 13) * 3_600 + field(14, 16) * 60 + field(17, 19);
    SystemTime::UNIX_EPOCH + Duration::from_secs(seconds.try_into().unwrap())
}

fn activity(before: &str, after: &str, at: &str, forced: bool) -> PushRecord {
    PushRecord::Activity {
        before: Some(before.to_string()),
        after: after.to_string(),
        at: time(at),
        forced,
    }
}

fn force_push(before: &str, after: &str, at: &str) -> PushRecord {
    PushRecord::ForcePush {
        before: Some(before.to_string()),
        after: after.to_string(),
        at: time(at),
    }
}

fn check_suite(previous: &str, next: &str, at: &str) -> PushRecord {
    PushRecord::CheckSuite {
        previous: Some(previous.to_string()),
        next: next.to_string(),
        at: time(at),
    }
}

fn review(commit: &str, at: &str) -> PushRecord {
    PushRecord::Review {
        commit: commit.to_string(),
        at: time(at),
    }
}

fn polled(head: &str, at: &str) -> PushRecord {
    PushRecord::Polled {
        head: head.to_string(),
        at: time(at),
    }
}

fn commits(shas: &[&str]) -> Vec<String> {
    shas.iter().map(|sha| sha.to_string()).collect()
}

fn heads(revisions: &Revisions) -> Vec<&str> {
    revisions
        .iter()
        .map(|revision| revision.head.as_str())
        .collect()
}

fn previous(revisions: &Revisions) -> Vec<Option<&str>> {
    revisions
        .iter()
        .map(|revision| revision.previous.as_deref())
        .collect()
}

fn forced(revisions: &Revisions) -> Vec<Option<bool>> {
    revisions.iter().map(|revision| revision.forced).collect()
}

fn between(from: &str, to: &str) -> Comparison {
    Comparison::Between {
        from: from.to_string(),
        to: to.to_string(),
    }
}

// Normal pushes only: six pushes to a branch in the base repository, each starting CI. The
// current commits hold every head.

const NORMAL_OPENED: &str = "2026-10-02T15:06:30Z";

const NORMAL_HEADS: [&str; 6] = [
    "3f2a9c1e", "8b7d4e20", "c41f6a93", "1e9b0d57", "a6c3e8f1", "57d2b4c0",
];

fn normal_commits() -> Vec<String> {
    let [r1, r2, r3, r4, r5, r6] = NORMAL_HEADS;
    commits(&[
        "0a1b2c3d", r1, "4e5f6a7b", r2, r3, "9c8d7e6f", "2b3c4d5e", r4, "6f7a8b9c", r5, r6,
    ])
}

#[test]
fn normal_pushes_from_the_activity_api() {
    let [r1, r2, r3, r4, r5, r6] = NORMAL_HEADS;
    let records = [
        activity(NO_COMMIT, r1, "2026-10-02T15:04:11Z", false),
        check_suite(NO_COMMIT, r1, "2026-10-02T15:04:19Z"),
        activity(r1, r2, "2026-10-02T17:40:52Z", false),
        check_suite(r1, r2, "2026-10-02T17:41:03Z"),
        activity(r2, r3, "2026-10-03T09:12:30Z", false),
        check_suite(r2, r3, "2026-10-03T09:12:37Z"),
        activity(r3, r4, "2026-10-05T11:58:02Z", false),
        check_suite(r3, r4, "2026-10-05T11:58:44Z"),
        activity(r4, r5, "2026-10-06T08:21:45Z", false),
        check_suite(r4, r5, "2026-10-06T08:21:51Z"),
        polled(r5, "2026-10-06T08:22:30Z"),
        activity(r5, r6, "2026-10-06T14:07:19Z", false),
        check_suite(r5, r6, "2026-10-06T14:07:26Z"),
        polled(r6, "2026-10-06T14:08:00Z"),
    ];
    let revisions = build_revisions(&records, &normal_commits(), r6, time(NORMAL_OPENED));

    assert_eq!(heads(&revisions), NORMAL_HEADS);
    assert_eq!(
        previous(&revisions),
        [None, Some(r1), Some(r2), Some(r3), Some(r4), Some(r5)]
    );
    assert_eq!(
        forced(&revisions),
        [
            None,
            Some(false),
            Some(false),
            Some(false),
            Some(false),
            Some(false)
        ]
    );
    assert_eq!(
        revisions.as_slice()[3].pushed_at,
        PushedAt::Exact(time("2026-10-05T11:58:02Z"))
    );
    assert_eq!(
        revisions.latest().unwrap().sources,
        [
            Source::Activity,
            Source::CheckSuite,
            Source::Poller,
            Source::Head
        ]
        .into()
    );
    assert_eq!(revisions.label(r4).as_deref(), Some("r4"));
}

#[test]
fn normal_pushes_from_check_suites_when_the_activity_api_is_unavailable() {
    let [r1, r2, r3, r4, r5, r6] = NORMAL_HEADS;
    // Listed in the order GitHub returns them: by commit, not by time.
    let records = [
        check_suite(r5, r6, "2026-10-06T14:07:26Z"),
        check_suite(r3, r4, "2026-10-05T11:58:44Z"),
        check_suite(NO_COMMIT, r1, "2026-10-02T15:04:19Z"),
        check_suite(r4, r5, "2026-10-06T08:21:51Z"),
        check_suite(r1, r2, "2026-10-02T17:41:03Z"),
        check_suite(r2, r3, "2026-10-03T09:12:37Z"),
    ];
    let revisions = build_revisions(&records, &normal_commits(), r6, time(NORMAL_OPENED));

    assert_eq!(heads(&revisions), NORMAL_HEADS);
    assert_eq!(
        previous(&revisions),
        [None, Some(r1), Some(r2), Some(r3), Some(r4), Some(r5)]
    );
    assert_eq!(
        forced(&revisions),
        [
            None,
            Some(false),
            Some(false),
            Some(false),
            Some(false),
            Some(false)
        ]
    );
    assert_eq!(
        revisions.get(r4).unwrap().pushed_at,
        PushedAt::By(time("2026-10-05T11:58:44Z"))
    );
}

// Force-pushes: a branch opened, force-pushed, pushed once normally, then force-pushed twice; the
// last is GitHub rebasing the branch after retargeting the pull request. Modeled on cli/cli#14745.

const FORCE_PUSH_OPENED: &str = "2026-10-19T09:35:48Z";

const R1: &str = "ca3cb242";
const R2: &str = "a842bc7a";
const R3: &str = "aae5f981";
const R4: &str = "17e9abeb";
const R5: &str = "21660dd7";

/// The current commits: the rebase replaced every earlier head.
fn rebased_commits() -> Vec<String> {
    commits(&["9e1b2c3d", "4f5a6b7c", R5])
}

fn force_push_activity() -> Vec<PushRecord> {
    vec![
        activity(NO_COMMIT, R1, "2026-10-19T09:30:40Z", false),
        activity(R1, R2, "2026-10-19T12:37:13Z", true),
        activity(R2, R3, "2026-10-19T12:52:34Z", false),
        activity(R3, R4, "2026-10-21T09:34:20Z", true),
        activity(R4, R5, "2026-10-21T11:45:24Z", true),
    ]
}

/// The timeline and check suites: force-push events a few seconds after each force-push, reviews
/// on r1, r2 and r3, and the one check suite whose push the head repository started.
fn force_push_timeline() -> Vec<PushRecord> {
    vec![
        review(R1, "2026-10-19T10:12:05Z"),
        force_push(R1, R2, "2026-10-19T12:37:15Z"),
        review(R2, "2026-10-19T12:45:30Z"),
        check_suite(R2, R3, "2026-10-19T12:52:51Z"),
        review(R3, "2026-10-19T13:05:47Z"),
        force_push(R3, R4, "2026-10-21T09:34:22Z"),
        force_push(R4, R5, "2026-10-21T11:45:26Z"),
        polled(R5, "2026-11-06T10:00:00Z"),
    ]
}

#[test]
fn force_pushes_from_every_source() {
    let mut records = force_push_activity();
    records.extend(force_push_timeline());
    let revisions = build_revisions(&records, &rebased_commits(), R5, time(FORCE_PUSH_OPENED));

    assert_eq!(heads(&revisions), [R1, R2, R3, R4, R5]);
    assert_eq!(
        previous(&revisions),
        [None, Some(R1), Some(R2), Some(R3), Some(R4)]
    );
    assert_eq!(
        forced(&revisions),
        [None, Some(true), Some(false), Some(true), Some(true)]
    );
    assert_eq!(
        revisions.get(R2).unwrap().pushed_at,
        PushedAt::Exact(time("2026-10-19T12:37:13Z"))
    );
    assert_eq!(
        revisions.get(R3).unwrap().sources,
        [
            Source::Activity,
            Source::ForcePush,
            Source::Review,
            Source::CheckSuite
        ]
        .into()
    );
}

#[test]
fn force_pushes_without_the_activity_api() {
    let revisions = build_revisions(
        &force_push_timeline(),
        &rebased_commits(),
        R5,
        time(FORCE_PUSH_OPENED),
    );

    assert_eq!(heads(&revisions), [R1, R2, R3, R4, R5]);
    assert_eq!(
        previous(&revisions),
        [None, Some(R1), Some(R2), Some(R3), Some(R4)]
    );
    // Nothing says r1 and r3 weren't forced, and the rebase took both out of the current commits.
    assert_eq!(
        forced(&revisions),
        [None, Some(true), None, Some(true), Some(true)]
    );
    assert_eq!(
        revisions.get(R1).unwrap().pushed_at,
        PushedAt::By(time("2026-10-19T10:12:05Z"))
    );
    assert_eq!(
        revisions.get(R3).unwrap().pushed_at,
        PushedAt::By(time("2026-10-19T12:52:51Z"))
    );
}

#[test]
fn a_normal_push_only_a_force_push_event_saw() {
    // Without the review or the check suite, r3 is only the head the second force-push replaced.
    let revisions = build_revisions(
        &[
            force_push(R1, R2, "2026-10-19T12:37:15Z"),
            force_push(R3, R4, "2026-10-21T09:34:22Z"),
            force_push(R4, R5, "2026-10-21T11:45:26Z"),
            polled(R5, "2026-11-06T10:00:00Z"),
        ],
        &rebased_commits(),
        R5,
        time(FORCE_PUSH_OPENED),
    );

    assert_eq!(heads(&revisions), [R1, R2, R3, R4, R5]);
    let r3 = revisions.get(R3).unwrap();
    assert_eq!(r3.pushed_at, PushedAt::By(time("2026-10-21T09:34:22Z")));
    assert_eq!(r3.sources, [Source::ForcePush].into());
    assert_eq!(r3.previous.as_deref(), Some(R2));
}

#[test]
fn a_review_on_a_commit_since_force_pushed_away() {
    let mut records = force_push_activity();
    records.extend(force_push_timeline());
    let mut revisions = build_revisions(&records, &rebased_commits(), R5, time(FORCE_PUSH_OPENED));

    // You approved r3; the author has force-pushed twice since.
    assert_eq!(
        default_comparison(&revisions, Some(R3), None),
        between(R3, R5)
    );
    // You've seen r5 here since, without reviewing it again.
    assert_eq!(
        default_comparison(&revisions, Some(R3), Some(R5)),
        Comparison::Full {
            nothing_new_since: Some(R5.to_string())
        }
    );
    // A file you marked viewed at r2 on this Mac, and one marked viewed elsewhere.
    assert_eq!(
        viewed_file_comparison(&revisions, ViewedState::Dismissed, Some(R2), Some(R3)),
        Some(FileComparison::SinceViewed {
            from: R2.to_string(),
            to: R5.to_string()
        })
    );
    assert_eq!(
        viewed_file_comparison(&revisions, ViewedState::Dismissed, None, Some(R3)),
        Some(FileComparison::SinceLastReview {
            from: R3.to_string(),
            to: R5.to_string()
        })
    );
    assert_eq!(
        viewed_file_comparison(&revisions, ViewedState::Viewed, None, Some(R3)),
        None
    );

    // GitHub no longer has r3, and Lo never fetched it.
    assert!(revisions.mark_unavailable(R3));
    assert_eq!(heads(&revisions), [R1, R2, R3, R4, R5]);
    assert_eq!(
        default_comparison(&revisions, Some(R3), None),
        between(R2, R5)
    );
}

// Pushes before the pull request opened: the author pushed a branch three times, each starting
// CI, then opened the pull request and pushed twice more. The check suites on the current commits
// include the three pushes from before the pull request existed.

const EARLY: [&str; 5] = ["2c4e6a8b", "3d5f7b9c", "4e6a8c0d", "5f7b9d1e", "6a8c0e2f"];

#[test]
fn pushes_before_the_pull_request_opened() {
    let [e1, e2, e3, p1, p2] = EARLY;
    let records = [
        check_suite(NO_COMMIT, e1, "2026-11-02T09:01:12Z"),
        check_suite(e1, e2, "2026-11-02T09:40:31Z"),
        check_suite(e2, e3, "2026-11-02T10:15:09Z"),
        polled(e3, "2026-11-02T10:20:00Z"),
        check_suite(e3, p1, "2026-11-02T14:03:44Z"),
        check_suite(p1, p2, "2026-11-03T08:47:20Z"),
    ];
    let revisions = build_revisions(&records, &commits(&EARLY), p2, time("2026-11-02T10:18:02Z"));

    assert_eq!(heads(&revisions), [e3, p1, p2]);
    assert_eq!(previous(&revisions), [Some(e2), Some(e3), Some(p1)]);
    assert_eq!(forced(&revisions), [Some(false); 3]);
}

// A review submitted after a force-push, on a head that push replaced, on a fork pull request
// with no activity log and no check suites. The author pushed X and then Y, both unseen, then
// force-pushed Z over Y; Lo first saw the pull request at Z. A review started on X was
// submitted after that.

const X: &str = "5e4d3c2b";
const Y: &str = "9a8b7c6d";
const Z: &str = "c0ffee12";

#[test]
fn a_review_submitted_after_a_force_push_on_a_head_it_replaced() {
    let records = [
        force_push(Y, Z, "2026-10-30T16:02:41Z"),
        polled(Z, "2026-10-30T16:10:00Z"),
        review(X, "2026-10-30T16:25:18Z"),
    ];
    let revisions = build_revisions(
        &records,
        &commits(&["7b6a5948", Z]),
        Z,
        time("2026-10-29T11:00:00Z"),
    );

    assert_eq!(revisions.latest().unwrap().head, Z);
    assert_eq!(default_comparison(&revisions, Some(X), None), between(X, Z));
    assert_eq!(
        viewed_file_comparison(&revisions, ViewedState::Dismissed, Some(Y), Some(X)),
        Some(FileComparison::SinceViewed {
            from: Y.to_string(),
            to: Z.to_string()
        })
    );
}

// A rebase on a fork pull request from before the activity API's history: no activity log and no
// check suites. The author pushed a fix, rebased onto the moved base branch adding a commit, then
// pushed once more.

const OPENED: &str = "b7e4d2a1";
const FIXED: &str = "c81f09e3";
const REBASED: &str = "f4a7b3c2";
const LATEST: &str = "0d9e8f7a";

#[test]
fn a_rebase() {
    let records = [
        polled(OPENED, "2022-11-14T16:20:05Z"),
        polled(FIXED, "2022-11-15T10:03:41Z"),
        review(FIXED, "2022-11-15T11:30:12Z"),
        force_push(FIXED, REBASED, "2022-11-17T09:15:58Z"),
        polled(LATEST, "2022-11-17T14:47:20Z"),
    ];
    let current = commits(&["e1d2c3b4", "a5b6c7d8", "9f8e7d6c", REBASED, LATEST]);
    let revisions = build_revisions(&records, &current, LATEST, time("2022-11-14T16:19:40Z"));

    assert_eq!(heads(&revisions), [OPENED, FIXED, REBASED, LATEST]);
    assert_eq!(
        previous(&revisions),
        [None, Some(OPENED), Some(FIXED), Some(REBASED)]
    );
    assert_eq!(forced(&revisions), [None, None, Some(true), Some(false)]);

    // Your review was on the fix, before the rebase.
    assert_eq!(
        default_comparison(&revisions, Some(FIXED), None),
        between(FIXED, LATEST)
    );
    assert_eq!(
        default_comparison(&revisions, Some(FIXED), Some(REBASED)),
        between(REBASED, LATEST)
    );
}

// Pushes only the poller saw: a fork pull request whose fork was deleted and that has no CI and no
// reviews yet, open in Lo throughout.

const POLLED_OPENED: &str = "2026-09-08T08:00:00Z";

const P1: &str = "d3c2b1a0";
const P2: &str = "7a6b5c4d";
const P3: &str = "e9f8a7b6";
const P4: &str = "5c4b3a29";

fn polled_records() -> Vec<PushRecord> {
    vec![
        polled(P1, "2026-09-08T08:01:30Z"),
        polled(P2, "2026-09-08T09:46:00Z"),
        polled(P3, "2026-09-09T13:12:30Z"),
    ]
}

#[test]
fn pushes_only_the_poller_saw() {
    let current = commits(&["1a2b3c4d", P1, "5e6f7a8b", P2, P3]);
    let revisions = build_revisions(&polled_records(), &current, P3, time(POLLED_OPENED));

    assert_eq!(heads(&revisions), [P1, P2, P3]);
    assert_eq!(previous(&revisions), [None, Some(P1), Some(P2)]);
    assert_eq!(forced(&revisions), [None, Some(false), Some(false)]);
    assert_eq!(
        revisions.get(P2).unwrap().pushed_at,
        PushedAt::By(time("2026-09-08T09:46:00Z"))
    );
    assert_eq!(
        default_comparison(&revisions, None, Some(P2)),
        between(P2, P3)
    );
}

#[test]
fn a_force_push_only_the_poller_saw() {
    let mut records = polled_records();
    records.push(polled(P4, "2026-09-10T07:30:00Z"));
    // The author squashed everything into one commit.
    let revisions = build_revisions(&records, &commits(&[P4]), P4, time(POLLED_OPENED));

    assert_eq!(heads(&revisions), [P1, P2, P3, P4]);
    assert_eq!(forced(&revisions), [None, None, None, Some(true)]);
}

// Conflicting sources, on a fork pull request without the activity API: a review started on the
// first head and submitted after the force-push, a force-push event three seconds after the push,
// and a check suite for the same commit pushed to a backport branch, which names another previous
// head.

const BEFORE: &str = "6b5a4938";
const AFTER: &str = "f0e1d2c3";

fn conflicting_records() -> Vec<PushRecord> {
    vec![
        polled(BEFORE, "2026-10-27T13:40:55Z"),
        review(BEFORE, "2026-10-27T14:05:31Z"),
        force_push(BEFORE, AFTER, "2026-10-27T14:02:13Z"),
        polled(AFTER, "2026-10-27T14:03:00Z"),
        check_suite("e5f6a7b8", AFTER, "2026-10-27T14:20:42Z"),
    ]
}

fn build_conflicting(records: &[PushRecord]) -> Revisions {
    build_revisions(
        records,
        &commits(&["0f1e2d3c", AFTER]),
        AFTER,
        time("2026-10-27T13:40:40Z"),
    )
}

#[test]
fn conflicting_sources() {
    let revisions = build_conflicting(&conflicting_records());

    assert_eq!(heads(&revisions), [BEFORE, AFTER]);
    let after = revisions.latest().unwrap();
    assert_eq!(after.previous.as_deref(), Some(BEFORE));
    assert_eq!(after.forced, Some(true));
    assert_eq!(
        after.pushed_at,
        PushedAt::Exact(time("2026-10-27T14:02:13Z"))
    );
    assert_eq!(
        revisions.get(BEFORE).unwrap().pushed_at,
        PushedAt::By(time("2026-10-27T13:40:55Z"))
    );
}

#[test]
fn conflicting_sources_place_a_review_submitted_after_the_next_push() {
    // Without the poller's sighting, the review alone would put the first head after the second.
    let records: Vec<_> = (conflicting_records().into_iter())
        .filter(|record| !matches!(record, PushRecord::Polled { head, .. } if head == BEFORE))
        .collect();
    let revisions = build_conflicting(&records);

    assert_eq!(heads(&revisions), [BEFORE, AFTER]);
    assert_eq!(
        revisions.get(BEFORE).unwrap().pushed_at,
        PushedAt::By(time("2026-10-27T14:02:13Z"))
    );
}

#[test]
fn conflicting_sources_trust_the_activity_api_first() {
    let mut records = conflicting_records();
    records.push(activity(NO_COMMIT, BEFORE, "2026-10-27T13:40:30Z", false));
    records.push(activity(BEFORE, AFTER, "2026-10-27T14:02:10Z", true));
    let revisions = build_conflicting(&records);

    assert_eq!(heads(&revisions), [BEFORE, AFTER]);
    assert_eq!(
        revisions.latest().unwrap().pushed_at,
        PushedAt::Exact(time("2026-10-27T14:02:10Z"))
    );
    assert_eq!(
        revisions.get(BEFORE).unwrap().pushed_at,
        PushedAt::Exact(time("2026-10-27T13:40:30Z"))
    );
}
