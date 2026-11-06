//! Importing accounts from the `gh` CLI.
//!
//! rozed asks `gh` itself rather than reading its files or Keychain items, whose format is `gh`'s
//! own business: `gh auth status --json hosts` lists the accounts without their tokens, and
//! `gh auth token --user <login>` prints one account's token, which rozed reads through a pipe.
//! `GH_TOKEN` and `GITHUB_TOKEN` are removed from `gh`'s environment, so it reports the accounts
//! it stores rather than whatever token the environment holds.

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow, bail};
use http_client::HttpClient;
use serde::Deserialize;

use crate::client::GithubClient;
use crate::credentials::HOST;
use crate::token::Token;
use crate::user::{AuthenticatedUser, missing_scopes, parse_scopes};

/// Set to the path of a `gh` executable to use that one.
pub const GH_PATH_ENV: &str = "GH_PATH";

/// Variables that make `gh` use a token from the environment instead of its stored accounts.
const TOKEN_ENV: &[&str] = &[
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
];

/// Where to look for `gh`, in order. An app started from the Finder gets launchd's minimal
/// `PATH`, without Homebrew, so rozed looks in the usual places itself.
pub fn gh_candidates(gh_path: Option<OsString>, home: Option<PathBuf>) -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = gh_path.into_iter().map(PathBuf::from).collect();
    candidates.push("/opt/homebrew/bin/gh".into());
    candidates.push("/usr/local/bin/gh".into());
    if let Some(home) = home {
        candidates.push(home.join(".local/bin/gh"));
    }
    candidates
}

fn is_executable(path: &Path) -> bool {
    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

/// Finds `gh`: `$GH_PATH`, Homebrew's and `~/.local/bin`, then the login shell's `PATH`.
pub fn find_gh() -> Option<PathBuf> {
    let candidates = gh_candidates(
        std::env::var_os(GH_PATH_ENV),
        std::env::var_os("HOME").map(PathBuf::from),
    );
    if let Some(found) = candidates.into_iter().find(|path| is_executable(path)) {
        return Some(found);
    }
    let shell = std::env::var_os("SHELL").unwrap_or_else(|| "/bin/zsh".into());
    let output = Command::new(shell)
        .args(["-lc", "command -v gh"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let path = PathBuf::from(String::from_utf8(output.stdout).ok()?.trim());
    is_executable(&path).then_some(path)
}

/// `gh` with an environment that can't override its stored accounts and never prompts.
pub fn gh_command(gh: &Path) -> Command {
    let mut command = Command::new(gh);
    for name in TOKEN_ENV {
        command.env_remove(name);
    }
    command
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null());
    command
}

/// An account `gh` is signed in to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhAccount {
    pub login: String,
    /// Whether it's `gh`'s active account for github.com.
    pub active: bool,
    /// `success` when `gh` could use the token just now.
    pub state: String,
    pub scopes: Vec<String>,
    /// A short-lived token that `gh` refreshes; rozed's copy expires within hours.
    pub refreshable: bool,
}

impl GhAccount {
    pub fn is_usable(&self) -> bool {
        self.state == "success"
    }

    /// rozed's scopes the token lacks. `gh` doesn't ask for `workflow` unless told to, so its
    /// tokens can't merge PRs that change GitHub Actions workflows.
    pub fn missing_scopes(&self) -> Vec<&'static str> {
        missing_scopes(&self.scopes)
    }
}

#[derive(Deserialize)]
struct StatusJson {
    #[serde(default)]
    hosts: std::collections::HashMap<String, Vec<StatusEntry>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StatusEntry {
    login: String,
    #[serde(default)]
    active: bool,
    #[serde(default)]
    state: String,
    #[serde(default)]
    scopes: Option<String>,
    #[serde(default)]
    token_source: String,
    #[serde(default)]
    refreshable: bool,
}

/// Parses `gh auth status --json hosts`, skipping tokens that come from the environment.
pub fn parse_status(json: &[u8]) -> Result<Vec<GhAccount>> {
    let status: StatusJson =
        serde_json::from_slice(json).context("gh's account list isn't the JSON rozed expects")?;
    let mut accounts: Vec<GhAccount> = Vec::new();
    for entry in status.hosts.get(HOST).into_iter().flatten() {
        if TOKEN_ENV.contains(&entry.token_source.as_str())
            || accounts
                .iter()
                .any(|account| account.login.eq_ignore_ascii_case(&entry.login))
        {
            continue;
        }
        accounts.push(GhAccount {
            login: entry.login.clone(),
            active: entry.active,
            state: entry.state.clone(),
            scopes: parse_scopes(entry.scopes.as_deref()),
            refreshable: entry.refreshable,
        });
    }
    Ok(accounts)
}

fn run(mut command: Command, what: &str) -> Result<Vec<u8>> {
    let output = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("running gh to {what}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        if stderr.contains("unknown flag: --json") {
            bail!("rozed needs gh 2.80 or later to list its accounts. Update gh and try again.");
        }
        bail!("gh couldn't {what}: {stderr}");
    }
    Ok(output.stdout)
}

/// The github.com accounts `gh` is signed in to. Blocks while `gh` runs.
pub fn list_accounts(gh: &Path) -> Result<Vec<GhAccount>> {
    let mut command = gh_command(gh);
    command.args(["auth", "status", "--json", "hosts", "--hostname", HOST]);
    parse_status(&run(command, "list its accounts")?)
}

/// The token `gh` holds for `login`. Blocks while `gh` runs. The token is read from a pipe and
/// never logged.
pub fn read_token(gh: &Path, login: &str) -> Result<Token> {
    let mut command = gh_command(gh);
    command.args(["auth", "token", "--hostname", HOST, "--user", login]);
    let stdout = run(command, &format!("read {login}'s token"))?;
    let token =
        Token::new(String::from_utf8(stdout).map_err(|_| anyhow!("gh's token isn't text"))?);
    if token.is_empty() {
        bail!("gh has no token for {login}");
    }
    Ok(token)
}

/// Reads `login`'s token from `gh` and checks with GitHub that it belongs to `login`.
pub async fn import_account(
    http: Arc<dyn HttpClient>,
    gh: PathBuf,
    login: String,
    executor: gpui::BackgroundExecutor,
) -> Result<(AuthenticatedUser, Token)> {
    let token = executor
        .spawn({
            let login = login.clone();
            async move { read_token(&gh, &login) }
        })
        .await?;
    let user = GithubClient::new(http, token.clone())
        .user()
        .await
        .with_context(|| format!("checking gh's token for {login}"))?;
    if !user.user.login.eq_ignore_ascii_case(&login) {
        bail!("gh's token for {login} belongs to {}", user.user.login);
    }
    Ok((user, token))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_in_gh_path_then_homebrew_then_local_bin() {
        assert_eq!(
            gh_candidates(Some("/custom/gh".into()), Some("/Users/me".into())),
            [
                PathBuf::from("/custom/gh"),
                "/opt/homebrew/bin/gh".into(),
                "/usr/local/bin/gh".into(),
                "/Users/me/.local/bin/gh".into(),
            ]
        );
        assert_eq!(gh_candidates(None, None).len(), 2);
    }

    #[test]
    fn gh_runs_without_environment_tokens_or_prompts() {
        let command = gh_command(Path::new("/opt/homebrew/bin/gh"));
        let envs: Vec<(String, Option<String>)> = command
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        for name in TOKEN_ENV {
            assert!(
                envs.contains(&(name.to_string(), None)),
                "{name} isn't removed"
            );
        }
        assert!(envs.contains(&("GH_PROMPT_DISABLED".into(), Some("1".into()))));
        assert!(envs.contains(&("NO_COLOR".into(), Some("1".into()))));
    }

    #[test]
    fn parses_gh_auth_status() {
        // As gh 2.103.3 prints it, plus an account whose token is from GH_TOKEN, which gh
        // reports as an extra entry.
        let json = br#"{"hosts":{"github.com":[
            {"state":"success","active":true,"host":"github.com","login":"rozed-bot","tokenSource":"/tmp/gh/hosts.yml","scopes":"delete_repo, read:org, repo, workflow","gitProtocol":"https"},
            {"state":"success","active":false,"host":"github.com","login":"other","tokenSource":"keyring","scopes":"gist, read:org, repo","gitProtocol":"https","refreshable":true,"tokenExpiresAt":"2026-11-06T17:00:00Z"},
            {"state":"error","active":false,"host":"github.com","login":"broken","tokenSource":"keyring","gitProtocol":"https"},
            {"state":"success","active":true,"host":"github.com","login":"from-env","tokenSource":"GH_TOKEN","scopes":"repo"}
        ],"ghe.example.com":[{"state":"success","login":"enterprise","tokenSource":"keyring"}]}}"#;
        let accounts = parse_status(json).unwrap();
        let logins: Vec<_> = accounts.iter().map(|a| a.login.as_str()).collect();
        assert_eq!(logins, ["rozed-bot", "other", "broken"]);
        assert!(accounts[0].active && accounts[0].is_usable());
        assert!(accounts[0].missing_scopes().is_empty());
        assert_eq!(accounts[1].missing_scopes(), ["workflow"]);
        assert!(accounts[1].refreshable);
        assert!(!accounts[2].is_usable());
        assert!(parse_status(b"not json").is_err());
    }
}
