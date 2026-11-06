//! rozed's GraphQL queries, typed from GitHub's schema.
//!
//! Each query is a `.graphql` file in `crates/github/graphql/`, and `graphql_client` generates a
//! module of Rust types for it from `graphql/schema.graphql`. Generated fields GitHub deprecates
//! carry `#[deprecated]`, so after a schema refresh CI's clippy fails on any use of one.

use std::collections::HashSet;

use graphql_client::GraphQLQuery;
use serde::{Deserialize, Serialize};

use crate::client::GithubClient;
use crate::error::GithubError;

// GitHub's custom scalars, as the generated code names them.
type DateTime = chrono::DateTime<chrono::Utc>;
#[allow(clippy::upper_case_acronyms)]
type URI = String;
type GitObjectID = String;

/// How many items a page asks for: GitHub's maximum.
pub const PAGE_SIZE: i64 = 100;
/// The most open pull requests rozed reads from one repository.
pub const MAX_OPEN_PULL_REQUESTS: usize = 1000;
/// How many times a pull request's files are read again from the start when its head moves
/// while they're paged through.
const MAX_RESTARTS: usize = 2;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/schema.graphql",
    query_path = "graphql/viewer.graphql",
    response_derives = "Debug, Clone, PartialEq",
    variables_derives = "Debug, Clone"
)]
pub struct Viewer;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/schema.graphql",
    query_path = "graphql/pull_request.graphql",
    response_derives = "Debug, Clone, PartialEq, Serialize",
    variables_derives = "Debug, Clone"
)]
pub struct PullRequest;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/schema.graphql",
    query_path = "graphql/pull_request_check.graphql",
    response_derives = "Debug, Clone, PartialEq, Serialize",
    variables_derives = "Debug, Clone"
)]
pub struct PullRequestCheck;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/schema.graphql",
    query_path = "graphql/open_pull_requests.graphql",
    response_derives = "Debug, Clone, PartialEq, Serialize",
    variables_derives = "Debug, Clone"
)]
pub struct OpenPullRequests;

pub type ViewerData = viewer::ViewerViewer;
pub type PullRequestData = pull_request::PullRequestRepositoryPullRequest;
pub type PullRequestFile = pull_request::PullRequestRepositoryPullRequestFilesNodes;
pub use pull_request::{FileViewedState, PullRequestState};
pub type PullRequestCheckData = pull_request_check::PullRequestCheckRepositoryPullRequest;
pub type OpenPullRequest = open_pull_requests::OpenPullRequestsRepositoryPullRequestsNodes;

/// A repository's open pull requests.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenPullRequestList {
    pub pull_requests: Vec<OpenPullRequest>,
    /// How many are open, which is more than `pull_requests` holds when it was capped, or when
    /// GitHub left some out as null.
    pub total_count: i64,
    /// Whether reading stopped at [`MAX_OPEN_PULL_REQUESTS`] with more left.
    pub truncated: bool,
}

impl OpenPullRequestList {
    /// Whether there were more open pull requests than [`MAX_OPEN_PULL_REQUESTS`].
    pub fn is_truncated(&self) -> bool {
        self.truncated
    }
}

/// Follows a connection's pages, refusing a cursor that doesn't move the paging on.
struct Pager {
    what: &'static str,
    cursors: HashSet<String>,
    /// Pages read so far, and the most there can be.
    pages: usize,
    max_pages: usize,
}

impl Pager {
    /// For `total` items read `page_size` a page. A page more than the count needs is allowed,
    /// since the count can grow while paging.
    fn new(what: &'static str, total: i64, page_size: i64) -> Self {
        let total = usize::try_from(total).unwrap_or(0);
        let page_size = usize::try_from(page_size).unwrap_or(1).max(1);
        Self {
            what,
            cursors: HashSet::new(),
            pages: 1,
            max_pages: total.div_ceil(page_size) + 1,
        }
    }

    /// The cursor to read the next page after, or `None` when the last page has been read.
    fn next(
        &mut self,
        has_next_page: bool,
        end_cursor: Option<String>,
    ) -> Result<Option<String>, GithubError> {
        if !has_next_page {
            return Ok(None);
        }
        let what = self.what;
        let cursor = end_cursor.ok_or_else(|| {
            GithubError::Inconsistent(format!("more {what} were left, but no cursor for them"))
        })?;
        if !self.cursors.insert(cursor.clone()) {
            return Err(GithubError::Inconsistent(format!(
                "a page of {what} came back again"
            )));
        }
        self.pages += 1;
        if self.pages > self.max_pages {
            return Err(GithubError::Inconsistent(format!(
                "more pages of {what} than their count allows"
            )));
        }
        Ok(Some(cursor))
    }
}

fn not_found(what: String) -> GithubError {
    GithubError::NotFound {
        message: format!("Could not find {what}"),
        documentation_url: None,
    }
}

impl GithubClient {
    /// The signed-in user.
    pub async fn viewer(&self) -> Result<ViewerData, GithubError> {
        Ok(self
            .graphql_keeping_partial::<Viewer>(viewer::Variables, 1)
            .await?
            .viewer)
    }

    /// A pull request with every one of its files.
    pub async fn pull_request(
        &self,
        owner: &str,
        name: &str,
        number: i64,
    ) -> Result<PullRequestData, GithubError> {
        self.pull_request_paged(owner, name, number, PAGE_SIZE)
            .await
    }

    /// [`Self::pull_request`], reading `page_size` files a page. If the head moves while the
    /// files are paged through, they're read again from the start, so they all belong to one
    /// revision.
    pub async fn pull_request_paged(
        &self,
        owner: &str,
        name: &str,
        number: i64,
        page_size: i64,
    ) -> Result<PullRequestData, GithubError> {
        let fetch = |after: Option<String>| async move {
            // `repository.pullRequest` is what was asked for; errors below it only leave out
            // what they're about.
            self.graphql_keeping_partial::<PullRequest>(
                pull_request::Variables {
                    owner: owner.to_string(),
                    name: name.to_string(),
                    number,
                    files_first: page_size,
                    files_after: after,
                },
                2,
            )
            .await?
            .repository
            .and_then(|repository| repository.pull_request)
            .ok_or_else(|| not_found(format!("pull request {owner}/{name}#{number}")))
        };
        'restart: for _ in 0..=MAX_RESTARTS {
            let mut pull_request = fetch(None).await?;
            let Some(files) = pull_request.files.as_mut() else {
                return Ok(pull_request);
            };
            let mut pager = Pager::new("files", files.total_count, page_size);
            while let Some(after) = pager.next(
                files.page_info.has_next_page,
                files.page_info.end_cursor.clone(),
            )? {
                let page = fetch(Some(after)).await?;
                if page.head_ref_oid != pull_request.head_ref_oid {
                    log::info!(
                        "{owner}/{name}#{number} was pushed to while its files were read; \
                         reading them again"
                    );
                    continue 'restart;
                }
                let next = page
                    .files
                    .ok_or_else(|| GithubError::Decode("a page of files went missing".into()))?;
                files
                    .nodes
                    .get_or_insert_with(Vec::new)
                    .extend(next.nodes.unwrap_or_default());
                files.page_info = next.page_info;
            }
            return Ok(pull_request);
        }
        Err(GithubError::Inconsistent(format!(
            "{owner}/{name}#{number} kept being pushed to while its files were read"
        )))
    }

    /// The cheap check for whether a pull request changed.
    pub async fn pull_request_check(
        &self,
        owner: &str,
        name: &str,
        number: i64,
    ) -> Result<PullRequestCheckData, GithubError> {
        self.graphql_keeping_partial::<PullRequestCheck>(
            pull_request_check::Variables {
                owner: owner.to_string(),
                name: name.to_string(),
                number,
            },
            2,
        )
        .await?
        .repository
        .and_then(|repository| repository.pull_request)
        .ok_or_else(|| not_found(format!("pull request {owner}/{name}#{number}")))
    }

    /// A repository's open pull requests, newest first, up to [`MAX_OPEN_PULL_REQUESTS`].
    pub async fn open_pull_requests(
        &self,
        owner: &str,
        name: &str,
    ) -> Result<OpenPullRequestList, GithubError> {
        self.open_pull_requests_paged(owner, name, PAGE_SIZE).await
    }

    /// [`Self::open_pull_requests`], reading `page_size` a page.
    pub async fn open_pull_requests_paged(
        &self,
        owner: &str,
        name: &str,
        page_size: i64,
    ) -> Result<OpenPullRequestList, GithubError> {
        let mut list = OpenPullRequestList {
            pull_requests: Vec::new(),
            total_count: 0,
            truncated: false,
        };
        let mut pager = None;
        let mut after = None;
        loop {
            let page = self
                .graphql_keeping_partial::<OpenPullRequests>(
                    open_pull_requests::Variables {
                        owner: owner.to_string(),
                        name: name.to_string(),
                        first: page_size,
                        after: after.take(),
                    },
                    2,
                )
                .await?
                .repository
                .ok_or_else(|| not_found(format!("repository {owner}/{name}")))?
                .pull_requests;
            list.total_count = page.total_count;
            list.pull_requests
                .extend(page.nodes.unwrap_or_default().into_iter().flatten());
            let pager = pager.get_or_insert_with(|| {
                let capped = page.total_count.min(MAX_OPEN_PULL_REQUESTS as i64);
                Pager::new("open pull requests", capped, page_size)
            });
            if list.pull_requests.len() >= MAX_OPEN_PULL_REQUESTS {
                list.truncated = page.page_info.has_next_page
                    || list.pull_requests.len() > MAX_OPEN_PULL_REQUESTS;
                list.pull_requests.truncate(MAX_OPEN_PULL_REQUESTS);
                return Ok(list);
            }
            match pager.next(page.page_info.has_next_page, page.page_info.end_cursor)? {
                Some(cursor) => after = Some(cursor),
                None => return Ok(list),
            }
        }
    }
}
