//! Viewed marks as the tab shows them: GitHub's, with the marks rozed is setting on top until
//! GitHub has answered (design §6 "Viewed marks").
//!
//! `e` marks the current file viewed, or clears the mark of a file that's viewed. The new mark
//! shows at once. If the write fails, the file shows GitHub's mark again and a toast says why.

use std::collections::{HashMap, HashSet};
use std::time::SystemTime;

use github::GithubError;
use review::revisions::{FileComparison, Revisions, ViewedState, viewed_file_comparison};

/// GitHub's viewed marks, and the ones rozed is setting.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ViewedMarks {
    /// GitHub's marks for the account acting, by path, as last read.
    github: HashMap<String, ViewedState>,
    /// Every path GitHub lists for the pull request, when it listed them all. GitHub caps a pull
    /// request's files, and leaves out what it can't show.
    listed: Option<HashSet<String>>,
    /// Marks set in rozed whose write hasn't finished, by path.
    pending: HashMap<String, Pending>,
    /// Counts the toggles, so a write that finishes after a later toggle of the same file doesn't
    /// undo it.
    toggles: u64,
    /// When GitHub last took a write for each path, so marks it gave before then don't undo it.
    written: HashMap<String, SystemTime>,
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

    /// GitHub's own mark on `path`, as last read or as a write that GitHub took set it.
    pub fn github_state(&self, path: &str) -> Option<ViewedState> {
        self.github.get(path).copied()
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

    /// Takes the paths GitHub lists for the pull request, or `None` when it didn't list them all.
    /// Returns whether they changed.
    pub fn set_listed(&mut self, listed: Option<HashSet<String>>) -> bool {
        if listed == self.listed {
            return false;
        }
        self.listed = listed;
        true
    }

    /// The path GitHub knows a file by, which its mark and the writes use, for a file the local
    /// copy lists at `path`: `path`, or, for a rename GitHub lists by its old path, `previous`.
    /// `None` when GitHub listed every file and neither is among them: GitHub refuses a mark on
    /// a path it doesn't list. While its list is partial, any path may be on it.
    pub fn github_path(&self, path: &str, previous: Option<&str>) -> Option<String> {
        let lists = |path: &str| match &self.listed {
            Some(listed) => listed.contains(path),
            None => self.github.contains_key(path),
        };
        if lists(path) {
            return Some(path.to_string());
        }
        if let Some(previous) = previous.filter(|previous| lists(previous)) {
            return Some(previous.to_string());
        }
        self.listed.is_none().then(|| path.to_string())
    }

    /// `marks`, as GitHub gave them at `fetched_at`, less what's out of date: a file GitHub took
    /// a write for after then keeps the mark it has now.
    pub fn without_stale(
        &self,
        mut marks: HashMap<String, ViewedState>,
        fetched_at: SystemTime,
    ) -> HashMap<String, ViewedState> {
        for (path, written_at) in &self.written {
            if fetched_at >= *written_at {
                continue;
            }
            match self.github.get(path) {
                Some(state) => marks.insert(path.clone(), *state),
                None => marks.remove(path),
            };
        }
        marks
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

    /// A toggle's write finished, at `at`. If it succeeded, GitHub's mark is the new one until
    /// GitHub says otherwise; if it failed, the file shows GitHub's mark again. Either way a later
    /// toggle of the same file still shows until its own write finishes.
    pub fn finish(&mut self, toggle: &Toggle, succeeded: bool, at: SystemTime) {
        if succeeded {
            self.github.insert(toggle.path.clone(), toggle.state());
            self.written.insert(toggle.path.clone(), at);
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
        GithubError::GraphQl { errors }
            if errors
                .iter()
                .any(|error| error.r#type.as_deref() == Some("UNPROCESSABLE")) =>
        {
            let messages: Vec<&str> = errors.iter().map(|error| error.message.as_str()).collect();
            format!(
                "GitHub doesn't list {} in this pull request ({})",
                toggle.path,
                messages.join("; ")
            )
        }
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

/// What the diff says about a file GitHub says changed since it was viewed, given the head this
/// Mac recorded when it was marked viewed. `None` for any other file.
///
/// Comparing two revisions arrives with the revision picker, so for now the diff always shows the
/// file's whole change; the note says since when it changed.
pub fn changed_since_viewed(
    revisions: &Revisions,
    state: Option<ViewedState>,
    viewed_at: Option<&str>,
) -> Option<String> {
    let comparison = viewed_file_comparison(revisions, state?, viewed_at, None)?;
    let since = |from: &str, when: &str| {
        let label = revisions
            .label(from)
            .unwrap_or_else(|| from.chars().take(7).collect());
        format!("Changed since {when} in {label}.")
    };
    let since = match comparison {
        // The comparison starts from an earlier revision when that one is gone; the note still
        // names the one the file was viewed in.
        FileComparison::SinceViewed { from, .. } => {
            since(viewed_at.unwrap_or(&from), "you viewed it")
        }
        FileComparison::SinceLastReview { from, .. } => since(&from, "your last review"),
        FileComparison::Full => "Changed since it was viewed.".to_string(),
    };
    Some(format!(
        "{since} rozed can't show only what changed since then yet, so this is the file's whole \
         diff."
    ))
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use github::GraphQlError;
    use review::revisions::{PushRecord, build_revisions};

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

        viewed.finish(&toggle, true, at(0));

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

        viewed.finish(&mark, false, at(0));
        viewed.finish(&clear, false, at(0));

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

        viewed.finish(&toggle, false, at(0));

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
        viewed.finish(&mark, true, at(0));
        assert_eq!(
            viewed.state("a"),
            Some(Unviewed),
            "the clear is still in flight"
        );
        viewed.finish(&clear, false, at(0));
        assert_eq!(viewed.state("a"), Some(Viewed));
    }

    #[test]
    fn marks_github_read_before_a_write_finished_dont_undo_it() {
        let mut viewed = github(&[("a", Unviewed), ("b", Unviewed)]);
        let toggle = viewed.toggle("a");
        viewed.finish(&toggle, true, at(10));

        // Read before the write finished, as a tab switch hands back: a's mark there is stale.
        let stale = viewed.without_stale(marks(&[("a", Unviewed), ("b", Viewed)]), at(9));
        assert_eq!(stale, marks(&[("a", Viewed), ("b", Viewed)]));
        assert!(viewed.set_github(stale));
        assert_eq!(viewed.state("a"), Some(Viewed));

        // Read after it, GitHub's answer stands.
        let fresh = viewed.without_stale(marks(&[("a", Dismissed), ("b", Viewed)]), at(11));
        assert_eq!(fresh, marks(&[("a", Dismissed), ("b", Viewed)]));
    }

    #[test]
    fn a_failed_write_doesnt_make_githubs_marks_stale() {
        let mut viewed = github(&[("a", Unviewed)]);
        let toggle = viewed.toggle("a");
        viewed.finish(&toggle, false, at(10));

        assert_eq!(
            viewed.without_stale(marks(&[("a", Dismissed)]), at(9)),
            marks(&[("a", Dismissed)])
        );
    }

    #[test]
    fn the_same_marks_from_github_arent_a_change() {
        let mut viewed = github(&[("a", Viewed)]);
        assert!(!viewed.set_github(marks(&[("a", Viewed)])));
        assert!(viewed.set_github(marks(&[("a", Unviewed)])));
    }

    fn listed(paths: &[&str]) -> Option<HashSet<String>> {
        Some(paths.iter().map(|path| path.to_string()).collect())
    }

    #[test]
    fn a_file_is_marked_by_the_path_github_lists_it_at() {
        let mut viewed = github(&[("a", Unviewed), ("old", Viewed)]);
        viewed.set_listed(listed(&["a", "old", "unknown-state"]));

        assert_eq!(viewed.github_path("a", None).as_deref(), Some("a"));
        // GitHub may list a state rozed doesn't know.
        assert_eq!(
            viewed.github_path("unknown-state", None).as_deref(),
            Some("unknown-state")
        );
        // A rename GitHub lists by its old path.
        assert_eq!(
            viewed.github_path("new", Some("old")).as_deref(),
            Some("old")
        );
        // GitHub listed every file, and not this one.
        assert_eq!(viewed.github_path("b", None), None);
        assert_eq!(viewed.github_path("b", Some("c")), None);
    }

    #[test]
    fn while_githubs_list_is_partial_any_file_may_be_on_it() {
        let mut viewed = github(&[("a", Unviewed), ("old", Viewed)]);
        viewed.set_listed(None);

        assert_eq!(viewed.github_path("b", None).as_deref(), Some("b"));
        assert_eq!(
            viewed.github_path("new", Some("old")).as_deref(),
            Some("old")
        );
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
                GithubError::GraphQl {
                    errors: vec![GraphQlError {
                        message: "Filepath must be part of pull request".into(),
                        r#type: Some("UNPROCESSABLE".into()),
                        path: vec![],
                        extensions: None,
                    }],
                },
                "Couldn't mark src/main.rs viewed: GitHub doesn't list src/main.rs in this pull \
                 request (Filepath must be part of pull request). It shows GitHub's mark again.",
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

    fn at(minute: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000 + minute * 60)
    }

    /// r1 to r3, heads `a1` to `a3`.
    fn revisions() -> Revisions {
        let pushed = |before: Option<&str>, after: &str, minute| PushRecord::Activity {
            before: before.map(str::to_string),
            after: after.to_string(),
            at: at(minute),
            forced: false,
        };
        build_revisions(
            &[
                pushed(None, "a1", 10),
                pushed(Some("a1"), "a2", 20),
                pushed(Some("a2"), "a3", 30),
            ],
            &["a1", "a2", "a3"].map(String::from),
            "a3",
            SystemTime::UNIX_EPOCH,
        )
    }

    #[test]
    fn a_file_that_changed_since_it_was_viewed_says_since_when() {
        assert_eq!(
            changed_since_viewed(&revisions(), Some(Dismissed), Some("a1")).as_deref(),
            Some(
                "Changed since you viewed it in r1. rozed can't show only what changed since \
                 then yet, so this is the file's whole diff."
            )
        );
        // Without a usable record: viewed elsewhere, or recorded at the latest revision.
        for viewed_at in [None, Some("a3"), Some("ffff")] {
            assert_eq!(
                changed_since_viewed(&revisions(), Some(Dismissed), viewed_at).as_deref(),
                Some(
                    "Changed since it was viewed. rozed can't show only what changed since then \
                     yet, so this is the file's whole diff."
                ),
                "{viewed_at:?}"
            );
        }
    }

    #[test]
    fn the_note_names_the_revision_the_file_was_viewed_in_even_when_its_gone() {
        // r2 was force-pushed away, so the comparison would start from r1.
        let mut revisions = revisions();
        assert!(revisions.mark_unavailable("a2"));
        assert_eq!(
            changed_since_viewed(&revisions, Some(Dismissed), Some("a2")).as_deref(),
            Some(
                "Changed since you viewed it in r2. rozed can't show only what changed since \
                 then yet, so this is the file's whole diff."
            )
        );
    }

    #[test]
    fn other_files_say_nothing() {
        for state in [None, Some(Viewed), Some(Unviewed)] {
            assert_eq!(changed_since_viewed(&revisions(), state, Some("a1")), None);
        }
    }
}
