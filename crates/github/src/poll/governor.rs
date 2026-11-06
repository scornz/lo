//! How often to poll: by tier, by whether rozed is in the foreground, and by how much of GitHub's
//! rate limit is left (design §3 "Rate limit", research §7 "Recommended scheme").

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use gpui::BackgroundExecutor;

use crate::rate_limit::{RateLimit, RateLimits};

/// The resources polling spends: REST requests and GraphQL points.
const RESOURCES: [&str; 2] = ["core", "graphql"];

/// The polling schedule and the rate-limit governor's thresholds.
#[derive(Clone, Debug)]
pub struct PollConfig {
    /// The open PR, while rozed is in the foreground. Design §3 asks for 15–30 s.
    pub open_interval: Duration,
    /// Every other watched PR, and the open PRs of each watched repository, in the foreground.
    pub watched_interval: Duration,
    /// Everything, while rozed is in the background.
    pub background_interval: Duration,
    /// The longest the open PR's details go without being fetched in full while rozed is in the
    /// foreground. Viewed marks set elsewhere change nothing a cheaper poll sees.
    pub details_max_age: Duration,
    /// Below this share of a rate limit left, every interval doubles.
    pub slow_below: f64,
    /// Below this share, only the open PR is polled, at most every `open_only_interval`.
    pub open_only_below: f64,
    pub open_only_interval: Duration,
    /// After a network error: the first wait before retrying, doubling up to `offline_max_wait`.
    pub offline_first_wait: Duration,
    pub offline_max_wait: Duration,
    /// After a secondary rate limit without a `retry-after`: the first wait, doubling up to
    /// `secondary_max_wait`. GitHub asks for at least a minute.
    pub secondary_first_wait: Duration,
    pub secondary_max_wait: Duration,
    /// The most of each backoff wait taken off at random, so retries don't line up.
    pub jitter: f64,
    /// The least time between the end of one write and the start of the next. GitHub asks for a
    /// second between mutations.
    pub write_spacing: Duration,
    /// Wall-clock time, for rate-limit resets and the age of cached data.
    pub wall_clock: WallClock,
}

impl Default for PollConfig {
    fn default() -> Self {
        Self {
            open_interval: Duration::from_secs(20),
            watched_interval: Duration::from_secs(2 * 60),
            background_interval: Duration::from_secs(5 * 60),
            details_max_age: Duration::from_secs(5 * 60),
            slow_below: 0.20,
            open_only_below: 0.05,
            open_only_interval: Duration::from_secs(2 * 60),
            offline_first_wait: Duration::from_secs(20),
            offline_max_wait: Duration::from_secs(5 * 60),
            secondary_first_wait: Duration::from_secs(60),
            secondary_max_wait: Duration::from_secs(15 * 60),
            jitter: 0.2,
            write_spacing: Duration::from_secs(1),
            wall_clock: WallClock::system(),
        }
    }
}

/// Wall-clock time. Tests tie it to GPUI's fake clock with [`WallClock::following`].
#[derive(Clone)]
pub struct WallClock(Arc<dyn Fn() -> SystemTime + Send + Sync>);

impl WallClock {
    pub fn system() -> Self {
        Self(Arc::new(SystemTime::now))
    }

    /// A clock that reads `start` now and then moves with `executor`'s clock, which in tests is
    /// GPUI's fake one.
    pub fn following(executor: BackgroundExecutor, start: SystemTime) -> Self {
        let origin = executor.now();
        Self(Arc::new(move || start + (executor.now() - origin)))
    }

    pub fn now(&self) -> SystemTime {
        (self.0)()
    }
}

impl fmt::Debug for WallClock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("WallClock").field(&self.now()).finish()
    }
}

/// Which schedule a poll follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    /// The PR in the active tab.
    Open,
    /// Other watched PRs, and the open PRs of the repositories they're in.
    Watched,
}

/// A rate limit running low.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LowLimit {
    /// `core` (REST) or `graphql`.
    pub resource: String,
    pub remaining: u32,
    pub limit: u32,
    /// When the limit resets.
    pub reset: SystemTime,
}

/// Why polling is paused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PauseReason {
    /// The `resource` limit is used up.
    UsedUp { resource: String },
    /// GitHub's secondary limits kicked in.
    Secondary,
}

/// How the rate limit is shaping polling, for a banner (design §3 "Rate limit", §11 "Rate limit
/// used up").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RateLimitState {
    /// Plenty left.
    #[default]
    Normal,
    /// Under 20% left: every interval is doubled. "Polling slowed: GitHub rate limit low."
    Slowed(LowLimit),
    /// Under 5% left: only the open PR is polled, every 2 minutes.
    OpenOnly(LowLimit),
    /// Nothing is polled until `until`: "GitHub rate limit used up. Resets at <time>."
    Paused {
        until: SystemTime,
        reason: PauseReason,
    },
}

impl RateLimitState {
    fn severity(&self) -> u8 {
        match self {
            Self::Normal => 0,
            Self::Slowed(_) => 1,
            Self::OpenOnly(_) => 2,
            Self::Paused { .. } => 3,
        }
    }

    pub fn is_paused(&self) -> bool {
        matches!(self, Self::Paused { .. })
    }
}

/// A pause GitHub asked for in an error, rather than in its rate-limit headers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cooldown {
    pub until: SystemTime,
    pub reason: PauseReason,
}

/// The governor's verdict at `now`: the worst state any resource is in. A window whose reset
/// time has passed has started over, so it doesn't count.
pub fn rate_limit_state(
    limits: &RateLimits,
    cooldown: Option<&Cooldown>,
    now: SystemTime,
    config: &PollConfig,
) -> RateLimitState {
    let mut worst = RateLimitState::Normal;
    let mut consider = |candidate: RateLimitState| {
        let worse = match (&worst, &candidate) {
            (RateLimitState::Paused { until: a, .. }, RateLimitState::Paused { until: b, .. }) => {
                b > a
            }
            _ => candidate.severity() > worst.severity(),
        };
        if worse {
            worst = candidate;
        }
    };
    if let Some(cooldown) = cooldown.filter(|cooldown| cooldown.until > now) {
        consider(RateLimitState::Paused {
            until: cooldown.until,
            reason: cooldown.reason.clone(),
        });
    }
    for resource in RESOURCES {
        let Some(limit) = limits.get(resource) else {
            continue;
        };
        let reset = limit.reset_time();
        if reset <= now {
            continue;
        }
        let low = |limit: RateLimit| LowLimit {
            resource: resource.to_string(),
            remaining: limit.remaining,
            limit: limit.limit,
            reset,
        };
        if limit.remaining == 0 {
            consider(RateLimitState::Paused {
                until: reset,
                reason: PauseReason::UsedUp {
                    resource: resource.to_string(),
                },
            });
        } else if limit.fraction_left() < config.open_only_below {
            consider(RateLimitState::OpenOnly(low(limit)));
        } else if limit.fraction_left() < config.slow_below {
            consider(RateLimitState::Slowed(low(limit)));
        }
    }
    worst
}

/// How long between polls of a tier, or `None` when the tier isn't polled at all.
pub fn interval(
    tier: Tier,
    foreground: bool,
    state: &RateLimitState,
    config: &PollConfig,
) -> Option<Duration> {
    let base = match (tier, foreground) {
        (Tier::Open, true) => config.open_interval,
        (Tier::Watched, true) => config.watched_interval,
        (_, false) => config.background_interval,
    };
    match state {
        RateLimitState::Normal => Some(base),
        RateLimitState::Slowed(_) => Some(base * 2),
        RateLimitState::OpenOnly(_) => {
            (tier == Tier::Open).then(|| base.max(config.open_only_interval))
        }
        RateLimitState::Paused { .. } => None,
    }
}

/// The wait before retry number `attempt` (from 1): `first` doubling up to `max`, less up to
/// `jitter` of it at random (`random` is in `0..1`).
pub fn backoff(attempt: u32, first: Duration, max: Duration, jitter: f64, random: f64) -> Duration {
    let doublings = attempt.saturating_sub(1).min(31);
    let wait = first
        .checked_mul(1 << doublings)
        .map_or(max, |wait| wait.min(max));
    wait.mul_f64(1.0 - jitter.clamp(0.0, 1.0) * random.clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RESET: u64 = 1_800_000_000;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn limits(pairs: &[(&str, u32)]) -> RateLimits {
        let mut limits = RateLimits::default();
        for (resource, remaining) in pairs {
            limits.record(
                resource,
                RateLimit {
                    limit: 5000,
                    remaining: *remaining,
                    used: 5000 - remaining,
                    reset: RESET,
                },
            );
        }
        limits
    }

    fn state(pairs: &[(&str, u32)]) -> RateLimitState {
        rate_limit_state(
            &limits(pairs),
            None,
            at(RESET - 600),
            &PollConfig::default(),
        )
    }

    fn low(resource: &str, remaining: u32) -> LowLimit {
        LowLimit {
            resource: resource.into(),
            remaining,
            limit: 5000,
            reset: at(RESET),
        }
    }

    #[test]
    fn slows_under_20_percent_and_polls_only_the_open_pr_under_5() {
        assert_eq!(state(&[]), RateLimitState::Normal);
        assert_eq!(state(&[("core", 1000)]), RateLimitState::Normal);
        assert_eq!(
            state(&[("core", 999)]),
            RateLimitState::Slowed(low("core", 999))
        );
        assert_eq!(
            state(&[("graphql", 250)]),
            RateLimitState::Slowed(low("graphql", 250))
        );
        assert_eq!(
            state(&[("graphql", 249)]),
            RateLimitState::OpenOnly(low("graphql", 249))
        );
        assert_eq!(
            state(&[("core", 1)]),
            RateLimitState::OpenOnly(low("core", 1))
        );
    }

    #[test]
    fn pauses_until_the_reset_when_used_up() {
        assert_eq!(
            state(&[("core", 0)]),
            RateLimitState::Paused {
                until: at(RESET),
                reason: PauseReason::UsedUp {
                    resource: "core".into()
                }
            }
        );
    }

    #[test]
    fn the_worst_resource_wins() {
        assert_eq!(
            state(&[("core", 900), ("graphql", 100)]),
            RateLimitState::OpenOnly(low("graphql", 100))
        );
        assert_eq!(
            state(&[("core", 0), ("graphql", 100)]),
            RateLimitState::Paused {
                until: at(RESET),
                reason: PauseReason::UsedUp {
                    resource: "core".into()
                }
            }
        );
    }

    #[test]
    fn other_resources_dont_count() {
        assert_eq!(state(&[("search", 0)]), RateLimitState::Normal);
    }

    #[test]
    fn a_window_that_has_reset_counts_as_new() {
        let limits = limits(&[("core", 0)]);
        let config = PollConfig::default();
        assert!(rate_limit_state(&limits, None, at(RESET - 1), &config).is_paused());
        assert_eq!(
            rate_limit_state(&limits, None, at(RESET), &config),
            RateLimitState::Normal
        );
    }

    #[test]
    fn a_cooldown_pauses_until_it_ends() {
        let config = PollConfig::default();
        let cooldown = Cooldown {
            until: at(RESET + 60),
            reason: PauseReason::Secondary,
        };
        let limits = limits(&[("core", 0)]);
        // The later of the two pauses wins.
        assert_eq!(
            rate_limit_state(&limits, Some(&cooldown), at(RESET - 600), &config),
            RateLimitState::Paused {
                until: at(RESET + 60),
                reason: PauseReason::Secondary
            }
        );
        assert_eq!(
            rate_limit_state(
                &RateLimits::default(),
                Some(&cooldown),
                at(RESET + 60),
                &config
            ),
            RateLimitState::Normal
        );
    }

    #[test]
    fn intervals_by_tier_window_and_rate_limit() {
        let config = PollConfig::default();
        let secs = |tier, foreground, state: &RateLimitState| {
            interval(tier, foreground, state, &config).map(|d| d.as_secs())
        };
        let normal = RateLimitState::Normal;
        assert_eq!(secs(Tier::Open, true, &normal), Some(20));
        assert_eq!(secs(Tier::Watched, true, &normal), Some(120));
        assert_eq!(secs(Tier::Open, false, &normal), Some(300));
        assert_eq!(secs(Tier::Watched, false, &normal), Some(300));

        let slowed = RateLimitState::Slowed(low("core", 900));
        assert_eq!(secs(Tier::Open, true, &slowed), Some(40));
        assert_eq!(secs(Tier::Watched, true, &slowed), Some(240));
        assert_eq!(secs(Tier::Open, false, &slowed), Some(600));

        let open_only = RateLimitState::OpenOnly(low("core", 100));
        assert_eq!(secs(Tier::Open, true, &open_only), Some(120));
        assert_eq!(secs(Tier::Open, false, &open_only), Some(300));
        assert_eq!(secs(Tier::Watched, true, &open_only), None);

        let paused = RateLimitState::Paused {
            until: at(RESET),
            reason: PauseReason::Secondary,
        };
        assert_eq!(secs(Tier::Open, true, &paused), None);
        assert_eq!(secs(Tier::Watched, false, &paused), None);
    }

    #[test]
    fn backoff_doubles_up_to_the_most_less_some_jitter() {
        let first = Duration::from_secs(20);
        let max = Duration::from_secs(300);
        let waits: Vec<u64> = (1..=7)
            .map(|attempt| backoff(attempt, first, max, 0.2, 0.0).as_secs())
            .collect();
        assert_eq!(waits, [20, 40, 80, 160, 300, 300, 300]);
        assert_eq!(backoff(1, first, max, 0.2, 0.5), Duration::from_secs(18));
        assert_eq!(backoff(5, first, max, 0.2, 0.999).as_secs(), 240);
        // Attempt 0 is treated as the first; huge attempts don't overflow.
        assert_eq!(backoff(0, first, max, 0.0, 0.0), first);
        assert_eq!(backoff(200, first, max, 0.0, 0.0), max);
    }
}
