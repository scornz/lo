//! rozed's local copy of each repository it shows, built on the git CLI (design §3, "Local
//! data", and §11, "The local copy").
//!
//! [`Git`] finds git and runs it in an environment of rozed's own, so the user's git config is
//! never read or written. [`RepoCache`] keeps a bare partial clone of each repository, and
//! [`Repo`] fetches pull requests, branches and commits into one and reads diffs and files from
//! it.
//!
//! ```no_run
//! use repo_cache::{Git, RepoCache};
//!
//! let cache = RepoCache::new("/Users/me/Library/Caches/rozed", Git::find()?)?;
//! let repo = cache.open("sharkdp", "fd", None)?;
//! let head = repo.fetch_pull(2212)?;
//! let base_tip = repo.fetch_branch("master")?;
//! let base = repo.merge_base(&head, &base_tip)?.expect("shared history");
//! for file in repo.changed_files(&base, &head)? {
//!     let old = file.old_path().map(|path| repo.read_blob(&base, path)).transpose()?;
//!     println!("{} ({:?}, {} bytes before)", file.path(), file.change, old.flatten().map_or(0, |o| o.len()));
//! }
//! # Ok::<(), repo_cache::Error>(())
//! ```

mod askpass;
mod cache;
mod cat_file;
mod diff;
mod error;
mod git;
mod oid;
mod progress;
mod repo;

pub use cache::RepoCache;
pub use diff::{Change, ChangedFile, FileMode, FileVersion, LineCounts};
pub use error::{Error, FetchFailure};
pub use git::{Git, GitVersion, Token};
pub use oid::Oid;
pub use progress::Progress;
pub use repo::Repo;
