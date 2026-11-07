//! Reads about a pull request that the poller keeps in the cache and current
//! ([`Poller::read_cached`](super::Poller::read_cached)).

use std::any::Any;
use std::fmt;
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use anyhow::{Result, anyhow};
use futures::future::{BoxFuture, FutureExt as _};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::cache::{CacheEntry, PrCache};
use crate::client::GithubClient;
use crate::error::GithubError;
use crate::pulls::Target;

/// Something about a pull request that [`Poller::read_cached`](super::Poller::read_cached) reads,
/// keeps in the cache under [`CacheEntry::NAME`], and reads again while the pull request is open
/// when the fields it watches in the pull request's check change, and on the 5-minute refresh.
/// No two have the same name.
pub trait CachedRead: CacheEntry + Clone + PartialEq + Send + Sync + 'static {
    /// The fields of the open pull request's check (`queries::PullRequestCheckData`) whose
    /// change means it's out of date, as JSON pointers into the check, which names fields as
    /// GraphQL does: `/headRefOid`, `/submittedReviews/totalCount`.
    const WATCHED: &'static [&'static str];

    /// Whether every part of it was read. One that isn't, because a part failed for a reason
    /// that may pass, is kept and shown, and read again after a wait, as a read that failed is,
    /// whatever its watched fields say.
    fn is_complete(&self) -> bool {
        true
    }
}

/// A read's value as the cache holds it, with the watched fields of the check it was read at.
#[derive(Serialize, Deserialize)]
#[serde(bound = "T: Serialize + DeserializeOwned")]
pub(crate) struct Stamped<T> {
    stamp: Option<String>,
    value: T,
}

impl<T: CachedRead> CacheEntry for Stamped<T> {
    const NAME: &'static str = T::NAME;

    fn shape() -> String {
        format!("{}\nstamped with the check's {:?}", T::shape(), T::WATCHED)
    }
}

/// The values of the fields `watched` names in a check's JSON. A field the check doesn't have
/// reads apart from a null one, so a misspelt field can't match a stamp made when it was there.
pub(crate) fn stamp(check: &serde_json::Value, watched: &[&str]) -> String {
    let values: Vec<String> = watched
        .iter()
        .map(|pointer| {
            check
                .pointer(pointer)
                .map_or("<absent>".into(), ToString::to_string)
        })
        .collect();
    values.join("\n")
}

/// A read's value, whatever its type.
pub(crate) type Value = Arc<dyn Any + Send + Sync>;

/// Runs a read with the account's client and the value Lo has.
pub(crate) type ReadFn = Arc<
    dyn Fn(GithubClient, Option<Value>) -> BoxFuture<'static, Result<Value, GithubError>>
        + Send
        + Sync,
>;

/// A read's value in the cache, the stamp it was read at, and when GitHub last confirmed it.
type Loaded = (Value, Option<String>, SystemTime);
type Load = fn(&PrCache, &str, &Target) -> Option<Loaded>;
type Put = fn(
    PrCache,
    String,
    Target,
    Option<String>,
    Value,
    SystemTime,
) -> BoxFuture<'static, Result<()>>;
type Touch = fn(PrCache, String, Target, SystemTime) -> BoxFuture<'static, Result<()>>;
type Restamp =
    fn(PrCache, String, Target, Option<String>, SystemTime) -> BoxFuture<'static, Result<()>>;

/// What the poller does with one type of read, whatever it is.
#[derive(Clone, Copy)]
pub(crate) struct ReadType {
    pub watched: &'static [&'static str],
    /// Whether two values are the same.
    pub same: fn(&Value, &Value) -> bool,
    /// Whether every part of a value was read ([`CachedRead::is_complete`]).
    pub complete: fn(&Value) -> bool,
    /// The value in the cache, the stamp it was read at, and when GitHub last confirmed it.
    pub load: Load,
    /// Stores a value and its stamp.
    pub put: Put,
    /// Notes that GitHub confirmed the stored value.
    pub touch: Touch,
    /// Notes that GitHub confirmed the stored value at another stamp, without writing it again.
    pub restamp: Restamp,
}

impl ReadType {
    pub fn of<T: CachedRead>() -> Self {
        Self {
            watched: T::WATCHED,
            same: same::<T>,
            complete: complete::<T>,
            load: load::<T>,
            put: put::<T>,
            touch: touch::<T>,
            restamp: restamp::<T>,
        }
    }
}

/// Whether two values are the same. Values of another type never are.
fn same<T: CachedRead>(a: &Value, b: &Value) -> bool {
    match (a.downcast_ref::<T>(), b.downcast_ref::<T>()) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

fn complete<T: CachedRead>(value: &Value) -> bool {
    value.downcast_ref::<T>().is_some_and(T::is_complete)
}

fn load<T: CachedRead>(cache: &PrCache, login: &str, target: &Target) -> Option<Loaded> {
    let cached = cache
        .get::<Stamped<T>>(login, target)
        .inspect_err(|error| log::warn!("reading the cache: {error:#}"))
        .ok()
        .flatten()?;
    let Stamped { stamp, value } = cached.value;
    Some((Arc::new(value), stamp, cached.fetched_at))
}

fn put<T: CachedRead>(
    cache: PrCache,
    login: String,
    target: Target,
    stamp: Option<String>,
    value: Value,
    at: SystemTime,
) -> BoxFuture<'static, Result<()>> {
    async move {
        let value =
            (value.downcast::<T>()).map_err(|_| anyhow!("a {} of another type", T::NAME))?;
        let stamped = Stamped {
            stamp,
            value: T::clone(&value),
        };
        cache.put(&login, &target, &stamped, None, at).await
    }
    .boxed()
}

fn touch<T: CachedRead>(
    cache: PrCache,
    login: String,
    target: Target,
    at: SystemTime,
) -> BoxFuture<'static, Result<()>> {
    async move { cache.touch::<Stamped<T>>(&login, &target, at).await }.boxed()
}

fn restamp<T: CachedRead>(
    cache: PrCache,
    login: String,
    target: Target,
    stamp: Option<String>,
    at: SystemTime,
) -> BoxFuture<'static, Result<()>> {
    async move {
        let stamp = serde_json::to_string(&stamp)?;
        (cache.update_json::<Stamped<T>>(&login, &target, "$.stamp", stamp, at)).await
    }
    .boxed()
}

/// A read the poller keeps for a pull request.
pub(crate) struct ReadState {
    pub ty: ReadType,
    pub run: ReadFn,
    pub value: Option<Value>,
    /// The watched fields of the check the value was read at, or `None` when it must be read
    /// again whatever they are.
    pub stamp: Option<String>,
    /// When GitHub last confirmed the value.
    pub fetched_at: Option<SystemTime>,
    /// How many times in a row reading it failed, or gave a value that isn't complete.
    pub failures: u32,
    /// When it may be read again after those failures.
    pub retry_at: Option<Instant>,
    /// How many times it was made due at once whatever its stamp (`Poller::read_again`). A run
    /// that started before the last time doesn't stamp its value, so it's read again.
    pub generation: u64,
    /// When it's due again after a write it shows, whatever its stamp.
    pub again_at: Option<Instant>,
}

/// One run of a read, as a poll.
#[derive(Clone)]
pub(crate) struct ReadJob {
    pub name: &'static str,
    pub run: ReadFn,
    pub known: Option<Value>,
    /// The watched fields of the check it's read at.
    pub stamp: String,
    /// The read's [`ReadState::generation`] when it started.
    pub generation: u64,
    /// When it started.
    pub started: Instant,
}

impl fmt::Debug for ReadJob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReadJob")
            .field("name", &self.name)
            .field("known", &self.known.is_some())
            .field("stamp", &self.stamp)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::cassette::{fixtures_dir, read_cassette};
    use crate::push_history::PushHistory;
    use crate::queries::{
        MergeFacts, PullRequestTimeline, ReviewThreadList, StackLayerList, ViewerPendingReview,
    };

    /// PR #1's recorded check, with a thread and a pending review of yours, each with a comment.
    fn check() -> serde_json::Value {
        let (_, interactions) =
            read_cassette(&fixtures_dir().join("graphql_pull_request_check_1")).unwrap();
        let body = interactions[0].1.response.body.clone().unwrap();
        let mut check = body["data"]["repository"]["pullRequest"].clone();
        check["reviewThreads"] = serde_json::json!({
            "totalCount": 1,
            "nodes": [{"isResolved": false, "comments": {"totalCount": 1}}]
        });
        check["viewerPendingReview"] = serde_json::json!({
            "nodes": [{"viewerDidAuthor": true, "comments": {"totalCount": 1}}]
        });
        check
    }

    /// Which of the review threads, your pending review and the timeline `edit` reads again.
    fn read_again(edit: impl FnOnce(&mut serde_json::Value)) -> [bool; 3] {
        let before = check();
        let mut after = before.clone();
        edit(&mut after);
        let changed = |watched: &[&str]| stamp(&before, watched) != stamp(&after, watched);
        [
            changed(ReviewThreadList::WATCHED),
            changed(ViewerPendingReview::WATCHED),
            changed(PullRequestTimeline::WATCHED),
        ]
    }

    fn add(value: &mut serde_json::Value, by: i64) {
        *value = (value.as_i64().unwrap() + by).into();
    }

    #[test]
    fn every_kept_read_watches_fields_the_check_has() {
        let check = check();
        for pointer in [
            PushHistory::WATCHED,
            StackLayerList::WATCHED,
            ReviewThreadList::WATCHED,
            ViewerPendingReview::WATCHED,
            PullRequestTimeline::WATCHED,
            MergeFacts::WATCHED,
        ]
        .concat()
        {
            assert!(check.pointer(pointer).is_some(), "{pointer}");
        }
    }

    #[test]
    fn threads_your_pending_review_and_the_timeline_are_read_again_only_when_they_change() {
        // Someone mentions the pull request elsewhere: only the whole timeline moves.
        let mentioned = read_again(|check| {
            check["timelineItems"]["updatedAt"] = "2026-11-07T12:00:00Z".into();
            add(&mut check["timelineItems"]["totalCount"], 1);
        });
        assert_eq!(mentioned, [false, false, false]);
        // A comment on the pull request, or a label.
        let commented = read_again(|check| {
            add(&mut check["comments"]["totalCount"], 1);
            add(&mut check["timelineEvents"]["filteredCount"], 1);
            check["timelineEvents"]["updatedAt"] = "2026-11-07T12:00:00Z".into();
        });
        assert_eq!(commented, [false, false, true]);
        // A comment edited: the timeline's items' latest time moves.
        let edited = read_again(|check| {
            check["timelineEvents"]["updatedAt"] = "2026-11-07T12:00:00Z".into();
        });
        assert_eq!(edited, [false, false, true]);
        // Checks start on the latest commit, and the description is edited.
        let checks = read_again(|check| {
            check["statusCheckRollup"] = serde_json::json!({"state": "PENDING"});
        });
        assert_eq!(checks, [false, false, true]);
        let described = read_again(|check| {
            check["lastEditedAt"] = "2026-11-07T12:00:00Z".into();
        });
        assert_eq!(described, [false, false, true]);
        // A reply, posted at once: a review of its own, in the timeline too.
        let replied = read_again(|check| {
            add(
                &mut check["reviewThreads"]["nodes"][0]["comments"]["totalCount"],
                1,
            );
            add(&mut check["reviews"]["totalCount"], 1);
            add(&mut check["submittedReviews"]["totalCount"], 1);
            add(&mut check["timelineEvents"]["filteredCount"], 1);
            check["timelineEvents"]["updatedAt"] = "2026-11-07T12:00:00Z".into();
        });
        assert_eq!(replied, [true, false, true]);
        // A thread resolved, or a push.
        let resolved = read_again(|check| {
            check["reviewThreads"]["nodes"][0]["isResolved"] = true.into();
        });
        assert_eq!(resolved, [true, false, false]);
        let pushed = read_again(|check| {
            check["headRefOid"] = "c7f30e9b1a5d7c3e9f1b5d7a3c9e1f5b7d3a9c62".into();
            add(&mut check["timelineEvents"]["filteredCount"], 1);
        });
        assert_eq!(pushed, [true, false, true]);
        // You add to your pending review, from Lo or from GitHub's web pages: the threads hold
        // your pending comments too.
        let pending = read_again(|check| {
            add(
                &mut check["viewerPendingReview"]["nodes"][0]["comments"]["totalCount"],
                1,
            );
        });
        assert_eq!(pending, [true, true, false]);
        // You delete it.
        let deleted = read_again(|check| {
            check["viewerPendingReview"]["nodes"] = serde_json::json!([]);
        });
        assert_eq!(deleted, [true, true, false]);
        // You submit it: its comments are posted, though no count of the threads moves.
        let submitted = read_again(|check| {
            check["viewerPendingReview"]["nodes"] = serde_json::json!([]);
            add(&mut check["submittedReviews"]["totalCount"], 1);
            add(&mut check["timelineEvents"]["filteredCount"], 1);
            check["timelineEvents"]["updatedAt"] = "2026-11-07T12:00:00Z".into();
        });
        assert_eq!(submitted, [true, true, true]);
    }

    #[test]
    fn what_decides_a_merge_is_read_again_when_github_may_decide_otherwise() {
        let before = check();
        let changed = |edit: &dyn Fn(&mut serde_json::Value)| {
            let mut after = before.clone();
            edit(&mut after);
            stamp(&before, MergeFacts::WATCHED) != stamp(&after, MergeFacts::WATCHED)
        };

        // Someone mentions the pull request elsewhere, or edits its description.
        assert!(!changed(&|check| add(
            &mut check["timelineItems"]["totalCount"],
            1
        )));
        assert!(!changed(&|check| {
            check["lastEditedAt"] = "2026-11-07T12:00:00Z".into();
        }));
        for edit in [
            // A push, or GitHub working out mergeability again.
            &(|check: &mut serde_json::Value| {
                check["headRefOid"] = "c7f30e9b1a5d7c3e9f1b5d7a3c9e1f5b7d3a9c62".into();
            }) as &dyn Fn(&mut serde_json::Value),
            &|check| check["mergeStateStatus"] = "DIRTY".into(),
            // The base moves on, or another one is picked.
            &|check| check["baseRefOid"] = "d4c3b2a1d4c3b2a1d4c3b2a1d4c3b2a1d4c3b2a1".into(),
            &|check| check["baseRefName"] = "release".into(),
            // A review, a check, a thread resolved, the queue, the draft.
            &|check| add(&mut check["submittedReviews"]["totalCount"], 1),
            &|check| check["statusCheckRollup"] = serde_json::json!({"state": "FAILURE"}),
            &|check| check["reviewThreads"]["nodes"][0]["isResolved"] = true.into(),
            &|check| check["mergeQueueEntry"] = serde_json::json!({"position": 1}),
            &|check| check["isDraft"] = true.into(),
            // Your pending review, which approve and ship submits.
            &|check| check["viewerPendingReview"]["nodes"] = serde_json::json!([]),
            // A review requested, auto-merge turned on, or a comment.
            &|check| add(&mut check["timelineEvents"]["filteredCount"], 1),
        ] {
            assert!(changed(edit));
        }
    }

    #[test]
    fn a_stamp_holds_the_watched_fields_values() {
        let check = serde_json::json!({
            "headRefOid": "abc",
            "reviews": {"totalCount": 9},
        });
        assert_eq!(
            stamp(&check, &["/headRefOid", "/reviews/totalCount"]),
            "\"abc\"\n9"
        );
        // A field the check doesn't have isn't a null one.
        assert_eq!(stamp(&check, &["/nope"]), "<absent>");
        assert_eq!(
            stamp(&serde_json::json!({"nope": null}), &["/nope"]),
            "null"
        );
    }
}
