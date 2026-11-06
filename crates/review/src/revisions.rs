//! A pull request's revisions: one per push to its branch (design §6, "Revisions").
//!
//! No single GitHub source lists every push. The repository activity API does when it's available;
//! otherwise force-push events, reviews, check suites and rozed's own poller each saw some heads.
//! [`build_revisions`] merges what they saw into one list, keyed by head SHA and ordered oldest
//! first. Revisions are stored by head SHA and numbered r1, r2… only for display, because a source
//! that reports late can insert one.

mod build;

use std::collections::BTreeSet;
use std::time::SystemTime;

pub use build::build_revisions;

/// Something a source saw on the pull request's head branch. Each variant's doc names the GitHub
/// data it comes from.
///
/// SHAs are full commit SHAs, as GitHub and git print them. GitHub's all-zero SHA, which stands for
/// "no commit" when a push creates the branch, reads as `None`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PushRecord {
    /// A push in the repository activity API for the head branch
    /// (`GET /repos/{owner}/{repo}/activity?ref=refs/heads/{branch}`): `before`, `after`,
    /// `timestamp`, and whether `activity_type` is `force_push`. Exact. The caller keeps the
    /// entries from the one that was the head when the pull request opened onwards, and drops
    /// `branch_deletion`.
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
    /// A review's commit (`PullRequestReview.commit`) and `submittedAt`. Reviews made in GitHub's
    /// web UI or in rozed are on the head, so the commit was pushed by then. A review the API made
    /// on an older commit adds that commit as a revision.
    Review { commit: String, at: SystemTime },
    /// The push that started a check suite (`CheckSuite.push`): `previousSha`, `nextSha`, and the
    /// suite's `createdAt`, up to a minute after the push.
    CheckSuite {
        previous: Option<String>,
        next: String,
        at: SystemTime,
    },
    /// A head rozed's poller saw (`head.sha`), and when it first saw it.
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
    /// rozed's poller.
    Poller,
    /// git: the pull request's head in rozed's local copy, the last of its current commits.
    Git,
}

/// When a revision was pushed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PushedAt {
    /// The push's time, from the activity API, or else a force-push event.
    Exact(SystemTime),
    /// It was pushed at or before this time: the earliest a review, check suite or rozed's poller
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
    pub previous: Option<String>,
    /// Whether the push was forced: `Some(true)` for a force-push, `Some(false)` for a normal push,
    /// `None` when no source says and the current commits can't tell.
    pub forced: Option<bool>,
    /// When it was pushed.
    pub pushed_at: PushedAt,
    /// The sources that saw this head.
    pub sources: BTreeSet<Source>,
}

/// A pull request's revisions, oldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Revisions {
    list: Vec<Revision>,
}

impl Revisions {
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

    /// The newest revision: the pull request's head.
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

    fn position(&self, head: &str) -> Option<usize> {
        self.list.iter().position(|revision| revision.head == head)
    }
}

/// The SHA, or `None` for GitHub's all-zero "no commit" SHA.
fn sha(value: &str) -> Option<&str> {
    (!value.is_empty() && !value.bytes().all(|byte| byte == b'0')).then_some(value)
}
