//! Running git: finding it, checking its version, and running it in an environment of rozed's own.
//!
//! rozed runs the git CLI as a child process. Every git it runs gets the same environment, built
//! from nothing rather than inherited and patched:
//!
//! - Only `PATH`, `TMPDIR`, `DEVELOPER_DIR` and the network variables (proxies and CA bundles) come
//!   from rozed's own environment. Everything else is dropped, including every `GIT_*` variable, so
//!   nothing adds config (`GIT_CONFIG_PARAMETERS`, `GIT_CONFIG_COUNT`), points git at another
//!   repository, template, program or attribute source (`GIT_DIR`, `GIT_TEMPLATE_DIR`,
//!   `GIT_EXEC_PATH`, `GIT_SSH_COMMAND`, `GIT_ATTR_SOURCE`) or prompts (`SSH_ASKPASS`).
//! - `HOME` is a folder of rozed's, so git and its HTTP library read none of the user's files there
//!   (`~/.gitconfig`, `~/.config/git/attributes`, `~/.netrc`).
//! - No system or global config (`GIT_CONFIG_NOSYSTEM`, `GIT_CONFIG_GLOBAL=/dev/null`), no
//!   credential helper but rozed's, and no prompts (`GIT_TERMINAL_PROMPT=0`, no askpass). The
//!   only credential git gets is the token, from a helper scoped to the remote's own URL
//!   (`https://github.com`), which reads it from a variable set only on that git. A proxy that
//!   asks for a password, or a host a request is redirected to, matches no helper, and git fails
//!   rather than prompting. Redirects are off anyway: rozed always fetches a repository by its
//!   canonical name.
//! - No hooks, no file-system monitor, no attributes but rozed's (none: not the user's file, and not
//!   a repository's own `.gitattributes`, in a clone or a checkout), no replace refs, and literal
//!   pathspecs.
//! - Nothing a server offers sends git to another host: no bundle URIs, promisor remotes or
//!   packfile URIs.
//! - Every command names its repository (`--git-dir`, and `--work-tree` for a checkout), and git
//!   never looks for one in the cache's folder or above it (`GIT_CEILING_DIRECTORIES`). A clone
//!   that's half deleted is an error, never a reason to use some repository further up, such as
//!   dotfiles kept in the home folder, with its own remote and config.
//! - HTTPS only. [`crate::RepoCache::with_remote_base`] allows another protocol for tests.
//! - `LC_ALL=C`, so the messages rozed reads are in English.
//!
//! The user's git config is never read or written, and nothing in it or in rozed's environment
//! (URL rewrites, credential helpers, filters, hooks) changes what this crate's git fetches or
//! runs.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::Instant;

use crate::Error;

/// A git version, from `git --version`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GitVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl GitVersion {
    /// The oldest git rozed runs: 2.41 added `--attr-source`, which keeps a repository's own
    /// `.gitattributes` out of its checkouts (2.40 added `git merge-tree --write-tree
    /// --merge-base`, which rebase-aware comparison needs).
    pub const MINIMUM: GitVersion = GitVersion {
        major: 2,
        minor: 41,
        patch: 0,
    };

    /// Parses `git --version`'s output, such as `git version 2.39.5 (Apple Git-154)`.
    pub fn parse(output: &str) -> Option<Self> {
        let version = output.trim().strip_prefix("git version ")?;
        let version = version.split_whitespace().next()?;
        // Anything after the numbers, such as `.rc1`, `-rc1` or `.windows.1`, is ignored.
        let mut numbers = version
            .split(['.', '-'])
            .map_while(|part| part.parse::<u32>().ok());
        Some(GitVersion {
            major: numbers.next()?,
            minor: numbers.next()?,
            patch: numbers.next().unwrap_or(0),
        })
    }
}

impl fmt::Display for GitVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// A GitHub token for git to sign in with. Its `Debug` hides the value and it has no `Display`, so
/// it can't end up in a log line or an error message by accident.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    /// Wraps a token, trimming the whitespace a file or a command's output leaves around it.
    pub fn new(token: impl Into<String>) -> Self {
        let token = token.into();
        Self(token.trim().to_string())
    }

    /// The token itself, for an `Authorization` header. The one way to read it.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Whether git can be given the token: not empty, and nothing that would split the
    /// credential helper's answer into more lines. GitHub's tokens are letters, digits and `_`.
    pub(crate) fn is_usable(&self) -> bool {
        !self.0.is_empty() && !self.0.chars().any(|c| c.is_control() || c.is_whitespace())
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(redacted)")
    }
}

/// The git rozed runs. Its `Debug` hides the proxy variables' values, which can hold a password.
#[derive(Clone)]
pub struct Git {
    program: PathBuf,
    version: GitVersion,
    /// The variables every git inherits from rozed's environment (see [`inherited_env`]).
    inherited: Vec<(OsString, OsString)>,
}

impl fmt::Debug for Git {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let inherited: Vec<String> = self
            .inherited
            .iter()
            .map(|(name, value)| {
                let name = name.to_string_lossy();
                if name.to_ascii_lowercase().ends_with("_proxy") {
                    format!("{name}=(redacted)")
                } else {
                    format!("{name}={}", value.to_string_lossy())
                }
            })
            .collect();
        f.debug_struct("Git")
            .field("program", &self.program)
            .field("version", &self.version)
            .field("inherited", &inherited)
            .finish()
    }
}

/// The empty tree's ID, which git knows without having it.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// Where rozed looks for git first: Apple's, from the Xcode Command Line Tools.
const SYSTEM_GIT: &str = "/usr/bin/git";

/// The variables git inherits from rozed's environment. Everything else is dropped.
const INHERITED_VARS: &[&str] = &[
    "PATH",
    "TMPDIR",
    // Proxies, in the spellings curl reads. It ignores `HTTP_PROXY`, which a web server can set
    // from a request header.
    "http_proxy",
    "https_proxy",
    "HTTPS_PROXY",
    "all_proxy",
    "ALL_PROXY",
    "no_proxy",
    "NO_PROXY",
    // Certificate authorities, for networks that inspect TLS.
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "CURL_CA_BUNDLE",
    // Which Xcode `/usr/bin/git` runs git from.
    "DEVELOPER_DIR",
];

/// The variable the credential helper reads the token from. It's set only on git children that
/// sign in.
pub(crate) const TOKEN_VAR: &str = "ROZED_GIT_TOKEN";

/// The credential helper, a shell function git runs with the action (`get`, `store` or `erase`)
/// as its argument. It answers `get` with the token in [`TOKEN_VAR`], and nothing else.
const CREDENTIAL_HELPER: &str = "!f() { test \"$1\" = get || exit 0; \
     printf 'username=x-access-token\\npassword=%s\\n' \"$ROZED_GIT_TOKEN\"; }; f";

impl Git {
    /// Finds git, `/usr/bin/git` first and then each `git` on `PATH`, and checks that it's new
    /// enough. rozed runs this at launch, and shows the error, which says how to fix it, if
    /// there's no usable git.
    pub fn find() -> Result<Self, Error> {
        let env: Vec<_> = std::env::vars_os().collect();
        let path = std::env::var_os("PATH").unwrap_or_default();
        let candidates = std::iter::once(PathBuf::from(SYSTEM_GIT))
            .chain(std::env::split_paths(&path).map(|dir| dir.join("git")));
        let mut tried = Vec::new();
        let mut too_old = None;
        for program in candidates {
            if !program.is_absolute() || tried.contains(&program) || !program.is_file() {
                continue;
            }
            tried.push(program.clone());
            match Self::with_env(program, env.clone()) {
                Ok(git) => return Ok(git),
                Err(error @ Error::GitTooOld { .. }) => {
                    too_old.get_or_insert(error);
                }
                Err(error) => log::warn!("{error}"),
            }
        }
        Err(too_old.unwrap_or(Error::GitMissing))
    }

    /// The git at `program`, if it runs and is new enough.
    pub fn at(program: impl Into<PathBuf>) -> Result<Self, Error> {
        Self::with_env(program.into(), std::env::vars_os().collect())
    }

    /// The git at `program`, as if rozed's environment were `env`.
    pub(crate) fn with_env(
        program: PathBuf,
        env: Vec<(OsString, OsString)>,
    ) -> Result<Self, Error> {
        let inherited = inherited_env(env);
        let output = Command::new(&program)
            .arg("--version")
            .env_clear()
            .envs(inherited.iter().map(|(name, value)| (name, value)))
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output();
        let version = match output {
            Ok(output) if output.status.success() => {
                GitVersion::parse(&String::from_utf8_lossy(&output.stdout))
            }
            Ok(output) => {
                log::warn!(
                    "`{} --version` exited with {}",
                    program.display(),
                    output.status
                );
                None
            }
            Err(error) => {
                log::warn!("can't run {}: {error}", program.display());
                None
            }
        };
        let Some(version) = version else {
            return Err(Error::GitMissing);
        };
        if version < GitVersion::MINIMUM {
            return Err(Error::GitTooOld {
                program,
                found: version,
            });
        }
        log::info!("using git {version} at {}", program.display());
        Ok(Self {
            program,
            version,
            inherited,
        })
    }

    pub fn program(&self) -> &Path {
        &self.program
    }

    pub fn version(&self) -> GitVersion {
        self.version
    }

    /// A git command with rozed's environment (see the module docs), working on the repository
    /// at `location`, which git is always told so it never looks for one. `home` is the empty
    /// folder git gets as its `HOME`, and git never looks for a repository in `ceiling` or above
    /// it. With `auth`, git signs in with its token when the remote asks, and only then.
    ///
    /// `config` is extra `name=value` config, which wins over rozed's own: tests allow `file://`.
    pub(crate) fn command(
        &self,
        location: Location<'_>,
        home: &Path,
        ceiling: &Path,
        auth: Option<Auth<'_>>,
        config: &[String],
    ) -> GitCommand {
        let mut command = Command::new(&self.program);
        command
            .env_clear()
            .envs(self.inherited.iter().map(|(name, value)| (name, value)))
            .env("HOME", home)
            .env("GIT_CEILING_DIRECTORIES", ceiling)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .env("GIT_LITERAL_PATHSPECS", "1")
            .env("LC_ALL", "C")
            .stdin(Stdio::null());
        match location {
            Location::Nowhere => {
                command.current_dir(home);
            }
            Location::Clone(git_dir) => {
                command.current_dir(home).arg(git_dir_arg(git_dir));
            }
            Location::Checkout { git_dir, work_tree } => {
                let mut work_tree_arg = OsString::from("--work-tree=");
                work_tree_arg.push(work_tree);
                command
                    .current_dir(work_tree)
                    .arg(git_dir_arg(git_dir))
                    .arg(work_tree_arg);
            }
        }
        command.args([
            // No credential helper, prompt or redirect: only rozed's helper, added below for the
            // remote's URL, gives git a credential.
            "-c",
            "credential.helper=",
            "-c",
            "core.askPass=",
            "-c",
            "http.followRedirects=false",
        ]);
        if let Some(auth) = auth
            && auth.token.is_usable()
        {
            command.env(TOKEN_VAR, auth.token.expose()).args([
                "-c".to_string(),
                format!("credential.{}.helper={CREDENTIAL_HELPER}", auth.remote),
            ]);
        }
        command.args([
            // Symlinks in a checkout become plain files holding their target, so nothing in a
            // checkout points outside it.
            "-c",
            "core.symlinks=false",
            // No garbage collection or maintenance in the background, behind rozed's back.
            "-c",
            "gc.auto=0",
            "-c",
            "maintenance.auto=false",
            // Give up on a connection that sends nothing for a minute, rather than hanging.
            // GitHub sends keep-alives while it prepares a pack.
            "-c",
            "http.lowSpeedLimit=1",
            "-c",
            "http.lowSpeedTime=60",
            // Nothing in a repository or rozed's environment makes git run a program: no hooks
            // (a checkout runs post-checkout) and no file-system monitor.
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            // No attributes: not the user's file, and not a repository's own `.gitattributes`,
            // whatever a clone's config says (`attr.tree`). `--attr-source`, below, reads
            // attributes from the empty tree instead of a checkout's files.
            "-c",
            "core.attributesFile=/dev/null",
            // Nothing a server offers makes git fetch from another host, which would ask for
            // credentials: no bundles, no other promisor remotes, no packfile URIs.
            "-c",
            "transfer.bundleURI=false",
            "-c",
            "promisor.acceptFromServer=None",
            "-c",
            "fetch.uriProtocols=",
            // HTTPS only: no ssh, git or ext transports, and no local paths.
            "-c",
            "protocol.allow=never",
            "-c",
            "protocol.https.allow=always",
        ]);
        command.arg(format!("--attr-source={EMPTY_TREE}"));
        for config in config {
            command.args(["-c", config]);
        }
        GitCommand {
            command,
            args: Vec::new(),
        }
    }
}

/// The repository a git command works on.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Location<'a> {
    /// None, for `git init` and the commands that need no repository.
    Nowhere,
    /// A bare clone.
    Clone(&'a Path),
    /// A checkout of a clone: its git dir, under the clone's `worktrees/`, and its folder.
    Checkout {
        git_dir: &'a Path,
        work_tree: &'a Path,
    },
}

fn git_dir_arg(git_dir: &Path) -> OsString {
    let mut arg = OsString::from("--git-dir=");
    arg.push(git_dir);
    arg
}

/// How git signs in: with `token`, for `remote` alone.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Auth<'a> {
    /// The remote's URL without a path, such as `https://github.com`: the only place the token
    /// goes.
    pub remote: &'a str,
    pub token: &'a Token,
}

/// Keeps the variables in [`INHERITED_VARS`] from `vars`, and drops the rest.
fn inherited_env(
    vars: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    vars.into_iter()
        .filter(|(name, _)| INHERITED_VARS.iter().any(|kept| OsStr::new(kept) == name))
        .collect()
}

/// A git command being built, which remembers its arguments for logs and errors. Its environment
/// never appears in either, so the token doesn't.
pub(crate) struct GitCommand {
    command: Command,
    args: Vec<String>,
}

/// A finished git command whose exit status the caller interprets.
pub(crate) struct Finished {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

impl GitCommand {
    pub fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.args.push(arg.as_ref().to_string_lossy().into_owned());
        self.command.arg(arg);
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        for arg in args {
            self = self.arg(arg);
        }
        self
    }

    /// The command as rozed logs it: `git` and the arguments rozed added.
    pub fn display(&self) -> String {
        format!("git {}", self.args.join(" "))
    }

    /// Runs the command and returns its output, or an error with git's message.
    pub fn run(self) -> Result<Vec<u8>, Error> {
        self.run_with_input(None)
    }

    /// Runs the command with `input` on its stdin.
    pub fn run_with_input(self, input: Option<&[u8]>) -> Result<Vec<u8>, Error> {
        let display = self.display();
        let finished = self.finish(input)?;
        if !finished.status.success() {
            return Err(Error::Git {
                command: display,
                message: failure_message(&finished),
            });
        }
        Ok(finished.stdout)
    }

    /// Runs the command and returns how it went, whatever its exit status.
    pub fn finish(mut self, input: Option<&[u8]>) -> Result<Finished, Error> {
        let display = self.display();
        let started = Instant::now();
        if input.is_some() {
            self.command.stdin(Stdio::piped());
        }
        let mut child = self
            .command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(Error::io(format!("running {display}")))?;
        if let Some(input) = input {
            let mut stdin = child.stdin.take().expect("stdin is piped");
            // git may exit without reading everything, so a broken pipe isn't an error here; its
            // exit status says what happened.
            let _ = stdin.write_all(input);
        }
        let output = child
            .wait_with_output()
            .map_err(Error::io(format!("running {display}")))?;
        log::debug!(
            "{display} exited with {} in {:?}",
            output.status,
            started.elapsed()
        );
        Ok(Finished {
            status: output.status,
            stdout: output.stdout,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    pub fn into_command(self) -> (Command, String) {
        let display = self.display();
        (self.command, display)
    }
}

/// What to say when git fails: its message, or its exit status when it printed nothing.
pub(crate) fn failure_message(finished: &Finished) -> String {
    let stderr = finished.stderr.trim();
    if stderr.is_empty() {
        format!("git exited with {}", finished.status)
    } else {
        stderr.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;

    use tempfile::TempDir;

    fn version(major: u32, minor: u32, patch: u32) -> GitVersion {
        GitVersion {
            major,
            minor,
            patch,
        }
    }

    #[test]
    fn parses_versions() {
        let cases = [
            ("git version 2.54.0 (Apple Git-157)\n", version(2, 54, 0)),
            ("git version 2.40.0", version(2, 40, 0)),
            ("git version 2.39.5 (Apple Git-154)", version(2, 39, 5)),
            ("git version 2.45.2.windows.1", version(2, 45, 2)),
            ("git version 2.40.0.rc1", version(2, 40, 0)),
            ("git version 2.40.0-rc1", version(2, 40, 0)),
            ("git version 2.41", version(2, 41, 0)),
            ("git version 3.0.0", version(3, 0, 0)),
        ];
        for (output, expected) in cases {
            assert_eq!(GitVersion::parse(output), Some(expected), "{output}");
        }
        for output in [
            "",
            "git version",
            "git version x.y",
            "hub version 2.40.0",
            "2.40.0",
        ] {
            assert_eq!(GitVersion::parse(output), None, "{output}");
        }
    }

    #[test]
    fn needs_git_2_41() {
        assert_eq!(GitVersion::MINIMUM, version(2, 41, 0));
        assert!(version(2, 40, 1) < GitVersion::MINIMUM);
        assert!(version(2, 41, 0) >= GitVersion::MINIMUM);
        assert!(version(2, 100, 0) > version(2, 40, 0));
        assert_eq!(version(2, 40, 1).to_string(), "2.40.1");
    }

    #[test]
    fn finds_a_git_new_enough() {
        let git = Git::find().unwrap();
        assert!(git.version() >= GitVersion::MINIMUM, "{:?}", git.version());
        assert!(git.program().is_absolute());
    }

    /// A script that pretends to be git and prints `version`.
    fn fake_git(dir: &Path, version: &str) -> PathBuf {
        let path = dir.join("git");
        fs::write(&path, format!("#!/bin/sh\necho 'git version {version}'\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn refuses_a_git_older_than_2_41() {
        let dir = TempDir::new().unwrap();
        let program = fake_git(dir.path(), "2.39.5 (Apple Git-154)");
        let error = Git::at(&program).unwrap_err();
        let Error::GitTooOld { found, .. } = &error else {
            panic!("{error:?}");
        };
        assert_eq!(*found, version(2, 39, 5));
        let message = error.to_string();
        assert!(message.contains("git 2.41.0 or newer"), "{message}");
        assert!(message.contains("is git 2.39.5"), "{message}");
        assert!(message.contains("xcode-select --install"), "{message}");
        // 2.40 has no `--attr-source`, which keeps a repository's attributes out of checkouts.
        assert!(matches!(
            Git::at(fake_git(dir.path(), "2.40.1")),
            Err(Error::GitTooOld { .. })
        ));

        assert_eq!(
            Git::at(fake_git(dir.path(), "2.41.0")).unwrap().version(),
            version(2, 41, 0)
        );
    }

    #[test]
    fn missing_git_says_how_to_install_it() {
        let dir = TempDir::new().unwrap();
        let error = Git::at(dir.path().join("no-git-here")).unwrap_err();
        assert!(matches!(error, Error::GitMissing), "{error:?}");
        assert!(error.to_string().contains("xcode-select --install"));

        // Something that runs but isn't git counts as missing too.
        let not_git = fake_git(dir.path(), "");
        fs::write(&not_git, "#!/bin/sh\necho hello\n").unwrap();
        assert!(matches!(Git::at(&not_git), Err(Error::GitMissing)));
    }

    fn vars(vars: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        vars.iter()
            .map(|(name, value)| (OsString::from(name), OsString::from(value)))
            .collect()
    }

    #[test]
    fn inherits_only_path_tmpdir_and_network_variables() {
        let kept = vars(&[
            ("PATH", "/usr/bin:/bin"),
            ("TMPDIR", "/tmp/x"),
            ("https_proxy", "http://proxy:3128"),
            ("NO_PROXY", "localhost"),
            ("SSL_CERT_FILE", "/etc/ca.pem"),
        ]);
        let dropped = vars(&[
            ("HOME", "/Users/someone"),
            ("XDG_CONFIG_HOME", "/Users/someone/.config"),
            ("GIT_DIR", "/elsewhere/.git"),
            ("GIT_CONFIG_PARAMETERS", "'core.pager'='evil'"),
            ("GIT_CONFIG_COUNT", "1"),
            ("GIT_SSL_NO_VERIFY", "1"),
            ("GIT_ASKPASS", "/tmp/steal"),
            ("SSH_ASKPASS", "/tmp/steal"),
            ("HTTP_PROXY", "http://from-a-header"),
            ("ROZED_GITHUB_TOKEN", "secret"),
            ("LANG", "fr_FR.UTF-8"),
        ]);
        let all = kept.iter().chain(&dropped).cloned();
        assert_eq!(inherited_env(all), kept);
    }

    /// The test's own git in `dir`, without the user's config.
    fn plain_git(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "rozed test")
            .env("GIT_AUTHOR_EMAIL", "test@rozed.invalid")
            .env("GIT_COMMITTER_NAME", "rozed test")
            .env("GIT_COMMITTER_EMAIL", "test@rozed.invalid")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    /// The URL rewrite a hostile config applies: every `file://` URL goes nowhere.
    const REWRITE: &str = "[url \"file:///nowhere/\"]\n\tinsteadOf = file://\n";

    /// A repository served over `file://`, and an environment in which every variable that could
    /// change what git reads, runs or fetches is set to something that would show.
    struct Hostile {
        dir: TempDir,
        /// The remote's URL.
        url: String,
        /// A working copy, with `first` and `second` commits on `trunk`. Both have a
        /// `.gitattributes` saying `*.txt -diff`, and `second` changes `a.txt`. Replace refs make
        /// `first` read as `second`.
        work: PathBuf,
        first: String,
        /// rozed's own `HOME` for git.
        home: PathBuf,
        env: Vec<(OsString, OsString)>,
    }

    impl Hostile {
        fn new() -> Self {
            let dir = TempDir::new().unwrap();
            let root = dir.path();
            let work = root.join("work");
            fs::create_dir(&work).unwrap();
            plain_git(&work, &["init", "-q", "-b", "trunk"]);
            fs::write(work.join("a.txt"), "a\n").unwrap();
            fs::write(work.join(".gitattributes"), "*.txt -diff\n").unwrap();
            plain_git(&work, &["add", "-A"]);
            plain_git(&work, &["commit", "-q", "-m", "first"]);
            let first = plain_git(&work, &["rev-parse", "HEAD"]);
            fs::write(work.join("a.txt"), "b\n").unwrap();
            plain_git(&work, &["commit", "-q", "-am", "second"]);
            let second = plain_git(&work, &["rev-parse", "HEAD"]);
            plain_git(root, &["clone", "-q", "--bare", "work", "remote.git"]);
            // Replace refs that would make `first` read as `second`.
            plain_git(&work, &["replace", &first, &second]);
            plain_git(
                &work,
                &["update-ref", &format!("refs/evil/{first}"), &second],
            );

            // A program that leaves a marker named after itself, so a test can see it ran.
            let markers = root.join("markers");
            let bin = root.join("bin");
            fs::create_dir_all(&markers).unwrap();
            fs::create_dir_all(&bin).unwrap();
            let program = |name: &str| {
                let path = bin.join(name);
                let script = format!("#!/bin/sh\ntouch '{}/{name}'\nexit 1\n", markers.display());
                fs::write(&path, script).unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
                path.to_str().unwrap().to_string()
            };
            let exec_path = root.join("exec-path");
            fs::create_dir_all(&exec_path).unwrap();
            fs::copy(
                program("git-upload-pack"),
                exec_path.join("git-upload-pack"),
            )
            .unwrap();
            let hooks = root.join("hooks");
            fs::create_dir_all(&hooks).unwrap();
            fs::copy(program("post-checkout"), hooks.join("post-checkout")).unwrap();
            let template = root.join("template");
            fs::create_dir_all(template.join("info")).unwrap();
            fs::write(template.join("info/hostile-template"), "").unwrap();

            // The user's config and attributes, everywhere git might look for them.
            let user_home = root.join("user-home");
            fs::create_dir_all(user_home.join(".config/git")).unwrap();
            fs::write(user_home.join(".gitconfig"), REWRITE).unwrap();
            fs::write(user_home.join(".config/git/config"), REWRITE).unwrap();
            fs::write(user_home.join(".config/git/attributes"), "* binary\n").unwrap();
            fs::write(
                user_home.join(".netrc"),
                "machine github.com login a password b\n",
            )
            .unwrap();
            let global = root.join("global-config");
            fs::write(&global, REWRITE).unwrap();
            let parameters = format!(
                "'url.file:///nowhere/.insteadof'='file://' 'core.hookspath'='{}' \
                 'core.fsmonitor'='{}' 'protocol.allow'='always'",
                hooks.display(),
                program("fsmonitor"),
            );
            let env = vars(&[
                ("PATH", "/usr/bin:/bin"),
                ("HOME", user_home.to_str().unwrap()),
                (
                    "XDG_CONFIG_HOME",
                    user_home.join(".config").to_str().unwrap(),
                ),
                ("GIT_CONFIG_GLOBAL", global.to_str().unwrap()),
                ("GIT_CONFIG_PARAMETERS", &parameters),
                ("GIT_CONFIG_COUNT", "1"),
                ("GIT_CONFIG_KEY_0", "url.file:///nowhere/.insteadOf"),
                ("GIT_CONFIG_VALUE_0", "file://"),
                ("GIT_DIR", "/nowhere/.git"),
                ("GIT_TEMPLATE_DIR", template.to_str().unwrap()),
                ("GIT_EXEC_PATH", exec_path.to_str().unwrap()),
                ("GIT_SSH_COMMAND", &program("ssh-command")),
                ("GIT_SSH", &program("ssh")),
                ("GIT_ASKPASS", &program("git-askpass")),
                ("SSH_ASKPASS", &program("ssh-askpass")),
                ("GIT_REPLACE_REF_BASE", "refs/evil/"),
                ("GIT_ATTR_SOURCE", &second),
                ("GIT_PROTOCOL_FROM_USER", "1"),
            ]);
            let home = root.join("rozed-home");
            fs::create_dir(&home).unwrap();
            let url = format!("file://{}", root.join("remote.git").display());
            Self {
                dir,
                url,
                work,
                first,
                home,
                env,
            }
        }

        /// git run by rozed from inside the hostile environment.
        fn git(&self, location: Location<'_>, config: &[&str]) -> GitCommand {
            let config: Vec<String> = config.iter().map(|c| c.to_string()).collect();
            Git::with_env(Git::find().unwrap().program, self.env.clone())
                .unwrap()
                .command(location, &self.home, self.dir.path(), None, &config)
        }

        /// The hostile programs that ran.
        fn programs_run(&self) -> Vec<String> {
            let mut names: Vec<String> = fs::read_dir(self.dir.path().join("markers"))
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }
    }

    const ALLOW_FILE: &str = "protocol.file.allow=always";

    #[test]
    fn hostile_config_has_no_effect() {
        let hostile = Hostile::new();

        // No URL rewrite, and git's own upload-pack, not GIT_EXEC_PATH's.
        let refs = hostile
            .git(Location::Nowhere, &[ALLOW_FILE])
            .args(["ls-remote", &hostile.url])
            .run()
            .unwrap();
        assert!(
            String::from_utf8(refs)
                .unwrap()
                .contains("refs/heads/trunk")
        );

        // git sees no config but rozed's own.
        let config = hostile
            .git(Location::Nowhere, &[])
            .args(["config", "--list", "--show-origin"])
            .run()
            .unwrap();
        let config = String::from_utf8(config).unwrap();
        assert!(
            config.lines().all(|line| line.starts_with("command line:")),
            "{config}"
        );
        for absent in ["insteadof", "hostile", "markers", "protocol.allow=always"] {
            assert!(!config.contains(absent), "{absent} in {config}");
        }
        for pinned in [
            "credential.helper=",
            "core.askpass=",
            "http.followredirects=false",
            "core.hookspath=/dev/null",
            "core.fsmonitor=false",
            "transfer.bundleuri=false",
            "promisor.acceptfromserver=None",
            "fetch.uriprotocols=",
        ] {
            assert!(
                config.lines().any(|line| line.ends_with(pinned)),
                "{pinned} not in {config}"
            );
        }
        assert_eq!(hostile.programs_run(), Vec::<String>::new());
    }

    #[test]
    fn hostile_hooks_and_templates_never_run() {
        let hostile = Hostile::new();
        let clone = hostile.dir.path().join("clone");

        // A new repository gets no hostile template, and a checkout runs no hook.
        hostile
            .git(Location::Nowhere, &[])
            .args(["init", "-q"])
            .arg(&clone)
            .run()
            .unwrap();
        assert!(!clone.join(".git/info/hostile-template").exists());
        let work = hostile.work.to_str().unwrap();
        let checkout = Location::Checkout {
            git_dir: &clone.join(".git"),
            work_tree: &clone,
        };
        hostile
            .git(checkout, &[ALLOW_FILE])
            .args(["fetch", "-q", work, "trunk"])
            .run()
            .unwrap();
        hostile
            .git(checkout, &[])
            .args(["checkout", "-q", "FETCH_HEAD"])
            .run()
            .unwrap();
        hostile
            .git(checkout, &[])
            .args(["status", "--porcelain"])
            .run()
            .unwrap();
        assert_eq!(hostile.programs_run(), Vec::<String>::new());
    }

    #[test]
    fn only_https_is_allowed() {
        let hostile = Hostile::new();
        for (url, transport) in [
            (hostile.url.as_str(), "file"),
            ("ssh://git@example.invalid/a/b.git", "ssh"),
            ("git@example.invalid:a/b.git", "ssh"),
            ("git://example.invalid/a/b.git", "git"),
            ("ext::sh -c touch% /tmp/rozed-ext", "ext"),
        ] {
            let error = hostile
                .git(Location::Nowhere, &[])
                .args(["ls-remote", url])
                .run()
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains(&format!("transport '{transport}' not allowed")),
                "{url}: {error}"
            );
        }
        assert_eq!(hostile.programs_run(), Vec::<String>::new());
    }

    #[test]
    fn hostile_attributes_and_replace_refs_have_no_effect() {
        let hostile = Hostile::new();

        // Neither the user's attributes nor GIT_ATTR_SOURCE's. A repository's own are the clone and
        // checkout tests' business.
        let attributes = hostile
            .git(Location::Clone(&hostile.dir.path().join("remote.git")), &[])
            .args(["check-attr", "binary", "diff", "--", "a.txt"])
            .run()
            .unwrap();
        assert_eq!(
            String::from_utf8(attributes).unwrap(),
            "a.txt: binary: unspecified\na.txt: diff: unspecified\n"
        );
        // `first` is itself, despite refs/replace/ and GIT_REPLACE_REF_BASE.
        let subject = hostile
            .git(
                Location::Checkout {
                    git_dir: &hostile.work.join(".git"),
                    work_tree: &hostile.work,
                },
                &[],
            )
            .args(["log", "-1", "--format=%s", &hostile.first])
            .run()
            .unwrap();
        assert_eq!(String::from_utf8(subject).unwrap(), "first\n");
    }

    /// Asks git for credentials for `url`, as it does when a server or proxy answers 401 or 407.
    fn credential_fill(
        git: &Git,
        auth: Option<Auth<'_>>,
        home: &Path,
        url: &str,
    ) -> Result<String, Error> {
        let output = git
            .command(Location::Nowhere, home, home, auth, &[])
            .args(["credential", "fill"])
            .run_with_input(Some(format!("url={url}\n\n").as_bytes()))?;
        Ok(String::from_utf8(output).unwrap())
    }

    const GITHUB: &str = "https://github.com";

    #[test]
    fn git_signs_in_to_the_remote_with_the_token() {
        let dir = TempDir::new().unwrap();
        let token = Token::new("gho_secret\n");
        let auth = Auth {
            remote: GITHUB,
            token: &token,
        };
        let git = Git::find().unwrap();

        let credentials =
            credential_fill(&git, Some(auth), dir.path(), "https://github.com/a/b.git").unwrap();

        assert!(
            credentials.contains("username=x-access-token\n"),
            "{credentials}"
        );
        assert!(credentials.contains("password=gho_secret\n"));
        // The token is in no argument, only in the variable the helper reads.
        let (command, display) = git
            .command(Location::Nowhere, dir.path(), dir.path(), Some(auth), &[])
            .args(["credential", "fill"])
            .into_command();
        assert!(!display.contains("gho_secret"));
        assert!(
            command
                .get_args()
                .all(|arg| !arg.to_string_lossy().contains("gho_secret"))
        );
        assert!(CREDENTIAL_HELPER.contains(&format!("${TOKEN_VAR}")));
    }

    #[test]
    fn the_token_goes_to_no_other_host_or_proxy() {
        let hostile = Hostile::new();
        let git = Git::with_env(Git::find().unwrap().program, hostile.env.clone()).unwrap();
        let token = Token::new("gho_secret");
        let auth = Auth {
            remote: GITHUB,
            token: &token,
        };

        for url in [
            // A proxy whose URL names a user and no password.
            "http://alice@proxy.example:3128",
            "https://alice@proxy.example:3128",
            // Another host, such as one a request was redirected to.
            "https://example.com/a/b.git",
            "https://github.com.example.com/a/b.git",
            "https://gist.github.com/a.git",
            // github.com, but not over HTTPS.
            "http://github.com/a/b.git",
            "https://github.com:8443/a/b.git",
        ] {
            let error = credential_fill(&git, Some(auth), &hostile.home, url).unwrap_err();
            assert!(
                error.to_string().contains("terminal prompts disabled"),
                "{url}: {error}"
            );
        }
        // Nor does any askpass the environment names run.
        assert_eq!(hostile.programs_run(), Vec::<String>::new());
    }

    #[test]
    fn a_token_that_would_split_the_helpers_answer_isnt_used() {
        let dir = TempDir::new().unwrap();
        let token = Token::new("gho_secret\nhost=example.com");
        let auth = Auth {
            remote: GITHUB,
            token: &token,
        };

        let error = credential_fill(
            &Git::find().unwrap(),
            Some(auth),
            dir.path(),
            "https://github.com/a/b.git",
        )
        .unwrap_err();

        assert!(error.to_string().contains("terminal prompts disabled"));
        assert!(Token::new(" gho_ok\n").is_usable());
        assert!(!Token::new("").is_usable());
        assert!(!Token::new("a b").is_usable());
    }

    #[test]
    fn without_a_token_git_never_prompts() {
        let hostile = Hostile::new();
        let git = Git::with_env(Git::find().unwrap().program, hostile.env.clone()).unwrap();

        let error =
            credential_fill(&git, None, &hostile.home, "https://github.com/a/b.git").unwrap_err();

        assert!(
            error.to_string().contains("terminal prompts disabled"),
            "{error}"
        );
        // Not GIT_ASKPASS's or SSH_ASKPASS's helper either.
        assert_eq!(hostile.programs_run(), Vec::<String>::new());
    }

    #[test]
    fn proxy_passwords_stay_out_of_debug_output() {
        let git = Git::with_env(
            Git::find().unwrap().program,
            vars(&[
                ("PATH", "/usr/bin:/bin"),
                ("https_proxy", "http://alice:hunter2@proxy:3128"),
                ("ALL_PROXY", "socks5://bob:swordfish@proxy:1080"),
            ]),
        )
        .unwrap();

        let debug = format!("{git:?}");

        assert!(!debug.contains("hunter2"), "{debug}");
        assert!(!debug.contains("swordfish"), "{debug}");
        assert!(debug.contains("https_proxy"), "{debug}");
        assert!(debug.contains("/usr/bin:/bin"), "{debug}");
    }

    #[test]
    fn tokens_stay_out_of_debug_output() {
        let token = Token::new("gho_secret");
        assert_eq!(format!("{token:?}"), "Token(redacted)");
        assert_eq!(token.expose(), "gho_secret");
    }
}
