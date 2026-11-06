//! rozed's GraphQL queries, typed from GitHub's schema.
//!
//! Each query is a `.graphql` file in `crates/github/graphql/`, and `graphql_client` generates a
//! module of Rust types for it from `graphql/schema.graphql`. Generated fields GitHub deprecates
//! carry `#[deprecated]`, so after a schema refresh CI's clippy fails on any use of one.

use graphql_client::GraphQLQuery;

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
    response_derives = "Debug, Clone, PartialEq",
    variables_derives = "Debug, Clone"
)]
pub struct PullRequest;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/schema.graphql",
    query_path = "graphql/pull_request_check.graphql",
    response_derives = "Debug, Clone, PartialEq",
    variables_derives = "Debug, Clone"
)]
pub struct PullRequestCheck;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/schema.graphql",
    query_path = "graphql/open_pull_requests.graphql",
    response_derives = "Debug, Clone, PartialEq",
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
#[derive(Debug, Clone, PartialEq)]
pub struct OpenPullRequestList {
    pub pull_requests: Vec<OpenPullRequest>,
    /// How many are open, which is more than `pull_requests` holds when it was capped.
    pub total_count: i64,
}

impl OpenPullRequestList {
    /// Whether there were more open pull requests than [`MAX_OPEN_PULL_REQUESTS`].
    pub fn is_truncated(&self) -> bool {
        usize::try_from(self.total_count).unwrap_or(usize::MAX) > self.pull_requests.len()
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
        Ok(self.graphql::<Viewer>(viewer::Variables).await?.viewer)
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

    /// [`Self::pull_request`], reading `page_size` files a page.
    pub async fn pull_request_paged(
        &self,
        owner: &str,
        name: &str,
        number: i64,
        page_size: i64,
    ) -> Result<PullRequestData, GithubError> {
        let fetch = |after: Option<String>| {
            self.graphql::<PullRequest>(pull_request::Variables {
                owner: owner.to_string(),
                name: name.to_string(),
                number,
                files_first: page_size,
                files_after: after,
            })
        };
        let missing = || not_found(format!("pull request {owner}/{name}#{number}"));
        let mut pull_request = fetch(None)
            .await?
            .repository
            .and_then(|repository| repository.pull_request)
            .ok_or_else(missing)?;
        let Some(files) = pull_request.files.as_mut() else {
            return Ok(pull_request);
        };
        while files.page_info.has_next_page {
            let next = fetch(files.page_info.end_cursor.clone())
                .await?
                .repository
                .and_then(|repository| repository.pull_request)
                .ok_or_else(missing)?
                .files
                .ok_or_else(|| GithubError::Decode("a page of files went missing".into()))?;
            files
                .nodes
                .get_or_insert_with(Vec::new)
                .extend(next.nodes.unwrap_or_default());
            files.page_info = next.page_info;
        }
        Ok(pull_request)
    }

    /// The cheap check for whether a pull request changed.
    pub async fn pull_request_check(
        &self,
        owner: &str,
        name: &str,
        number: i64,
    ) -> Result<PullRequestCheckData, GithubError> {
        self.graphql::<PullRequestCheck>(pull_request_check::Variables {
            owner: owner.to_string(),
            name: name.to_string(),
            number,
        })
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
        };
        let mut after = None;
        loop {
            let page = self
                .graphql::<OpenPullRequests>(open_pull_requests::Variables {
                    owner: owner.to_string(),
                    name: name.to_string(),
                    first: page_size,
                    after: after.take(),
                })
                .await?
                .repository
                .ok_or_else(|| not_found(format!("repository {owner}/{name}")))?
                .pull_requests;
            list.total_count = page.total_count;
            list.pull_requests
                .extend(page.nodes.unwrap_or_default().into_iter().flatten());
            if !page.page_info.has_next_page || list.pull_requests.len() >= MAX_OPEN_PULL_REQUESTS {
                list.pull_requests.truncate(MAX_OPEN_PULL_REQUESTS);
                return Ok(list);
            }
            after = page.page_info.end_cursor;
        }
    }
}
