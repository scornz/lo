//! rozed's local copy of each repository it shows, built on the git CLI (design §3, "Local
//! data", and §11, "The local copy").
//!
//! [`Git`] finds git and runs it in an environment of rozed's own, so the user's git config is
//! never read or written. [`RepoCache`] keeps a bare partial clone of each repository, and
//! [`Repo`] fetches pull requests, branches and commits into one and reads diffs and files from
//! it.
//!
//! ```no_run
//! use repo_cache::{Git, PullRequestRefs, RepoCache};
//!
//! let cache = RepoCache::new("/Users/me/Library/Caches/rozed", Git::find()?)?;
//! let repo = cache.open("sharkdp", "fd", None)?;
//! let pull = repo.fetch_pull(&PullRequestRefs {
//!     number: 2212,
//!     head: Some("22c70acc6992c0c51cb68527b87adf114d01acbc".parse()?),
//!     base_branch: "master".into(),
//!     base: None,
//! })?;
//! let base = repo.merge_base(&pull.head, &pull.base_tip)?.expect("shared history");
//! for file in repo.changed_files(&base, &pull.head)? {
//!     if file.is_submodule() || file.binary {
//!         continue;
//!     }
//!     let old = match &file.old {
//!         Some(old) => repo.read_blob(&old.oid)?.unwrap_or_default(),
//!         None => Vec::new(),
//!     };
//!     println!("{}: {:?}, {} bytes before", file.display_path(), file.change, old.len());
//! }
//! # Ok::<(), repo_cache::Error>(())
//! ```

mod cache;
mod cat_file;
mod checkout;
mod diff;
mod error;
mod git;
#[cfg(test)]
mod http_tests;
mod lock;
mod maintain;
mod oid;
mod progress;
mod repo;

pub use cache::RepoCache;
pub use diff::{Change, ChangedFile, FileMode, FileVersion, LineCounts};
pub use error::{Error, FetchFailure};
pub use git::{Git, GitVersion, Token};
pub use oid::Oid;
pub use progress::Progress;
pub use repo::{BaseSource, FetchedPull, HeadSource, PullRequestRefs, Repo};
