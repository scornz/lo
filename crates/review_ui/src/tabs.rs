//! Telling which open tab a link points at, so opening a pull request that's already open switches
//! to its tab (design §10 "Window").

use crate::pr_link::PrLink;

/// The pull request a tab shows: the link it was opened from, and once GitHub answered, the
/// repository's current owner and name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabKey {
    pub opened_as: PrLink,
    /// `owner/name` as GitHub names the repository now, which differs from the link's after a
    /// rename or transfer.
    pub canonical: Option<(String, String)>,
    /// The other names it was opened by, from tabs that turned out to show the same pull request
    /// and were merged into this one.
    pub aliases: Vec<(String, String)>,
}

impl TabKey {
    pub fn new(opened_as: PrLink) -> Self {
        Self {
            opened_as,
            canonical: None,
            aliases: Vec::new(),
        }
    }

    /// Whether `link` points at this tab's pull request, by any name it's known by. GitHub's
    /// owner and repository names ignore case.
    pub fn shows(&self, link: &PrLink) -> bool {
        let number = self.opened_as.number;
        number == link.number
            && self.names().any(|(owner, repo)| {
                owner.eq_ignore_ascii_case(&link.owner) && repo.eq_ignore_ascii_case(&link.repo)
            })
    }

    /// Whether `other` shows the same pull request as this tab, as GitHub named both.
    pub fn is_same_pull_request(&self, other: &TabKey) -> bool {
        match (&self.canonical, &other.canonical) {
            (Some((owner, repo)), Some((other_owner, other_repo))) => {
                self.opened_as.number == other.opened_as.number
                    && owner.eq_ignore_ascii_case(other_owner)
                    && repo.eq_ignore_ascii_case(other_repo)
            }
            _ => false,
        }
    }

    /// Takes the names `other`, a tab for the same pull request, was known by.
    pub fn merge(&mut self, other: TabKey) {
        let names: Vec<(String, String)> =
            other.names().map(|(o, r)| (o.into(), r.into())).collect();
        for (owner, repo) in names {
            let known = self.names().any(|(known_owner, known_repo)| {
                known_owner.eq_ignore_ascii_case(&owner) && known_repo.eq_ignore_ascii_case(&repo)
            });
            if !known {
                self.aliases.push((owner, repo));
            }
        }
    }

    /// Every `owner/name` the pull request is known by.
    fn names(&self) -> impl Iterator<Item = (&str, &str)> {
        std::iter::once((self.opened_as.owner.as_str(), self.opened_as.repo.as_str())).chain(
            self.canonical
                .iter()
                .chain(&self.aliases)
                .map(|(owner, repo)| (owner.as_str(), repo.as_str())),
        )
    }
}

/// The first of `tabs` that shows `link`'s pull request.
pub fn find_tab<'a>(tabs: impl IntoIterator<Item = &'a TabKey>, link: &PrLink) -> Option<usize> {
    tabs.into_iter().position(|tab| tab.shows(link))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(link: &str) -> PrLink {
        link.parse().unwrap()
    }

    #[test]
    fn a_link_to_an_open_pull_request_finds_its_tab() {
        let tabs = [
            TabKey::new(link("github.com/sharkdp/fd/pull/2212")),
            TabKey::new(link("github.com/rozed-dev/playground/pull/2")),
        ];
        assert_eq!(
            find_tab(
                &tabs,
                &link("https://github.com/rozed-dev/playground/pull/2")
            ),
            Some(1)
        );
        assert_eq!(
            find_tab(
                &tabs,
                &link("https://github.com/sharkdp/fd/pull/2212/files")
            ),
            Some(0)
        );
    }

    #[test]
    fn owner_and_repository_ignore_case() {
        let tabs = [TabKey::new(link("github.com/sharkdp/fd/pull/2212"))];
        assert_eq!(
            find_tab(&tabs, &link("github.com/SharkDP/FD/pull/2212")),
            Some(0)
        );
    }

    #[test]
    fn another_number_or_repository_is_another_pull_request() {
        let tabs = [TabKey::new(link("github.com/sharkdp/fd/pull/2212"))];
        for other in [
            "github.com/sharkdp/fd/pull/2213",
            "github.com/sharkdp/bat/pull/2212",
            "github.com/other/fd/pull/2212",
        ] {
            assert_eq!(find_tab(&tabs, &link(other)), None, "{other}");
        }
        assert_eq!(find_tab(&[], &link("github.com/sharkdp/fd/pull/1")), None);
    }

    #[test]
    fn a_tab_opened_by_a_new_name_matches_the_old_names_merged_into_it() {
        let mut tab = TabKey {
            opened_as: link("github.com/new-owner/new-name/pull/7"),
            canonical: Some(("new-owner".into(), "new-name".into())),
            aliases: Vec::new(),
        };
        assert!(!tab.shows(&link("github.com/old-owner/old-name/pull/7")));
        let duplicate = TabKey {
            opened_as: link("github.com/Old-Owner/old-name/pull/7"),
            canonical: Some(("new-owner".into(), "New-Name".into())),
            aliases: Vec::new(),
        };
        assert!(tab.is_same_pull_request(&duplicate));
        tab.merge(duplicate);
        assert!(tab.shows(&link("github.com/old-owner/old-name/pull/7")));
        assert!(!tab.shows(&link("github.com/old-owner/old-name/pull/8")));
    }

    #[test]
    fn tabs_are_the_same_pull_request_only_once_github_named_both() {
        let named = |link: &str, owner: &str, repo: &str| TabKey {
            opened_as: self::link(link),
            canonical: Some((owner.into(), repo.into())),
            aliases: Vec::new(),
        };
        let a = named("github.com/a/b/pull/1", "a", "b");
        assert!(a.is_same_pull_request(&named("github.com/x/y/pull/1", "A", "B")));
        assert!(!a.is_same_pull_request(&named("github.com/a/b/pull/2", "a", "b")));
        assert!(!a.is_same_pull_request(&named("github.com/a/c/pull/1", "a", "c")));
        assert!(!a.is_same_pull_request(&TabKey::new(link("github.com/a/b/pull/1"))));
    }

    #[test]
    fn a_renamed_repository_matches_by_its_old_and_new_name() {
        let tab = TabKey {
            opened_as: link("github.com/old-owner/old-name/pull/7"),
            canonical: Some(("new-owner".into(), "new-name".into())),
            aliases: Vec::new(),
        };
        assert!(tab.shows(&link("github.com/old-owner/old-name/pull/7")));
        assert!(tab.shows(&link("github.com/New-Owner/new-name/pull/7")));
        assert!(!tab.shows(&link("github.com/new-owner/new-name/pull/8")));
    }
}
