//! `OpenPullRequests`: a repository's open pull requests, with what stack detection needs.

use graphql_client::GraphQLQuery;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{DateTime, GitObjectID, MAX_OPEN_PULL_REQUESTS, PAGE_SIZE, Pager, not_found};
use crate::client::GithubClient;
use crate::error::GithubError;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/schema.graphql",
    query_path = "graphql/open_pull_requests.graphql",
    response_derives = "Debug, Clone, PartialEq, Serialize",
    variables_derives = "Debug, Clone"
)]
pub struct OpenPullRequests;

// The generated types, named from this module, such as `queries::open_pull_requests::Variables`.
pub use self::open_pull_requests::*;

pub type OpenPullRequest = OpenPullRequestsRepositoryPullRequestsNodes;

/// A repository's open pull requests.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OpenPullRequestList {
    /// Their shape is the `OpenPullRequests` query's, which the cache's version covers.
    #[schemars(with = "Vec<serde_json::Value>")]
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

impl GithubClient {
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
                    Variables {
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
