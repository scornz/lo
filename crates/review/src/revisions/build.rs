//! Merging what each source saw into one ordered list of revisions.

use std::collections::{BTreeSet, HashMap};
use std::time::SystemTime;

use super::{PushRecord, PushedAt, Revision, Revisions, Source, sha};

/// Merges push records from every source into the pull request's revisions, one per head SHA.
///
/// `commits` are the pull request's current commits from git, oldest first. `head` is its current
/// head (`headRefOid`), which is always the latest revision, even when no source saw it or a
/// source's time says otherwise.
///
/// - **Deduplicated by head.** A head several sources saw is one revision, listing them all. A
///   head pushed twice (`a` → `b` → `a`) is one revision, placed at its latest push.
/// - **Ordered by the push chain, ancestry and time.** A head comes after the head its push
///   replaced, and among the current commits, after its ancestors. Times order the rest. Times
///   from the activity API, or else a force-push event, are exact. A review, check suite or the
///   poller only says the head was pushed by then, and so does a later head's push, which places a
///   head no source dated, such as a force-push's `before`, just before the push that replaced it.
///   Heads outside the current commits come before the latest force-push when its head is in
///   them, since every push after it was then a fast-forward. Where the chain loops because a head
///   was pushed again, its latest push decides.
/// - **Previous head and forced.** The activity API is trusted first, then force-push events,
///   then check suites. With no source's word, the previous head is the revision before, and
///   ancestry in the current commits tells whether the push was forced when it can.
pub fn build_revisions(records: &[PushRecord], commits: &[String], head: &str) -> Revisions {
    let mut heads = Heads::default();
    for record in records {
        heads.add(record);
    }
    heads.see(head, Source::Head);
    let pull_head = sha(head).and_then(|head| heads.index.get(head).copied());

    let mut positions = HashMap::new();
    for (position, commit) in commits.iter().enumerate() {
        positions.entry(commit.clone()).or_insert(position);
    }
    let after = successors(&heads, &positions, pull_head);
    let latest = latest_push_times(&heads.seen, &after);
    let mut order = order(&latest, &after);
    if let Some(pull_head) = pull_head {
        order.retain(|&index| index != pull_head);
        order.push(pull_head);
    }

    let seen = &heads.seen;
    let list = order
        .iter()
        .enumerate()
        .map(|(position, &index)| {
            let head = &seen[index];
            let previous = head
                .previous()
                .or_else(|| Some(seen[order[position.checked_sub(1)?]].head));
            let forced = (head.forced())
                .or_else(|| forced_by_ancestry(previous?, head.head, &positions, &heads.index));
            Revision {
                head: head.head.to_string(),
                previous: previous.map(str::to_string),
                forced,
                pushed_at: match (head.exact(), latest[index]) {
                    (Some(at), _) => PushedAt::Exact(at),
                    (None, Some(by)) => PushedAt::By(by),
                    (None, None) => PushedAt::Unknown,
                },
                sources: head.sources.clone(),
                available: true,
            }
        })
        .collect();
    Revisions {
        list,
        commits: positions,
    }
}

/// Whether the push from `previous` to `head` was forced, as far as the current commits tell. A
/// push from an ancestor wasn't. A push back to an ancestor was, and so was a push over a revision
/// that's no longer in the current commits, since that one can't be the new head's ancestor.
fn forced_by_ancestry(
    previous: &str,
    head: &str,
    positions: &HashMap<String, usize>,
    revisions: &HashMap<&str, usize>,
) -> Option<bool> {
    let head = *positions.get(head)?;
    match positions.get(previous) {
        Some(&previous) => (previous != head).then_some(previous > head),
        None => revisions.contains_key(previous).then_some(true),
    }
}

/// The heads that must come after each head: the heads whose push replaced it, its nearest
/// descendant among the heads in the current commits, and, for a head outside them, the latest
/// force-push when its head is in them. Nothing comes after the pull request's head.
fn successors(
    heads: &Heads,
    positions: &HashMap<String, usize>,
    pull_head: Option<usize>,
) -> Vec<BTreeSet<usize>> {
    let mut after = vec![BTreeSet::new(); heads.seen.len()];
    let mut precedes = |earlier: usize, later: usize| {
        if earlier != later && Some(earlier) != pull_head {
            after[earlier].insert(later);
        }
    };
    for (index, head) in heads.seen.iter().enumerate() {
        if let Some(previous) = head.previous()
            && let Some(&previous) = heads.index.get(previous)
        {
            precedes(previous, index);
        }
    }
    let mut current: Vec<(usize, usize)> = (heads.seen.iter().enumerate())
        .filter_map(|(index, head)| Some((*positions.get(head.head)?, index)))
        .collect();
    current.sort_unstable();
    for pair in current.windows(2) {
        precedes(pair[0].1, pair[1].1);
    }
    // When the latest force-push's head is a current commit, every push since was a fast-forward,
    // so a head outside the current commits was pushed before it.
    if let Some(forced) = latest_force_push(&heads.seen)
        && positions.contains_key(heads.seen[forced].head)
    {
        for (index, head) in heads.seen.iter().enumerate() {
            if !positions.contains_key(head.head) {
                precedes(index, forced);
            }
        }
    }
    after
}

/// The head of the latest push the activity API or a force-push event says was forced.
fn latest_force_push(seen: &[Seen]) -> Option<usize> {
    (seen.iter().enumerate())
        .filter_map(|(index, head)| {
            let forced = (head.activity)
                .filter(|push| push.forced == Some(true))
                .or(head.force_push)?;
            Some((forced.at, index))
        })
        .max()
        .map(|(_, index)| index)
}

/// The latest each head can have been pushed: its exact time, or else the earliest of its
/// sightings and the times of the heads that must come after it. `None` when nothing bounds it.
fn latest_push_times(seen: &[Seen], after: &[BTreeSet<usize>]) -> Vec<Option<SystemTime>> {
    let mut latest: Vec<_> = seen
        .iter()
        .map(|head| head.exact().or(head.seen_by))
        .collect();
    // Each pass carries times at least one more head back, so this ends within a pass per head.
    loop {
        let mut changed = false;
        for (index, head) in seen.iter().enumerate() {
            if head.exact().is_some() {
                continue;
            }
            for &next in &after[index] {
                if let Some(by) = latest[next]
                    && latest[index].is_none_or(|own| by < own)
                {
                    latest[index] = Some(by);
                    changed = true;
                }
            }
        }
        if !changed {
            return latest;
        }
    }
}

/// The heads in revision order: each after the heads that must come before it, the earliest
/// first. Heads nothing bounds go last, and ties keep the order the records first named them in.
/// Where the constraints loop, the earliest head in the loop goes first.
fn order(latest: &[Option<SystemTime>], after: &[BTreeSet<usize>]) -> Vec<usize> {
    let rank = |index: usize| (latest[index].is_none(), latest[index], index);
    let mut waiting_on = vec![0usize; latest.len()];
    for &next in after.iter().flatten() {
        waiting_on[next] += 1;
    }
    let mut placed = vec![false; latest.len()];
    let mut order = Vec::with_capacity(latest.len());
    while order.len() < latest.len() {
        let unplaced = (0..latest.len()).filter(|&index| !placed[index]);
        let next = (unplaced.clone())
            .filter(|&index| waiting_on[index] == 0)
            .min_by_key(|&index| rank(index))
            .or_else(|| unplaced.min_by_key(|&index| rank(index)))
            .expect("a head is left to place");
        placed[next] = true;
        order.push(next);
        for &later in &after[next] {
            waiting_on[later] -= 1;
        }
    }
    order
}

/// Every head the records name, in the order they first name them.
#[derive(Default)]
struct Heads<'a> {
    seen: Vec<Seen<'a>>,
    index: HashMap<&'a str, usize>,
}

impl<'a> Heads<'a> {
    fn add(&mut self, record: &'a PushRecord) {
        let source = record.source();
        match record {
            PushRecord::Activity {
                before,
                after,
                at,
                forced,
            } => {
                if let Some(head) = self.see(after, source) {
                    keep_latest(&mut head.activity, Push::new(*at, before, Some(*forced)));
                }
            }
            PushRecord::ForcePush { before, after, at } => {
                if let Some(head) = self.see(after, source) {
                    keep_latest(&mut head.force_push, Push::new(*at, before, Some(true)));
                }
                // The head a force-push replaced was a revision too. Only force-push events add
                // the head before: an activity entry's or check suite's can predate the pull
                // request.
                if let Some(before) = before {
                    self.see(before, source);
                }
            }
            PushRecord::CheckSuite { previous, next, at } => {
                if let Some(head) = self.see(next, source) {
                    keep_latest(&mut head.check_suite, Push::new(*at, previous, None));
                    head.sighted(*at);
                }
            }
            PushRecord::Review { commit: head, at } | PushRecord::Polled { head, at } => {
                if let Some(head) = self.see(head, source) {
                    head.sighted(*at);
                }
            }
        }
    }

    /// The head's entry, noting that `source` saw it. `None` for the all-zero SHA.
    fn see(&mut self, head: &'a str, source: Source) -> Option<&mut Seen<'a>> {
        let head = sha(head)?;
        let index = *self.index.entry(head).or_insert_with(|| {
            self.seen.push(Seen::new(head));
            self.seen.len() - 1
        });
        let seen = &mut self.seen[index];
        seen.sources.insert(source);
        Some(seen)
    }
}

/// A push of a head, as one source reported it.
#[derive(Clone, Copy)]
struct Push<'a> {
    at: SystemTime,
    /// The head it replaced.
    before: Option<&'a str>,
    forced: Option<bool>,
}

impl<'a> Push<'a> {
    fn new(at: SystemTime, before: &'a Option<String>, forced: Option<bool>) -> Self {
        Self {
            at,
            before: before.as_deref().and_then(sha),
            forced,
        }
    }
}

/// Keeps the later of two pushes of a head: a revision stands for its head's latest push.
fn keep_latest<'a>(kept: &mut Option<Push<'a>>, push: Push<'a>) {
    if kept.is_none_or(|kept| push.at >= kept.at) {
        *kept = Some(push);
    }
}

/// What the sources saw of one head.
struct Seen<'a> {
    head: &'a str,
    sources: BTreeSet<Source>,
    /// The head's latest push in the activity API.
    activity: Option<Push<'a>>,
    /// The latest force-push to the head.
    force_push: Option<Push<'a>>,
    /// The latest check suite the head's push started.
    check_suite: Option<Push<'a>>,
    /// The earliest a review, check suite or the poller saw the head.
    seen_by: Option<SystemTime>,
}

impl<'a> Seen<'a> {
    fn new(head: &'a str) -> Self {
        Self {
            head,
            sources: BTreeSet::new(),
            activity: None,
            force_push: None,
            check_suite: None,
            seen_by: None,
        }
    }

    fn sighted(&mut self, at: SystemTime) {
        if self.seen_by.is_none_or(|by| at < by) {
            self.seen_by = Some(at);
        }
    }

    /// The pushes sources reported, most trusted first.
    fn reported(&self) -> impl Iterator<Item = &Push<'a>> {
        [&self.activity, &self.force_push, &self.check_suite]
            .into_iter()
            .flatten()
    }

    /// The push's exact time: the activity API's, or else a force-push event's.
    fn exact(&self) -> Option<SystemTime> {
        self.activity.or(self.force_push).map(|push| push.at)
    }

    fn previous(&self) -> Option<&'a str> {
        self.reported().find_map(|push| push.before)
    }

    fn forced(&self) -> Option<bool> {
        self.reported().find_map(|push| push.forced)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::super::{PushedAt, Revision, Source};
    use super::*;

    const NO_COMMIT: &str = "0000000000000000000000000000000000000000";

    /// `minute` minutes into a day in 2026.
    fn at(minute: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000 + minute * 60)
    }

    fn activity(before: Option<&str>, after: &str, minute: u64, forced: bool) -> PushRecord {
        PushRecord::Activity {
            before: before.map(str::to_string),
            after: after.to_string(),
            at: at(minute),
            forced,
        }
    }

    fn force_push(before: &str, after: &str, minute: u64) -> PushRecord {
        PushRecord::ForcePush {
            before: Some(before.to_string()),
            after: after.to_string(),
            at: at(minute),
        }
    }

    fn check_suite(previous: &str, next: &str, minute: u64) -> PushRecord {
        PushRecord::CheckSuite {
            previous: Some(previous.to_string()),
            next: next.to_string(),
            at: at(minute),
        }
    }

    fn review(commit: &str, minute: u64) -> PushRecord {
        PushRecord::Review {
            commit: commit.to_string(),
            at: at(minute),
        }
    }

    fn polled(head: &str, minute: u64) -> PushRecord {
        PushRecord::Polled {
            head: head.to_string(),
            at: at(minute),
        }
    }

    /// The revisions `records` make, over the current commits `commits`, for a pull request whose
    /// head is `head`.
    fn build(records: &[PushRecord], commits: &[&str], head: &str) -> Revisions {
        let commits: Vec<String> = commits.iter().map(|commit| commit.to_string()).collect();
        build_revisions(records, &commits, head)
    }

    fn heads(revisions: &Revisions) -> Vec<&str> {
        revisions
            .iter()
            .map(|revision| revision.head.as_str())
            .collect()
    }

    fn forced(revisions: &Revisions) -> Vec<Option<bool>> {
        revisions.iter().map(|revision| revision.forced).collect()
    }

    #[test]
    fn with_no_records_the_head_is_the_only_revision() {
        let revisions = build(&[], &[], "a1");

        assert_eq!(heads(&revisions), ["a1"]);
        let a1 = revisions.latest().unwrap();
        assert_eq!(a1.sources, [Source::Head].into());
        assert_eq!(a1.pushed_at, PushedAt::Unknown);
    }

    #[test]
    fn activity_pushes_become_revisions_in_time_order() {
        let revisions = build(
            &[
                activity(Some("a1"), "a2", 20, false),
                activity(None, "a1", 10, false),
                activity(Some("a2"), "b1", 30, true),
            ],
            &[],
            "b1",
        );

        assert_eq!(
            revisions.as_slice(),
            [
                Revision {
                    head: "a1".into(),
                    previous: None,
                    forced: Some(false),
                    pushed_at: PushedAt::Exact(at(10)),
                    sources: [Source::Activity].into(),
                    available: true,
                },
                Revision {
                    head: "a2".into(),
                    previous: Some("a1".into()),
                    forced: Some(false),
                    pushed_at: PushedAt::Exact(at(20)),
                    sources: [Source::Activity].into(),
                    available: true,
                },
                Revision {
                    head: "b1".into(),
                    previous: Some("a2".into()),
                    forced: Some(true),
                    pushed_at: PushedAt::Exact(at(30)),
                    sources: [Source::Activity, Source::Head].into(),
                    available: true,
                },
            ]
        );
    }

    #[test]
    fn the_all_zero_sha_of_a_new_branch_is_no_previous_head() {
        let revisions = build(&[activity(Some(NO_COMMIT), "a1", 10, false)], &[], "a1");

        assert_eq!(revisions.as_slice()[0].previous, None);
    }

    #[test]
    fn a_head_every_source_saw_is_one_revision() {
        let revisions = build(
            &[
                polled("b1", 33),
                check_suite("a1", "b1", 31),
                force_push("a1", "b1", 30),
                review("b1", 50),
                activity(Some("a1"), "b1", 29, true),
                activity(None, "a1", 10, false),
            ],
            &[],
            "b1",
        );

        assert_eq!(heads(&revisions), ["a1", "b1"]);
        assert_eq!(
            revisions.get("b1"),
            Some(&Revision {
                head: "b1".into(),
                previous: Some("a1".into()),
                forced: Some(true),
                pushed_at: PushedAt::Exact(at(29)),
                sources: [
                    Source::Activity,
                    Source::ForcePush,
                    Source::Review,
                    Source::CheckSuite,
                    Source::Poller,
                    Source::Head,
                ]
                .into(),
                available: true,
            })
        );
    }

    #[test]
    fn a_force_push_event_dates_a_head_the_activity_api_missed() {
        let revisions = build(&[force_push("a1", "b1", 30)], &[], "b1");

        assert_eq!(
            revisions.get("b1"),
            Some(&Revision {
                head: "b1".into(),
                previous: Some("a1".into()),
                forced: Some(true),
                pushed_at: PushedAt::Exact(at(30)),
                sources: [Source::ForcePush, Source::Head].into(),
                available: true,
            })
        );
    }

    #[test]
    fn reviews_check_suites_and_the_poller_date_a_head_by_its_first_sighting() {
        let revisions = build(
            &[
                review("a1", 40),
                polled("a1", 12),
                check_suite(NO_COMMIT, "a1", 11),
            ],
            &[],
            "a1",
        );

        let a1 = revisions.get("a1").unwrap();
        assert_eq!(a1.pushed_at, PushedAt::By(at(11)));
        assert_eq!(a1.previous, None);
        assert_eq!(a1.forced, None);
    }

    #[test]
    fn the_activity_api_names_the_previous_head_over_other_sources() {
        let revisions = build(
            &[
                check_suite("x9", "b1", 31),
                force_push("y9", "b1", 30),
                activity(Some("a1"), "b1", 29, true),
            ],
            &[],
            "b1",
        );

        assert_eq!(revisions.get("b1").unwrap().previous.as_deref(), Some("a1"));
    }

    #[test]
    fn a_force_push_event_names_the_previous_head_over_a_check_suite() {
        let revisions = build(
            &[check_suite("x9", "b1", 31), force_push("a1", "b1", 30)],
            &[],
            "b1",
        );

        assert_eq!(revisions.get("b1").unwrap().previous.as_deref(), Some("a1"));
    }

    #[test]
    fn a_check_suite_names_the_previous_head_when_nothing_else_does() {
        let revisions = build(&[polled("a1", 5), check_suite("a1", "a2", 21)], &[], "a2");

        assert_eq!(revisions.get("a2").unwrap().previous.as_deref(), Some("a1"));
    }

    #[test]
    fn a_revision_no_source_gives_a_previous_head_follows_the_one_before_it() {
        let revisions = build(
            &[polled("a1", 5), polled("a2", 25), polled("a3", 45)],
            &[],
            "a3",
        );

        assert_eq!(heads(&revisions), ["a1", "a2", "a3"]);
        let previous: Vec<_> = revisions.iter().map(|r| r.previous.as_deref()).collect();
        assert_eq!(previous, [None, Some("a1"), Some("a2")]);
        assert!(revisions.iter().all(|revision| revision.forced.is_none()));
    }

    #[test]
    fn a_head_pushed_twice_is_one_revision_at_its_latest_push() {
        let revisions = build(
            &[
                activity(None, "a1", 10, false),
                activity(Some("a1"), "b1", 20, true),
                activity(Some("b1"), "a1", 30, true),
            ],
            &[],
            "a1",
        );

        assert_eq!(heads(&revisions), ["b1", "a1"]);
        let a1 = revisions.latest().unwrap();
        assert_eq!(a1.previous.as_deref(), Some("b1"));
        assert_eq!(a1.forced, Some(true));
        assert_eq!(a1.pushed_at, PushedAt::Exact(at(30)));
    }

    #[test]
    fn a_force_pushed_over_head_comes_just_before_the_force_push() {
        let revisions = build(
            &[
                polled("a1", 5),
                force_push("a2", "b1", 50),
                polled("a0", 1),
                polled("b2", 60),
            ],
            &[],
            "b2",
        );

        assert_eq!(heads(&revisions), ["a0", "a1", "a2", "b1", "b2"]);
        let a2 = revisions.get("a2").unwrap();
        assert_eq!(a2.pushed_at, PushedAt::By(at(50)));
        assert_eq!(a2.sources, [Source::ForcePush].into());
        assert_eq!(a2.previous.as_deref(), Some("a1"));
    }

    #[test]
    fn the_push_chain_orders_heads_whose_check_suites_started_out_of_order() {
        // a1's suite started a minute after a2's: CI was slow to pick up the first push.
        let revisions = build(
            &[
                check_suite("a1", "a2", 11),
                check_suite(NO_COMMIT, "a1", 12),
            ],
            &[],
            "a2",
        );

        assert_eq!(heads(&revisions), ["a1", "a2"]);
        assert_eq!(revisions.get("a1").unwrap().pushed_at, PushedAt::By(at(11)));
    }

    #[test]
    fn ancestry_places_a_review_submitted_after_the_next_push() {
        // A review started on a1 and submitted after a2 was pushed is on a1.
        let revisions = build(
            &[polled("a2", 20), review("a1", 30)],
            &["c0", "a1", "c2", "a2"],
            "a2",
        );

        assert_eq!(heads(&revisions), ["a1", "a2"]);
        assert_eq!(revisions.get("a1").unwrap().pushed_at, PushedAt::By(at(20)));
    }

    #[test]
    fn a_head_pushed_back_to_stays_at_its_latest_push_though_it_is_an_ancestor() {
        // a2 descends from a1, and a3 from a2. The author reset the branch to a1, then pushed a3.
        let revisions = build(
            &[
                activity(None, "a1", 10, false),
                activity(Some("a1"), "a2", 20, false),
                activity(Some("a2"), "a1", 30, true),
                activity(Some("a1"), "a3", 40, false),
            ],
            &["a1", "a2", "a3"],
            "a3",
        );

        assert_eq!(heads(&revisions), ["a2", "a1", "a3"]);
    }

    #[test]
    fn the_head_is_a_revision_even_when_no_source_saw_it() {
        let revisions = build(&[polled("a1", 5)], &["a1", "a2"], "a2");

        assert_eq!(heads(&revisions), ["a1", "a2"]);
        let a2 = revisions.latest().unwrap();
        assert_eq!(a2.sources, [Source::Head].into());
        assert_eq!(a2.pushed_at, PushedAt::Unknown);
        assert_eq!(a2.previous.as_deref(), Some("a1"));
    }

    #[test]
    fn the_head_joins_the_revision_the_poller_saw() {
        let revisions = build(&[polled("a1", 5), polled("a2", 25)], &["a1", "a2"], "a2");

        assert_eq!(heads(&revisions), ["a1", "a2"]);
        assert_eq!(
            revisions.latest().unwrap().sources,
            [Source::Poller, Source::Head].into()
        );
    }

    #[test]
    fn the_head_is_latest_though_a_review_on_a_replaced_head_came_later() {
        // x1 was pushed, then y1, which a force-push replaced with z1. A review on x1 was
        // submitted after the force-push.
        let revisions = build(
            &[
                force_push("y1", "z1", 30),
                polled("z1", 31),
                review("x1", 40),
            ],
            &["z0", "z1"],
            "z1",
        );

        assert_eq!(heads(&revisions), ["y1", "x1", "z1"]);
    }

    #[test]
    fn the_head_is_latest_after_a_revert_only_the_poller_saw() {
        // b1 descends from a1; the author reset the branch back to a1.
        let revisions = build(
            &[
                polled("a1", 10),
                check_suite("a1", "b1", 20),
                polled("b1", 21),
                polled("a1", 30),
            ],
            &["a0", "a1"],
            "a1",
        );

        assert_eq!(heads(&revisions), ["b1", "a1"]);
    }

    #[test]
    fn the_head_is_latest_when_the_pollers_clock_runs_ahead() {
        // This Mac's clock is ten minutes fast: a1 and a2 were pushed at 5 and 6.
        let revisions = build(
            &[
                polled("a1", 15),
                polled("a2", 16),
                force_push("a2", "b1", 7),
            ],
            &["b0", "b1"],
            "b1",
        );

        assert_eq!(heads(&revisions), ["a1", "a2", "b1"]);
    }

    #[test]
    fn the_head_is_latest_when_the_current_commits_stop_short() {
        // The caller passed the first 250 commits, where GitHub's commit list stops.
        let commits: Vec<String> = (1..=250).map(|n| format!("c{n}")).collect();
        let commits: Vec<&str> = commits.iter().map(String::as_str).collect();
        let revisions = build(&[polled("c100", 10), polled("c293", 50)], &commits, "c293");

        assert_eq!(heads(&revisions), ["c100", "c293"]);
    }

    #[test]
    fn heads_rebased_away_come_before_the_latest_force_push() {
        // x1 was pushed, then y1, which a force-push replaced with a1; a2 and a3 followed. A
        // review on x1 was submitted after a2 was pushed.
        let revisions = build(
            &[
                force_push("y1", "a1", 20),
                review("x1", 35),
                polled("a3", 40),
            ],
            &["a1", "a2", "a3"],
            "a3",
        );

        assert_eq!(heads(&revisions), ["y1", "x1", "a1", "a3"]);
        let a3 = revisions.latest().unwrap();
        assert_eq!(a3.previous.as_deref(), Some("a1"));
        assert_eq!(a3.forced, Some(false));
    }

    #[test]
    fn a_push_from_an_ancestor_was_not_forced() {
        let revisions = build(
            &[polled("a1", 5), polled("a2", 25)],
            &["a1", "c2", "a2"],
            "a2",
        );

        assert_eq!(forced(&revisions), [None, Some(false)]);
    }

    #[test]
    fn a_push_over_a_revision_rebased_away_was_forced() {
        let revisions = build(&[polled("a1", 5), polled("b1", 25)], &["b0", "b1"], "b1");

        assert_eq!(heads(&revisions), ["a1", "b1"]);
        assert_eq!(forced(&revisions), [None, Some(true)]);
    }

    #[test]
    fn a_push_back_to_an_ancestor_was_forced() {
        // The author reset the branch from a2 to a1, then pushed a3 on top of a2 again.
        let revisions = build(
            &[
                polled("a2", 10),
                check_suite("a2", "a1", 20),
                polled("a3", 30),
            ],
            &["a1", "a2", "a3"],
            "a3",
        );

        assert_eq!(heads(&revisions), ["a2", "a1", "a3"]);
        assert_eq!(forced(&revisions), [None, Some(true), Some(false)]);
    }

    #[test]
    fn forced_is_unknown_when_neither_head_is_in_the_current_commits() {
        let revisions = build(&[polled("a1", 5), polled("a2", 25)], &["b1"], "b1");

        assert_eq!(heads(&revisions), ["a1", "a2", "b1"]);
        assert_eq!(forced(&revisions)[1], None);
    }

    #[test]
    fn forced_is_unknown_when_the_previous_head_is_no_revision() {
        // x0 was the branch's head before the pull request opened, and was force-pushed over.
        let revisions = build(&[check_suite("x0", "a1", 10)], &["a1"], "a1");

        assert_eq!(forced(&revisions), [None]);
    }

    #[test]
    fn a_source_saying_whether_the_push_was_forced_wins_over_ancestry() {
        let revisions = build(
            &[
                activity(None, "a1", 10, false),
                activity(Some("a1"), "a2", 20, true),
            ],
            &["a1", "a2"],
            "a2",
        );

        assert_eq!(forced(&revisions), [Some(false), Some(true)]);
    }

    #[test]
    fn revisions_are_numbered_from_r1_for_display() {
        let revisions = build(
            &[
                activity(None, "a1", 10, false),
                activity(Some("a1"), "a2", 20, false),
            ],
            &[],
            "a2",
        );

        assert_eq!(revisions.number("a1"), Some(1));
        assert_eq!(revisions.label("a2").as_deref(), Some("r2"));
        assert_eq!(revisions.number("ffff"), None);
        assert_eq!(revisions.label("ffff"), None);
        assert_eq!(revisions.latest().map(|r| r.head.as_str()), Some("a2"));
    }
}
