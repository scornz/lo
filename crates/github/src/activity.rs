//! A branch's pushes, from GitHub's repository activity API
//! (`GET /repos/{owner}/{repo}/activity?ref=refs/heads/{branch}`). It lists every push to a
//! branch, normal and forced, with its exact time and the heads before and after, from about
//! March 2023 on, and keeps listing them after the branch is deleted
//! (`docs/research/revisions.md` §1).
//!
//! The API pages with cursors in its `Link` header, which a `304` leaves out, so each page is kept
//! with its URL, its ETag and the next page's URL.

use chrono::{DateTime, Utc};
use http_client::Method;
use http_client::http::StatusCode;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::client::{GithubClient, RestResponse};
use crate::error::GithubError;
use crate::pulls::RepoRef;
use crate::queries::PAGE_SIZE;

/// The order the API lists pushes in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    /// Oldest first: later pushes only add to the last page, so earlier pages keep their ETags.
    Asc,
    /// Newest first: the pushes after a time come first.
    Desc,
}

impl Direction {
    fn as_str(self) -> &'static str {
        match self {
            Self::Asc => "asc",
            Self::Desc => "desc",
        }
    }
}

/// One push to a branch, as the activity API lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ActivityEntry {
    pub id: i64,
    /// The branch's head before the push: GitHub's all-zero SHA when the push created it.
    pub before: String,
    /// The branch's head after the push: the all-zero SHA when the push deleted it.
    pub after: String,
    #[schemars(with = "String")]
    pub timestamp: DateTime<Utc>,
    /// `push`, `force_push`, `branch_creation`, `branch_deletion`, `pr_merge` or
    /// `merge_queue_merge`.
    pub activity_type: String,
    /// Who pushed, unless GitHub no longer has the account.
    #[serde(default)]
    pub actor: Option<ActivityActor>,
}

/// The account behind a push.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ActivityActor {
    pub login: String,
}

impl ActivityEntry {
    pub fn is_forced(&self) -> bool {
        self.activity_type == "force_push"
    }

    pub fn is_deletion(&self) -> bool {
        self.activity_type == "branch_deletion"
    }
}

/// One page of a branch's pushes, with what's needed to ask about it again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ActivityPage {
    /// The URL it was read from.
    pub url: String,
    pub etag: Option<String>,
    /// The next page's URL, from the `Link` header.
    pub next: Option<String>,
    pub entries: Vec<ActivityEntry>,
}

/// A branch's pushes, as far as rozed read them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BranchActivity {
    pub branch: String,
    pub direction: Direction,
    /// The pages read, in the API's order.
    pub pages: Vec<ActivityPage>,
    /// Whether reading stopped at the most pages it reads, with more left.
    pub truncated: bool,
    /// Why GitHub showed none: the repository is gone, as a deleted fork is, or the account can't
    /// read it.
    pub unavailable: Option<String>,
}

impl BranchActivity {
    /// The branch's pushes read, oldest first.
    pub fn entries(&self) -> Vec<&ActivityEntry> {
        let mut entries: Vec<&ActivityEntry> =
            self.pages.iter().flat_map(|page| &page.entries).collect();
        if self.direction == Direction::Desc {
            entries.reverse();
        }
        entries
    }

    /// No pushes, before any were read.
    pub(crate) fn none(branch: &str, direction: Direction) -> Self {
        Self {
            branch: branch.to_string(),
            direction,
            pages: Vec::new(),
            truncated: false,
            unavailable: None,
        }
    }

    /// No pushes, for `reason`.
    pub(crate) fn unavailable(branch: &str, direction: Direction, reason: String) -> Self {
        Self {
            unavailable: Some(reason),
            ..Self::none(branch, direction)
        }
    }
}

/// The REST path of the first page of `branch`'s pushes in `repo`.
pub fn activity_path(repo: &RepoRef, branch: &str, direction: Direction) -> String {
    let query = serde_urlencoded::to_string([
        ("ref", format!("refs/heads/{branch}").as_str()),
        ("direction", direction.as_str()),
        ("per_page", &PAGE_SIZE.to_string()),
    ])
    .expect("strings encode");
    format!("/repos/{}/{}/activity?{query}", repo.owner, repo.name)
}

/// The URL a `Link` header gives for `rel="next"`.
fn next_link(link: &str) -> Option<String> {
    link.split(',').find_map(|part| {
        let (url, params) = part.split_once(';')?;
        let url = url.trim().strip_prefix('<')?.strip_suffix('>')?;
        params
            .split(';')
            .any(|param| param.trim() == r#"rel="next""#)
            .then(|| url.to_string())
    })
}

/// Why a branch has no pushes to show, when GitHub's error says the repository is gone or can't
/// be read, rather than that something went wrong.
fn unavailable_reason(error: &GithubError) -> Option<String> {
    match error {
        GithubError::NotFound { message, .. }
        | GithubError::SsoRequired { message, .. }
        | GithubError::OAuthAppRestricted { message, .. }
        | GithubError::Api {
            status: 403 | 409 | 410 | 451,
            message,
            ..
        } => Some(message.clone()),
        _ => None,
    }
}

impl GithubClient {
    /// `branch`'s pushes in `repo`, a page at a time in `direction`, up to `max_pages` pages.
    /// Reading stops after the first page holding a push `enough` accepts.
    ///
    /// Each page `known` has at the same URL is asked about with its ETag, and a `304` reuses it.
    /// A `304` leaves out the `Link` header, so a full page `known` has no next page for is asked
    /// for in full, since one may have come after it. Read newest first, an unchanged first page
    /// means nothing was pushed since, and `known` is returned as it is.
    ///
    /// A repository that's gone, or that the account can't read, isn't an error: it gives no
    /// pushes, and says why in `unavailable`.
    pub async fn branch_activity(
        &self,
        repo: &RepoRef,
        branch: &str,
        direction: Direction,
        known: Option<&BranchActivity>,
        max_pages: usize,
        enough: impl Fn(&ActivityEntry) -> bool,
    ) -> Result<BranchActivity, GithubError> {
        let known = known.filter(|known| {
            known.branch == branch && known.direction == direction && known.unavailable.is_none()
        });
        let mut activity = BranchActivity::none(branch, direction);
        let mut url = Some(activity_path(repo, branch, direction));
        while let Some(current) = url.take() {
            if activity.pages.len() >= max_pages {
                activity.truncated = true;
                break;
            }
            let index = activity.pages.len();
            let known_pages = known.map_or(&[][..], |known| &known.pages[..]);
            let known_page = known_pages.get(index).filter(|page| page.url == current);
            let refetch_in_full = known_page.is_some_and(|page| {
                page.next.is_none() && page.entries.len() >= PAGE_SIZE as usize
            });
            let etag = known_page
                .filter(|_| !refetch_in_full)
                .and_then(|page| page.etag.as_deref());
            let response = match self.send(Method::GET, &current, None, etag).await {
                Ok(response) => response,
                Err(error) => match unavailable_reason(&error) {
                    Some(reason) => {
                        log::info!("no pushes to show for {repo} {branch}: {reason}");
                        return Ok(BranchActivity::unavailable(branch, direction, reason));
                    }
                    None => return Err(error),
                },
            };
            let page = match page_from(response, current, known_page)? {
                Page::Unchanged(_) if index == 0 && direction == Direction::Desc => {
                    return Ok(known.expect("an unchanged page is a known one").clone());
                }
                Page::Unchanged(page) | Page::Changed(page) => page,
            };
            url = page.next.clone();
            let done = page.entries.iter().any(&enough);
            activity.pages.push(page);
            if done {
                break;
            }
        }
        Ok(activity)
    }
}

enum Page {
    Unchanged(ActivityPage),
    Changed(ActivityPage),
}

/// The page GitHub's answer holds: the known one for a `304`, which has to be there.
fn page_from(
    response: RestResponse,
    url: String,
    known: Option<&ActivityPage>,
) -> Result<Page, GithubError> {
    if response.status == StatusCode::NOT_MODIFIED {
        return known.cloned().map(Page::Unchanged).ok_or_else(|| {
            GithubError::Inconsistent(format!(
                "GitHub said {url} hadn't changed, but rozed never read it"
            ))
        });
    }
    Ok(Page::Changed(ActivityPage {
        etag: response.etag().map(str::to_string),
        next: response.header("link").and_then(next_link),
        entries: response.json()?,
        url,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_next_page_from_a_link_header() {
        let link = concat!(
            "<https://api.github.com/repositories/1/activity?ref=refs%2Fheads%2Fmain&after=Y3V>; ",
            r#"rel="next", <https://api.github.com/repositories/1/activity?before=X>; rel="prev""#
        );
        assert_eq!(
            next_link(link).as_deref(),
            Some(
                "https://api.github.com/repositories/1/activity?ref=refs%2Fheads%2Fmain&after=Y3V"
            )
        );
        assert_eq!(
            next_link(r#"<https://api.github.com/x?before=X>; rel="prev""#),
            None
        );
        assert_eq!(next_link(""), None);
    }

    #[test]
    fn asks_for_a_branch_by_its_full_ref() {
        assert_eq!(
            activity_path(
                &RepoRef::new("voidsipp", "fd"),
                "fix/1822-explicit-jemalloc",
                Direction::Asc
            ),
            "/repos/voidsipp/fd/activity?ref=refs%2Fheads%2Ffix%2F1822-explicit-jemalloc\
             &direction=asc&per_page=100"
        );
    }
}
