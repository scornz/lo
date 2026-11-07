//! Lo's GitHub layer as its tabs use it: the signed-in accounts, the cache of GitHub's answers,
//! and one poller per account.

use std::collections::HashMap;
use std::time::SystemTime;

use futures::FutureExt as _;
use futures::future::Shared;
use github::accounts::{Accounts, RepoAccount};
use github::cache::{PrCache, RECENT_PRS, RecentPr};
use github::db::LoDb;
use github::poll::{PollConfig, Poller, RequestQueue};
use github::queries::PullRequestData;
use github::viewed::ViewedRecords;
use github::{PullRef, Target};
use gpui::{App, AppContext as _, Context, Entity, Global, Task};

/// A pull request's details as Lo saved them.
#[derive(Clone, Debug)]
pub struct Saved {
    /// The account they were fetched with, whose viewed marks they hold.
    pub login: String,
    pub details: PullRequestData,
    /// When GitHub gave them.
    pub fetched_at: SystemTime,
}

/// The accounts, the cache and the pollers, once Lo's database is open.
pub struct GithubLayer {
    accounts: Entity<Accounts>,
    cache: PrCache,
    /// Where each account marked files viewed on this Mac.
    viewed: ViewedRecords,
    /// One per account that has a tab, by lowercased login.
    pollers: HashMap<String, Entity<Poller>>,
    /// Whether Lo is in the foreground, for pollers yet to start.
    foreground: bool,
}

/// The layer once the accounts database opened, or `None` when it couldn't.
pub type Ready = Shared<Task<Option<Entity<GithubLayer>>>>;

struct GlobalLayer(Ready);

impl Global for GlobalLayer {}

/// Sets the layer up from `accounts`, which resolves to the loaded accounts and their database, or
/// to `None` when the database couldn't be opened.
pub fn init(accounts: Task<Option<(Entity<Accounts>, LoDb)>>, cx: &mut App) {
    let ready = cx
        .spawn(async move |cx| {
            let (accounts, db) = accounts.await?;
            Some(cx.new(|_| {
                GithubLayer::new(accounts, PrCache::new(db.clone()), ViewedRecords::new(db))
            }))
        })
        .shared();
    cx.set_global(GlobalLayer(ready));
}

/// The layer, once [`init`]'s accounts are loaded. Without [`init`], as in tests, there's none,
/// and pull requests open without signing in.
pub fn ready(cx: &App) -> Ready {
    match cx.try_global::<GlobalLayer>() {
        Some(global) => global.0.clone(),
        None => Task::ready(None).shared(),
    }
}

/// The layer, if it's loaded.
pub fn loaded(cx: &App) -> Option<Entity<GithubLayer>> {
    ready(cx).peek().cloned().flatten()
}

impl GithubLayer {
    pub fn new(accounts: Entity<Accounts>, cache: PrCache, viewed: ViewedRecords) -> Self {
        Self {
            accounts,
            cache,
            viewed,
            pollers: HashMap::new(),
            foreground: true,
        }
    }

    pub fn accounts(&self) -> &Entity<Accounts> {
        &self.accounts
    }

    /// Where each account marked files viewed on this Mac.
    pub fn viewed_records(&self) -> &ViewedRecords {
        &self.viewed
    }

    /// The account's poller, started the first time a tab asks for it.
    pub fn poller(&mut self, login: &str, cx: &mut Context<Self>) -> Entity<Poller> {
        self.pollers
            .entry(login.to_ascii_lowercase())
            .or_insert_with(|| {
                let (accounts, cache) = (self.accounts.clone(), self.cache.clone());
                let foreground = self.foreground;
                cx.new(|cx| {
                    let mut poller = Poller::new(accounts, login, cache, PollConfig::default(), cx);
                    poller.set_foreground(foreground, cx);
                    poller
                })
            })
            .clone()
    }

    /// The pull requests opened recently with any account, newest first. It's read from Lo's
    /// database, so it's there at once, online or not.
    pub fn recent(&self) -> Vec<RecentPr> {
        self.cache
            .recent(RECENT_PRS)
            .inspect_err(|error| log::warn!("reading recent pull requests: {error:#}"))
            .unwrap_or_default()
    }

    /// Forgets that `pull` was opened under a name its repository no longer has, so it's listed
    /// once, by its name now.
    pub fn forget_recent(&self, pull: PullRef, cx: &App) {
        let cache = self.cache.clone();
        cx.background_spawn(async move {
            if let Err(error) = cache.forget_recent(&pull).await {
                log::warn!("forgetting {pull} as opened: {error:#}");
            }
        })
        .detach();
    }

    /// What Lo saved about `pull` most recently, whichever account fetched it, so the newest
    /// head shows. The viewed marks in it are that account's. It's read from Lo's database,
    /// so it's there at once, online or not.
    pub fn saved_details(&self, pull: &PullRef, cx: &App) -> Option<Saved> {
        let target = Target::Pull(pull.clone());
        self.accounts
            .read(cx)
            .accounts()
            .iter()
            .filter_map(|account| {
                match self.cache.get::<PullRequestData>(&account.login, &target) {
                    Ok(saved) => saved.map(|saved| Saved {
                        login: account.login.clone(),
                        details: saved.value,
                        fetched_at: saved.fetched_at,
                    }),
                    Err(error) => {
                        log::warn!("reading what Lo saved about {pull}: {error:#}");
                        None
                    }
                }
            })
            .max_by_key(|saved| saved.fetched_at)
    }

    /// Which account to open `owner/repo` with ([`Accounts::account_for_repo`]). Each account's
    /// check waits its turn in that account's queue, behind its polls.
    pub fn account_for_repo(
        &mut self,
        owner: &str,
        repo: &str,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<RepoAccount>> {
        let logins: Vec<String> = self
            .accounts
            .read(cx)
            .accounts()
            .iter()
            .map(|account| account.login.clone())
            .collect();
        let queues: HashMap<String, RequestQueue> = logins
            .iter()
            .map(|login| {
                let queue = self.poller(login, cx).read(cx).queue().clone();
                (login.to_ascii_lowercase(), queue)
            })
            .collect();
        self.accounts.update(cx, |accounts, cx| {
            accounts.account_for_repo(
                owner,
                repo,
                move |login| queues.get(&login.to_ascii_lowercase()).cloned(),
                cx,
            )
        })
    }

    /// Tells every poller whether Lo is in the foreground. In the background they all slow to
    /// every 5 minutes.
    pub fn set_foreground(&mut self, foreground: bool, cx: &mut Context<Self>) {
        self.foreground = foreground;
        for poller in self.pollers.values() {
            poller.update(cx, |poller, cx| poller.set_foreground(foreground, cx));
        }
    }
}
