//! Building revisions stays fast on very long push histories. Release builds only, since debug
//! builds are several times slower: `cargo test -p review --release --test revisions_scale`.
#![cfg(not(debug_assertions))]

use std::time::{Duration, Instant, SystemTime};

use review::revisions::{PushRecord, build_revisions, default_comparison};

const HEADS: usize = 20_000;
const LIMIT: Duration = Duration::from_millis(100);

fn at(seconds: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000 + seconds)
}

fn sha(index: usize) -> String {
    format!("{index:040x}")
}

/// Builds the revisions, picks the default comparison and numbers every revision, within the
/// limit.
fn assert_fast(records: &[PushRecord], commits: &[String]) {
    let start = Instant::now();
    let revisions = build_revisions(records, commits, &sha(HEADS), at(0));
    default_comparison(&revisions, Some(&sha(HEADS / 2)), Some(&sha(HEADS / 3)));
    for revision in revisions.iter() {
        assert!(revisions.number(&revision.head).is_some());
    }
    let elapsed = start.elapsed();

    assert_eq!(revisions.len(), HEADS);
    assert!(elapsed < LIMIT, "took {elapsed:?}");
}

#[test]
fn normal_pushes_each_with_ten_check_suites() {
    let mut records = Vec::new();
    for index in 1..=HEADS {
        let before = Some(sha(index - 1));
        let at = |seconds| at(index as u64 * 60 + seconds);
        records.push(PushRecord::Activity {
            before: before.clone(),
            after: sha(index),
            at: at(0),
            forced: false,
        });
        for seconds in 0..10 {
            records.push(PushRecord::CheckSuite {
                previous: before.clone(),
                next: sha(index),
                at: at(5 + seconds),
            });
        }
    }
    let commits: Vec<String> = (1..=HEADS).map(sha).collect();

    assert_fast(&records, &commits);
}

#[test]
fn heads_only_the_poller_saw_newest_first() {
    let records: Vec<_> = (1..=HEADS)
        .rev()
        .map(|index| PushRecord::Polled {
            head: sha(index),
            at: at(index as u64 * 60),
        })
        .collect();

    assert_fast(&records, &[sha(HEADS)]);
}

#[test]
fn late_reviews_along_the_current_commits() {
    // Each review came long after the push, so every time comes from the next head's.
    let mut records: Vec<_> = (1..HEADS)
        .map(|index| PushRecord::Review {
            commit: sha(index),
            at: at(10_000_000 + index as u64),
        })
        .collect();
    records.push(PushRecord::Polled {
        head: sha(HEADS),
        at: at(60),
    });
    let commits: Vec<String> = (1..=HEADS).map(sha).collect();

    assert_fast(&records, &commits);
}
