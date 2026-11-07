//! The pull request a tab shows: everything the tab knows about it, in one place, so a tab can
//! swap one pull request's state for another's, as a stack's tab does for each layer (design §4).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;

use github::PullRef;
use github::poll::Poller;
use github::viewed::ViewedRecords;
use gpui::Entity;
use project::WorktreeId;
use review::revisions::Revisions;

use super::Header;
use crate::account::Acting;
use crate::file_list::FileList;
use crate::poll_state::PollState;
use crate::pr_diff::LocalPullRequest;
use crate::pr_link::PrLink;
use crate::tabs::TabKey;
use crate::viewed_marks::ViewedMarks;

/// One pull request's state in a tab: what GitHub said about it, its files and viewed marks, its
/// revisions, how it's polled, and its local copy. A tab shows one at a time
/// ([`super::PrTab::show_pull`]).
pub struct ShownPull {
    /// The pull request's number.
    pub(super) number: u64,
    pub(super) key: TabKey,
    /// The account the tab acts as for it, once chosen.
    pub(super) acting: Option<Acting>,
    pub(super) header: Header,
    /// When it was opened, as GitHub says.
    pub(super) created_at: Option<SystemTime>,
    pub(super) files: FileList,
    /// GitHub's viewed marks, and the ones being set, kept for when the files arrive.
    pub(super) marks: ViewedMarks,
    pub(super) revisions: Option<Revisions>,
    /// Its GraphQL node ID, which writes name it by.
    pub(super) node_id: Option<String>,
    /// The head on screen: GitHub's at first, then the one the local copy fetched.
    pub(super) head: Option<String>,
    /// When Lo learned the head on screen. What GitHub said about its head before then is
    /// older news.
    pub(super) head_at: Option<SystemTime>,
    /// GitHub's latest head for it, from its details or its summary, and when GitHub gave it.
    /// `e` marks files only while it's the head on screen.
    pub(super) latest_head: Option<(String, SystemTime)>,
    /// Where this Mac's records say each file was marked viewed, by path.
    pub(super) viewed_at: HashMap<String, String>,
    /// This Mac's records of where files were marked viewed, once signed in.
    pub(super) records: Option<ViewedRecords>,
    /// The account's poller and the pull request it watches for the tab, until the tab closes.
    pub(super) watched: Option<(Entity<Poller>, PullRef)>,
    /// How the poller is doing, for the header.
    pub(super) poll: PollState,
    /// Its local copy, once it's copied.
    pub(super) local: Option<Arc<LocalPullRequest>>,
    /// The trusted checkout's worktree, which goes with the last tab that uses it.
    pub(super) checkout: Option<WorktreeId>,
    /// The head GitHub's REST API gave for it, which the local copy fetches.
    pub(super) api_head: Option<String>,
    /// The head GitHub's diff was listed at, once it shows and if GitHub's links say.
    pub(super) github_head: Option<String>,
}

impl ShownPull {
    /// The pull request `link` points at, before GitHub has said anything about it.
    pub fn new(link: PrLink) -> Self {
        Self {
            number: link.number,
            key: TabKey::new(link),
            acting: None,
            header: Header::default(),
            created_at: None,
            files: FileList::default(),
            marks: ViewedMarks::default(),
            revisions: None,
            node_id: None,
            head: None,
            head_at: None,
            latest_head: None,
            viewed_at: HashMap::new(),
            records: None,
            watched: None,
            poll: PollState::default(),
            local: None,
            checkout: None,
            api_head: None,
            github_head: None,
        }
    }

    pub fn number(&self) -> u64 {
        self.number
    }

    pub fn key(&self) -> &TabKey {
        &self.key
    }

    /// GitHub's latest head for the pull request, once GitHub said.
    pub fn latest_head(&self) -> Option<&str> {
        self.latest_head.as_ref().map(|(head, _)| head.as_str())
    }

    /// The head the diff shows, once one does.
    pub fn head(&self) -> Option<&str> {
        self.head.as_deref()
    }
}
