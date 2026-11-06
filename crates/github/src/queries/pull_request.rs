//! `PullRequest`: a pull request and every one of its files, with the viewer's viewed marks.

use graphql_client::GraphQLQuery;

use super::{DateTime, GitObjectID, PAGE_SIZE, Pager, URI, not_found};
use crate::client::GithubClient;
use crate::error::GithubError;

/// How many times a pull request's files are read again from the start when its head moves
/// while they're paged through.
const MAX_RESTARTS: usize = 2;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/schema.graphql",
    query_path = "graphql/pull_request.graphql",
    response_derives = "Debug, Clone, PartialEq, Serialize",
    variables_derives = "Debug, Clone"
)]
pub struct PullRequest;

// The generated types, named from this module, such as `queries::pull_request::Variables`.
pub use self::pull_request::*;

pub type PullRequestData = PullRequestRepositoryPullRequest;
pub type PullRequestFile = PullRequestRepositoryPullRequestFilesNodes;

impl GithubClient {
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
                Variables {
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
}
