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
}

impl TabKey {
    pub fn new(opened_as: PrLink) -> Self {
        Self {
            opened_as,
            canonical: None,
        }
    }

    /// Whether `link` points at this tab's pull request. GitHub's owner and repository names
    /// ignore case.
    pub fn shows(&self, link: &PrLink) -> bool {
        let same = |owner: &str, repo: &str, number: u64| {
            number == link.number
                && owner.eq_ignore_ascii_case(&link.owner)
                && repo.eq_ignore_ascii_case(&link.repo)
        };
        same(
            &self.opened_as.owner,
            &self.opened_as.repo,
            self.opened_as.number,
        ) || self
            .canonical
            .as_ref()
            .is_some_and(|(owner, repo)| same(owner, repo, self.opened_as.number))
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
    fn a_renamed_repository_matches_by_its_old_and_new_name() {
        let tab = TabKey {
            opened_as: link("github.com/old-owner/old-name/pull/7"),
            canonical: Some(("new-owner".into(), "new-name".into())),
        };
        assert!(tab.shows(&link("github.com/old-owner/old-name/pull/7")));
        assert!(tab.shows(&link("github.com/New-Owner/new-name/pull/7")));
        assert!(!tab.shows(&link("github.com/new-owner/new-name/pull/8")));
    }
}
