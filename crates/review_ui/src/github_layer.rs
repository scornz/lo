//! rozed's GitHub layer as its tabs use it: the signed-in accounts, the cache of GitHub's answers,
//! and one poller per account.

use std::collections::HashMap;

use futures::FutureExt as _;
use futures::future::Shared;
use github::accounts::Accounts;
use github::cache::PrCache;
use github::db::RozedDb;
use github::poll::{PollConfig, Poller};
use github::viewed::ViewedRecords;
use gpui::{App, AppContext as _, Context, Entity, Global, Task};

/// The accounts, the cache and the pollers, once rozed's database is open.
pub struct GithubLayer {
    accounts: Entity<Accounts>,
    cache: PrCache,
    /// Where each account marked files viewed on this Mac.
    viewed: ViewedRecords,
    /// One per account that has a tab, by lowercased login.
    pollers: HashMap<String, Entity<Poller>>,
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
                cx.new(|cx| Poller::new(accounts, login, cache, PollConfig::default(), cx))
            })
            .clone()
    }

    /// Tells every poller whether rozed is in the foreground. In the background they all slow to
    /// every 5 minutes.
    pub fn set_foreground(&self, foreground: bool, cx: &mut Context<Self>) {
        for poller in self.pollers.values() {
            poller.update(cx, |poller, cx| poller.set_foreground(foreground, cx));
        }
    }
}
