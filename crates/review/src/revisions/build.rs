//! Merging what each source saw into one ordered list of revisions.

use std::cmp::Reverse;
use std::collections::{BTreeSet, BinaryHeap, HashMap};
use std::time::SystemTime;

use super::{PushRecord, PushedAt, Revision, Revisions, Source, sha};

/// Merges push records from every source into the pull request's revisions, one per head SHA.
///
/// `commits` are the pull request's current commits from git, oldest first. `head` is its current
/// head (`headRefOid`), which is always the latest revision, even when no source saw it or a
/// source's time says otherwise. `opened_at` is when the pull request opened (`createdAt`).
///
/// - **Starting where the pull request opened.** The first revision is the head when the pull
///   request opened: the newest head pushed at or before `opened_at`. Older heads were pushed
///   before the pull request existed and aren't revisions.
/// - **Deduplicated by head.** A head several sources saw is one revision, listing them all. A
///   head pushed twice (`a` → `b` → `a`) is one revision, placed at its latest push.
/// - **Ordered by the push chain, ancestry and time.** A head comes after the head its push
///   replaced, and among the current commits, after its ancestors. Times order the rest. Times
///   from the activity API, or else a force-push event, are exact. A review, check suite or the
///   poller only says the head was pushed by then, and so does a later head's push, which places a
///   head no source dated, such as a force-push's `before`, just before the push that replaced it.
///   Heads outside the current commits come before the latest force-push when its head is in
///   them, since every push after it was then a fast-forward. Where these constraints loop because
///   a head was pushed again, the heads in the loop go by their own times, whatever order the
///   records came in.
/// - **Previous head and forced.** The activity API is trusted first, then force-push events,
///   then check suites, though a push that creates the branch says nothing about forcing. A check
///   suite names the previous head only when its previous SHA is a head another record names or a
///   current commit, and the earliest such suite does. With no source's word, the previous head is
///   the revision before, and ancestry in the current commits tells whether the push was forced
///   when it can. A branch re-created after a revision counts as forced unless ancestry says
///   otherwise.
pub fn build_revisions(
    records: &[PushRecord],
    commits: &[String],
    head: &str,
    opened_at: SystemTime,
) -> Revisions {
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
    heads.choose_check_suites(&positions);
    let after = successors(&heads, &positions, pull_head);
    let components = Components::new(&after);
    let latest = latest_push_times(&heads.seen, &after, &components);
    let mut order = order(&latest, &after, &components);
    if let Some(pull_head) = pull_head {
        order.retain(|&index| index != pull_head);
        order.push(pull_head);
    }
    let pushed_by_opening = |index: usize| latest[index].is_some_and(|at| at <= opened_at);
    if let Some(first) = order.iter().rposition(|&index| pushed_by_opening(index)) {
        order.drain(..first);
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
                .or_else(|| forced_by_ancestry(previous?, head.head, &positions, &heads.index))
                .or_else(|| (head.recreated() && previous.is_some()).then_some(true));
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
    Revisions { list }
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
    // A head pushed back to from its own descendant was pushed after it, so its place among the
    // current commits says nothing about when.
    let pushed_back = |index: usize| {
        let head = &heads.seen[index];
        let previous = head.previous().and_then(|previous| positions.get(previous));
        previous.is_some_and(|previous| Some(previous) > positions.get(head.head))
    };
    for pair in current.windows(2) {
        if !pushed_back(pair[0].1) {
            precedes(pair[0].1, pair[1].1);
        }
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

/// The strongly connected components of the heads under the constraints: sets of heads whose
/// constraints loop, which happens when a head was pushed again.
struct Components {
    /// Each component's heads. A component comes before every component with a constraint to it.
    members: Vec<Vec<usize>>,
    /// Each head's component.
    of: Vec<usize>,
}

impl Components {
    /// Tarjan's algorithm, without recursion so that long histories can't overflow the stack.
    fn new(after: &[BTreeSet<usize>]) -> Self {
        const UNSEEN: usize = usize::MAX;
        let count = after.len();
        let mut found = vec![UNSEEN; count];
        let mut low = vec![0; count];
        let mut on_stack = vec![false; count];
        let mut stack = Vec::new();
        let mut members = Vec::new();
        let mut next_found = 0;
        for root in 0..count {
            if found[root] != UNSEEN {
                continue;
            }
            found[root] = next_found;
            low[root] = next_found;
            next_found += 1;
            stack.push(root);
            on_stack[root] = true;
            let mut frames = vec![(root, after[root].iter())];
            while let Some((head, successors)) = frames.last_mut() {
                let head = *head;
                if let Some(&next) = successors.next() {
                    if found[next] == UNSEEN {
                        found[next] = next_found;
                        low[next] = next_found;
                        next_found += 1;
                        stack.push(next);
                        on_stack[next] = true;
                        frames.push((next, after[next].iter()));
                    } else if on_stack[next] {
                        low[head] = low[head].min(found[next]);
                    }
                    continue;
                }
                frames.pop();
                if let Some(&(parent, _)) = frames.last() {
                    low[parent] = low[parent].min(low[head]);
                }
                if low[head] == found[head] {
                    let mut component = Vec::new();
                    while let Some(member) = stack.pop() {
                        on_stack[member] = false;
                        component.push(member);
                        if member == head {
                            break;
                        }
                    }
                    members.push(component);
                }
            }
        }
        let mut of = vec![0; count];
        for (component, heads) in members.iter().enumerate() {
            for &head in heads {
                of[head] = component;
            }
        }
        Self { members, of }
    }
}

/// The latest each head can have been pushed: its exact time, or else the earliest of its
/// sightings and the times of the heads that must come after it. `None` when nothing bounds it.
///
/// Constraints inside a loop don't carry times: one of them is wrong, and which one depends on the
/// times themselves.
fn latest_push_times(
    seen: &[Seen],
    after: &[BTreeSet<usize>],
    components: &Components,
) -> Vec<Option<SystemTime>> {
    let mut latest: Vec<_> = seen
        .iter()
        .map(|head| head.exact().or(head.seen_by))
        .collect();
    // A component comes before every component with a constraint to it, so a head's successors
    // in other components already have their times when it's reached: one pass is enough.
    for heads in &components.members {
        for &index in heads {
            if seen[index].exact().is_some() {
                continue;
            }
            for &next in &after[index] {
                if components.of[next] != components.of[index]
                    && let Some(by) = latest[next]
                    && latest[index].is_none_or(|own| by < own)
                {
                    latest[index] = Some(by);
                }
            }
        }
    }
    latest
}

/// The heads in revision order: each after the heads that must come before it, the earliest
/// first. Heads nothing bounds go last, and ties keep the order the records first named them in.
/// The heads of a loop go together, by their own times.
fn order(
    latest: &[Option<SystemTime>],
    after: &[BTreeSet<usize>],
    components: &Components,
) -> Vec<usize> {
    let rank = |index: usize| (latest[index].is_none(), latest[index], index);
    let members: Vec<Vec<usize>> = (components.members.iter())
        .map(|heads| {
            let mut heads = heads.clone();
            heads.sort_by_key(|&index| rank(index));
            heads
        })
        .collect();
    let mut waiting_on = vec![0usize; members.len()];
    for (index, successors) in after.iter().enumerate() {
        for &next in successors {
            if components.of[next] != components.of[index] {
                waiting_on[components.of[next]] += 1;
            }
        }
    }
    // Components ready to place, earliest first: a component ranks as its earliest head.
    let mut ready: BinaryHeap<_> = (0..members.len())
        .filter(|&component| waiting_on[component] == 0)
        .map(|component| Reverse((rank(members[component][0]), component)))
        .collect();
    let mut order = Vec::with_capacity(after.len());
    while let Some(Reverse((_, component))) = ready.pop() {
        for &index in &members[component] {
            order.push(index);
            for &next in &after[index] {
                let later = components.of[next];
                if later == component {
                    continue;
                }
                waiting_on[later] -= 1;
                if waiting_on[later] == 0 {
                    ready.push(Reverse((rank(members[later][0]), later)));
                }
            }
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
                    // A push that creates the branch has no head before it to be forced over.
                    let created = before.as_deref().and_then(sha).is_none();
                    let forced = (!created).then_some(*forced);
                    keep_latest(&mut head.activity, Push::new(*at, before, forced));
                }
            }
            PushRecord::ForcePush { before, after, at } => {
                if let Some(head) = self.see(after, source) {
                    keep_latest(&mut head.force_push, Push::new(*at, before, Some(true)));
                }
                // The head a force-push replaced was a revision too. An activity entry's head
                // before is always another entry's head, except for the first.
                if let Some(before) = before {
                    self.see(before, source);
                }
            }
            PushRecord::CheckSuite { previous, next, at } => {
                if let Some(head) = self.see(next, source) {
                    head.check_suites.push(Push::new(*at, previous, None));
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

    /// Picks the check suite that names each head's previous head: the earliest whose previous SHA
    /// is a head another record names or a current commit. A suite for the same commit pushed to
    /// another branch, such as a backport, names that branch's head instead. The previous heads
    /// the chosen suites name are revisions too.
    fn choose_check_suites(&mut self, positions: &HashMap<String, usize>) {
        for index in 0..self.seen.len() {
            let known = |sha: &str| self.index.contains_key(sha) || positions.contains_key(sha);
            let chosen = (self.seen[index].check_suites.iter())
                .filter(|suite| suite.before.is_some_and(known))
                .min_by_key(|suite| suite.at)
                .copied();
            self.seen[index].check_suite = chosen;
        }
        let previous: Vec<&'a str> = (self.seen.iter())
            .filter_map(|head| head.check_suite?.before)
            .collect();
        for head in previous {
            self.see(head, Source::CheckSuite);
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
    /// Every check suite the head's push started.
    check_suites: Vec<Push<'a>>,
    /// The check suite that names the previous head, once chosen.
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
            check_suites: Vec::new(),
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

    /// Whether the activity API says the push created the branch, which is a re-creation when a
    /// revision came before it.
    fn recreated(&self) -> bool {
        self.activity.is_some_and(|push| push.before.is_none())
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
    /// head is `head`, opened before any of them.
    fn build(records: &[PushRecord], commits: &[&str], head: &str) -> Revisions {
        opened(records, commits, head, 0)
    }

    /// The same, for a pull request opened at `minute`.
    fn opened(records: &[PushRecord], commits: &[&str], head: &str, minute: u64) -> Revisions {
        let commits: Vec<String> = commits.iter().map(|commit| commit.to_string()).collect();
        build_revisions(records, &commits, head, at(minute))
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
                    forced: None,
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
                polled("x9", 1),
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
            &[
                polled("x9", 1),
                check_suite("x9", "b1", 31),
                force_push("a1", "b1", 30),
            ],
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
    fn a_check_suite_from_another_branch_names_no_previous_head() {
        // a2 was later pushed to a backport branch whose head was e5f6.
        let revisions = build(
            &[
                check_suite(NO_COMMIT, "a1", 10),
                check_suite("a1", "a2", 21),
                check_suite("e5f6", "a2", 50),
            ],
            &["a1", "a2"],
            "a2",
        );

        let a2 = revisions.get("a2").unwrap();
        assert_eq!(a2.previous.as_deref(), Some("a1"));
        assert_eq!(a2.forced, Some(false));
    }

    #[test]
    fn a_check_suite_naming_an_unknown_head_leaves_the_revision_before() {
        let revisions = build(&[polled("a1", 5), check_suite("e5f6", "a2", 20)], &[], "a2");

        assert_eq!(heads(&revisions), ["a1", "a2"]);
        assert_eq!(revisions.get("a2").unwrap().previous.as_deref(), Some("a1"));
    }

    #[test]
    fn the_earliest_check_suite_naming_a_known_head_names_the_previous_head() {
        let revisions = build(
            &[
                polled("a0", 1),
                polled("a1", 5),
                check_suite("a0", "a2", 30),
                check_suite("a1", "a2", 21),
            ],
            &[],
            "a2",
        );

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
    fn a_reviews_commit_is_a_revision_even_between_pushes() {
        // a2's push brought in c2 too; the API made a review on c2.
        let revisions = build(
            &[
                activity(None, "a1", 10, false),
                activity(Some("a1"), "a2", 20, false),
                review("c2", 35),
            ],
            &["a1", "c2", "a2"],
            "a2",
        );

        assert_eq!(heads(&revisions), ["a1", "c2", "a2"]);
        assert_eq!(revisions.get("a2").unwrap().previous.as_deref(), Some("a1"));
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
    fn pushes_before_the_pull_request_opened_are_not_revisions() {
        // p1 and p2 were pushed before the pull request opened at p2; p3 came after.
        let revisions = opened(
            &[
                check_suite(NO_COMMIT, "p1", 0),
                check_suite("p1", "p2", 20),
                check_suite("p2", "p3", 60),
                polled("p2", 31),
            ],
            &["p1", "p2", "p3"],
            "p3",
            30,
        );

        assert_eq!(heads(&revisions), ["p2", "p3"]);
        assert_eq!(revisions.get("p2").unwrap().previous.as_deref(), Some("p1"));
    }

    #[test]
    fn the_whole_activity_log_starts_at_the_head_when_the_pull_request_opened() {
        let revisions = opened(
            &[
                activity(None, "x0", 5, false),
                activity(Some("x0"), "a1", 10, false),
                activity(Some("a1"), "a2", 20, false),
            ],
            &["x0", "a1", "a2"],
            "a2",
            12,
        );

        assert_eq!(heads(&revisions), ["a1", "a2"]);
        assert_eq!(revisions.get("a1").unwrap().previous.as_deref(), Some("x0"));
    }

    #[test]
    fn with_nothing_dated_before_the_pull_request_opened_every_head_stays() {
        let revisions = opened(
            &[force_push("a1", "b1", 50), polled("b1", 51)],
            &["b1"],
            "b1",
            30,
        );

        assert_eq!(heads(&revisions), ["a1", "b1"]);
    }

    #[test]
    fn a_check_suites_previous_head_in_the_current_commits_is_a_revision() {
        // No other source saw a2, which only the next push's check suite names.
        let revisions = build(
            &[polled("a1", 5), check_suite("a2", "a3", 30)],
            &["a1", "a2", "a3"],
            "a3",
        );

        assert_eq!(heads(&revisions), ["a1", "a2", "a3"]);
        let a2 = revisions.get("a2").unwrap();
        assert_eq!(a2.sources, [Source::CheckSuite].into());
        assert_eq!(a2.pushed_at, PushedAt::By(at(30)));
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
    fn a_push_back_to_an_ancestor_is_ordered_the_same_whatever_the_record_order() {
        let records = [
            polled("a2", 10),
            check_suite("a2", "a1", 20),
            polled("a3", 30),
        ];
        let reversed: Vec<_> = records.iter().rev().cloned().collect();
        let suite_first = [records[1].clone(), records[0].clone(), records[2].clone()];

        for records in [&records[..], &reversed, &suite_first] {
            let revisions = build(records, &["a1", "a2", "a3"], "a3");

            assert_eq!(heads(&revisions), ["a2", "a1", "a3"]);
            assert_eq!(forced(&revisions), [None, Some(true), Some(false)]);
        }
    }

    #[test]
    fn heads_pushed_back_and_forth_go_by_their_own_times_whatever_the_record_order() {
        // a1 was pushed over by b1, then pushed back to; only check suites saw it.
        let records = [
            check_suite("a1", "b1", 20),
            check_suite("b1", "a1", 30),
            polled("c1", 40),
        ];
        let reversed: Vec<_> = records.iter().rev().cloned().collect();

        for records in [&records[..], &reversed] {
            let revisions = build(records, &[], "c1");

            assert_eq!(heads(&revisions), ["b1", "a1", "c1"]);
        }
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

        assert_eq!(forced(&revisions), [None, Some(true)]);
    }

    #[test]
    fn a_branch_recreated_over_an_unrelated_head_was_forced() {
        // The branch was deleted at a2 and created again at b1.
        let revisions = build(
            &[
                activity(None, "a1", 10, false),
                activity(Some("a1"), "a2", 20, false),
                activity(Some(NO_COMMIT), "b1", 40, false),
            ],
            &["b0", "b1"],
            "b1",
        );

        let b1 = revisions.latest().unwrap();
        assert_eq!(b1.previous.as_deref(), Some("a2"));
        assert_eq!(b1.forced, Some(true));
    }

    #[test]
    fn a_branch_recreated_at_a_descendant_was_not_forced() {
        let revisions = build(
            &[
                activity(None, "a1", 10, false),
                activity(Some("a1"), "a2", 20, false),
                activity(None, "a3", 40, false),
            ],
            &["a1", "a2", "a3"],
            "a3",
        );

        assert_eq!(forced(&revisions), [None, Some(false), Some(false)]);
    }

    #[test]
    fn a_branch_recreated_with_no_ancestry_to_go_on_counts_as_forced() {
        let revisions = build(
            &[
                activity(None, "a1", 10, false),
                activity(None, "b1", 40, false),
            ],
            &[],
            "b1",
        );

        assert_eq!(forced(&revisions), [None, Some(true)]);
    }

    #[test]
    fn a_branch_restored_at_its_head_is_one_revision() {
        let revisions = build(
            &[
                activity(None, "a1", 10, false),
                activity(Some("a1"), "a2", 20, false),
                activity(None, "a2", 40, false),
            ],
            &["a1", "a2"],
            "a2",
        );

        assert_eq!(heads(&revisions), ["a1", "a2"]);
        assert_eq!(forced(&revisions), [None, Some(false)]);
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
