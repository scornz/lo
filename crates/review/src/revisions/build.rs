//! Merging what each source saw into one ordered list of revisions.

use std::collections::{BTreeSet, HashMap};
use std::time::SystemTime;

use super::{PushRecord, PushedAt, Revision, Revisions, Source, sha};

/// Merges push records from every source into the pull request's revisions, one per head SHA.
///
/// `commits` are the pull request's current commits from git, oldest first.
///
/// - **Deduplicated by head.** A head several sources saw is one revision, listing them all. A
///   head pushed twice (`a` → `b` → `a`) is one revision, placed at its latest push.
/// - **Ordered by time.** Times from the activity API, or else a force-push event, are exact. A
///   review, check suite or the poller only says the head was pushed by then.
/// - **Previous head and forced.** The activity API is trusted first, then force-push events,
///   then check suites. A check suite never says whether its push was forced.
pub fn build_revisions(records: &[PushRecord], _commits: &[String]) -> Revisions {
    let mut heads = Heads::default();
    for record in records {
        heads.add(record);
    }
    let seen = heads.seen;

    let key = |index: usize| seen[index].exact().or(seen[index].seen_by);
    let mut order: Vec<usize> = (0..seen.len()).collect();
    // Undated heads go last; ties keep the order the records first named them in.
    order.sort_by_key(|&index| (key(index).is_none(), key(index), index));

    let list = order
        .iter()
        .enumerate()
        .map(|(position, &index)| {
            let head = &seen[index];
            let previous = head
                .previous()
                .or_else(|| Some(seen[order[position.checked_sub(1)?]].head));
            Revision {
                head: head.head.to_string(),
                previous: previous.map(str::to_string),
                forced: head.forced(),
                pushed_at: match (head.exact(), head.seen_by) {
                    (Some(at), _) => PushedAt::Exact(at),
                    (None, Some(by)) => PushedAt::By(by),
                    (None, None) => PushedAt::Unknown,
                },
                sources: head.sources.clone(),
            }
        })
        .collect();
    Revisions { list }
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

    fn heads(revisions: &Revisions) -> Vec<&str> {
        revisions
            .iter()
            .map(|revision| revision.head.as_str())
            .collect()
    }

    fn build(records: &[PushRecord]) -> Revisions {
        build_revisions(records, &[])
    }

    #[test]
    fn no_records_make_no_revisions() {
        let revisions = build(&[]);

        assert!(revisions.is_empty());
        assert_eq!(revisions.latest(), None);
    }

    #[test]
    fn activity_pushes_become_revisions_in_time_order() {
        let revisions = build(&[
            activity(Some("a1"), "a2", 20, false),
            activity(None, "a1", 10, false),
            activity(Some("a2"), "b1", 30, true),
        ]);

        assert_eq!(
            revisions.as_slice(),
            [
                Revision {
                    head: "a1".into(),
                    previous: None,
                    forced: Some(false),
                    pushed_at: PushedAt::Exact(at(10)),
                    sources: [Source::Activity].into(),
                },
                Revision {
                    head: "a2".into(),
                    previous: Some("a1".into()),
                    forced: Some(false),
                    pushed_at: PushedAt::Exact(at(20)),
                    sources: [Source::Activity].into(),
                },
                Revision {
                    head: "b1".into(),
                    previous: Some("a2".into()),
                    forced: Some(true),
                    pushed_at: PushedAt::Exact(at(30)),
                    sources: [Source::Activity].into(),
                },
            ]
        );
    }

    #[test]
    fn the_all_zero_sha_of_a_new_branch_is_no_previous_head() {
        let revisions = build(&[activity(
            Some("0000000000000000000000000000000000000000"),
            "a1",
            10,
            false,
        )]);

        assert_eq!(revisions.as_slice()[0].previous, None);
    }

    #[test]
    fn a_head_every_source_saw_is_one_revision() {
        let revisions = build(&[
            polled("b1", 33),
            check_suite("a1", "b1", 31),
            force_push("a1", "b1", 30),
            review("b1", 50),
            activity(Some("a1"), "b1", 29, true),
            activity(None, "a1", 10, false),
        ]);

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
                ]
                .into(),
            })
        );
    }

    #[test]
    fn a_force_push_event_dates_a_head_the_activity_api_missed() {
        let revisions = build(&[force_push("a1", "b1", 30)]);

        assert_eq!(
            revisions.get("b1"),
            Some(&Revision {
                head: "b1".into(),
                previous: Some("a1".into()),
                forced: Some(true),
                pushed_at: PushedAt::Exact(at(30)),
                sources: [Source::ForcePush].into(),
            })
        );
    }

    #[test]
    fn reviews_check_suites_and_the_poller_date_a_head_by_its_first_sighting() {
        let revisions = build(&[
            review("a1", 40),
            polled("a1", 12),
            check_suite("0000000000000000000000000000000000000000", "a1", 11),
        ]);

        let a1 = revisions.get("a1").unwrap();
        assert_eq!(a1.pushed_at, PushedAt::By(at(11)));
        assert_eq!(a1.previous, None);
        assert_eq!(a1.forced, None);
    }

    #[test]
    fn the_activity_api_names_the_previous_head_over_other_sources() {
        let revisions = build(&[
            check_suite("x9", "b1", 31),
            force_push("y9", "b1", 30),
            activity(Some("a1"), "b1", 29, true),
        ]);

        assert_eq!(revisions.get("b1").unwrap().previous.as_deref(), Some("a1"));
    }

    #[test]
    fn a_force_push_event_names_the_previous_head_over_a_check_suite() {
        let revisions = build(&[check_suite("x9", "b1", 31), force_push("a1", "b1", 30)]);

        assert_eq!(revisions.get("b1").unwrap().previous.as_deref(), Some("a1"));
    }

    #[test]
    fn a_check_suite_names_the_previous_head_when_nothing_else_does() {
        let revisions = build(&[polled("a1", 5), check_suite("a1", "a2", 21)]);

        assert_eq!(revisions.get("a2").unwrap().previous.as_deref(), Some("a1"));
    }

    #[test]
    fn a_revision_no_source_gives_a_previous_head_follows_the_one_before_it() {
        let revisions = build(&[polled("a1", 5), polled("a2", 25), polled("a3", 45)]);

        assert_eq!(heads(&revisions), ["a1", "a2", "a3"]);
        let previous: Vec<_> = revisions.iter().map(|r| r.previous.as_deref()).collect();
        assert_eq!(previous, [None, Some("a1"), Some("a2")]);
        assert!(revisions.iter().all(|revision| revision.forced.is_none()));
    }

    #[test]
    fn a_head_pushed_twice_is_one_revision_at_its_latest_push() {
        let revisions = build(&[
            activity(None, "a1", 10, false),
            activity(Some("a1"), "b1", 20, true),
            activity(Some("b1"), "a1", 30, true),
        ]);

        assert_eq!(heads(&revisions), ["b1", "a1"]);
        let a1 = revisions.latest().unwrap();
        assert_eq!(a1.previous.as_deref(), Some("b1"));
        assert_eq!(a1.forced, Some(true));
        assert_eq!(a1.pushed_at, PushedAt::Exact(at(30)));
    }

    #[test]
    fn revisions_are_numbered_from_r1_for_display() {
        let revisions = build(&[
            activity(None, "a1", 10, false),
            activity(Some("a1"), "a2", 20, false),
        ]);

        assert_eq!(revisions.number("a1"), Some(1));
        assert_eq!(revisions.label("a2").as_deref(), Some("r2"));
        assert_eq!(revisions.number("ffff"), None);
        assert_eq!(revisions.label("ffff"), None);
        assert_eq!(revisions.latest().map(|r| r.head.as_str()), Some("a2"));
    }
}
