//! Where the base branch was when a revision was pushed (design §6, "Rebase-aware comparison").
//!
//! A revision's base is the merge base of its head with the base branch's tip as it was when the
//! revision was pushed. Today's tip gives the same merge base as long as the branch only moved
//! forward since and doesn't contain the pull request's commits, which is the usual case. It
//! doesn't for a pull request merged with a merge commit or by a fast-forward, whose base branch
//! now holds its revisions, nor for a stacked or retargeted pull request, whose base branch was
//! force-pushed or replaced. [`base_tip_at`] picks the tip then from what GitHub and rozed saw.

use std::time::{Duration, SystemTime};

use super::{PushedAt, sha};

/// A change of the pull request's base branch: a `BaseRefChangedEvent` (`previousRefName`,
/// `currentRefName`, `createdAt`), or an `AutomaticBaseChangeSucceededEvent` (`oldBase`,
/// `newBase`), as when the branch below in a stack is merged and deleted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Retarget {
    /// The base branch before.
    pub from: String,
    /// The base branch after.
    pub to: String,
    pub at: SystemTime,
}

/// Something a source saw of where one of the pull request's base branches pointed. SHAs are full
/// commit SHAs; GitHub's all-zero SHA reads as `None`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BaseRecord {
    /// A push in the repository activity API for a base branch
    /// (`GET /repos/{owner}/{repo}/activity?ref=refs/heads/{branch}`): `before`, `after`,
    /// `timestamp`, and whether `activity_type` is `force_push`. Exact. A `branch_creation`
    /// entry's `before` and a `branch_deletion` entry's `after` are the all-zero SHA. The entries
    /// for a branch must run unbroken from the earliest one passed to the branch's latest push, as
    /// the API pages them, so that the latest at or before a time is the tip then.
    Activity {
        branch: String,
        before: Option<String>,
        after: Option<String>,
        at: SystemTime,
        forced: bool,
    },
    /// A `BaseRefForcePushedEvent`: `afterCommit` and `createdAt`, a few seconds after the push, on
    /// whichever branch the pull request targeted then. `after` is `None` when GitHub no longer has
    /// the commit. Exact for that moment, though normal pushes after it go unrecorded. It's taken
    /// as [`EVENT_LAG`] before `createdAt`, so a layer pushed just after the one below it, as
    /// stacking tools do, sees the push below.
    ForcePush {
        after: Option<String>,
        at: SystemTime,
    },
    /// A tip rozed saw itself, and when it first saw it, such as a head of the pull request below
    /// in a stack, whose branch is this one's base. The branch was there by then, or had moved on
    /// unseen.
    Seen {
        branch: String,
        tip: String,
        at: SystemTime,
    },
}

/// How long after a push GitHub makes its timeline event, at most: the activity API's time is
/// 1–4 s before the event's.
pub const EVENT_LAG: Duration = Duration::from_secs(5);

/// How a pull request merged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Merge {
    /// The commit GitHub names as merging it (`mergeCommit`): a merge commit, the squashed or last
    /// rebased commit, or the head itself, when the head was pushed to the base branch.
    pub commit: String,
    /// That commit's parents (`mergeCommit.parents`), its first two.
    pub parents: Vec<String>,
    /// The pull request's head when it merged (`headRefOid`).
    pub head: String,
}

impl Merge {
    /// Whether it merged with a merge commit of the base and the head: its first parent was the
    /// base branch's tip just before.
    fn with_merge_commit(&self) -> bool {
        matches!(self.parents.as_slice(), [_, second] if *second == self.head)
    }

    /// Whether the base branch now holds the pull request's commits, as it does after a merge
    /// commit or a fast-forward, so its tip now is no revision's base. A squash or rebase merge
    /// makes new commits instead.
    fn holds_the_pull_request(&self) -> bool {
        self.commit == self.head || self.parents.len() > 1
    }
}

/// What's known of a pull request's base branches over time, for [`base_tip_at`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BaseHistory {
    /// The base branch now (`baseRefName`).
    pub branch: String,
    /// Its tip now, as the local copy last fetched it.
    pub tip: Option<String>,
    /// How the pull request merged, when it did.
    pub merge: Option<Merge>,
    /// The pull request's retargets, in any order.
    pub retargets: Vec<Retarget>,
    /// Where base branches pointed, oldest first where times tie.
    pub records: Vec<BaseRecord>,
}

/// The base branch's tip when a revision was pushed, and how rozed knows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BaseTip {
    /// The tip's SHA.
    pub commit: String,
    pub source: BaseTipSource,
}

/// How [`base_tip_at`] knows a tip. All but [`BaseTipSource::Uncertain`] give the revision's base
/// as it was, though [`BaseTipSource::ForcePush`] and [`BaseTipSource::Seen`] can be a push or more
/// behind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BaseTipSource {
    /// The first parent of the merge commit that merged the pull request into the branch it
    /// targeted then, which only moved forward since.
    MergeParent,
    /// The branch's tip just before the push that merged the pull request into it, from the
    /// activity API: the push's `before`. The branch only moved forward since.
    BeforeMerge,
    /// The activity API's latest push to the branch at or before the revision's. Exact.
    Activity,
    /// The branch's tip now, since it only moved forward after the revision, as far as any source
    /// says, and doesn't hold the pull request's commits. Its merge base with the revision's head
    /// is the same as the tip then. For a branch the pull request no longer targets, its latest
    /// push the activity API read.
    Now,
    /// The latest force-push of the branch at or before the revision, from its timeline event.
    /// The branch can have moved forward after it.
    ForcePush,
    /// A tip rozed saw at or before the revision's push. The branch can have moved forward after
    /// it.
    Seen,
    /// The branch's tip now, or the pull request's base branch's when that isn't known, though
    /// the branch was force-pushed after the revision, or now holds the pull request's commits,
    /// and nothing says where it was then; or a tip that holds the revision's head. Its base can
    /// be wrong, so the caller says so.
    Uncertain,
}

/// The tip of the pull request's base branch when a revision was pushed at `pushed_at`, or
/// `None` when nothing names one.
///
/// - **The branch then.** Retargets after the push name the branch the pull request targeted
///   then. A push no source dates is taken as the latest.
/// - **Merged,** into the branch the push targeted, when that branch only moved forward since:
///   with a merge commit (its second parent the head), its first parent; otherwise the `before`
///   of the activity API's push that merged it (whose `after` is the merge commit or the head).
/// - **From the activity API:** its latest push to the branch at or before the revision's, which
///   is exact.
/// - **Moving forward only:** the branch's tip now, when no source says it was force-pushed,
///   deleted or re-created since, and it doesn't hold the pull request's commits. For a branch
///   the pull request no longer targets, that's its latest push the activity API read.
/// - **Otherwise,** the latest force-push event or tip rozed saw at or before the push, and failing
///   those, the branch's tip now, flagged [`BaseTipSource::Uncertain`].
/// - **Never one holding the revision.** `contains(tip, head)` says whether commit `tip` has the
///   revision's head, `head`, in its history (`Repo::is_ancestor(head, tip)` in the local copy).
///   A tip that does is no base for the revision: its branch took in the pull request's commits,
///   as when git-flow merges `main` back into `develop` after a pull request retargeted from
///   `develop` merged into `main`. It's flagged [`BaseTipSource::Uncertain`].
pub fn base_tip_at(
    history: &BaseHistory,
    pushed_at: PushedAt,
    head: &str,
    contains: impl Fn(&str, &str) -> bool,
) -> Option<BaseTip> {
    let mut tip = tip_at(history, pushed_at)?;
    if contains(&tip.commit, head) {
        tip.source = BaseTipSource::Uncertain;
    }
    Some(tip)
}

/// [`base_tip_at`], before the check that the tip doesn't hold the revision.
fn tip_at(history: &BaseHistory, pushed_at: PushedAt) -> Option<BaseTip> {
    let at = match pushed_at {
        PushedAt::Exact(at) | PushedAt::By(at) => Some(at),
        PushedAt::Unknown => None,
    };
    let branch = branch_at(history, at);
    let today = branch == history.branch;
    let tip = |commit: &str, source| {
        Some(BaseTip {
            commit: commit.to_string(),
            source,
        })
    };
    let forward_only = !moved_back_after(history, branch, at);
    // A merge is into today's base branch.
    let merge = history.merge.as_ref().filter(|_| today);
    if forward_only && let Some(merge) = merge {
        if merge.with_merge_commit() {
            return tip(&merge.parents[0], BaseTipSource::MergeParent);
        }
        if let Some(before) = before_merge(history, branch, merge) {
            return tip(before, BaseTipSource::BeforeMerge);
        }
    }
    if let Some(after) = latest(history, branch, at, activity_tip) {
        return tip(after, BaseTipSource::Activity);
    }
    let now = match today {
        true => history.tip.as_deref().and_then(sha),
        false => latest(history, branch, None, activity_tip),
    };
    let holds_the_pull_request = merge.is_some_and(Merge::holds_the_pull_request);
    if forward_only
        && !holds_the_pull_request
        && let Some(now) = now
    {
        return tip(now, BaseTipSource::Now);
    }
    if let Some(after) = latest(history, branch, at, |record| match record {
        BaseRecord::ForcePush { after, .. } => Some(after.as_deref().and_then(sha)),
        _ => None,
    }) {
        return tip(after, BaseTipSource::ForcePush);
    }
    if let Some(seen) = latest(history, branch, at, |record| match record {
        BaseRecord::Seen { tip, .. } => Some(sha(tip)),
        _ => None,
    }) {
        return tip(seen, BaseTipSource::Seen);
    }
    (now.or(history.tip.as_deref().and_then(sha)))
        .and_then(|now| tip(now, BaseTipSource::Uncertain))
}

/// The tip an activity API's push left, or `Some(None)` for a deletion.
fn activity_tip(record: &BaseRecord) -> Option<Option<&str>> {
    match record {
        BaseRecord::Activity { after, .. } => Some(after.as_deref().and_then(sha)),
        _ => None,
    }
}

/// The branch's tip just before the activity API's push that merged the pull request into it.
fn before_merge<'a>(history: &'a BaseHistory, branch: &str, merge: &Merge) -> Option<&'a str> {
    (history.records.iter())
        .filter_map(|record| match record {
            BaseRecord::Activity {
                branch: pushed,
                before,
                after: Some(after),
                at,
                ..
            } if pushed == branch && (*after == merge.commit || *after == merge.head) => {
                Some((*at, before.as_deref().and_then(sha)?))
            }
            _ => None,
        })
        .max_by_key(|(at, _)| *at)
        .map(|(_, before)| before)
}

/// The base branch the pull request targeted at `at`: the branch the first retarget after it
/// left, or today's when none came after. `None` is now.
fn branch_at(history: &BaseHistory, at: Option<SystemTime>) -> &str {
    let Some(at) = at else {
        return &history.branch;
    };
    (history.retargets.iter())
        .filter(|retarget| retarget.at > at)
        .min_by_key(|retarget| retarget.at)
        .map_or(&history.branch, |retarget| &retarget.from)
}

/// The branch a record is about. A force-push event is about the branch the pull request
/// targeted when it happened.
fn record_branch<'a>(history: &'a BaseHistory, record: &'a BaseRecord) -> &'a str {
    match record {
        BaseRecord::Activity { branch, .. } | BaseRecord::Seen { branch, .. } => branch,
        BaseRecord::ForcePush { .. } => branch_at(history, Some(record_time(record))),
    }
}

/// When a record's tip was there from: a force-push event's push was up to [`EVENT_LAG`] before
/// it.
fn record_time(record: &BaseRecord) -> SystemTime {
    match record {
        BaseRecord::Activity { at, .. } | BaseRecord::Seen { at, .. } => *at,
        BaseRecord::ForcePush { at, .. } => at.checked_sub(EVENT_LAG).unwrap_or(*at),
    }
}

/// Whether a source says `branch` moved anywhere but forward after `at`: a force-push, or the
/// branch deleted or created. Nothing comes after `None`, which is now.
fn moved_back_after(history: &BaseHistory, branch: &str, at: Option<SystemTime>) -> bool {
    let Some(at) = at else {
        return false;
    };
    history.records.iter().any(|record| {
        let moved_back = match record {
            BaseRecord::Activity {
                before,
                after,
                forced,
                ..
            } => {
                *forced
                    || before.as_deref().and_then(sha).is_none()
                    || after.as_deref().and_then(sha).is_none()
            }
            BaseRecord::ForcePush { .. } => true,
            BaseRecord::Seen { .. } => false,
        };
        moved_back && record_time(record) > at && record_branch(history, record) == branch
    })
}

/// The tip `tip_of` reads from the latest record about `branch` at or before `at`, among the
/// records it reads one from (`Some`). A record that names no commit (`Some(None)`), such as a
/// deletion, is a time the branch had no tip. Ties go to the later record.
fn latest<'a>(
    history: &'a BaseHistory,
    branch: &str,
    at: Option<SystemTime>,
    tip_of: impl Fn(&'a BaseRecord) -> Option<Option<&'a str>>,
) -> Option<&'a str> {
    (history.records.iter())
        .filter(|record| at.is_none_or(|at| record_time(record) <= at))
        .filter(|record| record_branch(history, record) == branch)
        .filter_map(|record| Some((record_time(record), tip_of(record)?)))
        .max_by_key(|(time, _)| *time)
        .and_then(|(_, tip)| tip)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    const NO_COMMIT: &str = "0000000000000000000000000000000000000000";

    /// `minute` minutes into a day in 2026.
    fn at(minute: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000 + minute * 60)
    }

    fn pushed(minute: u64) -> PushedAt {
        PushedAt::Exact(at(minute))
    }

    fn push(branch: &str, before: &str, after: &str, minute: u64) -> BaseRecord {
        BaseRecord::Activity {
            branch: branch.into(),
            before: Some(before.into()),
            after: Some(after.into()),
            at: at(minute),
            forced: false,
        }
    }

    fn force(branch: &str, before: &str, after: &str, minute: u64) -> BaseRecord {
        BaseRecord::Activity {
            branch: branch.into(),
            before: Some(before.into()),
            after: Some(after.into()),
            at: at(minute),
            forced: true,
        }
    }

    fn force_event(after: &str, minute: u64) -> BaseRecord {
        BaseRecord::ForcePush {
            after: Some(after.into()),
            at: at(minute),
        }
    }

    fn seen(branch: &str, tip: &str, minute: u64) -> BaseRecord {
        BaseRecord::Seen {
            branch: branch.into(),
            tip: tip.into(),
            at: at(minute),
        }
    }

    fn retarget(from: &str, to: &str, minute: u64) -> Retarget {
        Retarget {
            from: from.into(),
            to: to.into(),
            at: at(minute),
        }
    }

    /// A pull request into `main`, whose tip is `m9` now.
    fn into_main(records: Vec<BaseRecord>) -> BaseHistory {
        BaseHistory {
            branch: "main".into(),
            tip: Some("m9".into()),
            records,
            ..BaseHistory::default()
        }
    }

    /// The tip for a revision whose head no tip holds.
    fn found(history: &BaseHistory, pushed_at: PushedAt) -> Option<(String, BaseTipSource)> {
        base_tip_at(history, pushed_at, "h1", |_, _| false).map(|tip| (tip.commit, tip.source))
    }

    /// `minute` minutes and `second` seconds into the day.
    fn at_second(minute: u64, second: u64) -> SystemTime {
        at(minute) + Duration::from_secs(second)
    }

    fn tip(commit: &str, source: BaseTipSource) -> Option<(String, BaseTipSource)> {
        Some((commit.to_string(), source))
    }

    #[test]
    fn a_base_that_only_moved_forward_is_its_tip_now() {
        let history = into_main(vec![]);

        assert_eq!(found(&history, pushed(10)), tip("m9", BaseTipSource::Now));
        assert_eq!(
            found(&history, PushedAt::By(at(10))),
            tip("m9", BaseTipSource::Now)
        );
        assert_eq!(
            found(&history, PushedAt::Unknown),
            tip("m9", BaseTipSource::Now)
        );
    }

    #[test]
    fn the_activity_api_gives_the_tip_at_the_push() {
        let history = into_main(vec![
            push("main", "m1", "m2", 5),
            push("main", "m2", "m3", 15),
            push("main", "m3", "m9", 25),
        ]);

        assert_eq!(
            found(&history, pushed(20)),
            tip("m3", BaseTipSource::Activity)
        );
        // A push at the same second as the base's counts it.
        assert_eq!(
            found(&history, pushed(15)),
            tip("m3", BaseTipSource::Activity)
        );
        // Before the earliest entry read, today's tip will do.
        assert_eq!(found(&history, pushed(1)), tip("m9", BaseTipSource::Now));
    }

    fn merged(commit: &str, parents: &[&str], head: &str) -> Option<Merge> {
        Some(Merge {
            commit: commit.into(),
            parents: parents.iter().map(|parent| parent.to_string()).collect(),
            head: head.into(),
        })
    }

    #[test]
    fn a_pull_request_merged_with_a_merge_commit_takes_its_first_parent() {
        let mut history = into_main(vec![push("main", "m1", "m2", 5)]);
        history.merge = merged("mc", &["m8", "h3"], "h3");

        assert_eq!(
            found(&history, pushed(10)),
            tip("m8", BaseTipSource::MergeParent)
        );
    }

    #[test]
    fn a_squash_or_rebase_merge_takes_the_tip_before_the_merge_or_today_s() {
        // Squashed, or rebased, into new commits: the activity API's merge says where main was.
        for merge in [merged("sq", &["m8"], "h3"), merged("rb3", &["rb2"], "h3")] {
            let commit = merge.as_ref().unwrap().commit.clone();
            let mut history = into_main(vec![
                push("main", "m1", "m2", 5),
                push("main", "m8", &commit, 50),
            ]);
            history.merge = merge.clone();
            assert_eq!(
                found(&history, pushed(10)),
                tip("m8", BaseTipSource::BeforeMerge)
            );

            // Without it, main doesn't hold the pull request's commits, so today's tip does.
            let mut history = into_main(vec![]);
            history.merge = merge;
            assert_eq!(found(&history, pushed(10)), tip("m9", BaseTipSource::Now));
        }
    }

    #[test]
    fn a_fast_forward_takes_the_tip_before_it_and_without_it_is_uncertain() {
        // The head itself was pushed to main, so main holds every revision.
        let mut history = into_main(vec![
            push("main", "m1", "m2", 5),
            push("main", "m2", "m8", 30),
            push("main", "m8", "h3", 50),
        ]);
        history.merge = merged("h3", &["h2"], "h3");
        assert_eq!(
            found(&history, pushed(10)),
            tip("m8", BaseTipSource::BeforeMerge)
        );

        // Past the pushes read, today's tip would make every revision's diff empty.
        let mut history = into_main(vec![]);
        history.merge = merged("h3", &["h2"], "h3");
        assert_eq!(
            found(&history, pushed(10)),
            tip("m9", BaseTipSource::Uncertain)
        );
    }

    #[test]
    fn a_fast_forwarded_head_that_is_a_merge_commit_is_no_merge_parent() {
        // The author merged main into the branch, and the head was then pushed to main: the
        // head's first parent is the pull request's own commit.
        let mut history = into_main(vec![push("main", "m8", "h3", 50)]);
        history.merge = merged("h3", &["h2", "m7"], "h3");

        assert_eq!(
            found(&history, pushed(10)),
            tip("m8", BaseTipSource::BeforeMerge)
        );
        history.records.clear();
        assert_eq!(
            found(&history, pushed(10)),
            tip("m9", BaseTipSource::Uncertain)
        );
    }

    #[test]
    fn a_merge_parent_is_no_good_once_the_base_was_force_pushed() {
        let mut history = into_main(vec![
            push("main", "m1", "m2", 5),
            force("main", "m2", "n1", 30),
        ]);
        history.merge = merged("mc", &["m8", "h3"], "h3");

        assert_eq!(
            found(&history, pushed(10)),
            tip("m2", BaseTipSource::Activity)
        );
        // After the force-push, the base only moved forward again.
        assert_eq!(
            found(&history, pushed(40)),
            tip("m8", BaseTipSource::MergeParent)
        );
    }

    #[test]
    fn a_stack_whose_lower_layer_was_force_pushed_takes_the_lower_tip_then() {
        // The pull request targets `lower`, the branch of the pull request below, whose author
        // amended it at minute 30 and pushed again at 40.
        let history = BaseHistory {
            branch: "lower".into(),
            tip: Some("l3".into()),
            records: vec![
                push("lower", NO_COMMIT, "l1", 5),
                force("lower", "l1", "l2", 30),
                push("lower", "l2", "l3", 40),
            ],
            ..BaseHistory::default()
        };

        assert_eq!(
            found(&history, pushed(10)),
            tip("l1", BaseTipSource::Activity)
        );
        assert_eq!(
            found(&history, pushed(35)),
            tip("l2", BaseTipSource::Activity)
        );
        assert_eq!(
            found(&history, pushed(45)),
            tip("l3", BaseTipSource::Activity)
        );
    }

    #[test]
    fn without_the_activity_api_a_force_push_event_dates_the_lower_tip() {
        let history = BaseHistory {
            branch: "lower".into(),
            tip: Some("l3".into()),
            records: vec![force_event("l2", 30), force_event("l3", 50)],
            ..BaseHistory::default()
        };

        assert_eq!(
            found(&history, pushed(35)),
            tip("l2", BaseTipSource::ForcePush)
        );
        // Before any record, with a force-push since, nothing says where it was.
        assert_eq!(
            found(&history, pushed(10)),
            tip("l3", BaseTipSource::Uncertain)
        );
        // After the last force-push, it only moved forward.
        assert_eq!(found(&history, pushed(55)), tip("l3", BaseTipSource::Now));
    }

    #[test]
    fn a_force_push_event_whose_commit_github_lost_names_no_tip() {
        let history = BaseHistory {
            branch: "lower".into(),
            tip: Some("l3".into()),
            records: vec![
                force_event("l1", 10),
                BaseRecord::ForcePush {
                    after: None,
                    at: at(30),
                },
                force_event("l3", 50),
            ],
            ..BaseHistory::default()
        };

        assert_eq!(
            found(&history, pushed(35)),
            tip("l3", BaseTipSource::Uncertain)
        );
    }

    #[test]
    fn a_tip_rozed_saw_is_the_last_resort_before_today_s() {
        let history = BaseHistory {
            branch: "lower".into(),
            tip: Some("l3".into()),
            records: vec![
                seen("lower", "l1", 12),
                seen("lower", "l2", 33),
                force_event("l3", 50),
                // Another branch's tips say nothing about this one.
                seen("elsewhere", "e1", 34),
            ],
            ..BaseHistory::default()
        };

        assert_eq!(found(&history, pushed(36)), tip("l2", BaseTipSource::Seen));
        // A force-push event outranks a sighting.
        let mut with_event = history.clone();
        with_event.records.push(force_event("k1", 20));
        assert_eq!(
            found(&with_event, pushed(25)),
            tip("k1", BaseTipSource::ForcePush)
        );
    }

    #[test]
    fn a_retargeted_pull_request_takes_the_branch_it_targeted_then() {
        // Stacked on `lower` until GitHub retargeted it to `main` at minute 30, when `lower` was
        // merged and deleted.
        let mut history = into_main(vec![
            push("lower", NO_COMMIT, "l1", 5),
            push("lower", "l1", "l2", 15),
            push("lower", "l2", NO_COMMIT, 29),
            push("main", "m1", "m2", 2),
        ]);
        history.retargets = vec![retarget("lower", "main", 30)];

        assert_eq!(
            found(&history, pushed(20)),
            tip("l2", BaseTipSource::Activity)
        );
        assert_eq!(
            found(&history, pushed(31)),
            tip("m2", BaseTipSource::Activity)
        );
    }

    #[test]
    fn retargets_are_read_in_time_order() {
        let mut history = into_main(vec![
            seen("a", "a1", 1),
            seen("b", "b1", 11),
            seen("main", "m1", 21),
        ]);
        history.tip = None;
        history.retargets = vec![retarget("b", "main", 20), retarget("a", "b", 10)];

        assert_eq!(found(&history, pushed(5)), tip("a1", BaseTipSource::Seen));
        assert_eq!(found(&history, pushed(15)), tip("b1", BaseTipSource::Seen));
        assert_eq!(found(&history, pushed(25)), tip("m1", BaseTipSource::Seen));
    }

    #[test]
    fn an_old_base_that_only_moved_forward_is_its_latest_tip_read() {
        // Retargeted from a busy `develop` at minute 100; its pushes were read back to minute 50
        // only.
        let mut history = into_main(vec![
            push("develop", "d1", "d2", 50),
            push("develop", "d2", "d3", 90),
        ]);
        history.retargets = vec![retarget("develop", "main", 100)];

        assert_eq!(found(&history, pushed(20)), tip("d3", BaseTipSource::Now));
        assert_eq!(
            found(&history, pushed(60)),
            tip("d2", BaseTipSource::Activity)
        );
        // Force-pushed since, it's uncertain, with its own latest tip.
        history.records.push(force("develop", "d3", "e1", 95));
        assert_eq!(
            found(&history, pushed(20)),
            tip("e1", BaseTipSource::Uncertain)
        );
    }

    #[test]
    fn an_old_base_that_took_in_the_pull_request_later_is_uncertain() {
        // Opened against `develop`, retargeted to `main` at minute 100 and merged there; `main`
        // was then merged back into `develop`, whose tip now holds the pull request's head h1.
        let mut history = into_main(vec![push("develop", "d1", "d9", 150)]);
        history.retargets = vec![retarget("develop", "main", 100)];
        let holds_h1 = |tip: &str, head: &str| tip == "d9" && head == "h1";

        let tip = base_tip_at(&history, pushed(20), "h1", holds_h1).unwrap();
        assert_eq!(
            (tip.commit.as_str(), tip.source),
            ("d9", BaseTipSource::Uncertain)
        );
        // A tip that doesn't hold the head stands.
        let tip = base_tip_at(&history, pushed(20), "h0", holds_h1).unwrap();
        assert_eq!(
            (tip.commit.as_str(), tip.source),
            ("d9", BaseTipSource::Now)
        );
    }

    #[test]
    fn a_force_push_event_counts_from_a_few_seconds_before_it() {
        // The layer below was force-pushed to l2 at about 10:00:00; GitHub made its event at
        // 10:00:05, and this layer was pushed onto it at 10:00:02. A later push moved the base on.
        let history = BaseHistory {
            branch: "lower".into(),
            tip: Some("l3".into()),
            records: vec![
                BaseRecord::ForcePush {
                    after: Some("l2".into()),
                    at: at_second(10, 5),
                },
                BaseRecord::ForcePush {
                    after: Some("l3".into()),
                    at: at_second(20, 0),
                },
            ],
            ..BaseHistory::default()
        };
        let found_at = |second| found(&history, PushedAt::Exact(at_second(10, second)));

        assert_eq!(found_at(2), tip("l2", BaseTipSource::ForcePush));
        // Exactly the lag before the event counts; a second earlier doesn't.
        assert_eq!(found_at(0), tip("l2", BaseTipSource::ForcePush));
        assert_eq!(
            found(&history, PushedAt::Exact(at(10) - Duration::from_secs(1))),
            tip("l3", BaseTipSource::Uncertain)
        );
        assert_eq!(EVENT_LAG, Duration::from_secs(5));
    }

    #[test]
    fn a_retargeted_pull_request_with_no_record_of_its_old_base_is_uncertain() {
        let mut history = into_main(vec![]);
        history.retargets = vec![retarget("lower", "main", 30)];

        assert_eq!(
            found(&history, pushed(20)),
            tip("m9", BaseTipSource::Uncertain)
        );
        assert_eq!(found(&history, pushed(40)), tip("m9", BaseTipSource::Now));
    }

    #[test]
    fn a_force_push_event_belongs_to_the_branch_targeted_then() {
        // `lower`'s force-push at minute 10 happened while the pull request targeted it, so it
        // says nothing about `main`.
        let mut history = into_main(vec![force_event("l2", 10)]);
        history.retargets = vec![retarget("lower", "main", 30)];

        assert_eq!(
            found(&history, pushed(20)),
            tip("l2", BaseTipSource::ForcePush)
        );
        assert_eq!(
            found(&history, pushed(5)),
            tip("m9", BaseTipSource::Uncertain)
        );
        assert_eq!(found(&history, pushed(40)), tip("m9", BaseTipSource::Now));
    }

    #[test]
    fn a_base_deleted_or_recreated_since_isnt_moving_forward() {
        let deleted = into_main(vec![push("main", "m1", NO_COMMIT, 30)]);
        assert_eq!(
            found(&deleted, pushed(10)),
            tip("m9", BaseTipSource::Uncertain)
        );

        let recreated = into_main(vec![push("main", NO_COMMIT, "m9", 30)]);
        assert_eq!(
            found(&recreated, pushed(10)),
            tip("m9", BaseTipSource::Uncertain)
        );
    }

    #[test]
    fn records_at_the_same_second_go_by_their_order() {
        let history = into_main(vec![
            push("main", "m1", "m2", 5),
            push("main", "m2", "m3", 5),
        ]);

        assert_eq!(
            found(&history, pushed(5)),
            tip("m3", BaseTipSource::Activity)
        );
    }

    #[test]
    fn with_nothing_known_there_is_no_tip() {
        let history = BaseHistory {
            branch: "main".into(),
            ..BaseHistory::default()
        };

        assert_eq!(found(&history, pushed(10)), None);
        assert_eq!(
            found(
                &BaseHistory {
                    tip: Some(NO_COMMIT.into()),
                    ..history
                },
                pushed(10)
            ),
            None
        );
    }
}
