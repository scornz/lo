//! What a pull request's diff compares when it opens (design §6, "Default").

use super::Revisions;

/// What the pull request's diff compares. Revisions are named by head SHA.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Comparison {
    /// The whole pull request: from its base to the latest revision.
    Full {
        /// The latest revision's head when a marker is on it, for "Nothing new since rN" above
        /// the diff.
        nothing_new_since: Option<String>,
    },
    /// What changed from one revision to another.
    Between { from: String, to: String },
}

/// What to compare when the pull request opens, from two markers: the commit of your last review,
/// and the head of the newest revision on screen when you last left the pull request.
///
/// - The more recent marker's revision is compared with the latest.
/// - With neither marker, it's the full diff. A marker no revision has ([`Revisions::find`]) counts
///   as none.
/// - With the more recent marker on the latest revision, nothing is new: it's the full diff, with
///   "Nothing new since rN".
/// - When the marker's revision is no longer available, the comparison starts from the nearest
///   earlier revision that is. That shows a little more than what's new rather than hiding any of
///   it. With none, it's the full diff.
pub fn default_comparison(
    revisions: &Revisions,
    last_review: Option<&str>,
    last_seen: Option<&str>,
) -> Comparison {
    let full = Comparison::Full {
        nothing_new_since: None,
    };
    let Some(latest) = revisions.latest() else {
        return full;
    };
    let marker = [last_review, last_seen]
        .into_iter()
        .flatten()
        .filter_map(|commit| revisions.position(commit))
        .max();
    let Some(marker) = marker else {
        return full;
    };
    if marker == revisions.len() - 1 {
        return Comparison::Full {
            nothing_new_since: Some(latest.head.clone()),
        };
    }
    match revisions.available_from(marker) {
        Some(from) => Comparison::Between {
            from: from.head.clone(),
            to: latest.head.clone(),
        },
        None => full,
    }
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

    /// r1 to r4, heads `a1` to `a4`, over the current commits `a1 c2 a2 a3 a4`.
    fn revisions() -> Revisions {
        let commits = ["a1", "c2", "a2", "a3", "a4"].map(String::from);
        build_revisions(
            &[
                pushed(None, "a1", 10),
                pushed(Some("a1"), "a2", 20),
                pushed(Some("a2"), "a3", 30),
                pushed(Some("a3"), "a4", 40),
            ],
            &commits,
            "a4",
            SystemTime::UNIX_EPOCH,
        )
    }

    fn full() -> Comparison {
        Comparison::Full {
            nothing_new_since: None,
        }
    }

    fn between(from: &str, to: &str) -> Comparison {
        Comparison::Between {
            from: from.to_string(),
            to: to.to_string(),
        }
    }

    #[test]
    fn with_neither_marker_it_shows_the_full_diff() {
        assert_eq!(default_comparison(&revisions(), None, None), full());
    }

    #[test]
    fn with_no_revisions_it_shows_the_full_diff() {
        let comparison = default_comparison(&Revisions::default(), Some("a1"), Some("a2"));

        assert_eq!(comparison, full());
    }

    #[test]
    fn it_compares_your_last_review_with_the_latest() {
        assert_eq!(
            default_comparison(&revisions(), Some("a2"), None),
            between("a2", "a4")
        );
    }

    #[test]
    fn it_compares_the_last_revision_seen_here_with_the_latest() {
        assert_eq!(
            default_comparison(&revisions(), None, Some("a3")),
            between("a3", "a4")
        );
    }

    #[test]
    fn your_last_review_wins_when_it_is_more_recent() {
        assert_eq!(
            default_comparison(&revisions(), Some("a3"), Some("a2")),
            between("a3", "a4")
        );
    }

    #[test]
    fn the_last_revision_seen_here_wins_when_it_is_more_recent() {
        assert_eq!(
            default_comparison(&revisions(), Some("a1"), Some("a3")),
            between("a3", "a4")
        );
    }

    #[test]
    fn a_marker_on_the_latest_shows_the_full_diff_with_nothing_new() {
        assert_eq!(
            default_comparison(&revisions(), Some("a2"), Some("a4")),
            Comparison::Full {
                nothing_new_since: Some("a4".to_string()),
            }
        );
    }

    #[test]
    fn an_unknown_marker_is_ignored() {
        let revisions = revisions();

        assert_eq!(default_comparison(&revisions, Some("ffff"), None), full());
        assert_eq!(
            default_comparison(&revisions, Some("ffff"), Some("a2")),
            between("a2", "a4")
        );
    }

    #[test]
    fn your_last_review_on_a_commit_between_pushes_compares_from_that_commit() {
        // a2's push brought in c2 too; the API made a review on c2.
        let commits = ["a1", "c2", "a2", "a3", "a4"].map(String::from);
        let revisions = build_revisions(
            &[
                pushed(None, "a1", 10),
                pushed(Some("a1"), "a2", 20),
                pushed(Some("a2"), "a3", 30),
                pushed(Some("a3"), "a4", 40),
                PushRecord::Review {
                    commit: "c2".to_string(),
                    at: SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000 + 35 * 60),
                },
            ],
            &commits,
            "a4",
            SystemTime::UNIX_EPOCH,
        );

        assert_eq!(
            default_comparison(&revisions, Some("c2"), None),
            between("c2", "a4")
        );
    }

    #[test]
    fn a_review_whose_record_was_not_passed_counts_as_no_marker() {
        // c2 is a current commit, but no revision's head.
        let revisions = revisions();

        assert_eq!(default_comparison(&revisions, Some("c2"), None), full());
        assert_eq!(
            default_comparison(&revisions, Some("c2"), Some("a2")),
            between("a2", "a4")
        );
    }

    #[test]
    fn a_marker_on_an_unavailable_revision_compares_from_the_nearest_earlier_one() {
        let mut revisions = revisions();
        revisions.mark_unavailable("a3");

        assert_eq!(
            default_comparison(&revisions, Some("a3"), None),
            between("a2", "a4")
        );
    }

    #[test]
    fn a_marker_with_no_earlier_available_revision_shows_the_full_diff() {
        let mut revisions = revisions();
        revisions.mark_unavailable("a1");
        revisions.mark_unavailable("a2");

        assert_eq!(default_comparison(&revisions, Some("a2"), None), full());
    }
}
