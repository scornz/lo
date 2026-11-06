//! A pull request's revisions, one per push to its branch, and what to compare when it opens
//! (design §6, "Revisions" and "Viewed marks").
//!
//! No single GitHub source lists every push. The repository activity API does when it's available;
//! otherwise force-push events, reviews, check suites and Lo's own poller each saw some heads.
//! [`build_revisions`] merges what they saw into one list, keyed by head SHA and ordered oldest
//! first. Revisions are stored by head SHA and numbered r1, r2… only for display, because a source
//! that reports late can insert one. [`default_comparison`] picks what the pull request compares
//! when it opens, and [`viewed_file_comparison`] what a file shows once a push changed it after you
//! marked it viewed.
//!
//! ```
//! use std::time::{Duration, SystemTime};
//! use review::revisions::{Comparison, PushRecord, build_revisions, default_comparison};
//!
//! let at = |minute: u64| SystemTime::UNIX_EPOCH + Duration::from_secs(minute * 60);
//! let records = [
//!     PushRecord::Polled { head: "a1".into(), at: at(10) },
//!     // You reviewed a2, which a force-push then replaced with a rebased b1.
//!     PushRecord::Review { commit: "a2".into(), at: at(25) },
//!     PushRecord::ForcePush { before: Some("a2".into()), after: "b1".into(), at: at(30) },
//! ];
//! let revisions = build_revisions(&records, &["b0".into(), "b1".into()], "b1", at(0));
//!
//! let heads: Vec<_> = revisions.iter().map(|revision| revision.head.as_str()).collect();
//! assert_eq!(heads, ["a1", "a2", "b1"]);
//! assert_eq!(revisions.label("b1").as_deref(), Some("r3"));
//! assert_eq!(revisions.get("b1").unwrap().forced, Some(true));
//! assert_eq!(
//!     default_comparison(&revisions, Some("a2"), None),
//!     Comparison::Between { from: "a2".into(), to: "b1".into() },
//! );
//! ```

mod build;
mod compare;
mod viewed;

use std::collections::{BTreeSet, HashMap};
use std::time::SystemTime;

pub use build::build_revisions;
pub use compare::{Comparison, default_comparison};
pub use viewed::{FileComparison, ViewedState, viewed_file_comparison};

/// Something a source saw on the pull request's head branch. Each variant's doc names the GitHub
/// data it comes from.
///
/// SHAs are full commit SHAs, as GitHub and git print them. GitHub's all-zero SHA, which stands for
/// "no commit" when a push creates the branch, reads as `None`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PushRecord {
    /// A push in the repository activity API for the head branch
    /// (`GET /repos/{owner}/{repo}/activity?ref=refs/heads/{branch}`): `before`, `after`,
    /// `timestamp`, and whether `activity_type` is `force_push`. Exact. A `branch_creation`
    /// entry's `before` is the all-zero SHA, and its `forced` is ignored. The caller drops
    /// `branch_deletion` entries; entries from before the pull request opened are dropped here.
    Activity {
        before: Option<String>,
        after: String,
        at: SystemTime,
        forced: bool,
    },
    /// A `HeadRefForcePushedEvent` in the timeline: `beforeCommit`, `afterCommit` and `createdAt`,
    /// a few seconds after the push. `before` is `None` when GitHub no longer has the commit.
    ForcePush {
        before: Option<String>,
        after: String,
        at: SystemTime,
    },
    /// A submitted review's commit (`PullRequestReview.commit`) and `submittedAt`. The commit is
    /// always a revision, since it was the head when the reviewer looked; a review the API made on
    /// an older commit makes that commit one. Pending reviews have no `submittedAt` and don't
    /// count.
    Review { commit: String, at: SystemTime },
    /// The push that started a check suite (`CheckSuite.push`): `previousSha`, `nextSha`, and the
    /// suite's `createdAt`, up to a minute after the push. The caller drops suites whose `branch`
    /// is set and isn't the head branch; it's null once the branch is deleted. A suite whose
    /// previous SHA isn't a head another record names or a current commit, such as one for the same
    /// commit pushed to a backport branch, never names the previous head.
    CheckSuite {
        previous: Option<String>,
        next: String,
        at: SystemTime,
    },
    /// A head Lo's poller saw (`head.sha`), and when it first saw it.
    Polled { head: String, at: SystemTime },
}

impl PushRecord {
    /// The source the record came from.
    pub fn source(&self) -> Source {
        match self {
            Self::Activity { .. } => Source::Activity,
            Self::ForcePush { .. } => Source::ForcePush,
            Self::Review { .. } => Source::Review,
            Self::CheckSuite { .. } => Source::CheckSuite,
            Self::Polled { .. } => Source::Poller,
        }
    }
}

/// A source that saw a revision's head.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Source {
    /// The repository activity API.
    Activity,
    /// A force-push event, as the head before or after it.
    ForcePush,
    /// A review on the commit.
    Review,
    /// A check suite started by the push.
    CheckSuite,
    /// Lo's poller.
    Poller,
    /// The pull request's current head (`headRefOid`), which is always the latest revision.
    Head,
}

/// When a revision was pushed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PushedAt {
    /// The push's time, from the activity API, or else a force-push event.
    Exact(SystemTime),
    /// It was pushed at or before this time: the earliest a review, check suite or Lo's poller
    /// saw the head, or a later head's push time.
    By(SystemTime),
    /// No source dates it.
    Unknown,
}

/// One push to the pull request's branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Revision {
    /// The head commit's SHA. Revisions are keyed by it.
    pub head: String,
    /// The branch's head just before this push. A source's `before` or `previousSha` when one
    /// names it, else the revision before this one. `None` for the first revision unless a source
    /// names the head it replaced, which can be a commit pushed before the pull request opened.
    /// It can name a later revision: when a head was pushed again (`a` → `b` → `a`), `b`'s
    /// previous head is `a`, which sits after it at its latest push.
    pub previous: Option<String>,
    /// Whether the push was forced: `Some(true)` for a force-push, `Some(false)` for a normal push,
    /// `None` when no source says and the current commits can't tell.
    pub forced: Option<bool>,
    /// When it was pushed.
    pub pushed_at: PushedAt,
    /// The sources that saw this head.
    pub sources: BTreeSet<Source>,
    /// Whether Lo can still get the head commit. Revisions start out available; the caller
    /// flags one GitHub no longer has, and that Lo never fetched, with
    /// [`Revisions::mark_unavailable`]. It stays in the list, shown as "no longer available".
    pub available: bool,
}

/// A pull request's revisions, oldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Revisions {
    list: Vec<Revision>,
    /// Each revision's position in `list`, by head.
    positions: HashMap<String, usize>,
}

impl Revisions {
    fn new(list: Vec<Revision>) -> Self {
        let positions = (list.iter().enumerate())
            .map(|(position, revision)| (revision.head.clone(), position))
            .collect();
        Self { list, positions }
    }

    /// The revisions, oldest first.
    pub fn as_slice(&self) -> &[Revision] {
        &self.list
    }

    /// The revisions, oldest first.
    pub fn iter(&self) -> std::slice::Iter<'_, Revision> {
        self.list.iter()
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// The newest revision, which is always the pull request's head passed to [`build_revisions`].
    pub fn latest(&self) -> Option<&Revision> {
        self.list.last()
    }

    /// The revision with this head.
    pub fn get(&self, head: &str) -> Option<&Revision> {
        self.position(head).map(|index| &self.list[index])
    }

    /// The revision's display number: 1 for the oldest. It can change when a source reports a
    /// revision late, so store head SHAs, not numbers.
    pub fn number(&self, head: &str) -> Option<usize> {
        self.position(head).map(|index| index + 1)
    }

    /// The revision's display label: `r1` for the oldest.
    pub fn label(&self, head: &str) -> Option<String> {
        self.number(head).map(|number| format!("r{number}"))
    }

    /// The revision a marker points at: your last review's commit, the head last seen here, or the
    /// head a file was marked viewed at. Each is a revision's head when the records that saw it
    /// were passed to [`build_revisions`], as a review's always is; any other commit finds nothing.
    pub fn find(&self, commit: &str) -> Option<&Revision> {
        self.get(commit)
    }

    /// Flags the revision with this head as one GitHub no longer has. Returns whether there was
    /// one.
    pub fn mark_unavailable(&mut self, head: &str) -> bool {
        let Some(index) = self.position(head) else {
            return false;
        };
        self.list[index].available = false;
        true
    }

    fn position(&self, head: &str) -> Option<usize> {
        self.positions.get(head).copied()
    }

    /// The nearest revision at or before `index` that Lo can still get.
    fn available_from(&self, index: usize) -> Option<&Revision> {
        self.list[..=index]
            .iter()
            .rev()
            .find(|revision| revision.available)
    }
}

/// The SHA, or `None` for GitHub's all-zero "no commit" SHA.
fn sha(value: &str) -> Option<&str> {
    (!value.is_empty() && !value.bytes().all(|byte| byte == b'0')).then_some(value)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::*;

    fn polled(head: &str, minute: u64) -> PushRecord {
        PushRecord::Polled {
            head: head.to_string(),
            at: SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000 + minute * 60),
        }
    }

    /// Revisions `a1` and `a2` over the current commits `c1 a1 c3 c4 a2`, and `x1`, rebased away.
    fn revisions() -> Revisions {
        let commits = ["c1", "a1", "c3", "c4", "a2"].map(String::from);
        build_revisions(
            &[polled("x1", 1), polled("a1", 10), polled("a2", 20)],
            &commits,
            "a2",
            SystemTime::UNIX_EPOCH,
        )
    }

    fn found<'a>(revisions: &'a Revisions, commit: &str) -> Option<&'a str> {
        revisions
            .find(commit)
            .map(|revision| revision.head.as_str())
    }

    #[test]
    fn revisions_start_out_available() {
        assert!(revisions().iter().all(|revision| revision.available));
    }

    #[test]
    fn the_caller_flags_a_revision_github_no_longer_has() {
        let mut revisions = revisions();

        assert!(revisions.mark_unavailable("x1"));
        assert!(!revisions.get("x1").unwrap().available);
        assert!(revisions.get("a1").unwrap().available);
        assert!(!revisions.mark_unavailable("ffff"));
    }

    #[test]
    fn a_marker_on_a_head_finds_its_revision() {
        let revisions = revisions();

        assert_eq!(found(&revisions, "a1"), Some("a1"));
        assert_eq!(found(&revisions, "x1"), Some("x1"));
    }

    #[test]
    fn a_marker_on_a_commit_no_revision_has_finds_nothing() {
        let revisions = revisions();

        assert_eq!(found(&revisions, "c1"), None);
        assert_eq!(found(&revisions, "c3"), None);
        assert_eq!(found(&revisions, "ffff"), None);
    }
}
