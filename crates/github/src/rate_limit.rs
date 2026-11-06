//! How much of GitHub's rate limit an account has left.
//!
//! GitHub reports it in `x-ratelimit-*` headers on every response, per resource (`core` for REST,
//! `graphql`, `search` and so on), and in a GraphQL query's `rateLimit` field. The budget is per
//! user and shared with everything else the user runs, such as `gh`.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use http_client::http::HeaderMap;
use serde::Deserialize;

/// How far apart, in seconds, two answers' reset times can be and still be one window: a
/// GraphQL query's `rateLimit.resetAt` and its headers' `x-ratelimit-reset` can differ by a second.
pub const SAME_WINDOW: u64 = 2;

/// One resource's limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RateLimit {
    pub limit: u32,
    pub remaining: u32,
    pub used: u32,
    /// When the window resets, in seconds since the Unix epoch.
    pub reset: u64,
}

impl RateLimit {
    pub fn reset_time(&self) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(self.reset)
    }

    /// The share of the limit left, from 0 to 1.
    pub fn fraction_left(&self) -> f64 {
        if self.limit == 0 {
            0.0
        } else {
            f64::from(self.remaining) / f64::from(self.limit)
        }
    }
}

/// An account's limits, by resource.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RateLimits {
    resources: BTreeMap<String, RateLimit>,
    /// What the last GraphQL query that asked for `rateLimit { cost }` cost, in points.
    pub last_graphql_cost: Option<u32>,
}

impl RateLimits {
    pub fn get(&self, resource: &str) -> Option<RateLimit> {
        self.resources.get(resource).copied()
    }

    /// The REST API's limit.
    pub fn core(&self) -> Option<RateLimit> {
        self.get("core")
    }

    pub fn graphql(&self) -> Option<RateLimit> {
        self.get("graphql")
    }

    /// Records a limit GitHub reported.
    ///
    /// GitHub's servers can disagree from one response to the next, so within one window (reset
    /// times at most [`SAME_WINDOW`] apart) this keeps the lowest `remaining` seen. A later
    /// reset time starts a new window, and an answer from an earlier window, from a server that
    /// hasn't seen the reset yet, is ignored.
    pub fn record(&mut self, resource: &str, limit: RateLimit) {
        match self.resources.get_mut(resource) {
            Some(known) if known.reset.abs_diff(limit.reset) <= SAME_WINDOW => {
                known.remaining = known.remaining.min(limit.remaining);
                known.used = known.used.max(limit.used);
                known.limit = limit.limit;
                known.reset = known.reset.max(limit.reset);
            }
            Some(known) if limit.reset < known.reset => {}
            _ => {
                self.resources.insert(resource.to_string(), limit);
            }
        }
    }

    /// Records the limit in a response's `x-ratelimit-*` headers, if it has them.
    pub fn record_headers(&mut self, headers: &HeaderMap) {
        let number = |name: &str| -> Option<u64> {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().parse().ok())
        };
        let (Some(limit), Some(remaining), Some(reset)) = (
            number("x-ratelimit-limit"),
            number("x-ratelimit-remaining"),
            number("x-ratelimit-reset"),
        ) else {
            return;
        };
        let to_u32 = |n: u64| u32::try_from(n).unwrap_or(u32::MAX);
        let used = number("x-ratelimit-used").unwrap_or(limit.saturating_sub(remaining));
        let resource = headers
            .get("x-ratelimit-resource")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("core");
        self.record(
            resource,
            RateLimit {
                limit: to_u32(limit),
                remaining: to_u32(remaining),
                used: to_u32(used),
                reset,
            },
        );
    }

    /// Records a GraphQL query's `rateLimit` field.
    pub fn record_graphql_field(&mut self, field: &GraphQlRateLimit) {
        if let Some(cost) = field.cost {
            self.last_graphql_cost = Some(cost);
        }
        let reset = chrono::DateTime::parse_from_rfc3339(&field.reset_at)
            .ok()
            .and_then(|time| u64::try_from(time.timestamp()).ok());
        if let Some(reset) = reset {
            self.record(
                "graphql",
                RateLimit {
                    limit: field.limit,
                    remaining: field.remaining,
                    used: field.used,
                    reset,
                },
            );
        }
    }
}

/// GraphQL's `rateLimit { cost limit remaining used resetAt }`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GraphQlRateLimit {
    #[serde(default)]
    pub cost: Option<u32>,
    pub limit: u32,
    pub remaining: u32,
    pub used: u32,
    pub reset_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    use http_client::http::HeaderValue;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        pairs
            .iter()
            .map(|(name, value)| {
                (
                    http_client::http::HeaderName::from_static(name),
                    HeaderValue::from_static(value),
                )
            })
            .collect()
    }

    #[test]
    fn reads_the_headers_per_resource() {
        let mut limits = RateLimits::default();
        limits.record_headers(&headers(&[
            ("x-ratelimit-limit", "5000"),
            ("x-ratelimit-remaining", "4990"),
            ("x-ratelimit-used", "10"),
            ("x-ratelimit-reset", "1762419600"),
            ("x-ratelimit-resource", "graphql"),
        ]));
        assert_eq!(
            limits.graphql(),
            Some(RateLimit {
                limit: 5000,
                remaining: 4990,
                used: 10,
                reset: 1762419600
            })
        );
        assert_eq!(limits.core(), None);
    }

    #[test]
    fn headers_without_a_resource_are_core() {
        let mut limits = RateLimits::default();
        limits.record_headers(&headers(&[
            ("x-ratelimit-limit", "60"),
            ("x-ratelimit-remaining", "59"),
            ("x-ratelimit-reset", "1762419600"),
        ]));
        let core = limits.core().unwrap();
        assert_eq!((core.remaining, core.used), (59, 1));
    }

    #[test]
    fn missing_headers_change_nothing() {
        let mut limits = RateLimits::default();
        limits.record_headers(&headers(&[("x-ratelimit-limit", "5000")]));
        assert_eq!(limits, RateLimits::default());
    }

    #[test]
    fn keeps_the_lowest_remaining_within_a_window() {
        let mut limits = RateLimits::default();
        let at = |remaining, used, reset| RateLimit {
            limit: 5000,
            remaining,
            used,
            reset,
        };
        limits.record("core", at(4977, 23, 100));
        limits.record("core", at(4999, 1, 100));
        assert_eq!(limits.core(), Some(at(4977, 23, 100)));
        limits.record("core", at(4975, 25, 100));
        assert_eq!(limits.core(), Some(at(4975, 25, 100)));
        // A new window starts over.
        limits.record("core", at(4999, 1, 200));
        assert_eq!(limits.core(), Some(at(4999, 1, 200)));
    }

    #[test]
    fn ignores_a_late_answer_from_a_window_that_has_reset() {
        let mut limits = RateLimits::default();
        let at = |remaining, reset| RateLimit {
            limit: 5000,
            remaining,
            used: 5000 - remaining,
            reset,
        };
        limits.record("core", at(0, 100));
        limits.record("core", at(4999, 3700));
        // A server that hasn't seen the reset yet still answers with the old window.
        limits.record("core", at(0, 100));
        assert_eq!(limits.core(), Some(at(4999, 3700)));
    }

    #[test]
    fn answers_a_second_apart_are_one_window() {
        let mut limits = RateLimits::default();
        let at = |remaining, reset| RateLimit {
            limit: 5000,
            remaining,
            used: 5000 - remaining,
            reset,
        };
        limits.record("graphql", at(4990, 100));
        limits.record("graphql", at(4995, 101));
        assert_eq!(limits.graphql(), Some(at(4990, 101)));
        limits.record("graphql", at(4980, 100));
        assert_eq!(limits.graphql(), Some(at(4980, 101)));
    }

    #[test]
    fn reads_the_graphql_field() {
        let mut limits = RateLimits::default();
        let field: GraphQlRateLimit = serde_json::from_str(
            r#"{"cost":1,"limit":5000,"remaining":4998,"used":2,"resetAt":"2026-11-06T09:00:00Z"}"#,
        )
        .unwrap();
        limits.record_graphql_field(&field);
        assert_eq!(limits.last_graphql_cost, Some(1));
        let graphql = limits.graphql().unwrap();
        assert_eq!(graphql.remaining, 4998);
        assert_eq!(graphql.reset, 1_793_955_600);
        assert!((graphql.fraction_left() - 0.9996).abs() < 1e-9);
    }
}
