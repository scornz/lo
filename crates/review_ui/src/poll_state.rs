//! How a tab's polling is going, in words for its header: signed out, the rate limit, offline
//! with how old the details are, or a poll GitHub refused (design §3 "Rate limit", §11 "Offline",
//! "Rate limit used up", "Signed out").

use std::sync::Arc;
use std::time::SystemTime;

use github::poll::{PauseReason, PollStatus, RateLimitState};
use github::{GithubError, HelpLink};
use ui::Severity;

/// What a tab knows about its account's polling of its pull request.
#[derive(Clone, Debug, Default)]
pub struct PollState {
    /// The account's poller's status.
    pub status: PollStatus,
    /// Why the last poll of the pull request failed, until one succeeds.
    pub failure: Option<Arc<GithubError>>,
    /// When GitHub gave the details the tab shows.
    pub details_at: Option<SystemTime>,
}

/// A line for the header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PollNote {
    pub severity: Severity,
    pub text: String,
    /// GitHub's link to fix it, such as authorizing single sign-on.
    pub link: Option<HelpLink>,
}

impl PollNote {
    fn new(severity: Severity, text: String) -> Self {
        Self {
            severity,
            text,
            link: None,
        }
    }
}

/// What the header says about `login`'s polling at `now`, or nothing while it's going well.
pub fn note(login: &str, state: &PollState, now: SystemTime) -> Option<PollNote> {
    let status = &state.status;
    if status.signed_out {
        return Some(PollNote::new(
            Severity::Error,
            format!(
                "Signed out: GitHub refused {login}'s token. Sign in again from Accounts… in the \
                 Lo menu."
            ),
        ));
    }
    if let RateLimitState::Paused { until, reason } = &status.rate_limit {
        let why = match reason {
            PauseReason::UsedUp { .. } => "GitHub's rate limit is used up",
            PauseReason::Secondary => "GitHub asked Lo to slow down",
        };
        return Some(PollNote::new(
            Severity::Warning,
            format!(
                "{why}, so nothing updates until it resets {}.",
                in_words(*until, now)
            ),
        ));
    }
    if let Some(offline) = &status.offline {
        let details = match state.details_at {
            Some(at) => format!(" These details are from {}.", ago(at, now)),
            None => String::new(),
        };
        return Some(PollNote::new(
            Severity::Warning,
            format!(
                "Offline since {}.{details} Lo tries again {}.",
                ago(offline.since, now),
                in_words(offline.retry_at, now)
            ),
        ));
    }
    if let Some(error) = &state.failure {
        return Some(PollNote {
            severity: Severity::Warning,
            text: format!("Lo couldn't update this pull request: {error}"),
            link: error.help_link(),
        });
    }
    match &status.rate_limit {
        RateLimitState::Slowed(low) | RateLimitState::OpenOnly(low) => Some(PollNote::new(
            Severity::Info,
            format!(
                "GitHub's rate limit is low, so Lo checks less often until it resets {}.",
                in_words(low.reset, now)
            ),
        )),
        RateLimitState::Normal | RateLimitState::Paused { .. } => None,
    }
}

/// How long before `now` `then` was, in words: "just now", "5 minutes ago", "2 hours ago".
pub fn ago(then: SystemTime, now: SystemTime) -> String {
    let seconds = now.duration_since(then).unwrap_or_default().as_secs();
    match span(seconds) {
        None => "just now".to_string(),
        Some(span) => format!("{span} ago"),
    }
}

/// How long after `now` `then` is, in words: "in a moment", "in 5 minutes".
pub fn in_words(then: SystemTime, now: SystemTime) -> String {
    let seconds = then.duration_since(now).unwrap_or_default().as_secs();
    match span(seconds) {
        None => "in a moment".to_string(),
        Some(span) => format!("in {span}"),
    }
}

/// `seconds` in the largest whole unit, or `None` under a minute.
fn span(seconds: u64) -> Option<String> {
    let (count, unit) = match seconds {
        0..60 => return None,
        60..3_600 => (seconds / 60, "minute"),
        3_600..86_400 => (seconds / 3_600, "hour"),
        _ => (seconds / 86_400, "day"),
    };
    Some(match count {
        1 if unit == "hour" => "an hour".to_string(),
        1 => format!("a {unit}"),
        count => format!("{count} {unit}s"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration;

    use github::poll::{LowLimit, Offline};

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 + seconds)
    }

    const NOW: u64 = 1_000_000;

    fn note_for(state: PollState) -> Option<(Severity, String)> {
        note("rozed-bot", &state, at(NOW)).map(|note| (note.severity, note.text))
    }

    fn status(status: PollStatus) -> PollState {
        PollState {
            status,
            ..PollState::default()
        }
    }

    #[test]
    fn ages_read_in_words() {
        let ago = |seconds| super::ago(at(NOW - seconds), at(NOW));
        assert_eq!(ago(0), "just now");
        assert_eq!(ago(59), "just now");
        assert_eq!(ago(60), "a minute ago");
        assert_eq!(ago(150), "2 minutes ago");
        assert_eq!(ago(3_600), "an hour ago");
        assert_eq!(ago(7_300), "2 hours ago");
        assert_eq!(ago(86_400 + 3), "a day ago");
        // A clock that went back.
        assert_eq!(super::ago(at(NOW + 5), at(NOW)), "just now");
        assert_eq!(in_words(at(NOW + 30), at(NOW)), "in a moment");
        assert_eq!(in_words(at(NOW + 300), at(NOW)), "in 5 minutes");
        assert_eq!(in_words(at(NOW - 300), at(NOW)), "in a moment");
    }

    #[test]
    fn polling_that_goes_well_says_nothing() {
        assert_eq!(note_for(PollState::default()), None);
    }

    #[test]
    fn a_signed_out_account_asks_to_sign_in_again() {
        let state = status(PollStatus {
            signed_out: true,
            ..PollStatus::default()
        });
        assert_eq!(
            note_for(state),
            Some((
                Severity::Error,
                "Signed out: GitHub refused rozed-bot's token. Sign in again from Accounts… in \
                 the Lo menu."
                    .into()
            ))
        );
    }

    #[test]
    fn a_used_up_rate_limit_says_when_it_resets() {
        let paused = |reason| {
            status(PollStatus {
                rate_limit: RateLimitState::Paused {
                    until: at(NOW + 25 * 60),
                    reason,
                },
                ..PollStatus::default()
            })
        };
        assert_eq!(
            note_for(paused(PauseReason::UsedUp {
                resource: "graphql".into()
            })),
            Some((
                Severity::Warning,
                "GitHub's rate limit is used up, so nothing updates until it resets in 25 \
                 minutes."
                    .into()
            ))
        );
        assert_eq!(
            note_for(paused(PauseReason::Secondary)).unwrap().1,
            "GitHub asked Lo to slow down, so nothing updates until it resets in 25 minutes."
        );
        let low = LowLimit {
            resource: "core".into(),
            remaining: 100,
            limit: 5_000,
            reset: at(NOW + 2 * 3_600),
        };
        assert_eq!(
            note_for(status(PollStatus {
                rate_limit: RateLimitState::Slowed(low),
                ..PollStatus::default()
            })),
            Some((
                Severity::Info,
                "GitHub's rate limit is low, so Lo checks less often until it resets in 2 \
                 hours."
                    .into()
            ))
        );
    }

    #[test]
    fn offline_says_how_old_the_details_are_and_when_it_tries_again() {
        let offline = PollState {
            status: PollStatus {
                offline: Some(Offline {
                    since: at(NOW - 3 * 60),
                    retry_at: at(NOW + 2 * 60),
                    error: "the network is down".into(),
                }),
                ..PollStatus::default()
            },
            failure: None,
            details_at: Some(at(NOW - 2 * 3_600)),
        };
        assert_eq!(
            note_for(offline),
            Some((
                Severity::Warning,
                "Offline since 3 minutes ago. These details are from 2 hours ago. Lo tries \
                 again in 2 minutes."
                    .into()
            ))
        );
    }

    #[test]
    fn a_failed_poll_says_why_with_githubs_link() {
        let sso = GithubError::SsoRequired {
            message: "Resource protected by organization SAML enforcement.".into(),
            url: Some("https://github.com/orgs/acme/sso?authorization_request=A1".into()),
        };
        let state = PollState {
            failure: Some(Arc::new(sso)),
            ..PollState::default()
        };
        let note = note("rozed-bot", &state, at(NOW)).unwrap();
        assert_eq!(
            note.text,
            "Lo couldn't update this pull request: Resource protected by organization SAML \
             enforcement."
        );
        assert_eq!(
            note.link.unwrap().url(),
            "https://github.com/orgs/acme/sso?authorization_request=A1"
        );
    }
}
