//! GitHub's enums for a pull request's merge, named after GitHub's GraphQL enums. Each one parses
//! from its GraphQL name (`HAS_HOOKS`) or its REST name (`has_hooks`).

use crate::github::github_enum;

github_enum! {
    /// Whether a pull request is open: GraphQL's `PullRequestState`. REST has only `open` and
    /// `closed`, with `merged` beside it.
    #[derive(Default)]
    pub enum PullRequestState {
        #[default]
        Open = "OPEN",
        Closed = "CLOSED",
        Merged = "MERGED",
    }
}

github_enum! {
    /// Whether a pull request merges without conflicts: GraphQL's `MergeableState`. REST's
    /// `mergeable` is a nullable boolean; see the `From<Option<bool>>` conversion.
    #[derive(Default)]
    pub enum MergeableState {
        Mergeable = "MERGEABLE",
        Conflicting = "CONFLICTING",
        /// GitHub is still working it out. It does that in the background after a push to the
        /// head or the base, so a fresh read usually has it.
        #[default]
        Unknown = "UNKNOWN",
    }
}

impl From<Option<bool>> for MergeableState {
    /// REST's `mergeable`: `true`, `false`, or `null` while GitHub works it out.
    fn from(mergeable: Option<bool>) -> Self {
        match mergeable {
            Some(true) => Self::Mergeable,
            Some(false) => Self::Conflicting,
            None => Self::Unknown,
        }
    }
}

github_enum! {
    /// GitHub's summary of whether a pull request can merge: GraphQL's `mergeStateStatus`, REST's
    /// `mergeable_state`. It names one reason at a time, so a pull request that's both blocked and
    /// out of date says only `BLOCKED`.
    #[derive(Default)]
    pub enum MergeStateStatus {
        /// The base branch's rules require the head to be up to date with the base, and it isn't.
        Behind = "BEHIND",
        /// Something the base branch's rules require is missing: reviews, checks, resolved
        /// conversations or another rule.
        Blocked = "BLOCKED",
        /// It can merge, and its checks pass.
        Clean = "CLEAN",
        /// It conflicts with the base.
        Dirty = "DIRTY",
        /// It's a draft. Deprecated in GraphQL in favour of `isDraft`, but REST still reports it.
        Draft = "DRAFT",
        /// It can merge, and the server has pre-receive hooks.
        HasHooks = "HAS_HOOKS",
        /// GitHub is still working it out.
        #[default]
        Unknown = "UNKNOWN",
        /// It can merge, but a check that isn't required hasn't passed.
        Unstable = "UNSTABLE",
    }
}

github_enum! {
    /// Where a pull request stands on review: GraphQL's `PullRequestReviewDecision`.
    pub enum ReviewDecision {
        Approved = "APPROVED",
        ChangesRequested = "CHANGES_REQUESTED",
        ReviewRequired = "REVIEW_REQUIRED",
    }
}

github_enum! {
    /// A review's state: GraphQL's `PullRequestReviewState`, REST's review `state`.
    pub enum ReviewState {
        Approved = "APPROVED",
        ChangesRequested = "CHANGES_REQUESTED",
        Commented = "COMMENTED",
        Dismissed = "DISMISSED",
        Pending = "PENDING",
    }
}

github_enum! {
    /// How a pull request merges: GraphQL's `PullRequestMergeMethod`, which the merge queue's
    /// `configuration.mergeMethod` uses too, and REST's `merge_method`. Ordered as GitHub's merge
    /// button lists them.
    pub enum MergeMethod {
        Merge = "MERGE",
        Squash = "SQUASH",
        Rebase = "REBASE",
    }
}

github_enum! {
    /// Where a check run is: GraphQL's `CheckStatusState`, REST's check run `status`.
    pub enum CheckStatus {
        Requested = "REQUESTED",
        Queued = "QUEUED",
        Waiting = "WAITING",
        Pending = "PENDING",
        InProgress = "IN_PROGRESS",
        Completed = "COMPLETED",
    }
}

github_enum! {
    /// How a completed check run ended: GraphQL's `CheckConclusionState`, REST's check run
    /// `conclusion`.
    pub enum CheckConclusion {
        Success = "SUCCESS",
        Neutral = "NEUTRAL",
        Skipped = "SKIPPED",
        Failure = "FAILURE",
        Cancelled = "CANCELLED",
        TimedOut = "TIMED_OUT",
        ActionRequired = "ACTION_REQUIRED",
        StartupFailure = "STARTUP_FAILURE",
        Stale = "STALE",
    }
}

github_enum! {
    /// A commit status's state: GraphQL's `StatusState`, REST's status `state`.
    pub enum StatusState {
        Success = "SUCCESS",
        Pending = "PENDING",
        Failure = "FAILURE",
        Error = "ERROR",
        /// A required status that hasn't been reported on the commit yet.
        Expected = "EXPECTED",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::UnknownValue;

    #[test]
    fn values_parse_from_their_graphql_names() {
        assert_eq!("HAS_HOOKS".parse(), Ok(MergeStateStatus::HasHooks));
        assert_eq!(
            "CHANGES_REQUESTED".parse(),
            Ok(ReviewDecision::ChangesRequested)
        );
        assert_eq!(
            "CHANGES_REQUESTED".parse(),
            Ok(ReviewState::ChangesRequested)
        );
        assert_eq!("CONFLICTING".parse(), Ok(MergeableState::Conflicting));
        assert_eq!("MERGED".parse(), Ok(PullRequestState::Merged));
        assert_eq!("SQUASH".parse(), Ok(MergeMethod::Squash));
        assert_eq!("IN_PROGRESS".parse(), Ok(CheckStatus::InProgress));
        assert_eq!(
            "STARTUP_FAILURE".parse(),
            Ok(CheckConclusion::StartupFailure)
        );
        assert_eq!("EXPECTED".parse(), Ok(StatusState::Expected));
    }

    #[test]
    fn values_parse_from_their_rest_names() {
        assert_eq!("has_hooks".parse(), Ok(MergeStateStatus::HasHooks));
        assert_eq!("open".parse(), Ok(PullRequestState::Open));
        assert_eq!("rebase".parse(), Ok(MergeMethod::Rebase));
        assert_eq!("timed_out".parse(), Ok(CheckConclusion::TimedOut));
        assert_eq!("error".parse(), Ok(StatusState::Error));
    }

    #[test]
    fn an_unknown_value_is_an_error() {
        assert_eq!(
            "MERGE_QUEUED".parse::<MergeStateStatus>(),
            Err(UnknownValue("MERGE_QUEUED".to_string()))
        );
        assert_eq!("".parse::<MergeMethod>(), Err(UnknownValue(String::new())));
    }

    #[test]
    fn values_give_back_their_graphql_names() {
        assert_eq!(MergeStateStatus::HasHooks.as_str(), "HAS_HOOKS");
        assert_eq!(MergeMethod::Squash.as_str(), "SQUASH");
        assert_eq!(
            "timed_out".parse::<CheckConclusion>().unwrap().as_str(),
            "TIMED_OUT"
        );
    }

    #[test]
    fn rest_mergeable_maps_to_the_graphql_state() {
        assert_eq!(MergeableState::from(Some(true)), MergeableState::Mergeable);
        assert_eq!(
            MergeableState::from(Some(false)),
            MergeableState::Conflicting
        );
        assert_eq!(MergeableState::from(None), MergeableState::Unknown);
    }
}
