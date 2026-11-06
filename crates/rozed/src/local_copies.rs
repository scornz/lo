//! rozed's local copies of repositories (`repo_cache`), set up once at launch.
//!
//! At launch rozed finds git and checks it's new enough, then removes clones nobody used for 30
//! days and checkouts nobody used for 14 (design §3, "Cleanup"), on a background thread. Opening
//! a pull request waits for that and gets the same cache, or the same error, which says how to
//! install git. Then clones with too many packs are repacked, one at a time.

use std::path::PathBuf;
use std::sync::Arc;

use futures::FutureExt as _;
use futures::future::Shared;
use gpui::{App, Global, Task};
use repo_cache::{Git, RepoCache, Token};

/// The cache, once git has been checked.
type Ready = Result<Arc<RepoCache>, Arc<repo_cache::Error>>;

struct LocalCopies {
    ready: Shared<Task<Ready>>,
    token: Option<Token>,
}

impl Global for LocalCopies {}

/// Where the clones and checkouts live: `~/Library/Caches/rozed`. Zed's `paths` crate calls
/// rozed's cache folder its "temp dir".
pub fn cache_dir() -> PathBuf {
    paths::temp_dir().clone()
}

/// Checks git and cleans up the cache in the background. `token` signs git and GitHub's API in
/// for every repository; milestone 3's accounts replace it.
pub fn init(token: Option<Token>, cx: &mut App) {
    let ready = cx
        .background_executor()
        .spawn(async move {
            let git = Git::find().map_err(Arc::new)?;
            let cache = RepoCache::new(cache_dir(), git).map_err(Arc::new)?;
            match cache.remove_unused(RepoCache::UNUSED_FOR) {
                Ok(removed) if !removed.is_empty() => {
                    log::info!("removed clones unused for 30 days: {}", removed.join(", "));
                }
                Ok(_) => {}
                Err(error) => log::warn!("can't clean up {}: {error}", cache_dir().display()),
            }

            Ok(Arc::new(cache))
        })
        .shared();
    // Repacking can take a while for a big clone, so opening a pull request doesn't wait for it,
    // except in a clone being repacked, whose lock it holds.
    cx.background_executor()
        .spawn({
            let ready = ready.clone();
            async move {
                if let Ok(cache) = ready.await
                    && let Err(error) = cache.maintain(RepoCache::PACK_LIMIT)
                {
                    log::warn!("can't repack clones in {}: {error}", cache_dir().display());
                }
            }
        })
        .detach();
    cx.set_global(LocalCopies { ready, token });
}

/// The cache, once [`init`]'s check is done, or why rozed can't use git.
pub fn cache(cx: &App) -> impl Future<Output = anyhow::Result<Arc<RepoCache>>> + use<> {
    let ready = cx.global::<LocalCopies>().ready.clone();
    async move { ready.await.map_err(|error| anyhow::anyhow!("{error}")) }
}

/// The token to sign in with, if rozed has one.
pub fn token(cx: &App) -> Option<Token> {
    cx.global::<LocalCopies>().token.clone()
}
