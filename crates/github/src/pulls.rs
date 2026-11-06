//! Which pull request or repository something is about, and GitHub's REST view of a pull request.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A repository on github.com. GitHub matches owners and names case-insensitively; so does
/// rozed's database, but these compare as written, so callers should be consistent.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RepoRef {
    pub owner: String,
    pub name: String,
}

impl RepoRef {
    pub fn new(owner: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            owner: owner.into(),
            name: name.into(),
        }
    }

    /// The REST path of the repository's open pull requests, a page of 100 in GitHub's default
    /// order. Keeping the order and parameters fixed keeps its ETag stable.
    pub fn open_pulls_path(&self) -> String {
        format!(
            "/repos/{}/{}/pulls?state=open&per_page=100",
            self.owner, self.name
        )
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
}

/// One side of a pull request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullBranch {
    #[serde(rename = "ref")]
    pub name: String,
    pub sha: String,
}
