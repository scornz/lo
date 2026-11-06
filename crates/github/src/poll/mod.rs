//! Polling GitHub for changes to the pull requests rozed has open (design §3 "GitHub API").

mod governor;
mod poller;
mod queue;

pub use governor::{
    Cooldown, LowLimit, PauseReason, PollConfig, RateLimitState, Tier, WallClock, backoff,
    interval, rate_limit_state,
};
pub use poller::{Offline, PollEvent, PollStatus, Poller, PullRequestAnswer};
pub use queue::RequestQueue;
