//! rozed's GitHub layer as its tabs use it: the signed-in accounts, the cache of GitHub's answers,
//! and one poller per account.

use std::collections::HashMap;
use std::time::SystemTime;

use futures::FutureExt as _;
use futures::future::Shared;
use github::accounts::{Accounts, RepoAccount};
use github::cache::{PrCache, RECENT_PRS, RecentPr};
use github::db::RozedDb;
use github::poll::{PollConfig, Poller, RequestQueue};
use github::queries::PullRequestData;
use github::{PullRef, Target};
use gpui::{App, AppContext as _, Context, Entity, Global, Task};

/// A pull request's details as rozed saved them.
#[derive(Clone, Debug)]
pub struct Saved {
    /// The account they were fetched with, whose viewed marks they hold.
    pub login: String,
    pub details: PullRequestData,
    /// When GitHub gave them.
    pub fetched_at: SystemTime,
}

/// The accounts, the cache and the pollers, once rozed's database is open.
pub struct GithubLayer {
    accounts: Entity<Accounts>,
    cache: PrCache,
    /// One per account that has a tab, by lowercased login.
    pollers: HashMap<String, Entity<Poller>>,
    /// Whether rozed is in the foreground, for pollers yet to start.
    foreground: bool,
}

/// The layer once the accounts database opened, or `None` when it couldn't.
pub type Ready = Shared<Task<Option<Entity<GithubLayer>>>>;

struct GlobalLayer(Ready);

impl Global for GlobalLayer {}

/// Sets the layer up from `accounts`, which resolves to the loaded accounts and their database, or
/// to `None` when the database couldn't be opened.
pub fn init(accounts: Task<Option<(Entity<Accounts>, RozedDb)>>, cx: &mut App) {
    let ready = cx
        .spawn(async move |cx| {
            let (accounts, db) = accounts.await?;
            Some(cx.new(|_| GithubLayer::new(accounts, PrCache::new(db))))
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
    pub fn new(accounts: Entity<Accounts>, cache: PrCache) -> Self {
        Self {
            accounts,
            cache,
            pollers: HashMap::new(),
            foreground: true,
        }
    }

    pub fn accounts(&self) -> &Entity<Accounts> {
        &self.accounts
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

    /// The pull requests opened recently with any account, newest first. It's read from rozed's
    /// database, so it's there at once, online or not.
    pub fn recent(&self) -> Vec<RecentPr> {
        self.cache
            .recent(RECENT_PRS)
            .inspect_err(|error| log::warn!("reading recent pull requests: {error:#}"))
            .unwrap_or_default()
    }

    /// What rozed saved about `pull` the last time an account fetched it: the account remembered
    /// for its owner's, else the account it was last opened with's, else any account's that has
    /// it. It's read from rozed's database, so it's there at once, online or not.
    pub fn saved_details(&self, pull: &PullRef, cx: &App) -> Option<Saved> {
        let accounts = self.accounts.read(cx);
        let remembered = accounts
            .remembered_account(&pull.repo.owner)
            .inspect_err(|error| log::warn!("reading the account for {}: {error:#}", pull.repo))
            .ok()
            .flatten();
        let last_opened_with = self
            .recent()
            .into_iter()
            .find(|recent| &recent.pull == pull)
            .map(|recent| recent.account);
        let mut logins: Vec<String> = remembered.into_iter().chain(last_opened_with).collect();
        logins.extend(
            accounts
                .accounts()
                .iter()
                .map(|account| account.login.clone()),
        );
        let target = Target::Pull(pull.clone());
        let mut tried = Vec::new();
        for login in logins {
            let key = login.to_ascii_lowercase();
            if tried.contains(&key) {
                continue;
            }
            tried.push(key);
            match self.cache.get::<PullRequestData>(&login, &target) {
                Ok(Some(saved)) => {
                    return Some(Saved {
                        login,
                        details: saved.value,
                        fetched_at: saved.fetched_at,
                    });
                }
                Ok(None) => {}
                Err(error) => log::warn!("reading what rozed saved about {pull}: {error:#}"),
            }
        }
        None
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

    /// Tells every poller whether rozed is in the foreground. In the background they all slow to
    /// every 5 minutes.
    pub fn set_foreground(&mut self, foreground: bool, cx: &mut Context<Self>) {
        self.foreground = foreground;
        for poller in self.pollers.values() {
            poller.update(cx, |poller, cx| poller.set_foreground(foreground, cx));
        }
    }
}
