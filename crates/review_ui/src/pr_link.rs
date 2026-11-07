//! Pull request links, such as `https://github.com/sharkdp/fd/pull/2212`.

use std::fmt;
use std::str::FromStr;

/// A pull request on github.com, named by a link to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrLink {
    pub owner: String,
    pub repo: String,
    pub number: u64,
}

/// Why a string isn't a pull request link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrLinkError {
    /// Not a link to github.com.
    NotGitHub,
    /// A github.com link, but not to a pull request.
    NotAPullRequest,
    /// The owner isn't a valid GitHub user or organization name.
    InvalidOwner,
    /// The repository isn't a valid GitHub repository name.
    InvalidRepo,
    /// The pull request number isn't a positive whole number.
    InvalidNumber,
}

impl fmt::Display for PrLinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotGitHub => "it isn't a link to github.com",
            Self::NotAPullRequest => "it isn't a link to a pull request",
            Self::InvalidOwner => "its owner isn't a valid GitHub user or organization name",
            Self::InvalidRepo => "its repository isn't a valid GitHub repository name",
            Self::InvalidNumber => "its pull request number isn't a positive whole number",
        })
    }
}

impl std::error::Error for PrLinkError {}

/// The pull request tabs a link can point at. They all name the same pull request.
const TABS: [&str; 3] = ["files", "commits", "checks"];

impl FromStr for PrLink {
    type Err = PrLinkError;

    /// Parses `https://github.com/<owner>/<repo>/pull/<number>`, with or without the scheme, with
    /// a trailing slash, or pointing at the files, commits or checks tab. A query or fragment is
    /// ignored.
    fn from_str(link: &str) -> Result<Self, Self::Err> {
        let link = link.trim();
        let link = link.split(['?', '#']).next().unwrap_or_default();
        let rest = match link.split_once("://") {
            Some((scheme, rest))
                if scheme.eq_ignore_ascii_case("https") || scheme.eq_ignore_ascii_case("http") =>
            {
                rest
            }
            Some(_) => return Err(PrLinkError::NotGitHub),
            None => link,
        };
        let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
        if !(host.eq_ignore_ascii_case("github.com") || host.eq_ignore_ascii_case("www.github.com"))
        {
            return Err(PrLinkError::NotGitHub);
        }

        let segments: Vec<&str> = path.split('/').collect();
        let [owner, repo, "pull", number, tail @ ..] = segments.as_slice() else {
            return Err(PrLinkError::NotAPullRequest);
        };
        let tail_is_valid = match tail {
            [] | [""] => true,
            [tab] | [tab, ""] => TABS.contains(tab),
            _ => false,
        };
        if number.is_empty() || !tail_is_valid {
            return Err(PrLinkError::NotAPullRequest);
        }

        if !is_valid_owner(owner) {
            return Err(PrLinkError::InvalidOwner);
        }
        if !is_valid_repo(repo) {
            return Err(PrLinkError::InvalidRepo);
        }
        let number = number
            .bytes()
            .all(|byte| byte.is_ascii_digit())
            .then(|| number.parse::<u64>().ok())
            .flatten()
            .filter(|number| (1..=i64::MAX as u64).contains(number))
            .ok_or(PrLinkError::InvalidNumber)?;

        Ok(Self {
            owner: (*owner).to_string(),
            repo: (*repo).to_string(),
            number,
        })
    }
}

/// GitHub user and organization names: up to 39 ASCII letters, digits and hyphens, not starting
/// with a hyphen.
fn is_valid_owner(owner: &str) -> bool {
    (1..=39).contains(&owner.len())
        && !owner.starts_with('-')
        && owner
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

/// GitHub repository names: up to 100 ASCII letters, digits, `.`, `-` and `_`, other than `.` and
/// `..`.
fn is_valid_repo(repo: &str) -> bool {
    (1..=100).contains(&repo.len())
        && repo != "."
        && repo != ".."
        && repo
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

impl fmt::Display for PrLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}#{}", self.owner, self.repo, self.number)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fd_2212() -> PrLink {
        PrLink {
            owner: "sharkdp".into(),
            repo: "fd".into(),
            number: 2212,
        }
    }

    #[track_caller]
    fn parses(link: &str, expected: PrLink) {
        assert_eq!(link.parse::<PrLink>(), Ok(expected), "{link}");
    }

    #[track_caller]
    fn rejects(link: &str, expected: PrLinkError) {
        assert_eq!(link.parse::<PrLink>(), Err(expected), "{link}");
    }

    #[test]
    fn parses_a_pull_request_link() {
        parses("https://github.com/sharkdp/fd/pull/2212", fd_2212());
    }

    #[test]
    fn parses_without_a_scheme_or_over_http() {
        parses("github.com/sharkdp/fd/pull/2212", fd_2212());
        parses("http://github.com/sharkdp/fd/pull/2212", fd_2212());
        parses("HTTPS://GitHub.com/sharkdp/fd/pull/2212", fd_2212());
        parses("https://www.github.com/sharkdp/fd/pull/2212", fd_2212());
    }

    #[test]
    fn parses_with_a_trailing_slash() {
        parses("https://github.com/sharkdp/fd/pull/2212/", fd_2212());
    }

    #[test]
    fn parses_links_to_a_pull_requests_tabs() {
        for tab in ["files", "commits", "checks"] {
            parses(
                &format!("https://github.com/sharkdp/fd/pull/2212/{tab}"),
                fd_2212(),
            );
            parses(
                &format!("https://github.com/sharkdp/fd/pull/2212/{tab}/"),
                fd_2212(),
            );
        }
    }

    #[test]
    fn ignores_a_query_and_a_fragment() {
        parses(
            "https://github.com/sharkdp/fd/pull/2212#issuecomment-1",
            fd_2212(),
        );
        parses(
            "https://github.com/sharkdp/fd/pull/2212/files?w=1",
            fd_2212(),
        );
        parses(
            "https://github.com/sharkdp/fd/pull/2212/files?w=1#diff-abc",
            fd_2212(),
        );
    }

    #[test]
    fn ignores_surrounding_whitespace() {
        parses("  https://github.com/sharkdp/fd/pull/2212\n", fd_2212());
    }

    #[test]
    fn keeps_owner_and_repo_as_written() {
        parses(
            "https://github.com/rust-lang/rust.vim/pull/1",
            PrLink {
                owner: "rust-lang".into(),
                repo: "rust.vim".into(),
                number: 1,
            },
        );
        parses(
            "https://github.com/Some-Org/My_Repo-2/pull/77",
            PrLink {
                owner: "Some-Org".into(),
                repo: "My_Repo-2".into(),
                number: 77,
            },
        );
    }

    #[test]
    fn rejects_other_hosts() {
        for link in [
            "https://gitlab.com/sharkdp/fd/pull/2212",
            "https://github.com.evil.example/sharkdp/fd/pull/2212",
            "https://api.github.com/repos/sharkdp/fd/pulls/2212",
            "https://gist.github.com/sharkdp/fd/pull/2212",
            "https://github.com:8443/sharkdp/fd/pull/2212",
            "https://user@github.com/sharkdp/fd/pull/2212",
            "ftp://github.com/sharkdp/fd/pull/2212",
            "git@github.com:sharkdp/fd.git",
            "sharkdp/fd#2212",
            "",
        ] {
            rejects(link, PrLinkError::NotGitHub);
        }
    }

    #[test]
    fn rejects_links_to_other_github_pages() {
        for link in [
            "https://github.com/sharkdp/fd/issues/2212",
            "https://github.com/sharkdp/fd/pulls",
            "https://github.com/sharkdp/fd/pulls/2212",
            "https://github.com/sharkdp/fd/pull",
            "https://github.com/sharkdp/fd/pull/",
            "https://github.com/sharkdp/fd",
            "https://github.com/sharkdp",
            "https://github.com/",
            "https://github.com",
            "https://github.com/sharkdp/fd/pull/2212/conflicts",
            "https://github.com/sharkdp/fd/pull/2212/files/extra",
            "https://github.com/sharkdp/fd/pull/2212//",
            "https://github.com/sharkdp//fd/pull/2212",
        ] {
            rejects(link, PrLinkError::NotAPullRequest);
        }
    }

    #[test]
    fn rejects_numbers_that_arent_positive_whole_numbers() {
        for number in [
            "abc",
            "12a",
            "-1",
            "+1",
            "0",
            "1.5",
            " 1",
            "99999999999999999999999",
            // Above what GitHub's API, and Lo's database, can hold.
            "9223372036854775808",
        ] {
            rejects(
                &format!("https://github.com/sharkdp/fd/pull/{number}"),
                PrLinkError::InvalidNumber,
            );
        }
    }

    #[test]
    fn rejects_invalid_owners() {
        for owner in ["-sharkdp", "shark_dp", "shark.dp", "shärk", &"a".repeat(40)] {
            rejects(
                &format!("https://github.com/{owner}/fd/pull/1"),
                PrLinkError::InvalidOwner,
            );
        }
    }

    #[test]
    fn rejects_invalid_repos() {
        // Owner and repo name a folder in Lo's cache, so `.`, `..` and anything outside
        // GitHub's characters must never get through.
        for repo in [
            ".",
            "..",
            "f d",
            "fd%2F..",
            "fd\\..",
            "fd:x",
            &"a".repeat(101),
        ] {
            rejects(
                &format!("https://github.com/sharkdp/{repo}/pull/1"),
                PrLinkError::InvalidRepo,
            );
        }
    }

    #[test]
    fn displays_as_owner_repo_number() {
        assert_eq!(fd_2212().to_string(), "sharkdp/fd#2212");
    }

    #[test]
    fn errors_say_why() {
        assert_eq!(
            PrLinkError::NotAPullRequest.to_string(),
            "it isn't a link to a pull request"
        );
    }
}
