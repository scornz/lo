//! `Timeline`: a pull request's timeline (design §8, "Timeline"), its description and its items,
//! newest first, mapped into Lo's [`TimelineItem`]s. The open pull request's poller keeps it in
//! the cache and current ([`crate::poll::Poller::timeline`]).
//!
//! GraphQL has no ETags, so a timeline read again asks only for its newest page, and keeps the
//! older items it has: each item's cursor says where the page joins them. A long timeline costs a
//! point a page the first time, and a point each time it's read again. It's read in full again
//! when an older item may have changed:
//!
//! - the page doesn't join what Lo has, or the counts don't move together, as after a deletion;
//! - the time of the newest item it shows moved, but nothing on the page changed, as when an
//!   item is added while an older one is deleted. An edit doesn't move that time (checked live),
//!   so an older comment's edit by someone else shows at the next full read;
//! - the page has a new dismissal of a review it doesn't hold, or a new force-push to a commit it
//!   doesn't hold, which changes or replaces older items;
//! - and at least every [`READ_IN_FULL_EVERY`], for what none of those show, such as checks run
//!   again on an older commit.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use chrono::Utc;
use graphql_client::GraphQLQuery;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{DateTime, GitObjectID, PAGE_SIZE, URI, not_found};
use crate::cache::{CacheEntry, schema_shape};
use crate::client::GithubClient;
use crate::error::GithubError;
use crate::poll::CachedRead;
use crate::pulls::PullRef;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/schema.graphql",
    query_path = "graphql/timeline.graphql",
    response_derives = "Debug, Clone, PartialEq, Serialize",
    variables_derives = "Debug, Clone",
    // An item type GitHub adds after Lo's schema reads as `Unknown`, not as an error.
    fragments_other_variant = "true"
)]
pub struct Timeline;

// The generated types, named from this module, such as `queries::timeline::Variables`.
pub use self::timeline::*;

/// The most pages of a timeline read, newest first. Older items are left out
/// ([`PullRequestTimeline::left_out`]).
pub const MAX_TIMELINE_PAGES: usize = 10;

/// How often a timeline kept current is read in full rather than by its newest page.
pub const READ_IN_FULL_EVERY: Duration = Duration::from_secs(60 * 60);

/// A pull request's timeline: its description, and its items oldest first, as GitHub lists them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PullRequestTimeline {
    pub description: Description,
    /// The items, oldest first, each by its cursor: every item, or the latest
    /// [`MAX_TIMELINE_PAGES`] pages of them.
    pub items: Vec<TimelineEntry>,
    /// How many older items weren't read, past [`MAX_TIMELINE_PAGES`] pages.
    pub left_out: usize,
    /// How many items GitHub counted when it was read (`filteredCount`). A read of the newest
    /// page joins this one only when the two counts differ by as many items as it adds.
    pub count: usize,
    /// When the newest item the timeline shows was made, as of when it was read. An edit doesn't
    /// move it (checked live, in the `review_writes_e2e_23_*` recordings).
    #[schemars(with = "Option<String>")]
    pub shown_updated_at: Option<chrono::DateTime<Utc>>,
    /// When it was last read in full. Reads in between ask only for its newest page.
    #[schemars(with = "String")]
    pub read_in_full_at: chrono::DateTime<Utc>,
}

/// The pull request's description: its first post.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Description {
    /// `author.login`. `None` for a deleted account.
    pub author: Option<String>,
    /// `body`, in Markdown.
    pub body: String,
    /// `createdAt`.
    #[schemars(with = "String")]
    pub created_at: chrono::DateTime<Utc>,
    /// `lastEditedAt`, once it's been edited.
    #[schemars(with = "Option<String>")]
    pub last_edited_at: Option<chrono::DateTime<Utc>>,
}

/// An item and its cursor, which says where it is in the timeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TimelineEntry {
    pub cursor: String,
    pub item: TimelineItem,
}

/// An item of a pull request's timeline (design §8, "Timeline"). Each variant names the GraphQL
/// types it comes from; every other type is [`TimelineItem::Other`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum TimelineItem {
    /// `IssueComment`: a comment on the pull request itself.
    Comment(Comment),
    /// `PullRequestReview`: a review, whose comments are its threads'.
    Review(Review),
    /// `PullRequestCommit`: a commit on the head branch, with its checks.
    Commit(Commit),
    /// `HeadRefForcePushedEvent`, or `BaseRefForcePushedEvent` on the base branch.
    ForcePushed {
        event: Event,
        branch: Branch,
        /// The branch's commit before and after, `None` when GitHub no longer has it.
        before: Option<String>,
        after: Option<String>,
    },
    /// `LabeledEvent`, or `UnlabeledEvent` when `added` is false.
    Label {
        event: Event,
        name: String,
        /// The label's color, as six hex digits.
        color: String,
        added: bool,
    },
    /// `AssignedEvent`, or `UnassignedEvent` when `added` is false. `assignee` is a login, `None`
    /// for a deleted account.
    Assignment {
        event: Event,
        assignee: Option<String>,
        added: bool,
    },
    /// `ReviewRequestedEvent`, or `ReviewRequestRemovedEvent` when `added` is false. `reviewer`
    /// is `None` for a deleted account or team.
    ReviewRequest {
        event: Event,
        reviewer: Option<Reviewer>,
        added: bool,
    },
    /// `CrossReferencedEvent`: another issue or pull request mentioned this one. `source` is
    /// `None` when it's of a kind Lo doesn't know.
    CrossReference {
        event: Event,
        source: Option<Reference>,
        /// `willCloseTarget`: merging the source closes this pull request.
        will_close: bool,
    },
    /// `MergedEvent`: `commit` merged it into the branch `into`.
    Merged {
        event: Event,
        commit: Option<String>,
        into: String,
    },
    /// `AddedToMergeQueueEvent`, or `RemovedFromMergeQueueEvent` when `added` is false, with
    /// GitHub's reason.
    MergeQueue {
        event: Event,
        added: bool,
        reason: Option<String>,
    },
    /// `DeployedEvent`: deployed to `environment`, whose latest status is `state`, a
    /// `DeploymentStatusState`.
    Deployed {
        event: Event,
        environment: Option<String>,
        state: Option<String>,
    },
    /// `DeploymentEnvironmentChangedEvent`: a deployment's status changed to `state`, a
    /// `DeploymentStatusState`.
    DeploymentChanged {
        event: Event,
        environment: Option<String>,
        state: String,
    },
    /// `ConvertToDraftEvent`, or `ReadyForReviewEvent` when `draft` is false.
    Draft { event: Event, draft: bool },
    /// `ClosedEvent`.
    Closed(Event),
    /// `ReopenedEvent`.
    Reopened(Event),
    /// `BaseRefChangedEvent`, `AutomaticBaseChangeSucceededEvent` or
    /// `AutomaticBaseChangeFailedEvent`: the branch it targets changed, or GitHub failed to change
    /// it, from `from` to `to`.
    BaseChanged {
        event: Event,
        from: String,
        to: String,
        how: BaseChange,
    },
    /// `ReviewDismissedEvent`: `reviewer`'s review, whose node ID is `review`, and which was
    /// `previous_state`, a `PullRequestReviewState`, was dismissed.
    ReviewDismissed {
        event: Event,
        review: Option<String>,
        reviewer: Option<String>,
        message: Option<String>,
        previous_state: String,
    },
    /// `AutoMergeEnabledEvent`, `AutoRebaseEnabledEvent`, `AutoSquashEnabledEvent` or
    /// `AutoMergeDisabledEvent`.
    AutoMerge { event: Event, change: AutoMerge },
    /// `AddedToStackEvent`, or `RemovedFromStackEvent` when `added` is false.
    Stack {
        event: Event,
        /// The stack's number, which isn't a pull request's.
        number: Option<i64>,
        added: bool,
    },
    /// Any other item, by its `__typename`, which Lo shows nothing of. `None` for an item GitHub
    /// left out, or of a type newer than Lo's schema.
    Other { typename: Option<String> },
}

/// Who did something, and when: an event's `actor.login` and `createdAt`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Event {
    /// `None` for a deleted account.
    pub actor: Option<String>,
    #[schemars(with = "String")]
    pub at: chrono::DateTime<Utc>,
}

/// `IssueComment`'s fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Comment {
    pub id: String,
    /// `author.login`. `None` for a deleted account.
    pub author: Option<String>,
    /// `body`, in Markdown.
    pub body: String,
    #[schemars(with = "String")]
    pub created_at: chrono::DateTime<Utc>,
    #[schemars(with = "Option<String>")]
    pub last_edited_at: Option<chrono::DateTime<Utc>>,
    pub url: String,
    /// `minimizedReason` when it's hidden (`isMinimized`), such as `outdated`, or an empty
    /// string when GitHub gives none.
    pub hidden: Option<String>,
    pub viewer_can_update: bool,
    pub viewer_can_delete: bool,
}

/// `PullRequestReview`'s fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Review {
    pub id: String,
    /// `author.login`. `None` for a deleted account.
    pub author: Option<String>,
    /// `body`, in Markdown, which may be empty.
    pub body: String,
    /// `state`, a `PullRequestReviewState`: `APPROVED`, `CHANGES_REQUESTED`, `COMMENTED`,
    /// `DISMISSED` or `PENDING`.
    pub state: String,
    #[schemars(with = "String")]
    pub created_at: chrono::DateTime<Utc>,
    /// `None` while it's pending.
    #[schemars(with = "Option<String>")]
    pub submitted_at: Option<chrono::DateTime<Utc>>,
    #[schemars(with = "Option<String>")]
    pub last_edited_at: Option<chrono::DateTime<Utc>>,
    pub url: String,
    /// `isMinimized`.
    pub hidden: bool,
    /// `commit.oid`: the revision it reviewed. `None` once GitHub no longer has the commit.
    pub commit: Option<String>,
    /// `comments.totalCount`: its review comments, which the threads show.
    pub comments: usize,
}

/// `PullRequestCommit`'s commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Commit {
    pub oid: String,
    /// `messageHeadline`.
    pub headline: String,
    /// The author's GitHub login, when GitHub matched their email to an account.
    pub author_login: Option<String>,
    /// The author's name, as the commit gives it.
    pub author_name: Option<String>,
    /// `committedDate`.
    #[schemars(with = "String")]
    pub committed_at: chrono::DateTime<Utc>,
    /// `statusCheckRollup.state`, a `StatusState`, when it has checks or statuses.
    pub checks: Option<String>,
}

/// Which branch a force-push moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Branch {
    Head,
    Base,
}

/// Who was asked for a review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Reviewer {
    /// A person, bot or mannequin, by login.
    Account(String),
    /// A team, as `org/team`, or an enterprise team by its slug.
    Team(String),
}

/// An issue or pull request that mentioned this one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Reference {
    pub kind: ReferenceKind,
    /// `repository.nameWithOwner`: `owner/repo`.
    pub repository: String,
    pub number: i64,
    pub title: String,
    pub url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ReferenceKind {
    Issue,
    PullRequest,
}

/// How a pull request's base branch changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum BaseChange {
    /// Someone changed it (`BaseRefChangedEvent`).
    Changed,
    /// GitHub changed it after the branch it targeted was merged and deleted
    /// (`AutomaticBaseChangeSucceededEvent`).
    Automatic,
    /// GitHub tried to, and couldn't (`AutomaticBaseChangeFailedEvent`).
    AutomaticFailed,
}

/// What happened to auto-merge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum AutoMerge {
    /// Turned on, to merge with a merge commit (`AutoMergeEnabledEvent`).
    EnabledMerge,
    /// Turned on, to rebase (`AutoRebaseEnabledEvent`).
    EnabledRebase,
    /// Turned on, to squash (`AutoSquashEnabledEvent`).
    EnabledSquash,
    /// Turned off (`AutoMergeDisabledEvent`), with GitHub's reason.
    Disabled { reason: Option<String> },
}

impl PullRequestTimeline {
    /// Whether it's time to read it in full, at `now`.
    fn due_in_full(&self, now: SystemTime) -> bool {
        let read_at = SystemTime::from(self.read_in_full_at);
        now.duration_since(read_at)
            .is_ok_and(|age| age >= READ_IN_FULL_EVERY)
            || read_at > now
    }

    /// This timeline with `page`, its newest, joined on: the items from the page's first on are
    /// the page's. `None` when it must be read in full instead: the page doesn't join, the items
    /// it adds don't match how far GitHub's count moved, or an older item may have changed (see
    /// the module's docs).
    fn joined(&self, page: &Page, max_items: usize) -> Option<Self> {
        let first = page.entries.first()?;
        let at = (self.items.iter()).position(|entry| entry.cursor == first.cursor)?;
        let on_page: HashSet<&str> = (page.entries.iter())
            .map(|entry| entry.cursor.as_str())
            .collect();
        let mut items: Vec<TimelineEntry> = (self.items[..at].iter())
            .filter(|entry| !on_page.contains(entry.cursor.as_str()))
            .cloned()
            .collect();
        items.extend(page.entries.iter().cloned());
        let added = items.len() as i64 - self.items.len() as i64;
        if added != page.count as i64 - self.count as i64 {
            return None;
        }
        let known: HashMap<&str, &TimelineItem> = (self.items.iter())
            .map(|entry| (entry.cursor.as_str(), &entry.item))
            .collect();
        let new: Vec<&TimelineItem> = (page.entries.iter())
            .filter(|entry| !known.contains_key(entry.cursor.as_str()))
            .map(|entry| &entry.item)
            .collect();
        let changed = !new.is_empty()
            || (page.entries.iter())
                .any(|entry| known.get(entry.cursor.as_str()) != Some(&&entry.item));
        if page.shown_updated_at != self.shown_updated_at && !changed {
            return None;
        }
        if new.iter().any(|item| !page.holds_subject(item)) {
            return None;
        }
        let dropped = items.len().saturating_sub(max_items);
        items.drain(..dropped);
        Some(Self {
            description: page.description.clone(),
            items,
            left_out: self.left_out + dropped,
            count: page.count,
            shown_updated_at: page.shown_updated_at,
            read_in_full_at: self.read_in_full_at,
        })
    }
}

/// A page of a timeline, newest first.
struct Page {
    description: Description,
    entries: Vec<TimelineEntry>,
    /// How many items the timeline has, when the page was read without a cursor; otherwise how
    /// many come before the cursor.
    count: usize,
    /// When the newest item the timeline shows was made. An edit doesn't move it.
    shown_updated_at: Option<chrono::DateTime<Utc>>,
    has_previous_page: bool,
    start_cursor: Option<String>,
}

impl Page {
    /// Whether the page holds what `item` changes, when it's an item that changes an older one: a
    /// dismissal changes the review it dismisses, and a force-push replaces the commits before it
    /// with those up to the one it pushed. A force-push of the base branch changes the pull
    /// request's commits, none of which it names.
    fn holds_subject(&self, item: &TimelineItem) -> bool {
        match item {
            TimelineItem::ReviewDismissed { review, .. } => (self.entries.iter()).any(|entry| {
                matches!(&entry.item, TimelineItem::Review(held) if Some(&held.id) == review.as_ref())
            }),
            TimelineItem::ForcePushed { branch, after, .. } => {
                *branch == Branch::Head
                    && (self.entries.iter()).any(|entry| {
                        matches!(&entry.item, TimelineItem::Commit(held) if Some(&held.oid) == after.as_ref())
                    })
            }
            _ => true,
        }
    }

    /// The cursor to read the page before this one before, or `None` when this is the first.
    ///
    /// GitHub lists items that share a time in one order, but `before` compares them in another,
    /// so reading before a page's first item skips some of the items that share its time and
    /// repeats others (seen on cli/cli#13057, whose rebased commits share a time). Reading before
    /// the page's first item with a later time reads all of them, and the ones this page holds
    /// are dropped as repeats, so each page after the newest reads one item or more again. That
    /// needs the time each cursor holds, in a format GitHub doesn't document, so a cursor that
    /// doesn't read as one, or a page whose items share one time, is read before its start, as
    /// GitHub's paging intends. `skip`, GitHub's offset paging, isn't an alternative: on
    /// rust-lang/rust#145108 it lists 252 of 547 items.
    fn before(&self) -> Option<String> {
        if !self.has_previous_page {
            return None;
        }
        let first = self.entries.first().and_then(|entry| {
            let time = cursor_time(&entry.cursor);
            if time.is_none() {
                cursor_unread(&entry.cursor);
            }
            time
        });
        let later = first.and_then(|first| {
            (self.entries.iter())
                .find(|entry| cursor_time(&entry.cursor).is_some_and(|time| time != first))
        });
        later
            .map(|entry| entry.cursor.clone())
            .or_else(|| self.start_cursor.clone())
    }
}

/// Says, once, that a timeline cursor doesn't hold a time Lo can read, as when GitHub changes
/// their format: long timelines then page as GitHub's paging intends, which can skip items that
/// share a time.
fn cursor_unread(cursor: &str) {
    static SAID: AtomicBool = AtomicBool::new(false);
    if !SAID.swap(true, Ordering::Relaxed) {
        log::warn!("a timeline cursor holds no time Lo can read: {cursor}");
    }
}

/// The time a timeline item's cursor holds, as its bytes, comparable for equality only. A cursor
/// is `cursor:v2:` and a MessagePack array whose first element is the time, in base64.
fn cursor_time(cursor: &str) -> Option<Vec<u8>> {
    let bytes = base64_decode(cursor)?;
    let rest = bytes.strip_prefix(b"cursor:v2:")?;
    let (&array, rest) = rest.split_first()?;
    // A fixarray of at least one element.
    if array & 0xf0 != 0x90 || array == 0x90 {
        return None;
    }
    // An integer: a positive fixint, or one of the int and uint formats.
    let length = match *rest.first()? {
        0x00..=0x7f => 1,
        0xcc | 0xd0 => 2,
        0xcd | 0xd1 => 3,
        0xce | 0xd2 => 5,
        0xcf | 0xd3 => 9,
        _ => return None,
    };
    rest.get(..length).map(<[u8]>::to_vec)
}

/// Base64 in either alphabet, padded or not, or `None` when it isn't base64.
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut bytes = Vec::with_capacity(text.len() * 3 / 4);
    let (mut bits, mut count) = (0u32, 0u32);
    for character in text.trim_end_matches('=').bytes() {
        let value = match character {
            b'A'..=b'Z' => character - b'A',
            b'a'..=b'z' => character - b'a' + 26,
            b'0'..=b'9' => character - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        };
        bits = (bits << 6) | u32::from(value);
        count += 6;
        if count >= 8 {
            count -= 8;
            bytes.push((bits >> count) as u8);
            bits &= (1 << count) - 1;
        }
    }
    Some(bytes)
}

impl CacheEntry for PullRequestTimeline {
    const NAME: &'static str = OPERATION_NAME;

    fn shape() -> String {
        format!(
            "{}\n{}\n{MAX_TIMELINE_PAGES} pages of {PAGE_SIZE}",
            schema_shape::<Self>(),
            QUERY
        )
    }
}

impl CachedRead for PullRequestTimeline {
    /// The check counts the items the timeline shows and holds when the newest was made; so an
    /// item added or deleted changes them, and a mention or a subscription doesn't. An edit
    /// changes neither (checked live), but it moves the pull request's own `updatedAt`, which a
    /// new title and other changes move too: reading the newest page again then is cheap, and it
    /// shows an edit there at the next poll. It never reads in full on that alone, which could
    /// cost 10 pages for each edit, so an edit of an older item by someone else shows at the next
    /// full read, within the hour; Lo's own edits read it in full themselves
    /// ([`crate::poll::Poller::edit_comment`]). With them, the latest commit's checks, and when
    /// the description was last edited.
    ///
    /// (`pull_request_check.graphql`'s comment on `timelineEvents` still says an edit moves its
    /// time. Changing it changes the query's hash, so it waits for the next change to that query,
    /// which re-records its cassettes anyway.)
    const WATCHED: &'static [&'static str] = &[
        "/timelineEvents",
        "/statusCheckRollup",
        "/lastEditedAt",
        "/updatedAt",
    ];
}

impl GithubClient {
    /// `pull`'s timeline, newest first, at most [`MAX_TIMELINE_PAGES`] pages. With `known`,
    /// read before, it reads only the newest page, unless it's due to be read in full at `now`
    /// or the page doesn't join `known`.
    pub async fn timeline(
        &self,
        pull: &PullRef,
        known: Option<&PullRequestTimeline>,
        now: SystemTime,
    ) -> Result<PullRequestTimeline, GithubError> {
        self.timeline_paged(pull, known, now, PAGE_SIZE).await
    }

    /// [`Self::timeline`], reading `page_size` items a page.
    pub async fn timeline_paged(
        &self,
        pull: &PullRef,
        known: Option<&PullRequestTimeline>,
        now: SystemTime,
        page_size: i64,
    ) -> Result<PullRequestTimeline, GithubError> {
        let max_items = MAX_TIMELINE_PAGES * usize::try_from(page_size).unwrap_or(1);
        let newest = self.timeline_page(pull, page_size, None).await?;
        if !newest.has_previous_page {
            counted(pull, newest.count, newest.entries.len());
            return Ok(PullRequestTimeline {
                left_out: 0,
                count: newest.count,
                shown_updated_at: newest.shown_updated_at,
                description: newest.description,
                items: newest.entries,
                read_in_full_at: now.into(),
            });
        }
        if let Some(known) = known.filter(|known| !known.due_in_full(now))
            && let Some(joined) = known.joined(&newest, max_items)
        {
            return Ok(joined);
        }
        let count = newest.count;
        let mut seen: HashSet<String> = (newest.entries.iter())
            .map(|entry| entry.cursor.clone())
            .collect();
        let mut asked = HashSet::new();
        let mut before = newest.before();
        let mut pages = vec![newest.entries];
        let mut truncated = false;
        while let Some(cursor) = before.take() {
            if pages.len() >= MAX_TIMELINE_PAGES {
                truncated = true;
                break;
            }
            if !asked.insert(cursor.clone()) {
                return Err(GithubError::Inconsistent(
                    "a page of timeline items came back again".into(),
                ));
            }
            let page = self.timeline_page(pull, page_size, Some(cursor)).await?;
            before = page.before();
            // It holds again the items the page after it has before `cursor`.
            let fresh: Vec<TimelineEntry> = (page.entries.into_iter())
                .filter(|entry| seen.insert(entry.cursor.clone()))
                .collect();
            if fresh.is_empty() {
                break;
            }
            pages.push(fresh);
        }
        let mut items: Vec<TimelineEntry> = pages.into_iter().rev().flatten().collect();
        let dropped = items.len().saturating_sub(max_items);
        items.drain(..dropped);
        let left_out = if truncated || dropped > 0 {
            count.saturating_sub(items.len())
        } else {
            counted(pull, count, items.len());
            0
        };
        Ok(PullRequestTimeline {
            left_out,
            count,
            shown_updated_at: newest.shown_updated_at,
            description: newest.description,
            items,
            read_in_full_at: now.into(),
        })
    }

    /// The page of `pull`'s timeline before `before`, or the newest.
    async fn timeline_page(
        &self,
        pull: &PullRef,
        page_size: i64,
        before: Option<String>,
    ) -> Result<Page, GithubError> {
        let pull_request = self
            .graphql_keeping_partial::<Timeline>(
                Variables {
                    owner: pull.repo.owner.clone(),
                    name: pull.repo.name.clone(),
                    number: pull.number,
                    last: page_size,
                    before,
                },
                2,
            )
            .await?
            .repository
            .and_then(|repository| repository.pull_request)
            .ok_or_else(|| not_found(format!("pull request {pull}")))?;
        let items = pull_request.timeline_items;
        Ok(Page {
            description: Description {
                author: pull_request.author.map(|author| author.login),
                body: pull_request.body,
                created_at: pull_request.created_at,
                last_edited_at: pull_request.last_edited_at,
            },
            entries: (items.edges.into_iter().flatten().flatten())
                .map(|edge| TimelineEntry {
                    cursor: edge.cursor,
                    item: edge
                        .node
                        .map_or(TimelineItem::Other { typename: None }, item),
                })
                .collect(),
            count: usize::try_from(items.filtered_count).unwrap_or_default(),
            shown_updated_at: Some(pull_request.timeline_events.updated_at),
            has_previous_page: items.page_info.has_previous_page,
            start_cursor: items.page_info.start_cursor,
        })
    }
}

/// Says when GitHub counts more or fewer items than it listed, for a timeline read to its oldest
/// item. Joins still work, since they compare how far the counts move.
fn counted(pull: &PullRef, count: usize, listed: usize) {
    if count != listed {
        log::info!("GitHub counts {count} timeline items of {pull}, and listed {listed}");
    }
}

/// [`GithubClient::timeline`] as [`crate::poll::Poller::read_cached`] runs it.
pub async fn read_timeline(
    client: GithubClient,
    pull: PullRef,
    known: Option<Arc<PullRequestTimeline>>,
    now: SystemTime,
) -> Result<PullRequestTimeline, GithubError> {
    client.timeline(&pull, known.as_deref(), now).await
}

/// The name GitHub gives a value of one of its enums, as the generated type serializes it.
fn github_name<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(name)) => name,
        _ => String::new(),
    }
}

type Node = TimelineRepositoryPullRequestTimelineItemsEdgesNode;

/// The item `node` is.
fn item(node: Node) -> TimelineItem {
    // Every event names who did it and when the same way.
    macro_rules! event {
        ($event:expr) => {
            Event {
                actor: $event.actor.map(|actor| actor.login),
                at: $event.created_at,
            }
        };
    }
    // An assignee by login.
    macro_rules! assignee {
        ($value:expr, $kind:ident) => {
            match $value {
                Some($kind::User(account)) => Some(account.login),
                Some($kind::Bot(account)) => Some(account.login),
                Some($kind::Mannequin(account)) => Some(account.login),
                Some($kind::Organization(account)) => Some(account.login),
                Some($kind::Unknown) | None => None,
            }
        };
    }
    // Who was asked for a review.
    macro_rules! reviewer {
        ($value:expr, $kind:ident) => {
            match $value {
                Some($kind::User(account)) => Some(Reviewer::Account(account.login)),
                Some($kind::Bot(account)) => Some(Reviewer::Account(account.login)),
                Some($kind::Mannequin(account)) => Some(Reviewer::Account(account.login)),
                Some($kind::Team(team)) => Some(Reviewer::Team(team.combined_slug)),
                Some($kind::EnterpriseTeam(team)) => Some(Reviewer::Team(team.slug)),
                Some($kind::Unknown) | None => None,
            }
        };
    }
    match node {
        Node::IssueComment(comment) => TimelineItem::Comment(Comment {
            id: comment.id,
            author: comment.author.map(|author| author.login),
            body: comment.body,
            created_at: comment.created_at,
            last_edited_at: comment.last_edited_at,
            url: comment.url,
            hidden: (comment.is_minimized).then(|| comment.minimized_reason.unwrap_or_default()),
            viewer_can_update: comment.viewer_can_update,
            viewer_can_delete: comment.viewer_can_delete,
        }),
        Node::PullRequestReview(review) => TimelineItem::Review(Review {
            id: review.id,
            author: review.author.map(|author| author.login),
            body: review.body,
            state: github_name(&review.state),
            created_at: review.created_at,
            submitted_at: review.submitted_at,
            last_edited_at: review.last_edited_at,
            url: review.url,
            hidden: review.is_minimized,
            commit: review.commit.map(|commit| commit.oid),
            comments: usize::try_from(review.comments.total_count).unwrap_or_default(),
        }),
        Node::PullRequestCommit(commit) => {
            let commit = commit.git_commit;
            let author = commit.author;
            TimelineItem::Commit(Commit {
                oid: commit.oid,
                headline: commit.message_headline,
                author_login: (author.as_ref())
                    .and_then(|author| author.user.as_ref())
                    .map(|user| user.login.clone()),
                author_name: author.and_then(|author| author.name),
                committed_at: commit.committed_date,
                checks: (commit.status_check_rollup).map(|rollup| github_name(&rollup.state)),
            })
        }
        Node::HeadRefForcePushedEvent(push) => TimelineItem::ForcePushed {
            event: event!(push),
            branch: Branch::Head,
            before: push.before_commit.map(|commit| commit.oid),
            after: push.after_commit.map(|commit| commit.oid),
        },
        Node::BaseRefForcePushedEvent(push) => TimelineItem::ForcePushed {
            event: event!(push),
            branch: Branch::Base,
            before: push.before_commit.map(|commit| commit.oid),
            after: push.after_commit.map(|commit| commit.oid),
        },
        Node::LabeledEvent(labeled) => TimelineItem::Label {
            event: event!(labeled),
            name: labeled.label.name,
            color: labeled.label.color,
            added: true,
        },
        Node::UnlabeledEvent(unlabeled) => TimelineItem::Label {
            event: event!(unlabeled),
            name: unlabeled.label.name,
            color: unlabeled.label.color,
            added: false,
        },
        Node::AssignedEvent(assigned) => TimelineItem::Assignment {
            assignee: assignee!(
                assigned.assignee,
                TimelineRepositoryPullRequestTimelineItemsEdgesNodeOnAssignedEventAssignee
            ),
            event: event!(assigned),
            added: true,
        },
        Node::UnassignedEvent(unassigned) => TimelineItem::Assignment {
            assignee: assignee!(
                unassigned.assignee,
                TimelineRepositoryPullRequestTimelineItemsEdgesNodeOnUnassignedEventAssignee
            ),
            event: event!(unassigned),
            added: false,
        },
        Node::ReviewRequestedEvent(requested) => TimelineItem::ReviewRequest {
            reviewer: reviewer!(
                requested.requested_reviewer,
                TimelineRepositoryPullRequestTimelineItemsEdgesNodeOnReviewRequestedEventRequestedReviewer
            ),
            event: event!(requested),
            added: true,
        },
        Node::ReviewRequestRemovedEvent(removed) => TimelineItem::ReviewRequest {
            reviewer: reviewer!(
                removed.requested_reviewer,
                TimelineRepositoryPullRequestTimelineItemsEdgesNodeOnReviewRequestRemovedEventRequestedReviewer
            ),
            event: event!(removed),
            added: false,
        },
        Node::CrossReferencedEvent(reference) => {
            use TimelineRepositoryPullRequestTimelineItemsEdgesNodeOnCrossReferencedEventSource as Source;
            let source = match reference.source {
                Source::Issue(issue) => Some(Reference {
                    kind: ReferenceKind::Issue,
                    repository: issue.repository.name_with_owner,
                    number: issue.number,
                    title: issue.title,
                    url: issue.url,
                }),
                Source::PullRequest(pull) => Some(Reference {
                    kind: ReferenceKind::PullRequest,
                    repository: pull.repository.name_with_owner,
                    number: pull.number,
                    title: pull.title,
                    url: pull.url,
                }),
                Source::Unknown => None,
            };
            TimelineItem::CrossReference {
                event: event!(reference),
                source,
                will_close: reference.will_close_target,
            }
        }
        Node::MergedEvent(merged) => TimelineItem::Merged {
            event: event!(merged),
            commit: merged.commit.map(|commit| commit.oid),
            into: merged.merge_ref_name,
        },
        Node::AddedToMergeQueueEvent(added) => TimelineItem::MergeQueue {
            event: event!(added),
            added: true,
            reason: None,
        },
        Node::RemovedFromMergeQueueEvent(removed) => TimelineItem::MergeQueue {
            event: event!(removed),
            added: false,
            reason: removed.reason,
        },
        Node::DeployedEvent(deployed) => TimelineItem::Deployed {
            event: event!(deployed),
            environment: deployed.deployment.environment,
            state: (deployed.deployment.latest_status).map(|status| github_name(&status.state)),
        },
        Node::DeploymentEnvironmentChangedEvent(changed) => TimelineItem::DeploymentChanged {
            event: event!(changed),
            environment: changed.deployment_status.deployment.environment,
            state: github_name(&changed.deployment_status.state),
        },
        Node::ConvertToDraftEvent(draft) => TimelineItem::Draft {
            event: event!(draft),
            draft: true,
        },
        Node::ReadyForReviewEvent(ready) => TimelineItem::Draft {
            event: event!(ready),
            draft: false,
        },
        Node::ClosedEvent(closed) => TimelineItem::Closed(event!(closed)),
        Node::ReopenedEvent(reopened) => TimelineItem::Reopened(event!(reopened)),
        Node::BaseRefChangedEvent(changed) => TimelineItem::BaseChanged {
            event: event!(changed),
            from: changed.previous_ref_name,
            to: changed.current_ref_name,
            how: BaseChange::Changed,
        },
        Node::AutomaticBaseChangeSucceededEvent(changed) => TimelineItem::BaseChanged {
            event: event!(changed),
            from: changed.old_base,
            to: changed.new_base,
            how: BaseChange::Automatic,
        },
        Node::AutomaticBaseChangeFailedEvent(failed) => TimelineItem::BaseChanged {
            event: event!(failed),
            from: failed.old_base,
            to: failed.new_base,
            how: BaseChange::AutomaticFailed,
        },
        Node::ReviewDismissedEvent(dismissed) => TimelineItem::ReviewDismissed {
            review: (dismissed.review.as_ref()).map(|review| review.id.clone()),
            reviewer: (dismissed.review.as_ref())
                .and_then(|review| review.author.as_ref())
                .map(|author| author.login.clone()),
            message: dismissed.dismissal_message.clone(),
            previous_state: github_name(&dismissed.previous_review_state),
            event: event!(dismissed),
        },
        Node::AutoMergeEnabledEvent(enabled) => TimelineItem::AutoMerge {
            event: event!(enabled),
            change: AutoMerge::EnabledMerge,
        },
        Node::AutoRebaseEnabledEvent(enabled) => TimelineItem::AutoMerge {
            event: event!(enabled),
            change: AutoMerge::EnabledRebase,
        },
        Node::AutoSquashEnabledEvent(enabled) => TimelineItem::AutoMerge {
            event: event!(enabled),
            change: AutoMerge::EnabledSquash,
        },
        Node::AutoMergeDisabledEvent(disabled) => TimelineItem::AutoMerge {
            change: AutoMerge::Disabled {
                reason: disabled.reason.clone(),
            },
            event: event!(disabled),
        },
        Node::AddedToStackEvent(added) => TimelineItem::Stack {
            event: event!(added),
            number: added.stack_number,
            added: true,
        },
        Node::RemovedFromStackEvent(removed) => TimelineItem::Stack {
            event: event!(removed),
            number: removed.stack_number,
            added: false,
        },
        Node::Unknown => TimelineItem::Other { typename: None },
        other => TimelineItem::Other {
            typename: (serde_json::to_value(&other).ok())
                .and_then(|value| value["__typename"].as_str().map(String::from)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two of cli/cli#13057's cursors that share a time (commits a rebase pushed), and one later.
    const SAME_TIME: [&str; 2] = [
        "Y3Vyc29yOnYyOpPPAAABnd7d-1gH2gAzMzQ2MzU4MjYzMDpjMDBiYzZhOWI0OTk5ZTRmZDM1ZTE0NTdiMzdiZTIwNzg4YjllOTRj",
        "Y3Vyc29yOnYyOpPPAAABnd7d-1gH2gAzMzQ2MzU4MjYzMDozZjFkZjUxM2M1MzBjMDMwNWZiMTBjZmMzZmIyYTIxNTMwY2ZlYTk2",
    ];
    const LATER: &str = "Y3Vyc29yOnYyOpPPAAABnd7eAygBqzI1MDM5MDQ0OTIx";

    #[test]
    fn a_cursor_holds_its_items_time() {
        let [a, b] = SAME_TIME.map(cursor_time);
        assert!(a.is_some());
        assert_eq!(a, b);
        assert!(cursor_time(LATER).is_some());
        assert_ne!(cursor_time(LATER), a);
        assert!(base64_decode(LATER).unwrap().ends_with(b"25039044921"));
        // sharkdp/fd#2212's, padded.
        assert!(cursor_time("Y3Vyc29yOnYyOpPPAAABoaDfW7gAqjY2MDA3MzI5MjM=").is_some());
        for unknown in ["", "not base64!", "Y3Vyc29yOnYxOg==", "Y3Vyc29yOnYyOg=="] {
            assert_eq!(cursor_time(unknown), None, "{unknown}");
        }
        assert_eq!(base64_decode("Y3Vyc29yOnYyOg==").unwrap(), b"cursor:v2:");
        assert_eq!(base64_decode("_-8").unwrap(), [0xff, 0xef]);
    }

    fn other() -> TimelineItem {
        TimelineItem::Other {
            typename: Some("SubscribedEvent".into()),
        }
    }

    fn at(minute: i64) -> chrono::DateTime<Utc> {
        chrono::DateTime::UNIX_EPOCH + chrono::Duration::minutes(minute)
    }

    fn event() -> Event {
        Event {
            actor: Some("alice".into()),
            at: at(0),
        }
    }

    fn comment(body: &str) -> TimelineItem {
        TimelineItem::Comment(Comment {
            id: "IC_1".into(),
            author: Some("alice".into()),
            body: body.into(),
            created_at: at(0),
            last_edited_at: None,
            url: String::new(),
            hidden: None,
            viewer_can_update: true,
            viewer_can_delete: true,
        })
    }

    fn review(id: &str) -> TimelineItem {
        TimelineItem::Review(Review {
            id: id.into(),
            author: Some("bob".into()),
            body: String::new(),
            state: "APPROVED".into(),
            created_at: at(0),
            submitted_at: Some(at(0)),
            last_edited_at: None,
            url: String::new(),
            hidden: false,
            commit: None,
            comments: 0,
        })
    }

    fn commit(oid: &str) -> TimelineItem {
        TimelineItem::Commit(Commit {
            oid: oid.into(),
            headline: String::new(),
            author_login: None,
            author_name: None,
            committed_at: at(0),
            checks: None,
        })
    }

    fn dismissed(review: &str) -> TimelineItem {
        TimelineItem::ReviewDismissed {
            event: event(),
            review: Some(review.into()),
            reviewer: Some("bob".into()),
            message: None,
            previous_state: "APPROVED".into(),
        }
    }

    fn force_pushed(branch: Branch, after: &str) -> TimelineItem {
        TimelineItem::ForcePushed {
            event: event(),
            branch,
            before: None,
            after: Some(after.into()),
        }
    }

    fn entries(items: &[(&str, TimelineItem)]) -> Vec<TimelineEntry> {
        (items.iter())
            .map(|(cursor, item)| TimelineEntry {
                cursor: cursor.to_string(),
                item: item.clone(),
            })
            .collect()
    }

    fn plain<'a>(cursors: &[&'a str]) -> Vec<(&'a str, TimelineItem)> {
        cursors.iter().map(|cursor| (*cursor, other())).collect()
    }

    fn description() -> Description {
        Description {
            author: Some("alice".into()),
            body: String::new(),
            created_at: chrono::DateTime::UNIX_EPOCH,
            last_edited_at: None,
        }
    }

    /// A newest page holding `items`, read when GitHub counted `count` items and the items the
    /// timeline shows last changed at minute `changed`.
    fn page(items: &[(&str, TimelineItem)], count: usize, changed: i64) -> Page {
        Page {
            description: description(),
            entries: entries(items),
            count,
            shown_updated_at: Some(at(changed)),
            has_previous_page: true,
            start_cursor: items.first().map(|(cursor, _)| cursor.to_string()),
        }
    }

    /// A timeline holding `items` with `left_out` older ones, read when GitHub counted `count`
    /// items and the items it shows last changed at minute 0.
    fn timeline(
        items: &[(&str, TimelineItem)],
        left_out: usize,
        count: usize,
    ) -> PullRequestTimeline {
        PullRequestTimeline {
            description: description(),
            items: entries(items),
            left_out,
            count,
            shown_updated_at: Some(at(0)),
            read_in_full_at: chrono::DateTime::UNIX_EPOCH,
        }
    }

    fn cursors(timeline: &PullRequestTimeline) -> Vec<&str> {
        (timeline.items.iter())
            .map(|entry| entry.cursor.as_str())
            .collect()
    }

    #[test]
    fn a_newest_page_joins_the_items_before_it() {
        let known = timeline(&plain(&["a", "b", "c", "d"]), 1, 5);
        // One item added after d.
        let newest = page(&plain(&["c", "d", "e"]), 6, 1);
        let joined = known.joined(&newest, 100).unwrap();
        assert_eq!(cursors(&joined), ["a", "b", "c", "d", "e"]);
        assert_eq!((joined.left_out, joined.count), (1, 6));
        assert_eq!(joined.shown_updated_at, Some(at(1)));
        // Past the most it keeps, the oldest are left out.
        let joined = known.joined(&newest, 4).unwrap();
        assert_eq!(cursors(&joined), ["b", "c", "d", "e"]);
        assert_eq!(joined.left_out, 2);
        // An item on the page edited: the page holds it as it is now.
        let mut items = plain(&["a", "b", "c", "d"]);
        items[2].1 = comment("Before");
        let known = timeline(&items, 0, 4);
        let edited = [("c", comment("After")), ("d", other())];
        let joined = known.joined(&page(&edited, 4, 1), 100).unwrap();
        assert_eq!(joined.items[2].item, comment("After"));
    }

    #[test]
    fn a_newest_page_that_doesnt_add_up_is_read_in_full() {
        let known = timeline(&plain(&["a", "b", "c", "d"]), 0, 4);
        // An older item deleted while one was added, or one added before the page.
        assert!(
            known
                .joined(&page(&plain(&["c", "d", "e"]), 4, 1), 100)
                .is_none()
        );
        assert!(
            known
                .joined(&page(&plain(&["c", "d", "e"]), 6, 1), 100)
                .is_none()
        );
        // More items were added than a page holds.
        assert!(
            known
                .joined(&page(&plain(&["x", "y", "z"]), 7, 1), 100)
                .is_none()
        );
    }

    #[test]
    fn a_change_to_the_shown_items_the_page_doesnt_hold_is_read_in_full() {
        let mut items = plain(&["a", "b", "c", "d"]);
        items[0].1 = comment("Old");
        let known = timeline(&items, 0, 4);
        // The newest item's time moved, but the page is as it was: an item came and an older one
        // went.
        assert!(
            known
                .joined(&page(&plain(&["c", "d"]), 4, 1), 100)
                .is_none()
        );
        // Nothing the timeline shows changed, such as a mention.
        assert!(
            known
                .joined(&page(&plain(&["c", "d"]), 4, 0), 100)
                .is_some()
        );
    }

    #[test]
    fn a_new_dismissal_or_force_push_of_what_the_page_doesnt_hold_is_read_in_full() {
        let mut items = plain(&["a", "b", "c"]);
        items[0].1 = review("PRR_old");
        items[1].1 = commit("c1");
        let known = timeline(&items, 0, 3);

        // The approval it dismisses is older than the page.
        let late = [("c", other()), ("d", dismissed("PRR_old"))];
        assert!(known.joined(&page(&late, 4, 1), 100).is_none());
        // A force-push whose commits were committed before the page, as a rebase keeping its
        // committer dates leaves them; and one of the base branch.
        let pushed = [("c", other()), ("d", force_pushed(Branch::Head, "c9"))];
        assert!(known.joined(&page(&pushed, 4, 1), 100).is_none());
        let base = [("c", other()), ("d", force_pushed(Branch::Base, "b9"))];
        assert!(known.joined(&page(&base, 4, 1), 100).is_none());

        // The page holds what they change.
        let held = [
            ("c", other()),
            ("d", review("PRR_new")),
            ("e", dismissed("PRR_new")),
            ("f", commit("c9")),
            ("g", force_pushed(Branch::Head, "c9")),
        ];
        assert!(known.joined(&page(&held, 7, 1), 100).is_some());
        // A dismissal it has already joined changes nothing more.
        let joined = timeline(&late, 2, 4);
        assert!(joined.joined(&page(&late, 4, 0), 100).is_some());
    }

    #[test]
    fn a_page_is_read_before_its_first_item_with_a_later_time() {
        let items = [
            (SAME_TIME[0], other()),
            (SAME_TIME[1], other()),
            (LATER, other()),
        ];
        let mut tied = page(&items, 3, 0);
        assert_eq!(tied.before().as_deref(), Some(LATER));
        // All of one time, or cursors Lo can't read: before its start.
        tied.entries.pop();
        assert_eq!(tied.before().as_deref(), Some(SAME_TIME[0]));
        assert_eq!(
            page(&plain(&["x", "y"]), 2, 0).before().as_deref(),
            Some("x")
        );
        // The first page has none before it.
        tied.has_previous_page = false;
        assert_eq!(tied.before(), None);
    }

    #[test]
    fn a_timeline_is_read_in_full_hourly() {
        let known = timeline(&plain(&["a"]), 0, 1);
        let read = SystemTime::from(known.read_in_full_at);
        assert!(!known.due_in_full(read + READ_IN_FULL_EVERY - Duration::from_secs(1)));
        assert!(known.due_in_full(read + READ_IN_FULL_EVERY));
        // A clock that went back.
        assert!(known.due_in_full(read - Duration::from_secs(1)));
    }
}
