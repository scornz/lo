//! Viewed marks as the tab shows them: GitHub's, with the marks rozed is setting on top until
//! GitHub has answered (design §6 "Viewed marks").
//!
//! `e` marks the current file viewed, or clears the mark of a file that's viewed. The new mark
//! shows at once. If the write fails, the file shows GitHub's mark again and a toast says why.

use std::collections::HashMap;

use github::GithubError;
use review::revisions::ViewedState;

/// GitHub's viewed marks, and the ones rozed is setting.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ViewedMarks {
    /// GitHub's marks for the account acting, by path, as last read.
    github: HashMap<String, ViewedState>,
    /// Marks set in rozed whose write hasn't finished, by path.
    pending: HashMap<String, Pending>,
    /// Counts the toggles, so a write that finishes after a later toggle of the same file doesn't
    /// undo it.
    toggles: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pending {
    state: ViewedState,
    toggle: u64,
}

/// What `e` asked GitHub to do to a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toggle {
    pub path: String,
    /// Whether it marks the file viewed or clears its mark.
    pub mark: bool,
    id: u64,
}

impl Toggle {
    /// The mark the file shows once the write succeeds.
    pub fn state(&self) -> ViewedState {
        if self.mark {
            ViewedState::Viewed
        } else {
            ViewedState::Unviewed
        }
    }
}

impl ViewedMarks {
    /// The mark `path` shows: the one rozed is setting, or GitHub's. `None` when GitHub hasn't
    /// said.
    pub fn state(&self, path: &str) -> Option<ViewedState> {
        match self.pending.get(path) {
            Some(pending) => Some(pending.state),
            None => self.github.get(path).copied(),
        }
    }

    /// Every file's mark as [`Self::state`] gives it, by path.
    pub fn shown(&self) -> HashMap<String, ViewedState> {
        let mut shown = self.github.clone();
        shown.extend(
            self.pending
                .iter()
                .map(|(path, pending)| (path.clone(), pending.state)),
        );
        shown
    }

    /// Takes GitHub's marks. Marks rozed is setting still show over them. Returns whether
    /// GitHub's marks changed.
    pub fn set_github(&mut self, marks: HashMap<String, ViewedState>) -> bool {
        if marks == self.github {
            return false;
        }
        self.github = marks;
        true
    }

    /// Whether a write for `path` hasn't finished.
    pub fn is_pending(&self, path: &str) -> bool {
        self.pending.contains_key(path)
    }

    /// `e` on `path`: a viewed file's mark is cleared, and any other file, unviewed, changed since
    /// viewed or with no mark known, is marked viewed. The new mark shows at once.
    pub fn toggle(&mut self, path: &str) -> Toggle {
        let mark = self.state(path) != Some(ViewedState::Viewed);
        self.toggles += 1;
        let toggle = Toggle {
            path: path.to_string(),
            mark,
            id: self.toggles,
        };
        self.pending.insert(
            path.to_string(),
            Pending {
                state: toggle.state(),
                toggle: toggle.id,
            },
        );
        toggle
    }

    /// A toggle's write finished. If it succeeded, GitHub's mark is the new one until GitHub says
    /// otherwise; if it failed, the file shows GitHub's mark again. Either way a later toggle of
    /// the same file still shows until its own write finishes.
    pub fn finish(&mut self, toggle: &Toggle, succeeded: bool) {
        if succeeded {
            self.github.insert(toggle.path.clone(), toggle.state());
        }
        if self
            .pending
            .get(&toggle.path)
            .is_some_and(|pending| pending.toggle == toggle.id)
        {
            self.pending.remove(&toggle.path);
        }
    }
}

/// The toast for a toggle whose write failed, as `login`: what couldn't be done and why.
pub fn write_failed(toggle: &Toggle, login: &str, error: &GithubError) -> String {
    let what = if toggle.mark {
        format!("mark {} viewed", toggle.path)
    } else {
        format!("clear the viewed mark on {}", toggle.path)
    };
    let why = match error {
        GithubError::Network(_) => "rozed can't reach GitHub".to_string(),
        GithubError::RateLimited {
            secondary: false, ..
        } => format!("{login} has used up GitHub's rate limit"),
        GithubError::RateLimited {
            secondary: true, ..
        } => "GitHub asked rozed to slow down".to_string(),
        GithubError::Unauthorized { .. } => format!("{login} is signed out"),
        GithubError::TokenUnavailable(_) => format!("{login}'s token couldn't be read"),
        GithubError::NotFound { .. } => format!("{login} can't see this pull request"),
        GithubError::GraphQl { errors } => {
            let messages: Vec<&str> = errors.iter().map(|error| error.message.as_str()).collect();
            let forbidden = errors
                .iter()
                .any(|error| error.r#type.as_deref() == Some("FORBIDDEN"));
            if forbidden {
                format!("{login} isn't allowed to ({})", messages.join("; "))
            } else {
                format!("GitHub said: {}", messages.join("; "))
            }
        }
        GithubError::Api {
            status: 403,
            message,
            ..
        } => format!("{login} isn't allowed to ({message})"),
        other => other.to_string(),
    };
    format!("Couldn't {what}: {why}. It shows GitHub's mark again.")
}

#[cfg(test)]
mod tests {
    use github::GraphQlError;

    use super::*;

    fn marks(entries: &[(&str, ViewedState)]) -> HashMap<String, ViewedState> {
        entries
            .iter()
            .map(|(path, state)| (path.to_string(), *state))
            .collect()
    }

    fn github(entries: &[(&str, ViewedState)]) -> ViewedMarks {
        let mut viewed = ViewedMarks::default();
        viewed.set_github(marks(entries));
        viewed
    }

    use ViewedState::{Dismissed, Unviewed, Viewed};

    #[test]
    fn e_marks_a_file_viewed_unless_it_is_viewed() {
        let mut viewed = github(&[("a", Unviewed), ("b", Dismissed), ("c", Viewed)]);

        let a = viewed.toggle("a");
        let b = viewed.toggle("b");
        let c = viewed.toggle("c");
        let unknown = viewed.toggle("d");

        assert_eq!(
            (a.mark, b.mark, c.mark, unknown.mark),
            (true, true, false, true)
        );
        // The new marks show at once.
        assert_eq!(
            viewed.shown(),
            marks(&[("a", Viewed), ("b", Viewed), ("c", Unviewed), ("d", Viewed)])
        );
        assert!(viewed.is_pending("a"));
    }

    #[test]
    fn a_finished_write_shows_the_new_mark_until_github_says_otherwise() {
        let mut viewed = github(&[("a", Unviewed)]);
        let toggle = viewed.toggle("a");

        viewed.finish(&toggle, true);

        assert!(!viewed.is_pending("a"));
        assert_eq!(viewed.state("a"), Some(Viewed));
        // GitHub's next answer wins.
        assert!(viewed.set_github(marks(&[("a", Dismissed)])));
        assert_eq!(viewed.state("a"), Some(Dismissed));
    }

    #[test]
    fn a_failed_write_shows_githubs_mark_again() {
        let mut viewed = github(&[("a", Unviewed), ("b", Viewed)]);
        let mark = viewed.toggle("a");
        let clear = viewed.toggle("b");

        viewed.finish(&mark, false);
        viewed.finish(&clear, false);

        assert_eq!(viewed.shown(), marks(&[("a", Unviewed), ("b", Viewed)]));
        assert!(!viewed.is_pending("a"));
    }

    #[test]
    fn a_failed_write_reverts_to_githubs_latest_mark() {
        let mut viewed = github(&[("a", Unviewed)]);
        let toggle = viewed.toggle("a");
        // GitHub's marks change while the write is in flight: the pending mark still shows.
        viewed.set_github(marks(&[("a", Dismissed)]));
        assert_eq!(viewed.state("a"), Some(Viewed));

        viewed.finish(&toggle, false);

        assert_eq!(viewed.state("a"), Some(Dismissed));
    }

    #[test]
    fn a_write_that_finishes_after_a_later_toggle_doesnt_undo_it() {
        let mut viewed = github(&[("a", Unviewed)]);
        let mark = viewed.toggle("a");
        let clear = viewed.toggle("a");
        assert!(!clear.mark);
        assert_eq!(viewed.state("a"), Some(Unviewed));

        // The mark went through, then the clear failed: GitHub has the file viewed.
        viewed.finish(&mark, true);
        assert_eq!(
            viewed.state("a"),
            Some(Unviewed),
            "the clear is still in flight"
        );
        viewed.finish(&clear, false);
        assert_eq!(viewed.state("a"), Some(Viewed));
    }

    #[test]
    fn the_same_marks_from_github_arent_a_change() {
        let mut viewed = github(&[("a", Viewed)]);
        assert!(!viewed.set_github(marks(&[("a", Viewed)])));
        assert!(viewed.set_github(marks(&[("a", Unviewed)])));
    }

    fn toggle(path: &str, mark: bool) -> Toggle {
        Toggle {
            path: path.into(),
            mark,
            id: 1,
        }
    }

    #[test]
    fn the_toast_says_what_failed_and_why() {
        let marking = toggle("src/main.rs", true);
        let clearing = toggle("src/main.rs", false);
        let forbidden = GithubError::GraphQl {
            errors: vec![GraphQlError {
                message: "Resource not accessible by integration".into(),
                r#type: Some("FORBIDDEN".into()),
                path: vec![],
                extensions: None,
            }],
        };
        let rate_limited = |secondary| GithubError::RateLimited {
            message: "API rate limit exceeded".into(),
            secondary,
            reset: None,
            retry_after: None,
            resource: Some("graphql".into()),
        };
        let cases = [
            (
                &marking,
                GithubError::Network(anyhow::anyhow!("offline")),
                "Couldn't mark src/main.rs viewed: rozed can't reach GitHub. It shows GitHub's \
                 mark again.",
            ),
            (
                &clearing,
                GithubError::Network(anyhow::anyhow!("offline")),
                "Couldn't clear the viewed mark on src/main.rs: rozed can't reach GitHub. It \
                 shows GitHub's mark again.",
            ),
            (
                &marking,
                rate_limited(false),
                "Couldn't mark src/main.rs viewed: rozed-bot has used up GitHub's rate limit. It \
                 shows GitHub's mark again.",
            ),
            (
                &marking,
                rate_limited(true),
                "Couldn't mark src/main.rs viewed: GitHub asked rozed to slow down. It shows \
                 GitHub's mark again.",
            ),
            (
                &marking,
                forbidden,
                "Couldn't mark src/main.rs viewed: rozed-bot isn't allowed to (Resource not \
                 accessible by integration). It shows GitHub's mark again.",
            ),
            (
                &marking,
                GithubError::Unauthorized {
                    message: "Bad credentials".into(),
                },
                "Couldn't mark src/main.rs viewed: rozed-bot is signed out. It shows GitHub's \
                 mark again.",
            ),
        ];
        for (toggle, error, expected) in cases {
            assert_eq!(write_failed(toggle, "rozed-bot", &error), expected);
        }
    }
}
