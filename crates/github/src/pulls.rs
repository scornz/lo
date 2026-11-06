//! Which pull request or repository something is about, and GitHub's REST view of a pull request.

use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A repository on github.com. Owners and names match ignoring (ASCII) case, as on GitHub and
/// in rozed's database, and show as written.
#[derive(Clone, Debug)]
pub struct RepoRef {
    pub owner: String,
    pub name: String,
}

/// A name's bytes as compared: GitHub's owners and repository names are ASCII.
fn folded(name: &str) -> impl Iterator<Item = u8> + '_ {
    name.bytes().map(|byte| byte.to_ascii_lowercase())
}

impl PartialEq for RepoRef {
    fn eq(&self, other: &Self) -> bool {
        self.owner.eq_ignore_ascii_case(&other.owner) && self.name.eq_ignore_ascii_case(&other.name)
    }
}

impl Eq for RepoRef {}

impl Hash for RepoRef {
    fn hash<H: Hasher>(&self, state: &mut H) {
        for name in [&self.owner, &self.name] {
            folded(name).for_each(|byte| state.write_u8(byte));
            // Keeps "a/bc" and "ab/c" apart.
            state.write_u8(b'/');
        }
    }
}

impl Ord for RepoRef {
    fn cmp(&self, other: &Self) -> Ordering {
        folded(&self.owner)
            .cmp(folded(&other.owner))
            .then_with(|| folded(&self.name).cmp(folded(&other.name)))
    }
}

impl PartialOrd for RepoRef {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl RepoRef {
    pub fn new(owner: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            owner: owner.into(),
            name: name.into(),
        }
    }

    /// The REST path of a page (from 1) of the repository's open pull requests, 100 a page in
    /// GitHub's default order, newest first. Keeping the order and parameters fixed keeps each
    /// page's ETag stable.
    pub fn open_pulls_path(&self, page: usize) -> String {
        let path = format!(
            "/repos/{}/{}/pulls?state=open&per_page=100",
            self.owner, self.name
        );
        if page > 1 {
            format!("{path}&page={page}")
        } else {
            path
        }
    }
}

impl fmt::Display for RepoRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

/// A pull request on github.com.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PullRef {
    pub repo: RepoRef,
    pub number: i64,
}

impl PullRef {
    pub fn new(owner: impl Into<String>, name: impl Into<String>, number: i64) -> Self {
        Self {
            repo: RepoRef::new(owner, name),
            number,
        }
    }

    /// The pull request's REST path.
    pub fn rest_path(&self) -> String {
        format!(
            "/repos/{}/{}/pulls/{}",
            self.repo.owner, self.repo.name, self.number
        )
    }
}

impl fmt::Display for PullRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#{}", self.repo, self.number)
    }
}

/// What a poll or a cache entry is about: one pull request, or a repository as a whole.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Target {
    Pull(PullRef),
    Repo(RepoRef),
}

impl Target {
    pub fn repo(&self) -> &RepoRef {
        match self {
            Self::Pull(pull) => &pull.repo,
            Self::Repo(repo) => repo,
        }
    }

    /// The pull request's number, or 0 for the repository.
    pub fn number(&self) -> i64 {
        match self {
            Self::Pull(pull) => pull.number,
            Self::Repo(_) => 0,
        }
    }
}

impl From<PullRef> for Target {
    fn from(pull: PullRef) -> Self {
        Self::Pull(pull)
    }
}

impl From<RepoRef> for Target {
    fn from(repo: RepoRef) -> Self {
        Self::Repo(repo)
    }
}

/// The parts of GitHub's REST view of a pull request (`GET /repos/{owner}/{repo}/pulls/{number}`)
/// that a tab shows without the full details: enough to notice a push, a retitle, a merge or a
/// close.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PullSummary {
    pub number: i64,
    pub title: String,
    /// `open` or `closed`.
    pub state: String,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub merged_at: Option<String>,
    pub updated_at: String,
    pub head: PullBranch,
    pub base: PullBranch,
}

impl PullSummary {
    pub fn is_merged(&self) -> bool {
        self.merged_at.is_some()
    }

    /// When GitHub last updated the pull request, as an HTTP date for `If-Modified-Since`.
    /// GitHub's `Last-Modified` for a pull request is its `updated_at`, which a push to another
    /// branch or a star doesn't move, unlike its ETag.
    pub fn last_modified(&self) -> Option<String> {
        let time = chrono::DateTime::parse_from_rfc3339(&self.updated_at).ok()?;
        Some(
            time.with_timezone(&chrono::Utc)
                .format("%a, %d %b %Y %H:%M:%S GMT")
                .to_string(),
        )
    }
}

/// One side of a pull request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PullBranch {
    #[serde(rename = "ref")]
    pub name: String,
    pub sha: String,
}

/// A pull request as GitHub's REST list of a repository's open pull requests shows it: what the
/// open PR list ([`crate::queries::OpenPullRequestList`]) holds about it. Comparing these, not the
/// list's ETag, tells whether the list changed: the ETag also covers each pull request's embedded
/// repositories, which change with any push to the repository.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ListedPull {
    pub number: i64,
    pub title: String,
    #[serde(default)]
    pub draft: bool,
    pub updated_at: String,
    pub head: PullBranch,
    pub base: PullBranch,
    /// The GitHub stack it's in.
    #[serde(default)]
    pub stack: Option<ListedStack>,
}

/// A listed pull request's place in a GitHub stack.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ListedStack {
    pub number: i64,
    pub position: i64,
    pub size: i64,
    pub base: StackBase,
}

/// The branch a GitHub stack merges into.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StackBase {
    #[serde(rename = "ref")]
    pub name: String,
}

/// One page of a repository's open pull requests from GitHub's REST list, with its ETag.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ListedPage {
    pub etag: Option<String>,
    pub pulls: Vec<ListedPull>,
}

/// The pages of a repository's open pull requests rozed reads, newest first: as many as hold
/// the pull requests the open PR list does. Each page is asked about with its own ETag, so an
/// older pull request that's pushed to or closed is noticed too.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ListedPulls {
    pub pages: Vec<ListedPage>,
}

impl ListedPulls {
    pub fn pulls(&self) -> impl Iterator<Item = &ListedPull> {
        self.pages.iter().flat_map(|page| &page.pulls)
    }

    /// Whether both list the same pull requests, the same way, whatever their ETags.
    pub fn lists_the_same(&self, other: &Self) -> bool {
        self.pulls().eq(other.pulls())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::{BTreeSet, HashSet};

    #[test]
    fn owners_and_names_match_ignoring_case() {
        let written = PullRef::new("rozed-dev", "playground", 1);
        let shouted = PullRef::new("Rozed-Dev", "PLAYGROUND", 1);
        assert_eq!(written, shouted);
        assert_eq!(BTreeSet::from([written.clone(), shouted.clone()]).len(), 1);
        assert_eq!(HashSet::from([written.clone(), shouted.clone()]).len(), 1);
        // As written, for showing.
        assert_eq!(shouted.to_string(), "Rozed-Dev/PLAYGROUND#1");
        assert_ne!(written, PullRef::new("rozed-dev", "playground", 2));
        assert_ne!(
            RepoRef::new("rozed-dev", "playground"),
            RepoRef::new("rozed", "dev-playground")
        );
        assert!(RepoRef::new("A", "b") < RepoRef::new("a", "C"));
    }
}
