//! Importing accounts from a fake `gh`: a shell script that answers the two commands rozed runs
//! and fails if its environment would let a token override its accounts.

use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;

use github::gh_import;
use github::replay::ReplayHttpClient;
use gpui::TestAppContext;

const FAKE_GH: &str = r#"#!/bin/sh
if [ -n "${GH_TOKEN+set}" ] || [ -n "${GITHUB_TOKEN+set}" ]; then
  echo "a token is set in gh's environment" >&2
  exit 3
fi
if [ "$GH_PROMPT_DISABLED" != 1 ]; then
  echo "gh could prompt" >&2
  exit 4
fi
case "$*" in
  "auth status --json hosts --hostname github.com")
    cat "$(dirname "$0")/status.json" ;;
  "auth token --hostname github.com --user rozed-bot")
    printf 'gho_FAKE_FROM_GH\n' ;;
  "auth token --hostname github.com --user someone-else")
    printf 'gho_FAKE_SOMEONE_ELSE\n' ;;
  *)
    echo "no oauth token found for github.com" >&2
    exit 1 ;;
esac
"#;

const STATUS: &str = r#"{"hosts":{"github.com":[
  {"state":"success","active":true,"host":"github.com","login":"rozed-bot","tokenSource":"keyring","scopes":"read:org, repo, workflow","gitProtocol":"https"},
  {"state":"success","active":false,"host":"github.com","login":"someone-else","tokenSource":"keyring","scopes":"gist, read:org, repo","gitProtocol":"https"}
]}}"#;

fn fake_gh() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let gh = dir.path().join("gh");
    std::fs::write(&gh, FAKE_GH).unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(dir.path().join("status.json"), STATUS).unwrap();
    (dir, gh)
}

#[test]
fn lists_ghs_accounts() {
    let (_dir, gh) = fake_gh();
    let accounts = gh_import::list_accounts(&gh).unwrap();
    let summary: Vec<_> = accounts
        .iter()
        .map(|a| (a.login.as_str(), a.active, a.missing_scopes()))
        .collect();
    assert_eq!(
        summary,
        [
            ("rozed-bot", true, vec![]),
            ("someone-else", false, vec!["workflow"])
        ]
    );
}

#[test]
fn reads_a_token_through_a_pipe() {
    let (_dir, gh) = fake_gh();
    let token = gh_import::read_token(&gh, "rozed-bot").unwrap();
    assert_eq!(token.expose(), "gho_FAKE_FROM_GH");
}

#[test]
fn shows_ghs_error_for_an_unknown_account() {
    let (_dir, gh) = fake_gh();
    let error = gh_import::read_token(&gh, "nobody").unwrap_err();
    assert_eq!(
        error.to_string(),
        "gh couldn't read nobody's token: no oauth token found for github.com"
    );
}

#[gpui::test]
async fn imports_an_account_after_checking_its_token(cx: &mut TestAppContext) {
    let (_dir, gh) = fake_gh();
    let http = ReplayHttpClient::cassette("user");
    let (user, token) =
        gh_import::import_account(http.clone(), gh, "rozed-bot".into(), cx.executor())
            .await
            .unwrap();
    assert_eq!(user.user.login, "rozed-bot");
    assert_eq!(token.expose(), "gho_FAKE_FROM_GH");
    assert_eq!(
        http.seen()[0].header("authorization"),
        Some("token gho_FAKE_FROM_GH")
    );
}

#[gpui::test]
async fn refuses_a_token_that_belongs_to_someone_else(cx: &mut TestAppContext) {
    let (_dir, gh) = fake_gh();
    // GitHub says the token is rozed-bot's.
    let http = ReplayHttpClient::cassette("user");
    let error = gh_import::import_account(http, gh, "someone-else".into(), cx.executor())
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "gh's token for someone-else belongs to rozed-bot"
    );
}
