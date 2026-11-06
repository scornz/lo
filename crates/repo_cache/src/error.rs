//! What can go wrong, in terms the app can show and act on.

use std::io;
use std::path::PathBuf;

use crate::Oid;
use crate::git::GitVersion;

/// The fix for a missing or old git, which both errors end with.
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
    /// Fetching from GitHub failed. The UI shows it and tries again later (see
    /// [`Error::is_retryable`]).
    #[error("can't fetch {what}: {}", failure.explain(message))]
    Fetch {
        /// What was being fetched, e.g. "pull request #12".
        what: String,
        failure: FetchFailure,
        /// git's own message.
        message: String,
    },
    /// A commit rozed never fetched, which GitHub no longer has: it was force-pushed away long
    /// ago (design §11, "Missing revisions").
    #[error("commit {} is no longer available", oid.short(7))]
    Unavailable { oid: Oid },
    /// The disk is full. The UI offers to clear the cache.
    #[error("the disk is full")]
    DiskFull,
    /// A name or ID that can't be used, such as a branch name git refuses.
    #[error("`{value}` isn't a valid {what}")]
    Invalid { what: &'static str, value: String },
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

/// Why a fetch failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FetchFailure {
    /// The network failed or timed out.
    Network,
    /// GitHub refused the token, or wanted one and rozed had none, or the account can't use it
    /// here (single sign-on, an IP allow list).
    Auth,
    /// GitHub is limiting requests: a rate limit or an abuse limit, which wears off.
    RateLimited,
    /// The server's certificate didn't verify, so rozed sent it nothing.
    Certificate,
    /// The repository doesn't exist, or the account can't see it.
    RepoNotFound,
    /// The branch or pull request ref doesn't exist, or a commit asked for by ID doesn't.
    RefNotFound,
    /// Something else, in git's words.
    Other,
}

impl FetchFailure {
    fn explain(self, message: &str) -> String {
        let reason = match self {
            FetchFailure::Network => "the network failed",
            FetchFailure::RateLimited => "GitHub is limiting requests for now",
            FetchFailure::Certificate => {
                "the server's certificate didn't verify, so rozed sent it nothing"
            }
            FetchFailure::Auth => {
                "GitHub wants an account that can see the repository; sign in with one"
            }
            FetchFailure::RepoNotFound => {
                "the repository doesn't exist, or the account can't see it"
            }
            FetchFailure::RefNotFound => "GitHub doesn't have it",
            FetchFailure::Other => return message.to_string(),
        };
        // git's last line usually says what happened, e.g. "Could not resolve host: github.com".
        match message.lines().rev().find(|line| !line.trim().is_empty()) {
            Some(detail) => format!("{reason} ({})", detail.trim()),
            None => reason.to_string(),
        }
    }
}

impl Error {
    /// Whether trying again later might work: a fetch that failed on the network, on a rate
    /// limit, or for a reason rozed doesn't know.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Error::Fetch {
                failure: FetchFailure::Network | FetchFailure::RateLimited | FetchFailure::Other,
                ..
            }
        )
    }

    /// Wraps an I/O error with what rozed was doing. A full disk becomes [`Error::DiskFull`].
    pub(crate) fn io(context: impl Into<String>) -> impl FnOnce(io::Error) -> Self {
        move |source| {
            if source.kind() == io::ErrorKind::StorageFull {
                return Error::DiskFull;
            }
            Error::Io {
                context: context.into(),
                source,
            }
        }
    }

    /// The error for a git command that failed with `message` on stderr. `what` names what a
    /// fetch was fetching; any command can fetch, since reading a file in a partial clone
    /// downloads it.
    pub(crate) fn from_git(command: String, message: String, what: Option<&str>) -> Self {
        if message.contains("No space left on device") {
            return Error::DiskFull;
        }
        // The clone itself is gone or broken, which no fetch fixes; opening it again makes it anew.
        if message.contains("fatal: not a git repository") {
            return Error::Git { command, message };
        }
        let fetching = what.is_some() || message.contains("promisor remote");
        if !fetching {
            return Error::Git { command, message };
        }
        let failure = classify_fetch(&message);
        Error::Fetch {
            what: what.unwrap_or("the files to show").to_string(),
            failure,
            message,
        }
    }
}

/// Reads why a fetch failed from git's message, which is in English (rozed runs git with
/// `LC_ALL=C`).
fn classify_fetch(message: &str) -> FetchFailure {
    let has = |needles: &[&str]| needles.iter().any(|needle| message.contains(needle));
    let lowercase = message.to_ascii_lowercase();
    if has(&[
        "couldn't find remote ref",
        "not our ref",
        "unadvertised object",
    ]) {
        FetchFailure::RefNotFound
    } else if has(&["returned error: 429"])
        || ["rate limit", "abuse detection"]
            .iter()
            .any(|needle| lowercase.contains(needle))
    {
        FetchFailure::RateLimited
    } else if has(&[
        "Authentication failed",
        "could not read Username",
        "could not read Password",
        "Invalid username or password",
        "returned error: 401",
        "returned error: 403",
    ]) {
        FetchFailure::Auth
    } else if has(&[
        "Repository not found",
        "does not appear to be a git repository",
        "returned error: 404",
    ]) || is_github_repository_not_found(message)
    {
        FetchFailure::RepoNotFound
    } else if lowercase.contains("certificate") {
        FetchFailure::Certificate
    } else if has(&[
        "Could not resolve host",
        "Failed to connect",
        "Couldn't connect",
        "Connection refused",
        "Connection reset",
        "Connection timed out",
        "Operation timed out",
        "Network is unreachable",
        "transfer closed",
        "early EOF",
        "the remote end hung up",
        "RPC failed",
        "SSL",
        "TLS",
        "unable to access",
    ]) {
        FetchFailure::Network
    } else {
        FetchFailure::Other
    }
}

/// Whether a line of `message` is git's `fatal: repository '<url>' not found`, which GitHub's 404
/// for a repository leads to.
fn is_github_repository_not_found(message: &str) -> bool {
    message.lines().any(|line| {
        line.trim()
            .strip_prefix("fatal: repository '")
            .is_some_and(|rest| rest.ends_with("' not found"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fetch_failure(message: &str) -> FetchFailure {
        match Error::from_git("git fetch".into(), message.into(), Some("pull request #1")) {
            Error::Fetch { failure, .. } => failure,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn classifies_fetch_failures_from_gits_messages() {
        let cases = [
            (
                "fatal: unable to access 'https://github.com/a/b.git/': Could not resolve host: \
                 github.com",
                FetchFailure::Network,
            ),
            (
                "fatal: unable to access 'https://github.com/a/b.git/': Failed to connect to \
                 github.com port 443 after 75004 ms: Couldn't connect to server",
                FetchFailure::Network,
            ),
            (
                "error: RPC failed; curl 56 Recv failure: Connection reset by peer\nfatal: early \
                 EOF",
                FetchFailure::Network,
            ),
            (
                "fatal: could not read Username for 'https://github.com': terminal prompts \
                 disabled",
                FetchFailure::Auth,
            ),
            (
                "remote: Invalid username or token.\nfatal: Authentication failed for \
                 'https://github.com/a/b.git/'",
                FetchFailure::Auth,
            ),
            (
                "remote: Repository not found.\nfatal: repository 'https://github.com/a/b.git/' \
                 not found",
                FetchFailure::RepoNotFound,
            ),
            (
                "fatal: '/tmp/x/remote.git' does not appear to be a git repository\nfatal: Could \
                 not read from remote repository.",
                FetchFailure::RepoNotFound,
            ),
            (
                "fatal: couldn't find remote ref refs/pull/9/head",
                FetchFailure::RefNotFound,
            ),
            // A commit GitHub no longer has, asked for by ID.
            (
                "fatal: remote error: upload-pack: not our ref \
                 1234567890123456789012345678901234567890",
                FetchFailure::RefNotFound,
            ),
            (
                "error: Server does not allow request for unadvertised object \
                 1234567890123456789012345678901234567890",
                FetchFailure::RefNotFound,
            ),
            // Any 403 that isn't a rate limit is about the account: SSO, an IP allow list.
            (
                "fatal: unable to access 'https://github.com/a/b.git/': The requested URL \
                 returned error: 403",
                FetchFailure::Auth,
            ),
            (
                "remote: The 'org' organization has enabled or enforced SAML SSO.\nfatal: unable \
                 to access 'https://github.com/a/b.git/': The requested URL returned error: 403",
                FetchFailure::Auth,
            ),
            // Rate and abuse limits wear off.
            (
                "remote: API rate limit exceeded for user ID 1.\nfatal: unable to access \
                 'https://github.com/a/b.git/': The requested URL returned error: 403",
                FetchFailure::RateLimited,
            ),
            (
                "remote: You have exceeded a secondary rate limit.\nfatal: unable to access \
                 'https://github.com/a/b.git/': The requested URL returned error: 403",
                FetchFailure::RateLimited,
            ),
            (
                "remote: You have triggered an abuse detection mechanism.\nfatal: unable to \
                 access 'https://github.com/a/b.git/': The requested URL returned error: 403",
                FetchFailure::RateLimited,
            ),
            (
                "fatal: unable to access 'https://github.com/a/b.git/': The requested URL \
                 returned error: 429",
                FetchFailure::RateLimited,
            ),
            // Only GitHub's own words for a missing repository.
            (
                "fatal: repository 'https://github.com/a/b.git/' not found",
                FetchFailure::RepoNotFound,
            ),
            ("fatal: path 'src/x' not found", FetchFailure::Other),
            ("error: object 1234 not found", FetchFailure::Other),
            // A certificate that fails to verify won't verify next time either.
            (
                "fatal: unable to access 'https://github.com/a/b.git/': SSL certificate problem: \
                 unable to get local issuer certificate",
                FetchFailure::Certificate,
            ),
            (
                "fatal: unable to access 'https://github.com/a/b.git/': SSL: no alternative \
                 certificate subject name matches target host name 'github.com'",
                FetchFailure::Certificate,
            ),
            (
                "fatal: unable to access 'https://github.com/a/b.git/': server certificate \
                 verification failed. CAfile: none CRLfile: none",
                FetchFailure::Certificate,
            ),
            // A TLS connection that drops is the network.
            (
                "fatal: unable to access 'https://github.com/a/b.git/': LibreSSL SSL_connect: \
                 SSL_ERROR_SYSCALL in connection to github.com:443",
                FetchFailure::Network,
            ),
            ("fatal: something new", FetchFailure::Other),
        ];
        for (message, expected) in cases {
            assert_eq!(fetch_failure(message), expected, "{message}");
        }
    }

    #[test]
    fn rate_limits_and_the_network_are_retryable_and_the_rest_isnt() {
        let retryable = |failure| {
            Error::Fetch {
                what: "pull request #1".into(),
                failure,
                message: String::new(),
            }
            .is_retryable()
        };
        assert!(retryable(FetchFailure::Network));
        assert!(retryable(FetchFailure::RateLimited));
        assert!(retryable(FetchFailure::Other));
        for failure in [
            FetchFailure::Auth,
            FetchFailure::Certificate,
            FetchFailure::RepoNotFound,
            FetchFailure::RefNotFound,
        ] {
            assert!(!retryable(failure), "{failure:?}");
        }
    }

    #[test]
    fn a_full_disk_is_its_own_error() {
        let error = Error::from_git(
            "git fetch".into(),
            "error: unable to write file: No space left on device".into(),
            Some("pull request #1"),
        );
        assert!(matches!(error, Error::DiskFull), "{error:?}");
        let io = io::Error::from(io::ErrorKind::StorageFull);
        assert!(matches!(Error::io("writing")(io), Error::DiskFull));
    }

    #[test]
    fn a_failed_lazy_download_is_a_fetch_error() {
        let error = Error::from_git(
            "git cat-file --batch".into(),
            "fatal: unable to access 'https://github.com/a/b.git/': Could not resolve host: \
             github.com\nfatal: could not fetch 6178079 from promisor remote"
                .into(),
            None,
        );
        assert!(error.is_retryable(), "{error:?}");
        assert!(
            error
                .to_string()
                .starts_with("can't fetch the files to show: the network failed"),
            "{error}"
        );
    }

    #[test]
    fn other_git_failures_stay_git_errors() {
        let error = Error::from_git(
            "git merge-base a b".into(),
            "fatal: Not a valid object name a".into(),
            None,
        );
        assert!(matches!(error, Error::Git { .. }), "{error:?}");
        assert!(!error.is_retryable());
    }

    #[test]
    fn messages_say_what_failed_and_why() {
        let error = Error::from_git(
            "git fetch".into(),
            "remote: Repository not found.\nfatal: repository 'https://github.com/a/b.git/' not \
             found"
                .into(),
            Some("pull request #12"),
        );
        assert_eq!(
            error.to_string(),
            "can't fetch pull request #12: the repository doesn't exist, or the account can't \
             see it (fatal: repository 'https://github.com/a/b.git/' not found)"
        );
        let oid = "22c70acc6992c0c51cb68527b87adf114d01acbc".parse().unwrap();
        assert_eq!(
            Error::Unavailable { oid }.to_string(),
            "commit 22c70ac is no longer available"
        );
    }
}
