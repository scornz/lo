//! `PullRequestRevisions`: what a pull request's revisions and their bases come from in GraphQL,
//! besides its branches' push history (`crate::push_history`).

use graphql_client::GraphQLQuery;

use super::{DateTime, GitObjectID, PAGE_SIZE, Pager, not_found};
use crate::client::GithubClient;
use crate::error::GithubError;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/schema.graphql",
    query_path = "graphql/pull_request_revisions.graphql",
    response_derives = "Debug, Clone, PartialEq",
    variables_derives = "Debug, Clone"
)]
pub struct PullRequestRevisions;

// The generated types, named from this module, such as
// `queries::pull_request_revisions::Variables`.
pub use self::pull_request_revisions::*;

pub type PullRequestRevisionsData = PullRequestRevisionsRepositoryPullRequest;
pub type TimelineEvent = PullRequestRevisionsRepositoryPullRequestTimelineItemsNodes;
pub type Review = PullRequestRevisionsRepositoryPullRequestReviewsNodes;

impl GithubClient {
    /// What a pull request's revisions come from in GraphQL, with every force-push and retarget
    /// event and every submitted review. `login` is the signed-in user's, whose last review is
    /// one of the markers revisions open from.
    pub async fn pull_request_revisions(
        &self,
        owner: &str,
        name: &str,
        number: i64,
        login: &str,
    ) -> Result<PullRequestRevisionsData, GithubError> {
        self.pull_request_revisions_paged(owner, name, number, login, PAGE_SIZE)
            .await
    }

    /// [`Self::pull_request_revisions`], reading `page_size` events and reviews a page. While one
    /// of the two has pages left, the other is asked for after its last cursor, and what comes
    /// back for it is ignored.
    pub async fn pull_request_revisions_paged(
        &self,
        owner: &str,
        name: &str,
        number: i64,
        login: &str,
        page_size: i64,
    ) -> Result<PullRequestRevisionsData, GithubError> {
        let fetch = |timeline_after: Option<String>, reviews_after: Option<String>| async move {
            self.graphql_keeping_partial::<PullRequestRevisions>(
                Variables {
                    owner: owner.to_string(),
                    name: name.to_string(),
                    number,
                    login: login.to_string(),
                    first: page_size,
                    timeline_after,
                    reviews_after,
                },
                2,
            )
            .await?
            .repository
            .and_then(|repository| repository.pull_request)
            .ok_or_else(|| not_found(format!("pull request {owner}/{name}#{number}")))
        };
        let mut pull_request = fetch(None, None).await?;
        let timeline = &pull_request.timeline_items;
        let mut timeline_pager = Pager::new("timeline events", timeline.filtered_count, page_size);
        let mut timeline_after = timeline_pager.next(
            timeline.page_info.has_next_page,
            timeline.page_info.end_cursor.clone(),
        )?;
        let reviews = pull_request.reviews.as_ref();
        let mut reviews_pager = Pager::new(
            "reviews",
            reviews.map_or(0, |reviews| reviews.total_count),
            page_size,
        );
        let mut reviews_after = match reviews {
            Some(reviews) => reviews_pager.next(
                reviews.page_info.has_next_page,
                reviews.page_info.end_cursor.clone(),
            )?,
            None => None,
        };
        while timeline_after.is_some() || reviews_after.is_some() {
            let timeline_end = pull_request.timeline_items.page_info.end_cursor.clone();
            let reviews_end = (pull_request.reviews.as_ref())
                .and_then(|reviews| reviews.page_info.end_cursor.clone());
            let page = fetch(
                timeline_after.clone().or(timeline_end),
                reviews_after.clone().or(reviews_end),
            )
            .await?;
            if timeline_after.is_some() {
                let next = page.timeline_items;
                let timeline = &mut pull_request.timeline_items;
                (timeline.nodes.get_or_insert_with(Vec::new))
                    .extend(next.nodes.unwrap_or_default());
                timeline.page_info = next.page_info;
                timeline_after = timeline_pager.next(
                    timeline.page_info.has_next_page,
                    timeline.page_info.end_cursor.clone(),
                )?;
            }
            if reviews_after.is_some() {
                let next = page
                    .reviews
                    .ok_or_else(|| GithubError::Decode("a page of reviews went missing".into()))?;
                let reviews = pull_request
                    .reviews
                    .as_mut()
                    .expect("there was a page of them before");
                (reviews.nodes.get_or_insert_with(Vec::new)).extend(next.nodes.unwrap_or_default());
                reviews.page_info = next.page_info;
                reviews_after = reviews_pager.next(
                    reviews.page_info.has_next_page,
                    reviews.page_info.end_cursor.clone(),
                )?;
            }
        }
        Ok(pull_request)
    }
}
