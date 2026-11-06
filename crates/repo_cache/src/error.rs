//! What can go wrong, in terms the app can show and act on.

use std::io;
use std::path::PathBuf;

use crate::git::GitVersion;

/// The fix for a missing or old git, which every git error message ends with.
const INSTALL_GIT: &str =
    "Install or update the Xcode Command Line Tools: run `xcode-select --install` in Terminal.";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// No git could be found or run.
    #[error("rozed needs git and can't find it. {INSTALL_GIT}")]
    GitMissing,
    /// The git found is older than [`GitVersion::MINIMUM`].
    #[error(
        "rozed needs git {minimum} or newer, and {} is git {found}. {INSTALL_GIT}",
        program.display(),
        minimum = GitVersion::MINIMUM
    )]
    GitTooOld { program: PathBuf, found: GitVersion },
    /// A git command failed for a reason none of the other variants covers.
    #[error("`{command}` failed: {message}")]
    Git { command: String, message: String },
    /// Reading or writing the cache failed.
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: io::Error,
    },
}

impl Error {
    pub(crate) fn io(context: impl Into<String>) -> impl FnOnce(io::Error) -> Self {
        move |source| Error::Io {
            context: context.into(),
            source,
        }
    }
}
