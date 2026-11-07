//! Lo's GraphQL queries, typed from GitHub's schema.
//!
//! Each query is a `.graphql` file in `crates/github/graphql/`, with a module of the same name
//! here. `graphql_client` generates the query's Rust types from `graphql/schema.graphql`, and the
//! module names them and holds the [`GithubClient`](crate::GithubClient) methods that run the
//! query. Generated fields GitHub deprecates carry `#[deprecated]`, so after a schema refresh CI's
//! clippy fails on any use of one. The mutations that comment and review are run by
//! [`crate::review_writes`], which explains what GitHub refuses.

use std::collections::HashSet;

use crate::error::GithubError;

pub mod add_comment;
pub mod add_pull_request_review;
pub mod add_pull_request_review_thread;
pub mod add_pull_request_review_thread_reply;
pub mod delete_issue_comment;
pub mod delete_pull_request_review;
pub mod delete_pull_request_review_comment;
pub mod mark_file_as_viewed;
pub mod merge_checks;
pub mod merge_queue;
pub mod merge_snapshot;
pub mod merged_pull_requests;
pub mod open_pull_requests;
pub mod pending_review;
pub mod pull_request;
pub mod pull_request_check;
pub mod pull_request_revisions;
pub mod resolve_review_thread;
pub mod review_state;
pub mod review_thread_comments;
pub mod review_threads;
pub mod stack_layers;
pub mod submit_pull_request_review;
pub mod timeline;
pub mod trunk_rules;
pub mod unmark_file_as_viewed;
pub mod unresolve_review_thread;
pub mod update_issue_comment;
pub mod update_pull_request_review_comment;
pub mod viewer;

pub use add_comment::AddComment;
pub use add_pull_request_review::AddPullRequestReview;
pub use add_pull_request_review_thread::AddPullRequestReviewThread;
pub use add_pull_request_review_thread_reply::AddPullRequestReviewThreadReply;
pub use delete_issue_comment::DeleteIssueComment;
pub use delete_pull_request_review::DeletePullRequestReview;
pub use delete_pull_request_review_comment::DeletePullRequestReviewComment;
pub use mark_file_as_viewed::MarkFileAsViewed;
pub use merge_checks::MergeChecks;
pub use merge_queue::{MergeQueue, MergeQueueEntry, MergeQueueList};
pub use merge_snapshot::{
    BranchRule, BranchRules, CheckFacts, Local, MergeFacts, MergeSnapshot, Protection,
    PullRequestFacts, RepositoryFacts,
};
pub use merged_pull_requests::{BranchLookup, MergedPullRequest, MergedPullRequests, StackEntry};
pub use open_pull_requests::{OpenPullRequest, OpenPullRequestList, OpenPullRequests};
pub use pending_review::{PendingReview, PendingReviewNode, ViewerPendingReview};
pub use pull_request::{
    FileViewedState, PullRequest, PullRequestData, PullRequestFile, PullRequestState,
};
pub use pull_request_check::{
    MergeQueueEntryState, MergeStateStatus, PullRequestCheck, PullRequestCheckData,
    PullRequestReviewDecision, StatusState,
};
pub use pull_request_revisions::{PullRequestRevisions, PullRequestRevisionsData};
pub use resolve_review_thread::ResolveReviewThread;
pub use review_state::ReviewState;
pub use review_thread_comments::ReviewThreadComments;
pub use review_threads::{ReviewThread, ReviewThreadList, ReviewThreads};
pub use stack_layers::{StackLayer, StackLayerList, StackLayers, ViewedFiles};
pub use submit_pull_request_review::SubmitPullRequestReview;
pub use timeline::{PullRequestTimeline, Timeline, TimelineEntry, TimelineItem};
pub use trunk_rules::{TrunkRuleList, TrunkRules};
pub use unmark_file_as_viewed::UnmarkFileAsViewed;
pub use unresolve_review_thread::UnresolveReviewThread;
pub use update_issue_comment::UpdateIssueComment;
pub use update_pull_request_review_comment::UpdatePullRequestReviewComment;
pub use viewer::{Viewer, ViewerData};

// GitHub's custom scalars, as the generated code names them.
type DateTime = chrono::DateTime<chrono::Utc>;
#[allow(clippy::upper_case_acronyms)]
type URI = String;
type GitObjectID = String;

/// How many items a page asks for: GitHub's maximum.
pub const PAGE_SIZE: i64 = 100;
/// The most open pull requests Lo reads from one repository.
pub const MAX_OPEN_PULL_REQUESTS: usize = 1000;

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

/// A viewed-mark mutation's answer: GitHub names the pull request it changed. Without errors, a
/// missing one is an answer Lo doesn't understand, so it isn't taken as done.
fn marked<T>(pull_request: Option<T>) -> Result<(), GithubError> {
    pull_request.map(|_| ()).ok_or_else(|| {
        GithubError::Decode("GitHub didn't say which pull request it changed".into())
    })
}

fn not_found(what: String) -> GithubError {
    GithubError::NotFound {
        message: format!("Could not find {what}"),
        documentation_url: None,
    }
}
