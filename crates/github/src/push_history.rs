//! A pull request's push history (design §6, "Revisions"): what GitHub says about the pushes to
//! its head branch, and about where its base branch was over time. `review::revisions` builds the
//! pull request's revisions from it ([`PushHistory::push_records`]) and finds each one's base
//! ([`PushHistory::base_history`]).
//!
//! It's read from GraphQL's `PullRequestRevisions` query, and from the activity API for the head
//! branch, in the head's repository, which can be a fork, and for each branch the pull request
//! targeted. The open pull request's poller keeps it in the cache and current
//! ([`crate::poll::Poller::push_history`]).

use std::sync::Arc;
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use http_client::http::StatusCode;
use http_client::{Method, Url};
use review::revisions::{BaseHistory, BaseRecord, Merge, PushRecord, Retarget};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::activity::{ActivityEntry, BranchActivity, Direction};
use crate::cache::{CacheEntry, schema_shape};
use crate::client::{API_URL, API_VERSION, GithubClient, RestResponse};
use crate::error::GithubError;
use crate::poll::CachedRead;
use crate::pulls::{PullRef, RepoRef};
use crate::queries::PAGE_SIZE;
use crate::queries::pull_request_revisions::{
    self, PullRequestRevisionsData, Review, TimelineEvent,
};
use crate::revision_marks::SeenHead;

/// The most pages of the head branch's pushes read, newest first back to the one the pull
/// request opened with: a thousand pushes.
pub const MAX_HEAD_PAGES: usize = 10;
/// The most pages of a base branch's pushes read, newest first back to the pull request's first
/// revision. A busy base branch's older pushes are left out; a base that only moved forward
/// doesn't need them (`review::revisions::base_tip_at`).
pub const MAX_BASE_PAGES: usize = 3;

/// What GitHub says about a pull request's pushes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PushHistory {
    /// When the pull request opened (`createdAt`).
    #[schemars(with = "String")]
    pub opened_at: DateTime<Utc>,
    /// When it was closed or merged, while it is (`closedAt`).
    #[schemars(with = "Option<String>")]
    pub closed_at: Option<DateTime<Utc>>,
    /// Its head now (`headRefOid`).
    pub head: String,
    pub head_branch: String,
    /// The repository holding the head branch, or `None` once a fork is deleted.
    pub head_repo: Option<HeadRepo>,
    /// The branch it targets now (`baseRefName`).
    pub base_branch: String,
    pub merged: bool,
    /// The commit that merged it, with its parents: two for a merge commit.
    pub merge_commit: Option<MergeCommit>,
    /// The head branch's pushes, newest first back to the one the pull request opened with, its
    /// first revision. Older pushes, to the branch before the pull request, aren't read. Past
    /// [`MAX_HEAD_PAGES`] it's `truncated`, holding the latest pushes only.
    pub head_activity: BranchActivity,
    /// The pushes of each branch it targeted, newest first back to its first revision: today's
    /// base branch first, then those it targeted before, latest first.
    pub base_activity: Vec<BranchActivity>,
    /// `HeadRefForcePushedEvent`s.
    pub head_force_pushes: Vec<ForcePushEvent>,
    /// `BaseRefForcePushedEvent`s: force-pushes of the branch it targeted at the time.
    pub base_force_pushes: Vec<ForcePushEvent>,
    /// `BaseRefChangedEvent`s and `AutomaticBaseChangeSucceededEvent`s.
    pub retargets: Vec<RetargetEvent>,
    /// Submitted reviews: approvals, change requests, comments and dismissed reviews.
    pub reviews: Vec<SubmittedReview>,
    /// The signed-in user's last submitted review.
    pub viewer_last_review: Option<SubmittedReview>,
    /// The pushes that started check suites on the latest 100 commits.
    pub check_suites: Vec<CheckSuitePush>,
    /// Heads of force-pushes GitHub no longer has, named by the issue timeline.
    #[serde(default)]
    pub lost_heads: Vec<String>,
    /// The issue timeline's force-pushes, read only when GraphQL's events lost a head.
    #[serde(default)]
    pub lost_head_timeline: Option<TimelineForcePushes>,
}

/// The REST issue timeline's force-pushes of the head branch, page by page with ETags.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TimelineForcePushes {
    pub pages: Vec<TimelinePage>,
    /// Why the latest read failed, for a reason that may pass. The pages from the one that failed
    /// on are an earlier read's, if there was one.
    pub failed: Option<String>,
}

/// One page of the issue timeline.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TimelinePage {
    /// The URL it was read from.
    pub url: String,
    pub etag: Option<String>,
    /// The next page's URL, from the `Link` header, which a `304` leaves out.
    pub next: Option<String>,
    /// How many events of every kind it holds, which says whether it was full.
    pub events: usize,
    /// Its `head_ref_force_pushed` events.
    pub force_pushes: Vec<NamedForcePush>,
}

/// A force-push of the head, as the issue timeline names it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NamedForcePush {
    #[schemars(with = "String")]
    pub at: DateTime<Utc>,
    /// Its head (`commit_id`), named even when GitHub no longer has it.
    pub commit: String,
}

/// A repository holding a head branch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HeadRepo {
    pub owner: String,
    pub name: String,
}

/// The commit that merged a pull request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MergeCommit {
    pub oid: String,
    /// Its first two parents: the base branch's tip before the merge first.
    pub parents: Vec<String>,
}

/// A force-push from the timeline. Its commits are `None` once GitHub no longer has them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ForcePushEvent {
    /// A few seconds after the push.
    #[schemars(with = "String")]
    pub at: DateTime<Utc>,
    pub before: Option<String>,
    pub after: Option<String>,
}

/// A change of the base branch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RetargetEvent {
    #[schemars(with = "String")]
    pub at: DateTime<Utc>,
    pub from: String,
    pub to: String,
    /// Whether GitHub did it, when the branch it targeted was merged and deleted.
    pub automatic: bool,
}

/// A submitted review.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SubmittedReview {
    #[schemars(with = "String")]
    pub at: DateTime<Utc>,
    /// The commit it was on, unless GitHub no longer has it.
    pub commit: Option<String>,
    /// Its author's login, unless GitHub no longer has the account. Not read for the signed-in
    /// user's own last review.
    pub author: Option<String>,
}

/// The push that started a check suite on one of the pull request's commits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CheckSuitePush {
    /// The commit the suite ran on.
    pub commit: String,
    /// When the suite was made, up to a minute after the push.
    #[schemars(with = "String")]
    pub at: DateTime<Utc>,
    /// The branch it ran for, or `None` once that branch is deleted.
    pub branch: Option<String>,
    /// The push's `previousSha` and `nextSha`.
    pub previous: Option<String>,
    pub next: Option<String>,
}

impl From<&HeadRepo> for RepoRef {
    fn from(repo: &HeadRepo) -> Self {
        RepoRef::new(repo.owner.clone(), repo.name.clone())
    }
}

impl CacheEntry for PushHistory {
    const NAME: &'static str = "PushHistory";

    fn shape() -> String {
        format!(
            "REST {API_VERSION}, activity {PAGE_SIZE} a page, at most {MAX_HEAD_PAGES} for the \
             head and {MAX_BASE_PAGES} for a base\n{}\n{}",
            schema_shape::<Self>(),
            pull_request_revisions::QUERY
        )
    }
}

impl CachedRead for PushHistory {
    /// A push changes the head, a retarget the base, a merge or close the state, and a submitted
    /// review, your own pending one included, the count of submitted reviews; force-pushes and
    /// retargets are among the revision events. A comment, which moves the timeline, changes none
    /// of them.
    const WATCHED: &'static [&'static str] = &[
        "/headRefOid",
        "/baseRefName",
        "/state",
        "/submittedReviews/totalCount",
        "/revisionEvents/filteredCount",
    ];

    /// Whether every branch's pushes were read, or GitHub has none to show.
    fn is_complete(&self) -> bool {
        (std::iter::once(&self.head_activity).chain(&self.base_activity))
            .all(|activity| activity.failed.is_none())
            && (self.lost_head_timeline.as_ref()).is_none_or(|timeline| timeline.failed.is_none())
    }
}

impl PushHistory {
    /// The pull request's push history as GraphQL tells it, without the activity API's.
    fn from_graphql(data: PullRequestRevisionsData) -> Self {
        let mut history = Self {
            opened_at: data.created_at,
            closed_at: data.closed.then_some(data.closed_at).flatten(),
            head: data.head_ref_oid,
            head_activity: BranchActivity::none(&data.head_ref_name, Direction::Desc),
            head_branch: data.head_ref_name,
            head_repo: data.head_repository.map(|repo| HeadRepo {
                owner: repo.owner.login,
                name: repo.name,
            }),
            base_branch: data.base_ref_name,
            merged: data.merged,
            merge_commit: data.merge_commit.map(|commit| MergeCommit {
                oid: commit.oid,
                parents: (commit.parents.nodes.unwrap_or_default().into_iter())
                    .flatten()
                    .map(|parent| parent.oid)
                    .collect(),
            }),
            base_activity: Vec::new(),
            head_force_pushes: Vec::new(),
            base_force_pushes: Vec::new(),
            retargets: Vec::new(),
            reviews: (data.reviews)
                .and_then(|reviews| reviews.nodes)
                .unwrap_or_default()
                .into_iter()
                .flatten()
                .filter_map(submitted)
                .collect(),
            viewer_last_review: (data.viewer_last_review)
                .and_then(|reviews| reviews.nodes)
                .unwrap_or_default()
                .into_iter()
                .flatten()
                .filter_map(|review| {
                    Some(SubmittedReview {
                        at: review.submitted_at?,
                        commit: review.commit.map(|commit| commit.oid),
                        author: None,
                    })
                })
                .next_back(),
            check_suites: Vec::new(),
            lost_heads: Vec::new(),
            lost_head_timeline: None,
        };
        for event in data
            .timeline_items
            .nodes
            .unwrap_or_default()
            .into_iter()
            .flatten()
        {
            match event {
                TimelineEvent::HeadRefForcePushedEvent(event) => {
                    history.head_force_pushes.push(ForcePushEvent {
                        at: event.created_at,
                        before: event.before_commit.map(|commit| commit.oid),
                        after: event.after_commit.map(|commit| commit.oid),
                    })
                }
                TimelineEvent::BaseRefForcePushedEvent(event) => {
                    history.base_force_pushes.push(ForcePushEvent {
                        at: event.created_at,
                        before: event.before_commit.map(|commit| commit.oid),
                        after: event.after_commit.map(|commit| commit.oid),
                    })
                }
                TimelineEvent::BaseRefChangedEvent(event) => {
                    history.retargets.push(RetargetEvent {
                        at: event.created_at,
                        from: event.previous_ref_name,
                        to: event.current_ref_name,
                        automatic: false,
                    })
                }
                TimelineEvent::AutomaticBaseChangeSucceededEvent(event) => {
                    history.retargets.push(RetargetEvent {
                        at: event.created_at,
                        from: event.old_base,
                        to: event.new_base,
                        automatic: true,
                    })
                }
                _ => {}
            }
        }
        for node in data.commits.nodes.unwrap_or_default().into_iter().flatten() {
            let commit = node.commit;
            let suites = (commit.check_suites)
                .and_then(|suites| suites.nodes)
                .unwrap_or_default();
            for suite in suites.into_iter().flatten() {
                let push = suite.push;
                history.check_suites.push(CheckSuitePush {
                    commit: commit.oid.clone(),
                    at: suite.created_at,
                    branch: suite.branch.map(|branch| branch.name),
                    previous: push.as_ref().and_then(|push| push.previous_sha.clone()),
                    next: push.and_then(|push| push.next_sha),
                });
            }
        }
        history
    }

    /// When the pull request opened.
    pub fn opened_at(&self) -> SystemTime {
        self.opened_at.into()
    }

    /// The commit of the signed-in user's last review, the "your last review" marker.
    pub fn last_review(&self) -> Option<&str> {
        self.viewer_last_review.as_ref()?.commit.as_deref()
    }

    /// What every source saw on the head branch, for `review::revisions::build_revisions`, with
    /// the heads Lo's pollers saw (`seen`):
    ///
    /// - the head branch's pushes from the activity API, but deletions, and pushes after the pull
    ///   request was closed, which belong to whatever reused the branch;
    /// - force-push events, those whose head GitHub lost included when the issue timeline named
    ///   it ([`Self::unavailable_heads`]);
    /// - submitted reviews on commits GitHub still has. When the activity API's log of the head
    ///   branch is complete ([`Self::complete_log`]), a review on a commit it never had as its
    ///   head is a [`PushRecord::ReviewedCommit`];
    /// - check suites a push to the head branch started on the commit it pushed, made before the
    ///   pull request was closed. A suite's branch is unknown once it's deleted, and then it
    ///   counts. Without a complete log, a suite on one of the latest commits that no push started,
    ///   as a fork's pull request's are in the base repository, says the commit was the head by
    ///   then: GitHub makes suites for the heads a pull request is pushed to.
    pub fn push_records(&self, seen: &[SeenHead]) -> Vec<PushRecord> {
        let open_then = |at: &DateTime<Utc>| self.closed_at.is_none_or(|closed| *at <= closed);
        let complete = self.complete_log();
        let entries = self.head_activity.entries();
        let mut records = Vec::new();
        for entry in &entries {
            if !entry.is_deletion() && open_then(&entry.timestamp) {
                records.push(PushRecord::Activity {
                    before: Some(entry.before.clone()),
                    after: entry.after.clone(),
                    at: entry.timestamp.into(),
                    forced: entry.is_forced(),
                });
            }
        }
        for push in &self.head_force_pushes {
            if let Some(after) = &push.after {
                records.push(PushRecord::ForcePush {
                    before: push.before.clone(),
                    after: after.clone(),
                    at: push.at.into(),
                });
            }
        }
        for review in &self.reviews {
            let Some(commit) = review.commit.clone() else {
                continue;
            };
            let at = review.at.into();
            let pushed = entries.iter().any(|entry| entry.after == commit);
            records.push(if complete && !pushed {
                PushRecord::ReviewedCommit { commit, at }
            } else {
                PushRecord::Review { commit, at }
            });
        }
        for suite in &self.check_suites {
            let on_head_branch = suite.branch.as_ref().is_none_or(|b| *b == self.head_branch);
            if !on_head_branch || !open_then(&suite.at) {
                continue;
            }
            match (&suite.previous, &suite.next) {
                (previous, Some(next)) if *next == suite.commit => {
                    records.push(PushRecord::CheckSuite {
                        previous: previous.clone(),
                        next: next.clone(),
                        at: suite.at.into(),
                    })
                }
                (None, None) if !complete => records.push(PushRecord::CheckSuite {
                    previous: None,
                    next: suite.commit.clone(),
                    at: suite.at.into(),
                }),
                _ => {}
            }
        }
        for head in seen {
            records.push(PushRecord::Polled {
                head: head.head.clone(),
                at: head.first_seen,
            });
        }
        records
    }

    /// Whether the activity API's log of the head branch holds every push the pull request had:
    /// it was read, back to the push the pull request opened with, and each push since starts
    /// where the one before ended, up to the head.
    pub fn complete_log(&self) -> bool {
        let activity = &self.head_activity;
        if activity.unavailable.is_some() || activity.failed.is_some() || activity.truncated {
            return false;
        }
        let entries: Vec<_> = (activity.entries().into_iter())
            .filter(|entry| !entry.is_deletion())
            .collect();
        let reached_back = entries
            .iter()
            .any(|entry| entry.timestamp <= self.opened_at);
        let unbroken = entries
            .windows(2)
            .all(|pair| pair[1].before == pair[0].after);
        let ends_at_head = entries.last().is_some_and(|entry| entry.after == self.head);
        reached_back && unbroken && ends_at_head
    }

    /// The heads GitHub no longer has, which its force-push events name only through the issue
    /// timeline: `review::revisions::Revisions::mark_unavailable` shows them as no longer
    /// available (design §11). The local copy can still have one it fetched before, so a caller
    /// marks only those it lacks too (`repo_cache::Repo::fetch_commits`).
    pub fn unavailable_heads(&self) -> &[String] {
        &self.lost_heads
    }

    /// Names the heads of force-pushes GitHub lost from the issue timeline's force-pushes, by
    /// time. A timeline force-push whose head another event already names is left out first, so
    /// two in the same second can't give a lost one the head of one GitHub kept; the rest pair
    /// up in order.
    fn name_lost_heads(&mut self, timeline: &TimelineForcePushes) {
        let known: Vec<String> = (self.head_force_pushes.iter())
            .filter_map(|push| push.after.clone())
            .collect();
        let mut candidates: Vec<&NamedForcePush> = (timeline.pages.iter())
            .flat_map(|page| &page.force_pushes)
            .filter(|named| !known.contains(&named.commit))
            .collect();
        self.lost_heads.clear();
        for push in &mut self.head_force_pushes {
            if push.after.is_some() {
                continue;
            }
            if let Some(index) = candidates.iter().position(|named| named.at == push.at) {
                let commit = candidates.remove(index).commit.clone();
                self.lost_heads.push(commit.clone());
                push.after = Some(commit);
            }
        }
    }

    /// What's known of the base branches over time, for `review::revisions::base_tip_at`, with
    /// `tip` the base branch's tip as the local copy last fetched it. Tips Lo saw itself, such
    /// as the heads of the pull request below in a stack, can be added to its records.
    pub fn base_history(&self, tip: Option<&str>) -> BaseHistory {
        let merge = (self.merge_commit.as_ref())
            .filter(|_| self.merged)
            .map(|commit| Merge {
                commit: commit.oid.clone(),
                parents: commit.parents.clone(),
                head: self.head.clone(),
            });
        let mut records = Vec::new();
        for activity in &self.base_activity {
            for entry in activity.entries() {
                records.push(BaseRecord::Activity {
                    branch: activity.branch.clone(),
                    before: Some(entry.before.clone()),
                    after: Some(entry.after.clone()),
                    at: entry.timestamp.into(),
                    forced: entry.is_forced(),
                });
            }
        }
        for push in &self.base_force_pushes {
            records.push(BaseRecord::ForcePush {
                after: push.after.clone(),
                at: push.at.into(),
            });
        }
        BaseHistory {
            branch: self.base_branch.clone(),
            tip: tip.map(str::to_string),
            merge,
            retargets: (self.retargets.iter())
                .map(|retarget| Retarget {
                    from: retarget.from.clone(),
                    to: retarget.to.clone(),
                    at: retarget.at.into(),
                })
                .collect(),
            records,
        }
    }

    /// The earliest time a base branch's pushes are needed from: when the head the pull request
    /// opened with was pushed, its first revision.
    fn since(&self) -> DateTime<Utc> {
        let opened = self.opened_at;
        (self.head_activity.entries().into_iter())
            .filter(|entry| !entry.is_deletion() && entry.timestamp <= opened)
            .map(|entry| entry.timestamp)
            .max()
            .unwrap_or(opened)
    }

    /// Every branch the pull request targeted: today's first, then the earlier ones, latest
    /// first.
    fn base_branches(&self) -> Vec<String> {
        let mut retargets: Vec<&RetargetEvent> = self.retargets.iter().collect();
        retargets.sort_by_key(|retarget| std::cmp::Reverse(retarget.at));
        let mut branches = vec![self.base_branch.clone()];
        for retarget in retargets {
            if !branches.contains(&retarget.from) {
                branches.push(retarget.from.clone());
            }
        }
        branches
    }
}

/// A submitted review, as [`SubmittedReview`]. A pending one has no time and isn't.
fn submitted(review: Review) -> Option<SubmittedReview> {
    Some(SubmittedReview {
        at: review.submitted_at?,
        commit: review.commit.map(|commit| commit.oid),
        author: review.author.map(|author| author.login),
    })
}

impl GithubClient {
    /// `pull`'s push history, read as `login`. Each page of pushes `known` holds is asked about
    /// with its ETag, so an unchanged one is free. A head repository that's gone, such as a
    /// deleted fork, or one the account can't read, leaves the head branch with no pushes and
    /// `unavailable` saying why, and so does a base branch the account can't read.
    pub async fn push_history(
        &self,
        pull: &PullRef,
        login: &str,
        known: Option<&PushHistory>,
    ) -> Result<PushHistory, GithubError> {
        let (owner, name) = (&pull.repo.owner, &pull.repo.name);
        let data = self
            .pull_request_revisions(owner, name, pull.number, login)
            .await?;
        let mut history = PushHistory::from_graphql(data);
        if history
            .head_force_pushes
            .iter()
            .any(|push| push.after.is_none())
        {
            let known = known.and_then(|known| known.lost_head_timeline.as_ref());
            let timeline = self.timeline_force_pushes(pull, known).await?;
            history.name_lost_heads(&timeline);
            history.lost_head_timeline = Some(timeline);
        }
        // Newest first, back to the push the pull request opened with: a long-lived branch, or
        // one a bot reuses for every release, can have far more pushes from before.
        let opened = history.opened_at;
        history.head_activity = match &history.head_repo {
            Some(repo) => {
                let known = known.map(|known| &known.head_activity);
                (self.read_branch(
                    &repo.into(),
                    &history.head_branch,
                    known,
                    MAX_HEAD_PAGES,
                    |entry| entry.timestamp <= opened,
                ))
                .await?
            }
            None => BranchActivity::unavailable(
                &history.head_branch,
                Direction::Desc,
                "GitHub shows no repository for its head branch: it was deleted, or the account \
                 can't see it"
                    .into(),
            ),
        };
        let since = history.since();
        for branch in history.base_branches() {
            let known = known.and_then(|known| {
                (known.base_activity.iter()).find(|activity| activity.branch == branch)
            });
            let activity = self
                .read_branch(&pull.repo, &branch, known, MAX_BASE_PAGES, |entry| {
                    entry.timestamp <= since
                })
                .await?;
            history.base_activity.push(activity);
        }
        Ok(history)
    }
}

/// The most pages of the issue timeline read for the heads of force-pushes GitHub lost.
const MAX_TIMELINE_PAGES: usize = 10;

/// The REST path of the first page of `pull`'s issue timeline.
fn timeline_path(pull: &PullRef) -> String {
    let mut url = Url::parse(API_URL).expect("the API's URL parses");
    url.path_segments_mut()
        .expect("the API's URL has a path")
        .clear()
        .extend([
            "repos",
            &pull.repo.owner,
            &pull.repo.name,
            "issues",
            &pull.number.to_string(),
            "timeline",
        ]);
    format!("{}?per_page={PAGE_SIZE}", url.path())
}

/// A page of the issue timeline from GitHub's answer, or why it isn't one.
fn timeline_page(response: &RestResponse, url: String) -> Result<TimelinePage, String> {
    let events: Vec<serde_json::Value> = response.json().map_err(|error| error.to_string())?;
    let force_pushes = (events.iter())
        .filter(|event| event["event"] == "head_ref_force_pushed")
        .filter_map(|event| {
            Some(NamedForcePush {
                at: event["created_at"].as_str()?.parse().ok()?,
                commit: event["commit_id"].as_str()?.to_string(),
            })
        })
        .collect();
    Ok(TimelinePage {
        url,
        etag: response.etag().map(str::to_string),
        next: response.header("link").and_then(crate::activity::next_link),
        events: events.len(),
        force_pushes,
    })
}

impl GithubClient {
    /// The force-pushes of `pull`'s head in the REST issue timeline, whose
    /// `head_ref_force_pushed` events keep `commit_id` when GraphQL's `afterCommit` is null
    /// (`docs/research/revisions.md` §6). Each page `known` has is asked about with its ETag. Only
    /// the network, the rate limit and the token fail the read; anything else, a page that isn't
    /// a list of events included, keeps the pages read and what `known` had for the rest, and
    /// flags the read `failed`, so the push history is read again after a wait.
    async fn timeline_force_pushes(
        &self,
        pull: &PullRef,
        known: Option<&TimelineForcePushes>,
    ) -> Result<TimelineForcePushes, GithubError> {
        let known_pages = known.map_or(&[][..], |known| &known.pages[..]);
        let mut read = TimelineForcePushes::default();
        let mut url = Some(timeline_path(pull));
        while let Some(current) = url.take() {
            if read.pages.len() >= MAX_TIMELINE_PAGES {
                break;
            }
            let index = read.pages.len();
            let known_page = known_pages.get(index).filter(|page| page.url == current);
            // A 304 leaves out the Link header, so a full last page is asked for in full.
            let refetch_in_full = known_page
                .is_some_and(|page| page.next.is_none() && page.events >= PAGE_SIZE as usize);
            let etag =
                (known_page.filter(|_| !refetch_in_full)).and_then(|page| page.etag.as_deref());
            let page = match self.send(Method::GET, &current, None, etag).await {
                Err(error) if stops_the_read(&error) => return Err(error),
                Err(error) => Err(error.to_string()),
                Ok(response) if response.status == StatusCode::NOT_MODIFIED => {
                    known_page.cloned().ok_or_else(|| {
                        format!("GitHub said {current} hadn't changed, but Lo never read it")
                    })
                }
                Ok(response) => timeline_page(&response, current),
            };
            match page {
                Ok(page) => {
                    url = page.next.clone();
                    read.pages.push(page);
                }
                Err(reason) => {
                    log::warn!("reading {pull}'s timeline for lost heads: {reason}");
                    read.pages.extend(known_pages.iter().skip(index).cloned());
                    read.failed = Some(reason);
                    break;
                }
            }
        }
        Ok(read)
    }

    /// [`GithubClient::branch_activity`], newest first. An error that isn't the network's, the
    /// rate limit's or the token's fails only this branch's read: it keeps what `known` read
    /// before, flagged `failed`, so the rest of the push history still shows and is kept, and
    /// the whole is read again after a wait ([`CachedRead::is_complete`]).
    async fn read_branch(
        &self,
        repo: &RepoRef,
        branch: &str,
        known: Option<&BranchActivity>,
        max_pages: usize,
        enough: impl Fn(&ActivityEntry) -> bool,
    ) -> Result<BranchActivity, GithubError> {
        let direction = Direction::Desc;
        match (self.branch_activity(repo, branch, direction, known, max_pages, enough)).await {
            Err(error) if !stops_the_read(&error) => {
                log::warn!("reading the pushes to {repo} {branch}: {error:#}");
                let reason = error.to_string();
                Ok(BranchActivity::failed(known, branch, direction, reason))
            }
            result => result,
        }
    }
}

/// Whether an error fails a whole push history: the network, the rate limit and the token stop
/// every request after it too, and the poller waits them out.
pub(crate) fn stops_the_read(error: &GithubError) -> bool {
    matches!(
        error,
        GithubError::Network(_)
            | GithubError::RateLimited { .. }
            | GithubError::Unauthorized { .. }
            | GithubError::TokenUnavailable(_)
    )
}

/// [`GithubClient::push_history`] as [`crate::poll::Poller::read_cached`] runs it.
pub async fn read_push_history(
    client: GithubClient,
    pull: PullRef,
    login: String,
    known: Option<Arc<PushHistory>>,
) -> Result<PushHistory, GithubError> {
    client.push_history(&pull, &login, known.as_deref()).await
}
