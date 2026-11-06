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
pub trait CachedRead: CacheEntry + Clone + PartialEq + Send + Sync + 'static {
    /// The fields of the open pull request's check (`queries::PullRequestCheckData`) whose
    /// change means it's out of date, as JSON pointers into the check, which names fields as
    /// GraphQL does: `/headRefOid`, `/timelineItems/updatedAt`.
    const WATCHED: &'static [&'static str];
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

/// The values of the fields `watched` names in a check's JSON.
pub(crate) fn stamp(check: &serde_json::Value, watched: &[&str]) -> String {
    let values: Vec<String> = watched
        .iter()
        .map(|pointer| {
            check
                .pointer(pointer)
                .map_or("null".into(), ToString::to_string)
        })
        .collect();
    values.join("\n")
}

/// A read's value, whatever its type.
pub(crate) type Value = Arc<dyn Any + Send + Sync>;

/// Runs a read with the account's client and the value rozed has.
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

/// What the poller does with one type of read, whatever it is.
#[derive(Clone, Copy)]
pub(crate) struct ReadType {
    pub watched: &'static [&'static str],
    /// Whether two values are the same.
    pub same: fn(&Value, &Value) -> bool,
    /// The value in the cache, the stamp it was read at, and when GitHub last confirmed it.
    pub load: Load,
    /// Stores a value and its stamp.
    pub put: Put,
    /// Notes that GitHub confirmed the stored value.
    pub touch: Touch,
}

impl ReadType {
    pub fn of<T: CachedRead>() -> Self {
        Self {
            watched: T::WATCHED,
            same: same::<T>,
            load: load::<T>,
            put: put::<T>,
            touch: touch::<T>,
        }
    }
}

fn same<T: CachedRead>(a: &Value, b: &Value) -> bool {
    a.downcast_ref::<T>() == b.downcast_ref::<T>()
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

/// A read the poller keeps for a pull request.
pub(crate) struct ReadState {
    pub ty: ReadType,
    pub run: ReadFn,
    pub value: Option<Value>,
    /// The watched fields of the check the value was read at.
    pub stamp: Option<String>,
    /// When GitHub last confirmed the value.
    pub fetched_at: Option<SystemTime>,
    /// When reading it last failed, so it isn't tried again at once.
    pub failed_at: Option<Instant>,
}

/// One run of a read, as a poll.
#[derive(Clone)]
pub(crate) struct ReadJob {
    pub name: &'static str,
    pub run: ReadFn,
    pub known: Option<Value>,
    /// The watched fields of the check it's read at.
    pub stamp: String,
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

    #[test]
    fn a_stamp_holds_the_watched_fields_values() {
        let check = serde_json::json!({
            "headRefOid": "abc",
            "timelineItems": {"updatedAt": "2026-11-06T19:28:56Z", "totalCount": 9},
        });
        assert_eq!(
            stamp(&check, &["/headRefOid", "/timelineItems/updatedAt"]),
            "\"abc\"\n\"2026-11-06T19:28:56Z\""
        );
        // A field the check doesn't have reads as null, so the stamp never matches a typo's
        // by accident of an older check.
        assert_eq!(stamp(&check, &["/nope"]), "null");
    }
}
