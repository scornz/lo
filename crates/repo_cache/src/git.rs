//! Running git: finding it, checking its version, and running it in an environment of rozed's own.
//!
//! rozed runs the git CLI as a child process. Every git it runs gets the same clean environment:
//!
//! - Only `PATH`, `TMPDIR` and the network variables (proxies and CA bundles) come from rozed's
//!   own environment. Everything else is dropped, including every `GIT_*` variable, so nothing
//!   points git at another repository, adds config (`GIT_CONFIG_PARAMETERS`) or turns off TLS
//!   checks.
//! - `HOME` is a folder of rozed's, so git and its HTTP library read none of the user's files there
//!   (`~/.gitconfig`, `~/.config/git/attributes`, `~/.netrc`).
//! - No system or global config (`GIT_CONFIG_NOSYSTEM`, `GIT_CONFIG_GLOBAL=/dev/null`), no
//!   credential helper, and no prompts (`GIT_TERMINAL_PROMPT=0`). The only credential git gets is
//!   the token rozed hands it through `GIT_ASKPASS` (see [`crate::askpass`]).
//! - `LC_ALL=C`, so the messages rozed reads are in English.
//!
//! The user's git config is never read or written, and nothing in it (URL rewrites, credential
//! helpers, filters, hooks) changes what rozed fetches or runs.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::Instant;

use crate::Error;
use crate::askpass;

/// A git version, from `git --version`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GitVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl GitVersion {
    /// The oldest git rozed runs: 2.40 added `git merge-tree --write-tree --merge-base`, which
    /// rebase-aware comparison needs.
    pub const MINIMUM: GitVersion = GitVersion {
        major: 2,
        minor: 40,
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

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(redacted)")
    }
}

/// The git rozed runs.
#[derive(Clone, Debug)]
pub struct Git {
    program: PathBuf,
    version: GitVersion,
    /// The variables every git inherits from rozed's environment (see [`inherited_env`]).
    inherited: Vec<(OsString, OsString)>,
}

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

/// The variable the askpass helper reads the token from. It's set only on git children that
/// sign in.
pub(crate) const TOKEN_VAR: &str = "ROZED_GIT_TOKEN";

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
    fn with_env(program: PathBuf, env: Vec<(OsString, OsString)>) -> Result<Self, Error> {
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

    /// A git command with rozed's environment (see the module docs), running in `dir`. `home` is
    /// the empty folder git gets as its `HOME`. With `auth`, git signs in with its token when a
    /// server asks.
    pub(crate) fn command(&self, dir: &Path, home: &Path, auth: Option<Auth<'_>>) -> GitCommand {
        let mut command = Command::new(&self.program);
        command
            .env_clear()
            .envs(self.inherited.iter().map(|(name, value)| (name, value)))
            .env("HOME", home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C")
            .current_dir(dir)
            .stdin(Stdio::null());
        if let Some(auth) = auth {
            command
                .env("GIT_ASKPASS", auth.askpass)
                .env(TOKEN_VAR, auth.token.expose());
        }
        command.args([
            // No credential helper, so the token from askpass is the only credential.
            "-c",
            "credential.helper=",
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
        ]);
        GitCommand {
            command,
            args: Vec::new(),
        }
    }
}

/// How git signs in: the askpass helper, and the token it answers with.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Auth<'a> {
    pub askpass: &'a Path,
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

/// Makes `askpass` available to [`Git::command`] callers in this crate's tests.
#[cfg(test)]
pub(crate) fn test_askpass(dir: &Path) -> PathBuf {
    askpass::install(dir).unwrap()
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
    fn needs_git_2_40() {
        assert_eq!(GitVersion::MINIMUM, version(2, 40, 0));
        assert!(version(2, 39, 9) < GitVersion::MINIMUM);
        assert!(version(2, 40, 0) >= GitVersion::MINIMUM);
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
    fn refuses_a_git_older_than_2_40() {
        let dir = TempDir::new().unwrap();
        let program = fake_git(dir.path(), "2.39.5 (Apple Git-154)");
        let error = Git::at(&program).unwrap_err();
        let Error::GitTooOld { found, .. } = &error else {
            panic!("{error:?}");
        };
        assert_eq!(*found, version(2, 39, 5));
        let message = error.to_string();
        assert!(message.contains("git 2.40.0 or newer"), "{message}");
        assert!(message.contains("is git 2.39.5"), "{message}");
        assert!(message.contains("xcode-select --install"), "{message}");

        assert_eq!(
            Git::at(fake_git(dir.path(), "2.40.0")).unwrap().version(),
            version(2, 40, 0)
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

    /// A bare repository with one commit on `trunk`, served over `file://`.
    fn remote(dir: &Path) -> String {
        let work = dir.join("work");
        fs::create_dir(&work).unwrap();
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .args(args)
                .current_dir(&work)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_AUTHOR_NAME", "rozed test")
                .env("GIT_AUTHOR_EMAIL", "test@rozed.invalid")
                .env("GIT_COMMITTER_NAME", "rozed test")
                .env("GIT_COMMITTER_EMAIL", "test@rozed.invalid")
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        };
        git(&["init", "-q", "-b", "trunk"]);
        fs::write(work.join("a.txt"), "a\n").unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "-q", "-m", "first"]);
        git(&["clone", "-q", "--bare", ".", "../remote.git"]);
        format!("file://{}", dir.join("remote.git").display())
    }

    /// The URL rewrite a hostile config applies: every `file://` URL goes nowhere.
    const REWRITE: &str = "[url \"file:///nowhere/\"]\n\tinsteadOf = file://\n";

    #[test]
    fn hostile_config_and_variables_have_no_effect() {
        let dir = TempDir::new().unwrap();
        let url = remote(dir.path());

        // The user's config, everywhere git might look for it.
        let user_home = dir.path().join("user-home");
        fs::create_dir_all(user_home.join(".config/git")).unwrap();
        fs::write(user_home.join(".gitconfig"), REWRITE).unwrap();
        fs::write(user_home.join(".config/git/config"), REWRITE).unwrap();
        fs::write(user_home.join(".config/git/attributes"), "* binary\n").unwrap();
        let global = dir.path().join("global-config");
        fs::write(&global, REWRITE).unwrap();
        let hostile = vars(&[
            ("PATH", "/usr/bin:/bin"),
            ("HOME", user_home.to_str().unwrap()),
            (
                "XDG_CONFIG_HOME",
                user_home.join(".config").to_str().unwrap(),
            ),
            ("GIT_CONFIG_GLOBAL", global.to_str().unwrap()),
            (
                "GIT_CONFIG_PARAMETERS",
                "'url.file:///nowhere/.insteadof'='file://'",
            ),
            ("GIT_CONFIG_COUNT", "1"),
            ("GIT_CONFIG_KEY_0", "url.file:///nowhere/.insteadOf"),
            ("GIT_CONFIG_VALUE_0", "file://"),
            ("GIT_DIR", "/nowhere/.git"),
        ]);
        let git = Git::with_env(Git::find().unwrap().program, hostile).unwrap();
        let home = dir.path().join("rozed-home");
        fs::create_dir(&home).unwrap();

        // Without the rewrite, the remote is reachable.
        let refs = git
            .command(dir.path(), &home, None)
            .args(["ls-remote", &url])
            .run()
            .unwrap();
        assert!(
            String::from_utf8(refs)
                .unwrap()
                .contains("refs/heads/trunk")
        );

        // git sees no config but rozed's own.
        let config = git
            .command(dir.path(), &home, None)
            .args(["config", "--list", "--show-origin"])
            .run()
            .unwrap();
        let config = String::from_utf8(config).unwrap();
        assert!(
            config.lines().all(|line| line.starts_with("command line:")),
            "{config}"
        );
        assert!(!config.contains("insteadof"), "{config}");
        assert!(config.contains("credential.helper="), "{config}");

        // Nor the user's attributes.
        let attributes = git
            .command(&dir.path().join("work"), &home, None)
            .args(["check-attr", "binary", "--", "a.txt"])
            .run()
            .unwrap();
        assert_eq!(
            String::from_utf8(attributes).unwrap(),
            "a.txt: binary: unspecified\n"
        );
    }

    #[test]
    fn askpass_answers_both_prompts() {
        let dir = TempDir::new().unwrap();
        let askpass = test_askpass(dir.path());
        let ask = |prompt: &str| {
            let output = Command::new(&askpass)
                .arg(prompt)
                .env_clear()
                .env(TOKEN_VAR, "gho_secret")
                .output()
                .unwrap();
            (
                output.status.success(),
                String::from_utf8(output.stdout).unwrap(),
            )
        };
        assert_eq!(
            ask("Username for 'https://github.com': "),
            (true, "x-access-token\n".into())
        );
        assert_eq!(
            ask("Password for 'https://x-access-token@github.com': "),
            (true, "gho_secret\n".into())
        );
        assert!(!ask("Are you sure you want to continue connecting (yes/no)? ").0);

        // The helper holds no token itself.
        assert!(!fs::read_to_string(&askpass).unwrap().contains("gho_secret"));
        let mode = fs::metadata(&askpass).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    /// Asks git for github.com's credentials, as it does when a server answers 401.
    fn credential_fill(auth: Option<Auth<'_>>, home: &Path) -> Result<Vec<u8>, Error> {
        Git::find()
            .unwrap()
            .command(home, home, auth)
            .args(["credential", "fill"])
            .run_with_input(Some(b"protocol=https\nhost=github.com\n\n"))
    }

    #[test]
    fn git_signs_in_with_the_token_from_askpass() {
        let dir = TempDir::new().unwrap();
        let askpass = test_askpass(dir.path());
        let token = Token::new("gho_secret\n");
        let auth = Auth {
            askpass: &askpass,
            token: &token,
        };

        let command = Git::find()
            .unwrap()
            .command(dir.path(), dir.path(), Some(auth))
            .args(["credential", "fill"]);
        assert!(!command.display().contains("gho_secret"));
        let (command, _) = command.into_command();
        assert!(
            command
                .get_args()
                .all(|arg| !arg.to_string_lossy().contains("gho_secret"))
        );

        let credentials =
            String::from_utf8(credential_fill(Some(auth), dir.path()).unwrap()).unwrap();
        assert!(
            credentials.contains("username=x-access-token\n"),
            "{credentials}"
        );
        assert!(credentials.contains("password=gho_secret\n"));
    }

    #[test]
    fn without_a_token_git_never_prompts() {
        let dir = TempDir::new().unwrap();
        let error = credential_fill(None, dir.path()).unwrap_err();
        assert!(
            error.to_string().contains("terminal prompts disabled"),
            "{error}"
        );
    }

    #[test]
    fn tokens_stay_out_of_debug_output() {
        let token = Token::new("gho_secret");
        assert_eq!(format!("{token:?}"), "Token(redacted)");
        assert_eq!(token.expose(), "gho_secret");
    }
}
