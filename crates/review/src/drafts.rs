//! Text typed into comment boxes and not yet posted (design §11, "Unsent text").
//!
//! [`Drafts`] keeps each comment box's text by account, pull request and place until it's posted.
//! The app saves it on this Mac whenever it changes and restores it at launch, so a crash or
//! restart loses nothing. Logins, owners and repositories ignore case, as GitHub's do.

use std::collections::BTreeMap;

use crate::threads::Lines;

/// A pull request, by its repository and number. [`Drafts`] ignores the case of `owner` and
/// `repo`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PullRequestId {
    pub owner: String,
    pub repo: String,
    pub number: u64,
}

/// Where a comment box is.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Place {
    /// A new thread on `lines` of the revision with head `commit`, or on the file when `lines` is
    /// `None`. Lines are GitHub's, as [`NewThread`](crate::pending::NewThread) takes them.
    Thread {
        commit: String,
        path: String,
        lines: Option<Lines>,
    },
    /// A reply to the thread with this id.
    Reply { thread: String },
    /// An edit of the comment with this id.
    Edit { comment: String },
    /// The submit dialog's summary.
    Summary,
    /// The comment box at the bottom of the timeline, for a comment on the pull request itself.
    Conversation,
}

/// What a draft is kept by: the account writing it, by login, the pull request, and the comment
/// box. [`Drafts`] ignores the case of `account`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DraftKey {
    pub account: String,
    pub pull_request: PullRequestId,
    pub place: Place,
}

impl DraftKey {
    /// The key as [`Drafts`] keeps it: login, owner and repository lowercased. GitHub's are ASCII.
    fn lowercased(&self) -> Self {
        Self {
            account: self.account.to_ascii_lowercase(),
            pull_request: self.pull_request.lowercased(),
            place: self.place.clone(),
        }
    }

    /// Whether it's `account`'s on `pull_request`, ignoring case.
    fn is_on(&self, account: &str, pull_request: &PullRequestId) -> bool {
        self.account.eq_ignore_ascii_case(account) && self.pull_request.is(pull_request)
    }
}

impl PullRequestId {
    fn lowercased(&self) -> Self {
        Self {
            owner: self.owner.to_ascii_lowercase(),
            repo: self.repo.to_ascii_lowercase(),
            number: self.number,
        }
    }

    /// Whether it's the same pull request as `other`, ignoring case.
    fn is(&self, other: &Self) -> bool {
        self.owner.eq_ignore_ascii_case(&other.owner)
            && self.repo.eq_ignore_ascii_case(&other.repo)
            && self.number == other.number
    }
}

/// Text typed into comment boxes and not yet posted, by [`DraftKey`], with its login, owner and
/// repository lowercased.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Drafts {
    drafts: BTreeMap<DraftKey, String>,
}

impl Drafts {
    pub fn new() -> Self {
        Self::default()
    }

    /// The draft at `key`.
    pub fn get(&self, key: &DraftKey) -> Option<&str> {
        self.drafts.get(&key.lowercased()).map(String::as_str)
    }

    /// Keeps `text` as the draft at `key`, as it's typed. Blank text removes the draft. Returns
    /// whether anything changed, so the app saves only then.
    pub fn set(&mut self, key: DraftKey, text: &str) -> bool {
        let key = key.lowercased();
        if text.trim().is_empty() {
            return self.drafts.remove(&key).is_some();
        }
        if self.get(&key) == Some(text) {
            return false;
        }
        self.drafts.insert(key, text.to_string());
        true
    }

    /// Removes the draft at `key` and returns it: once its comment is posted, or its box is
    /// closed without posting.
    pub fn take(&mut self, key: &DraftKey) -> Option<String> {
        self.drafts.remove(&key.lowercased())
    }

    /// The drafts `account` has on `pull_request`, by place, to reopen their comment boxes.
    pub fn on<'a>(
        &'a self,
        account: &'a str,
        pull_request: &'a PullRequestId,
    ) -> impl Iterator<Item = (&'a Place, &'a str)> + 'a {
        (self.drafts.iter())
            .filter(move |(key, _)| key.is_on(account, pull_request))
            .map(|(key, text)| (&key.place, text.as_str()))
    }

    /// Every draft, ordered by key, for the app to save. Keys come back lowercased.
    pub fn iter(&self) -> impl Iterator<Item = (&DraftKey, &str)> {
        (self.drafts.iter()).map(|(key, text)| (key, text.as_str()))
    }

    pub fn len(&self) -> usize {
        self.drafts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.drafts.is_empty()
    }
}

impl FromIterator<(DraftKey, String)> for Drafts {
    /// The drafts the app saved, skipping blank ones.
    fn from_iter<I: IntoIterator<Item = (DraftKey, String)>>(drafts: I) -> Self {
        let drafts = (drafts.into_iter())
            .filter(|(_, text)| !text.trim().is_empty())
            .map(|(key, text)| (key.lowercased(), text))
            .collect();
        Self { drafts }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::threads::Line;

    fn pull_request(number: u64) -> PullRequestId {
        PullRequestId {
            owner: "acme".to_string(),
            repo: "shop".to_string(),
            number,
        }
    }

    fn key(account: &str, number: u64, place: Place) -> DraftKey {
        DraftKey {
            account: account.to_string(),
            pull_request: pull_request(number),
            place,
        }
    }

    fn on_line(line: u32) -> Place {
        Place::Thread {
            commit: "a3".to_string(),
            path: "src/parse.rs".to_string(),
            lines: Some(Lines::one(Line::right(line))),
        }
    }

    #[test]
    fn a_draft_is_kept_until_it_is_posted() {
        let mut drafts = Drafts::new();
        let key = key("alice", 7, on_line(12));

        assert!(drafts.set(key.clone(), "Off by"));
        assert!(drafts.set(key.clone(), "Off by one?"));
        assert_eq!(drafts.get(&key), Some("Off by one?"));

        assert_eq!(drafts.take(&key), Some("Off by one?".to_string()));
        assert_eq!(drafts.get(&key), None);
        assert_eq!(drafts.take(&key), None);
    }

    #[test]
    fn setting_the_same_text_changes_nothing() {
        let mut drafts = Drafts::new();
        let key = key("alice", 7, Place::Summary);
        drafts.set(key.clone(), "Looks good.");

        assert!(!drafts.set(key, "Looks good."));
    }

    #[test]
    fn blank_text_removes_the_draft() {
        let mut drafts = Drafts::new();
        let key = key("alice", 7, Place::Conversation);
        drafts.set(key.clone(), "Thanks!");

        assert!(drafts.set(key.clone(), " \n"));
        assert_eq!(drafts.get(&key), None);
        assert!(!drafts.set(key, ""));
        assert!(drafts.is_empty());
    }

    #[test]
    fn drafts_are_kept_apart_by_account_pull_request_and_place() {
        let mut drafts = Drafts::new();
        drafts.set(key("alice", 7, on_line(12)), "Alice's");
        drafts.set(key("bob", 7, on_line(12)), "Bob's");
        drafts.set(key("alice", 8, on_line(12)), "On #8");
        drafts.set(key("alice", 7, on_line(13)), "Line 13");
        drafts.set(
            key(
                "alice",
                7,
                Place::Reply {
                    thread: "PRRT_1".to_string(),
                },
            ),
            "A reply",
        );

        assert_eq!(drafts.len(), 5);
        assert_eq!(drafts.get(&key("alice", 7, on_line(12))), Some("Alice's"));
        assert_eq!(drafts.get(&key("bob", 7, on_line(12))), Some("Bob's"));
        assert_eq!(drafts.get(&key("alice", 8, on_line(12))), Some("On #8"));
    }

    #[test]
    fn it_lists_an_accounts_drafts_on_a_pull_request() {
        let mut drafts = Drafts::new();
        drafts.set(key("alice", 7, on_line(13)), "Line 13");
        drafts.set(key("bob", 7, on_line(12)), "Bob's");
        drafts.set(key("alice", 8, on_line(12)), "On #8");
        drafts.set(key("alice", 7, Place::Summary), "Summary");

        let seven = pull_request(7);
        let listed: Vec<_> = drafts.on("alice", &seven).collect();
        assert_eq!(
            listed,
            [(&on_line(13), "Line 13"), (&Place::Summary, "Summary")]
        );
    }

    #[test]
    fn saved_drafts_restore_without_blank_ones() {
        let saved = vec![
            (key("alice", 7, Place::Summary), "Summary".to_string()),
            (key("alice", 7, Place::Conversation), "  ".to_string()),
        ];
        let drafts: Drafts = saved.into_iter().collect();

        assert_eq!(
            drafts.iter().collect::<Vec<_>>(),
            [(&key("alice", 7, Place::Summary), "Summary")]
        );
    }

    #[test]
    fn owners_repositories_and_logins_ignore_case() {
        // As GitHub's do: a pull request opened from a pasted link may spell them otherwise.
        let spelled = |account: &str, owner: &str, repo: &str| DraftKey {
            account: account.to_string(),
            pull_request: PullRequestId {
                owner: owner.to_string(),
                repo: repo.to_string(),
                number: 7,
            },
            place: Place::Summary,
        };
        let mut drafts = Drafts::new();
        drafts.set(spelled("Alice", "Acme", "Shop"), "Looks good.");

        assert_eq!(
            drafts.get(&spelled("alice", "acme", "shop")),
            Some("Looks good.")
        );
        assert!(!drafts.set(spelled("ALICE", "acme", "SHOP"), "Looks good."));
        assert!(drafts.set(spelled("alice", "ACME", "shop"), "Ship it."));
        assert_eq!(drafts.len(), 1);
        let pull_request = spelled("", "ACME", "shop").pull_request;
        assert_eq!(
            drafts.on("aLiCe", &pull_request).collect::<Vec<_>>(),
            [(&Place::Summary, "Ship it.")]
        );
        assert_eq!(
            drafts.iter().collect::<Vec<_>>(),
            [(&spelled("alice", "acme", "shop"), "Ship it.")]
        );
        assert_eq!(
            drafts.take(&spelled("Alice", "Acme", "Shop")),
            Some("Ship it.".to_string())
        );

        let saved = vec![(spelled("Alice", "Acme", "Shop"), "Saved".to_string())];
        let restored: Drafts = saved.into_iter().collect();
        assert_eq!(
            restored.get(&spelled("alice", "acme", "shop")),
            Some("Saved")
        );
    }
}
