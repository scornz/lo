//! What a file shows when GitHub says it changed since you viewed it (design §6, "Viewed marks").

use super::Revisions;

/// A file's viewed mark on GitHub, which is per viewer: `PullRequestChangedFile.viewerViewedState`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ViewedState {
    /// `VIEWED`: marked viewed, and unchanged since.
    Viewed,
    /// `UNVIEWED`: not marked viewed.
    Unviewed,
    /// `DISMISSED`: marked viewed, then a push changed it.
    Dismissed,
}

/// What to show of a file GitHub says changed since you viewed it. Revisions are named by head
/// SHA, and `to` is always the latest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileComparison {
    /// What changed since the revision this Mac recorded when you marked the file viewed.
    SinceViewed { from: String, to: String },
    /// What changed since your last review: the file was marked viewed elsewhere, or this Mac's
    /// record of it is no use.
    SinceLastReview { from: String, to: String },
    /// The file's whole diff in the pull request, with no marker to compare from.
    Full,
}

/// What to show of a file, given its viewed mark on GitHub, the head this Mac recorded when you
/// marked it viewed (`viewed_at`), and the commit of your last review. `None` for a file GitHub
/// doesn't say changed since viewed: a viewed file stays collapsed, and an unviewed one shows the
/// pull request's comparison.
///
/// - This Mac's record wins, since it's where you actually viewed the file. It's no use when no
///   revision has that head, or when it's on the latest revision, which GitHub's mark says the
///   file changed after.
/// - Without a usable record, the file was marked viewed elsewhere: compare from your last review,
///   unless that's on the latest revision too.
/// - With neither, it's the file's whole diff.
/// - A last review on a commit that was never a head compares from the revision before the one
///   containing it, as in [`default_comparison`](super::default_comparison).
/// - As in [`default_comparison`](super::default_comparison), a marker on a revision rozed can no
///   longer get compares from the nearest earlier one it can, or shows the whole diff when there's
///   none, rather than falling back to a later marker that could hide changes.
pub fn viewed_file_comparison(
    revisions: &Revisions,
    state: ViewedState,
    viewed_at: Option<&str>,
    last_review: Option<&str>,
) -> Option<FileComparison> {
    if state != ViewedState::Dismissed {
        return None;
    }
    let Some(latest) = revisions.latest() else {
        return Some(FileComparison::Full);
    };
    let to = latest.head.clone();
    let before_latest = |commit: Option<&str>| {
        revisions
            .covered(commit?)
            .filter(|&index| index + 1 < revisions.len())
    };
    let comparison = if let Some(viewed) = before_latest(viewed_at) {
        (revisions.available_from(viewed)).map(|from| FileComparison::SinceViewed {
            from: from.head.clone(),
            to,
        })
    } else if let Some(review) = before_latest(last_review) {
        (revisions.available_from(review)).map(|from| FileComparison::SinceLastReview {
            from: from.head.clone(),
            to,
        })
    } else {
        None
    };
    Some(comparison.unwrap_or(FileComparison::Full))
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::super::{PushRecord, build_revisions};
    use super::*;

    fn pushed(before: Option<&str>, after: &str, minute: u64) -> PushRecord {
        PushRecord::Activity {
            before: before.map(str::to_string),
            after: after.to_string(),
            at: SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000 + minute * 60),
            forced: false,
        }
    }

    /// r1 to r4, heads `a1` to `a4`.
    fn revisions() -> Revisions {
        let commits = ["a1", "a2", "a3", "a4"].map(String::from);
        build_revisions(
            &[
                pushed(None, "a1", 10),
                pushed(Some("a1"), "a2", 20),
                pushed(Some("a2"), "a3", 30),
                pushed(Some("a3"), "a4", 40),
            ],
            &commits,
        )
    }

    fn dismissed(
        revisions: &Revisions,
        viewed_at: Option<&str>,
        last_review: Option<&str>,
    ) -> Option<FileComparison> {
        viewed_file_comparison(revisions, ViewedState::Dismissed, viewed_at, last_review)
    }

    fn since_viewed(from: &str) -> Option<FileComparison> {
        Some(FileComparison::SinceViewed {
            from: from.to_string(),
            to: "a4".to_string(),
        })
    }

    fn since_last_review(from: &str) -> Option<FileComparison> {
        Some(FileComparison::SinceLastReview {
            from: from.to_string(),
            to: "a4".to_string(),
        })
    }

    #[test]
    fn a_viewed_file_is_skipped() {
        let comparison =
            viewed_file_comparison(&revisions(), ViewedState::Viewed, Some("a2"), Some("a1"));

        assert_eq!(comparison, None);
    }

    #[test]
    fn an_unviewed_file_is_skipped() {
        let comparison =
            viewed_file_comparison(&revisions(), ViewedState::Unviewed, Some("a2"), Some("a1"));

        assert_eq!(comparison, None);
    }

    #[test]
    fn a_file_viewed_on_this_mac_compares_from_where_it_was_viewed() {
        assert_eq!(
            dismissed(&revisions(), Some("a2"), None),
            since_viewed("a2")
        );
    }

    #[test]
    fn this_macs_record_wins_over_a_later_review() {
        assert_eq!(
            dismissed(&revisions(), Some("a1"), Some("a3")),
            since_viewed("a1")
        );
    }

    #[test]
    fn a_file_viewed_elsewhere_compares_from_your_last_review() {
        assert_eq!(
            dismissed(&revisions(), None, Some("a2")),
            since_last_review("a2")
        );
    }

    #[test]
    fn a_last_review_on_a_commit_that_was_never_a_head_compares_from_the_revision_before_it() {
        let commits = ["a1", "a2", "c3", "a3", "a4"].map(String::from);
        let revisions = build_revisions(
            &[
                pushed(None, "a1", 10),
                pushed(Some("a1"), "a2", 20),
                pushed(Some("a2"), "a3", 30),
                pushed(Some("a3"), "a4", 40),
            ],
            &commits,
        );

        assert_eq!(
            dismissed(&revisions, None, Some("c3")),
            since_last_review("a2")
        );
    }

    #[test]
    fn a_record_of_an_unknown_head_falls_back_to_your_last_review() {
        assert_eq!(
            dismissed(&revisions(), Some("ffff"), Some("a2")),
            since_last_review("a2")
        );
    }

    #[test]
    fn a_record_on_the_latest_revision_falls_back_to_your_last_review() {
        // GitHub says the file changed since it was viewed, so a record at the latest is stale.
        assert_eq!(
            dismissed(&revisions(), Some("a4"), Some("a2")),
            since_last_review("a2")
        );
    }

    #[test]
    fn with_neither_marker_it_shows_the_files_whole_diff() {
        assert_eq!(
            dismissed(&revisions(), None, None),
            Some(FileComparison::Full)
        );
    }

    #[test]
    fn a_last_review_on_the_latest_revision_shows_the_files_whole_diff() {
        assert_eq!(
            dismissed(&revisions(), None, Some("a4")),
            Some(FileComparison::Full)
        );
    }

    #[test]
    fn with_no_revisions_it_shows_the_files_whole_diff() {
        assert_eq!(
            dismissed(&Revisions::default(), Some("a2"), Some("a1")),
            Some(FileComparison::Full)
        );
    }

    #[test]
    fn a_record_on_an_unavailable_revision_compares_from_the_nearest_earlier_one() {
        let mut revisions = revisions();
        revisions.mark_unavailable("a2");

        assert_eq!(
            dismissed(&revisions, Some("a2"), Some("a3")),
            since_viewed("a1")
        );
    }

    #[test]
    fn a_record_with_no_earlier_available_revision_shows_the_files_whole_diff() {
        let mut revisions = revisions();
        revisions.mark_unavailable("a1");
        revisions.mark_unavailable("a2");

        assert_eq!(
            dismissed(&revisions, Some("a2"), Some("a3")),
            Some(FileComparison::Full)
        );
    }

    #[test]
    fn a_last_review_on_an_unavailable_revision_compares_from_the_nearest_earlier_one() {
        let mut revisions = revisions();
        revisions.mark_unavailable("a3");

        assert_eq!(
            dismissed(&revisions, None, Some("a3")),
            since_last_review("a2")
        );
    }
}
