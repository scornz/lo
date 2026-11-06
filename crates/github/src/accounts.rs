//! The signed-in accounts: their tokens, their database rows, and which one opens a PR.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, anyhow, bail};
use credentials_provider::CredentialsProvider;
use futures::StreamExt as _;
use futures::channel::{mpsc, oneshot};
use gpui::{App, AppContext as _, Context, Entity, Global, Task};
use http_client::HttpClient;

use crate::client::{GithubClient, OnUnauthorized};
use crate::credentials::{HOST, server_for};
use crate::db::{Account, AccountSource, RozedDb};
use crate::error::GithubError;
use crate::gh_import;
use crate::poll::RequestQueue;
use crate::token::Token;
use crate::user::AuthenticatedUser;

/// Every account rozed is signed in to, as a GPUI model. Views observe it to redraw when an
/// account is added, removed or signed out.
///
/// Its clients handle a `401` themselves: GitHub refusing a token signs the account out, after
/// one more read of the token from `gh` for an account imported from it ([`Self::refused`]).
pub struct Accounts {
    db: RozedDb,
    store: Arc<dyn CredentialsProvider>,
    http: Arc<dyn HttpClient>,
    accounts: Vec<Account>,
    /// Lowercased logins whose token GitHub refused, or whose token is missing.
    signed_out: HashMap<String, SignedOut>,
    /// Clients for accounts whose token has been read, by lowercased login.
    clients: HashMap<String, GithubClient>,
    /// Where clients report a refused token. [`Self::refused`] handles one at a time.
    refusals: mpsc::UnboundedSender<Refusal>,
    /// The `gh` to re-read tokens from, when not searched for.
    gh: Option<PathBuf>,
    _handle_refusals: Task<()>,
}

/// Why an account is signed out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignedOut {
    /// GitHub refused its token (a `401`).
    Refused,
    /// Its token isn't in the token store any more.
    TokenMissing,
}

/// A client's report that GitHub refused `token`, waiting for a token to retry with.
struct Refusal {
    login: String,
    token: Token,
    reply: oneshot::Sender<Option<Token>>,
}

struct GlobalAccounts(Entity<Accounts>);

impl Global for GlobalAccounts {}

/// Which account to open an owner's PR with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountChoice {
    Account(String),
    /// Several accounts can see the repo and none of them was chosen for it: ask, then remember
    /// the answer for the owner, or for the repo alone when the owner's account is another one
    /// (see [`Remembered::owner`]).
    Ask(Vec<String>),
    /// The account remembered for the repo, its own or its owner's, can't check the repo just
    /// now: it's signed out, its token can't be read, or GitHub didn't answer yes or "not
    /// found". The problems say why. rozed doesn't fall back to another account, which would
    /// review as someone else.
    Unavailable(String),
    /// No signed-in account can see it, as far as GitHub said.
    None,
}

/// The accounts remembered for a repository: one chosen for it alone, because the owner's
/// couldn't see it, and the one chosen for its owner. Removed accounts aren't remembered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Remembered {
    pub repo: Option<String>,
    pub owner: Option<String>,
}

impl Remembered {
    /// The account that opens the repo, which is never swapped for another: its own, else its
    /// owner's.
    pub fn chosen(&self) -> Option<&str> {
        self.repo.as_deref().or(self.owner.as_deref())
    }

    /// The remembered account among `logins`, the repo's own first.
    pub fn pick<'a>(&self, logins: &'a [String]) -> Option<&'a String> {
        let find = |login: &str| logins.iter().find(|l| l.eq_ignore_ascii_case(login));
        self.repo
            .as_deref()
            .and_then(find)
            .or_else(|| self.owner.as_deref().and_then(find))
    }
}

/// What [`Accounts::account_for_repo`] found.
#[derive(Debug)]
pub struct RepoAccount {
    /// The repo's owner and name as GitHub names it now (its `full_name`), which can differ from
    /// those asked about after a rename or transfer. As asked when no account could see it.
    pub owner: String,
    pub name: String,
    pub choice: AccountChoice,
    /// The accounts remembered for the repo, by the name GitHub gives it now.
    pub remembered: Remembered,
    /// The accounts GitHub gave an answer other than yes or "not found", with its error: a
    /// refused token, a rate limit, single sign-on, an org's OAuth restriction, a server error,
    /// or no answer at all while another account got one. [`GithubError::help_link`] has
    /// GitHub's link to fix it, where there is one.
    pub problems: Vec<(String, GithubError)>,
}

/// Picks among the accounts that can see a repo: the only one, else the one remembered for it,
/// its own before its owner's, else ask.
pub fn choose_account(visible: &[String], remembered: &Remembered) -> AccountChoice {
    match visible {
        [] => AccountChoice::None,
        [only] => AccountChoice::Account(only.clone()),
        several => match remembered.pick(several) {
            Some(login) => AccountChoice::Account(login.clone()),
            None => AccountChoice::Ask(several.to_vec()),
        },
    }
}

/// The owner and name in a `/repos/{owner}/{repo}` answer's `full_name`.
fn full_name(answer: &serde_json::Value) -> Option<(String, String)> {
    let (owner, name) = answer.get("full_name")?.as_str()?.split_once('/')?;
    check_repo_name(owner, name).ok()?;
    Some((owner.to_string(), name.to_string()))
}

/// Refuses an owner or repository name GitHub wouldn't give, before it goes into a path.
fn check_repo_name(owner: &str, repo: &str) -> Result<()> {
    let is_name = |name: &str, extra: &[char]| {
        !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || extra.contains(&c))
    };
    if !is_name(owner, &[]) || !is_name(repo, &['.']) || repo == "." || repo == ".." {
        bail!("{owner}/{repo} isn't a GitHub repository");
    }
    Ok(())
}

fn key(login: &str) -> String {
    login.to_ascii_lowercase()
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default()
}

impl Accounts {
    /// Reads the accounts from the database.
    pub fn load(
        db: RozedDb,
        store: Arc<dyn CredentialsProvider>,
        http: Arc<dyn HttpClient>,
        cx: &mut App,
    ) -> Result<Entity<Self>> {
        let accounts = db.accounts().context("reading accounts")?;
        Ok(cx.new(|cx| {
            let (refusals, mut received) = mpsc::unbounded::<Refusal>();
            let handle_refusals = cx.spawn(async move |this: gpui::WeakEntity<Self>, cx| {
                while let Some(refusal) = received.next().await {
                    let Ok(retry) = this.update(cx, |this, cx| {
                        this.refused(&refusal.login, refusal.token, cx)
                    }) else {
                        break;
                    };
                    refusal.reply.send(retry.await).ok();
                }
            });
            Self {
                db,
                store,
                http,
                accounts,
                signed_out: HashMap::new(),
                clients: HashMap::new(),
                refusals,
                gh: None,
                _handle_refusals: handle_refusals,
            }
        }))
    }

    /// Re-reads `gh` accounts' tokens from this `gh` instead of searching for one.
    pub fn set_gh_path(&mut self, gh: PathBuf) {
        self.gh = Some(gh);
    }

    /// A client for `login` whose refused tokens come back to [`Self::refused`].
    fn new_client(&self, login: &str, token: Token) -> GithubClient {
        let refusals = self.refusals.clone();
        let login = login.to_string();
        let hook: OnUnauthorized = Arc::new(move |token| {
            let (reply, retry) = oneshot::channel();
            let refusal = Refusal {
                login: login.clone(),
                token,
                reply,
            };
            let sent = refusals.unbounded_send(refusal).is_ok();
            Box::pin(async move {
                if sent {
                    retry.await.ok().flatten()
                } else {
                    None
                }
            })
        });
        GithubClient::new(self.http.clone(), token).with_unauthorized_hook(hook)
    }

    pub fn global(cx: &App) -> Option<Entity<Self>> {
        cx.try_global::<GlobalAccounts>()
            .map(|global| global.0.clone())
    }

    pub fn set_global(accounts: Entity<Self>, cx: &mut App) {
        cx.set_global(GlobalAccounts(accounts));
    }

    pub fn http(&self) -> Arc<dyn HttpClient> {
        self.http.clone()
    }

    pub fn accounts(&self) -> &[Account] {
        &self.accounts
    }

    pub fn account(&self, login: &str) -> Option<&Account> {
        self.accounts
            .iter()
            .find(|account| account.login.eq_ignore_ascii_case(login))
    }

    pub fn is_signed_out(&self, login: &str) -> bool {
        self.signed_out.contains_key(&key(login))
    }

    /// Why the account is signed out, if it is.
    pub fn signed_out(&self, login: &str) -> Option<SignedOut> {
        self.signed_out.get(&key(login)).copied()
    }

    /// Stores the token and adds the account, replacing any account with the same login.
    ///
    /// Dropping the returned task doesn't stop it: the token and the row are always both written
    /// or, when the row can't be written for a new account, neither.
    pub fn add(
        &mut self,
        user: AuthenticatedUser,
        token: Token,
        source: AccountSource,
        cx: &mut Context<Self>,
    ) -> Task<Result<Account>> {
        let store = self.store.clone();
        let db = self.db.clone();
        let existing = self.account(&user.user.login).cloned();
        let added_at = existing
            .as_ref()
            .map(|existing| existing.added_at)
            .unwrap_or_else(now);
        let account = Account {
            host: HOST.to_string(),
            login: user.user.login.clone(),
            user_id: user.user.id,
            name: user.user.name.clone(),
            avatar_url: user.user.avatar_url.clone(),
            scopes: user.scopes.clone(),
            source,
            added_at,
        };
        let (done, result) = oneshot::channel();
        cx.spawn(async move |this, cx| {
            let added = async {
                let server = server_for(&account.login);
                store
                    .write_credentials(&server, &account.login, token.expose().as_bytes(), cx)
                    .await
                    .context("storing the token")?;
                if let Err(error) = db.save_account(account.clone()).await {
                    // A login rozed already had keeps its row, which the new token still fits.
                    if existing.is_none()
                        && let Err(delete_error) = store.delete_credentials(&server, cx).await
                    {
                        log::warn!(
                            "couldn't delete {}'s token after failing to save the account: \
                             {delete_error:#}",
                            account.login
                        );
                    }
                    return Err(error.context("saving the account"));
                }
                this.update(cx, |this, cx| {
                    let login = key(&account.login);
                    this.accounts
                        .retain(|existing| key(&existing.login) != login);
                    this.accounts.push(account.clone());
                    this.accounts.sort_by_key(|a| (a.added_at, key(&a.login)));
                    this.signed_out.remove(&login);
                    // The account's clients share their token, so they all switch to the new
                    // one. Any handed out before a sign-out take it when GitHub refuses theirs.
                    match this.clients.get(&login) {
                        Some(client) => client.set_token(token),
                        None => {
                            let client = this.new_client(&account.login, token);
                            this.clients.insert(login, client);
                        }
                    }
                    cx.notify();
                })?;
                Ok(account)
            }
            .await;
            done.send(added).ok();
        })
        .detach();
        cx.background_spawn(async move {
            result
                .await
                .unwrap_or_else(|_| Err(anyhow!("rozed quit while adding the account")))
        })
    }

    /// Deletes the account's token and forgets the account. The account is forgotten even when
    /// its token is already gone or can't be deleted; the error then says the token is left.
    pub fn remove(&mut self, login: &str, cx: &mut Context<Self>) -> Task<Result<()>> {
        let store = self.store.clone();
        let db = self.db.clone();
        let login = login.to_string();
        cx.spawn(async move |this, cx| {
            let server = server_for(&login);
            // The Keychain fails to delete an item that isn't there, so a failed delete only
            // counts if the token can still be read.
            let token_left = match store.delete_credentials(&server, cx).await {
                Ok(()) => None,
                Err(error) => match store.read_credentials(&server, cx).await {
                    Ok(None) => None,
                    Ok(Some(_)) | Err(_) => Some(error),
                },
            };
            db.delete_account(HOST.to_string(), login.clone())
                .await
                .context("removing the account")?;
            this.update(cx, |this, cx| {
                let login = key(&login);
                this.accounts.retain(|account| key(&account.login) != login);
                this.signed_out.remove(&login);
                this.clients.remove(&login);
                cx.notify();
            })?;
            match token_left {
                None => Ok(()),
                Some(error) => {
                    Err(error.context(format!("removed {login}, but couldn't delete its token")))
                }
            }
        })
    }

    /// Marks an account signed out: its token was revoked, or is missing. Its tabs ask the user
    /// to sign in again; other accounts keep working (design §11).
    pub fn mark_signed_out(&mut self, login: &str, why: SignedOut, cx: &mut Context<Self>) {
        let login = key(login);
        self.clients.remove(&login);
        if self.signed_out.insert(login, why) != Some(why) {
            cx.notify();
        }
    }

    /// GitHub refused `token`, `login`'s token: returns the token to retry with, or `None`.
    ///
    /// Every client [`Self::client`] hands out calls this on a `401`. A token that has been
    /// replaced since, by signing in again or by an earlier call, is answered with the new one.
    /// An account imported from `gh` gets its token from `gh` once more, since `gh`'s
    /// short-lived tokens expire within hours and `gh` refreshes them; if that fails, or for any
    /// other account, the account is signed out (design §11 "Signed out").
    pub fn refused(
        &mut self,
        login: &str,
        token: Token,
        cx: &mut Context<Self>,
    ) -> Task<Option<Token>> {
        let Some(account) = self.account(login).cloned() else {
            return Task::ready(None);
        };
        if self.is_signed_out(login) {
            return Task::ready(None);
        }
        if let Some(current) = self.clients.get(&key(login)).map(GithubClient::token)
            && current != token
        {
            return Task::ready(Some(current));
        }
        if account.source != AccountSource::Gh {
            log::info!("GitHub refused {login}'s token; signing the account out");
            self.mark_signed_out(login, SignedOut::Refused, cx);
            return Task::ready(None);
        }
        log::info!("GitHub refused {login}'s token; reading it from gh again");
        let (gh, http) = (self.gh.clone(), self.http.clone());
        let login = account.login;
        cx.spawn(async move |this, cx| {
            let executor = cx.background_executor().clone();
            let reread = async {
                let gh = match gh {
                    Some(gh) => gh,
                    None => executor
                        .spawn(async { gh_import::find_gh() })
                        .await
                        .context("rozed couldn't find gh")?,
                };
                let (user, new_token) =
                    gh_import::import_account(http, gh, login.clone(), executor).await?;
                if new_token == token {
                    bail!("gh has no newer token");
                }
                this.update(cx, |this, cx| {
                    this.add(user, new_token.clone(), AccountSource::Gh, cx)
                })?
                .await?;
                anyhow::Ok(new_token)
            }
            .await;
            match reread {
                Ok(new_token) => Some(new_token),
                Err(error) => {
                    log::info!(
                        "couldn't read {login}'s token from gh again ({error:#}); signing the \
                         account out"
                    );
                    this.update(cx, |this, cx| {
                        this.mark_signed_out(&login, SignedOut::Refused, cx)
                    })
                    .ok();
                    None
                }
            }
        })
    }

    /// A client for the account, reading its token from the store the first time.
    pub fn client(&mut self, login: &str, cx: &mut Context<Self>) -> Task<Result<GithubClient>> {
        let login_key = key(login);
        if self.signed_out.contains_key(&login_key) {
            return Task::ready(Err(anyhow!("{login} is signed out")));
        }
        if let Some(client) = self.clients.get(&login_key) {
            return Task::ready(Ok(client.clone()));
        }
        let Some(account) = self.account(login).cloned() else {
            return Task::ready(Err(anyhow!("no account {login}")));
        };
        let store = self.store.clone();
        cx.spawn(async move |this, cx| {
            let stored = store
                .read_credentials(&server_for(&account.login), cx)
                .await
                .context("reading the token")?;
            let Some((_, token)) = stored else {
                this.update(cx, |this, cx| {
                    this.mark_signed_out(&account.login, SignedOut::TokenMissing, cx)
                })?;
                return Err(anyhow!("{}'s token is missing", account.login));
            };
            let token =
                Token::new(String::from_utf8(token).context("the stored token isn't text")?);
            this.update(cx, |this, _| {
                let client = this.new_client(&account.login, token);
                this.clients
                    .entry(key(&account.login))
                    .or_insert(client)
                    .clone()
            })
        })
    }

    /// Picks the account to open `owner/repo` with: the one that can see it; if several can, the
    /// one remembered for the repo, its own before its owner's; else [`AccountChoice::Ask`]. The
    /// accounts are checked at once, each in its own queue when `queue_for` gives one (each
    /// poller's, so a check waits behind that account's polls rather than adding to them).
    ///
    /// The choices are read under the name GitHub gives the repo now, from its `full_name`, so a
    /// link to a renamed or transferred repo uses its current owner's choice.
    ///
    /// Only GitHub's "not found" counts as an account not seeing the repo; any other answer is a
    /// problem to show. When no account got an answer at all, the check fails with the first
    /// account's [`GithubError::Network`]; when some did, one that didn't is a problem too. The
    /// remembered account is never swapped for another: if it's signed out, its token can't be
    /// read or GitHub gave it any answer but yes or "not found", the choice is
    /// [`AccountChoice::Unavailable`], with a problem that says why.
    pub fn account_for_repo(
        &mut self,
        owner: &str,
        repo: &str,
        queue_for: impl Fn(&str) -> Option<RequestQueue>,
        cx: &mut Context<Self>,
    ) -> Task<Result<RepoAccount>> {
        if let Err(error) = check_repo_name(owner, repo) {
            return Task::ready(Err(error));
        }
        let asked = (owner.to_string(), repo.to_string());
        let path = format!("/repos/{owner}/{repo}");
        let checks: Vec<_> = self
            .accounts
            .iter()
            .filter(|account| !self.is_signed_out(&account.login))
            .map(|account| account.login.clone())
            .collect::<Vec<_>>()
            .into_iter()
            .map(|login| {
                let client = self.client(&login, cx);
                let queue = queue_for(&login);
                let path = path.clone();
                async move {
                    let client = match client.await {
                        Ok(client) => client,
                        Err(error) => {
                            return (
                                login,
                                Err(GithubError::TokenUnavailable(format!("{error:#}"))),
                            );
                        }
                    };
                    let check = || client.get::<serde_json::Value>(&path);
                    let answer = match queue {
                        Some(queue) => queue.read(check).await,
                        None => check().await,
                    };
                    (login, answer)
                }
            })
            .collect();
        cx.spawn(async move |this, cx| {
            let mut answers = futures::future::join_all(checks).await;
            let no_answer = |answer: &Result<serde_json::Value, GithubError>| {
                matches!(answer, Err(GithubError::Network(_)))
            };
            if !answers.is_empty() && answers.iter().all(|(_, answer)| no_answer(answer)) {
                let (_, first) = answers.swap_remove(0);
                return Err(first.expect_err("checked above").into());
            }
            let (owner, name) = answers
                .iter()
                .find_map(|(_, answer)| full_name(answer.as_ref().ok()?))
                .unwrap_or(asked);
            this.read_with(cx, |this, _| {
                let remembered = this.remembered(&owner, &name)?;
                let chosen = remembered.chosen().map(str::to_string);
                let is_chosen = |login: &str| {
                    chosen
                        .as_deref()
                        .is_some_and(|chosen| chosen.eq_ignore_ascii_case(login))
                };
                let mut visible = Vec::new();
                let mut problems = Vec::new();
                for (login, answer) in answers {
                    match answer {
                        Ok(_) => visible.push(login),
                        Err(GithubError::NotFound { .. }) => {}
                        // A missing token signed the account out, as signing out before the
                        // check would have, so it isn't checked; unless it's the chosen one.
                        Err(GithubError::TokenUnavailable(_))
                            if this.is_signed_out(&login) && !is_chosen(&login) => {}
                        // A refused token has signed the account out (see `Self::refused`).
                        Err(error) => problems.push((login, error)),
                    }
                }
                // A signed-out account isn't checked, so the chosen one is a problem by itself.
                if let Some(login) = &chosen
                    && this.is_signed_out(login)
                    && !problems.iter().any(|(problem, _)| is_chosen(problem))
                {
                    let message = format!(
                        "{login} is signed out; sign in again from Accounts… in the rozed menu"
                    );
                    problems.push((login.clone(), GithubError::Unauthorized { message }));
                }
                let unavailable = problems.iter().find(|(login, _)| is_chosen(login));
                let choice = match unavailable {
                    Some((login, _)) => AccountChoice::Unavailable(login.clone()),
                    None => choose_account(&visible, &remembered),
                };
                Ok(RepoAccount {
                    owner,
                    name,
                    choice,
                    remembered,
                    problems,
                })
            })?
        })
    }

    /// The accounts remembered for `owner/repo`: its own and its owner's. An account that's since
    /// been removed isn't remembered any more.
    pub fn remembered(&self, owner: &str, repo: &str) -> Result<Remembered> {
        let present = |login: String| self.account(&login).map(|account| account.login.clone());
        Ok(Remembered {
            repo: self.db.repo_account(HOST, owner, repo)?.and_then(present),
            owner: self.db.owner_account(HOST, owner)?.and_then(present),
        })
    }

    /// The account remembered for `owner`'s repos, by its login. An account that's since been
    /// removed isn't remembered any more.
    pub fn remembered_account(&self, owner: &str) -> Result<Option<String>> {
        Ok(self
            .db
            .owner_account(HOST, owner)?
            .and_then(|login| self.account(&login))
            .map(|account| account.login.clone()))
    }

    /// Remembers the account to use for an owner's repos.
    pub fn remember_account_for_owner(
        &self,
        owner: &str,
        login: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        let db = self.db.clone();
        let (owner, login) = (owner.to_string(), login.to_string());
        cx.background_spawn(
            async move { db.set_owner_account(HOST.to_string(), owner, login).await },
        )
    }

    /// Remembers the account to use for one repo, whose owner's account can't see it. The
    /// owner's choice stays for its other repos.
    pub fn remember_account_for_repo(
        &self,
        owner: &str,
        repo: &str,
        login: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        let db = self.db.clone();
        let (owner, repo, login) = (owner.to_string(), repo.to_string(), login.to_string());
        cx.background_spawn(async move {
            db.set_repo_account(HOST.to_string(), owner, repo, login)
                .await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::future::Future;
    use std::pin::Pin;

    use credentials_provider::test_support::FakeCredentialsProvider;
    use futures::future::BoxFuture;
    use gpui::{AsyncApp, TestAppContext};
    use http_client::http::HeaderValue;
    use http_client::{AsyncBody, Request, Response, Url};

    use crate::user::User;

    /// Behaves like GPUI's Keychain: deleting a token that isn't there is an error
    /// (`errSecItemNotFound`), unlike [`FakeCredentialsProvider`], which ignores it.
    #[derive(Default)]
    struct KeychainLike {
        fake: FakeCredentialsProvider,
        /// Refuse every delete, as when the user denies Keychain access.
        refuse_deletes: parking_lot::Mutex<bool>,
        /// Fail to read these logins' tokens, as from a locked Keychain.
        refuse_reads: parking_lot::Mutex<Vec<String>>,
    }

    impl KeychainLike {
        fn keys(&self) -> Vec<String> {
            let mut keys: Vec<_> = self.fake.state.lock().keys.keys().cloned().collect();
            keys.sort();
            keys
        }

        fn clear(&self) {
            self.fake.state.lock().keys.clear();
        }
    }

    type Boxed<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + 'a>>;

    impl CredentialsProvider for KeychainLike {
        fn read_credentials<'a>(
            &'a self,
            url: &'a str,
            cx: &'a AsyncApp,
        ) -> Boxed<'a, Option<(String, Vec<u8>)>> {
            let refused = self
                .refuse_reads
                .lock()
                .iter()
                .any(|login| server_for(login) == url);
            if refused {
                return Box::pin(async { Err(anyhow!("User interaction is not allowed.")) });
            }
            self.fake.read_credentials(url, cx)
        }

        fn write_credentials<'a>(
            &'a self,
            url: &'a str,
            username: &'a str,
            password: &'a [u8],
            cx: &'a AsyncApp,
        ) -> Boxed<'a, ()> {
            self.fake.write_credentials(url, username, password, cx)
        }

        fn delete_credentials<'a>(&'a self, url: &'a str, cx: &'a AsyncApp) -> Boxed<'a, ()> {
            if *self.refuse_deletes.lock() {
                return Box::pin(async { Err(anyhow!("keychain Delete was cancelled")) });
            }
            if !self.fake.state.lock().keys.contains_key(url) {
                return Box::pin(async {
                    Err(anyhow!("keychain Delete failed with OSStatus -25300"))
                });
            }
            self.fake.delete_credentials(url, cx)
        }
    }

    /// A canned answer for a token: the token, status, headers and body.
    type Answer = (
        &'static str,
        u16,
        &'static [(&'static str, &'static str)],
        &'static str,
    );

    /// Answers `GET /repos/...` with 200 for the tokens in `sees`, naming the repository
    /// `full_name` when it's set, 401 for `revoked`, no answer for `unreachable`, else 404, and
    /// `GET /user` with the token's owner from `owners`, else 401.
    #[derive(Default)]
    struct ByToken {
        sees: Vec<&'static str>,
        full_name: Option<&'static str>,
        revoked: Vec<&'static str>,
        unreachable: Vec<&'static str>,
        owners: Vec<(&'static str, &'static str)>,
        /// Other answers to `GET /repos/...`.
        answers: Vec<Answer>,
        /// How many requests were sent.
        sent: std::sync::atomic::AtomicUsize,
    }

    impl HttpClient for ByToken {
        fn user_agent(&self) -> Option<&HeaderValue> {
            None
        }

        fn proxy(&self) -> Option<&Url> {
            None
        }

        fn send(
            &self,
            request: Request<AsyncBody>,
        ) -> BoxFuture<'static, Result<Response<AsyncBody>>> {
            self.sent.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let token = request
                .headers()
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("token "))
                .unwrap_or_default()
                .to_string();
            if request.uri().path() == "/user" {
                let owner = self.owners.iter().find(|(t, _)| *t == token);
                let response = match owner {
                    Some((_, login)) => Response::builder()
                        .status(200)
                        .header("x-oauth-scopes", "read:org, repo, workflow")
                        .body(AsyncBody::from(format!(r#"{{"login":"{login}","id":1}}"#))),
                    None => Response::builder()
                        .status(401)
                        .body(AsyncBody::from(r#"{"message":"Bad credentials"}"#)),
                };
                return Box::pin(async move { Ok(response.unwrap()) });
            }
            if let Some((_, status, headers, body)) =
                self.answers.iter().find(|(t, ..)| *t == token)
            {
                let mut response = Response::builder().status(*status);
                for (name, value) in *headers {
                    response = response.header(*name, *value);
                }
                let response = response.body(AsyncBody::from(*body)).unwrap();
                return Box::pin(async move { Ok(response) });
            }
            if self.unreachable.contains(&token.as_str()) {
                return Box::pin(async {
                    Err(std::io::Error::from(std::io::ErrorKind::ConnectionRefused).into())
                });
            }
            let (status, body) = if self.sees.contains(&token.as_str()) {
                let body = match self.full_name {
                    Some(name) => format!(r#"{{"full_name":"{name}"}}"#),
                    None => r#"{"message":"x"}"#.to_string(),
                };
                (200, body)
            } else if self.revoked.contains(&token.as_str()) {
                (401, r#"{"message":"x"}"#.to_string())
            } else {
                (404, r#"{"message":"x"}"#.to_string())
            };
            Box::pin(async move {
                Ok(Response::builder()
                    .status(status)
                    .body(AsyncBody::from(body))
                    .unwrap())
            })
        }
    }

    fn user(login: &str, id: u64) -> AuthenticatedUser {
        AuthenticatedUser {
            user: User {
                login: login.into(),
                id,
                name: Some(format!("{login} name")),
                avatar_url: None,
            },
            scopes: vec!["read:org".into(), "repo".into(), "workflow".into()],
        }
    }

    async fn setup(
        name: &str,
        http: Arc<dyn HttpClient>,
        cx: &mut TestAppContext,
    ) -> (Entity<Accounts>, Arc<KeychainLike>, RozedDb) {
        let db = RozedDb::open_test(name).await;
        let store = Arc::new(KeychainLike::default());
        let accounts = cx
            .update(|cx| Accounts::load(db.clone(), store.clone(), http, cx))
            .unwrap();
        (accounts, store, db)
    }

    fn no_http() -> Arc<dyn HttpClient> {
        Arc::new(ByToken::default())
    }

    #[gpui::test]
    async fn adding_stores_the_token_and_the_account(cx: &mut TestAppContext) {
        let (accounts, store, db) = setup("adding", no_http(), cx).await;
        let account = accounts
            .update(cx, |accounts, cx| {
                accounts.add(
                    user("rozed-bot", 7),
                    Token::new("gho_FAKE"),
                    AccountSource::DeviceFlow,
                    cx,
                )
            })
            .await
            .unwrap();
        assert_eq!(account.login, "rozed-bot");
        assert_eq!(account.scopes, ["read:org", "repo", "workflow"]);
        assert_eq!(
            store.fake.state.lock().keys.get(&server_for("rozed-bot")),
            Some(&b"gho_FAKE".to_vec())
        );
        accounts.read_with(cx, |accounts, _| {
            assert_eq!(accounts.accounts(), std::slice::from_ref(&account));
        });

        // After a restart the account is still there, and its token is read from the store.
        let restarted = cx
            .update(|cx| Accounts::load(db.clone(), store.clone(), no_http(), cx))
            .unwrap();
        restarted.read_with(cx, |accounts, _| {
            assert_eq!(accounts.accounts(), std::slice::from_ref(&account));
        });
        let client = restarted
            .update(cx, |accounts, cx| accounts.client("ROZED-BOT", cx))
            .await
            .unwrap();
        assert_eq!(client.token().expose(), "gho_FAKE");
    }

    #[gpui::test]
    async fn adding_the_same_login_again_replaces_its_token(cx: &mut TestAppContext) {
        let (accounts, store, _) = setup("re-adding", no_http(), cx).await;
        for token in ["gho_OLD", "gho_NEW"] {
            accounts
                .update(cx, |accounts, cx| {
                    accounts.add(user("a", 1), Token::new(token), AccountSource::Gh, cx)
                })
                .await
                .unwrap();
        }
        accounts.read_with(cx, |accounts, _| assert_eq!(accounts.accounts().len(), 1));
        assert_eq!(
            store.fake.state.lock().keys.get(&server_for("a")),
            Some(&b"gho_NEW".to_vec())
        );
    }

    #[gpui::test]
    async fn removing_deletes_the_token_and_the_account(cx: &mut TestAppContext) {
        let (accounts, store, db) = setup("removing", no_http(), cx).await;
        for (login, id) in [("a", 1), ("b", 2)] {
            accounts
                .update(cx, |accounts, cx| {
                    accounts.add(
                        user(login, id),
                        Token::new("gho_FAKE"),
                        AccountSource::DeviceFlow,
                        cx,
                    )
                })
                .await
                .unwrap();
        }
        accounts
            .update(cx, |accounts, cx| accounts.remove("a", cx))
            .await
            .unwrap();
        accounts.read_with(cx, |accounts, _| {
            let logins: Vec<_> = accounts
                .accounts()
                .iter()
                .map(|a| a.login.as_str())
                .collect();
            assert_eq!(logins, ["b"]);
        });
        assert_eq!(store.keys(), [server_for("b")]);
        assert_eq!(db.accounts().unwrap().len(), 1);
    }

    async fn add(accounts: &Entity<Accounts>, login: &str, cx: &mut TestAppContext) {
        accounts
            .update(cx, |accounts, cx| {
                accounts.add(
                    user(login, 1),
                    Token::new("gho_FAKE"),
                    AccountSource::DeviceFlow,
                    cx,
                )
            })
            .await
            .unwrap();
    }

    #[gpui::test]
    async fn a_failed_row_write_leaves_no_token_behind(cx: &mut TestAppContext) {
        let (accounts, store, db) = setup("row_write_fails", no_http(), cx).await;
        // GitHub's ids fit in an i64; this one doesn't, so the database refuses the row.
        let error = accounts
            .update(cx, |accounts, cx| {
                accounts.add(
                    user("a", u64::MAX),
                    Token::new("gho_FAKE"),
                    AccountSource::DeviceFlow,
                    cx,
                )
            })
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("saving the account"),
            "{error:#}"
        );
        assert_eq!(store.keys(), Vec::<String>::new());
        assert_eq!(db.accounts().unwrap(), []);
    }

    #[gpui::test]
    async fn dropping_the_add_still_finishes_it(cx: &mut TestAppContext) {
        let (accounts, store, db) = setup("add_dropped", no_http(), cx).await;
        // Hold the token write, then drop the task, as the modal does when escape cancels it.
        let release = store.fake.defer_next_mutation();
        let task = accounts.update(cx, |accounts, cx| {
            accounts.add(
                user("a", 1),
                Token::new("gho_FAKE"),
                AccountSource::DeviceFlow,
                cx,
            )
        });
        cx.run_until_parked();
        drop(task);
        release.send(()).unwrap();
        cx.run_until_parked();
        // Both the token and the row are there, never just one of them.
        assert_eq!(store.keys(), [server_for("a")]);
        assert_eq!(db.accounts().unwrap().len(), 1);
        accounts.read_with(cx, |accounts, _| assert_eq!(accounts.accounts().len(), 1));
    }

    #[gpui::test]
    async fn a_case_only_rename_keeps_one_token(cx: &mut TestAppContext) {
        let (accounts, store, _) = setup("case_rename", no_http(), cx).await;
        add(&accounts, "rozed-bot", cx).await;
        add(&accounts, "Rozed-Bot", cx).await;
        assert_eq!(store.keys(), [server_for("rozed-bot")]);
        accounts
            .update(cx, |accounts, cx| accounts.remove("ROZED-BOT", cx))
            .await
            .unwrap();
        assert_eq!(store.keys(), Vec::<String>::new());
    }

    #[gpui::test]
    async fn accounts_added_in_the_same_second_keep_their_order_after_a_restart(
        cx: &mut TestAppContext,
    ) {
        let (_, store, db) = setup("same_second", no_http(), cx).await;
        // Two accounts added in the same second, whose logins differ in case.
        for login in ["B", "a"] {
            db.save_account(Account {
                host: HOST.to_string(),
                login: login.into(),
                user_id: 1,
                name: None,
                avatar_url: None,
                scopes: vec!["repo".into()],
                source: AccountSource::DeviceFlow,
                added_at: 1,
            })
            .await
            .unwrap();
        }
        let load = |cx: &mut TestAppContext| {
            cx.update(|cx| Accounts::load(db.clone(), store.clone(), no_http(), cx))
                .unwrap()
        };
        let logins = |accounts: &Entity<Accounts>, cx: &mut TestAppContext| {
            accounts.read_with(cx, |accounts, _| {
                let logins = accounts.accounts().iter().map(|a| a.login.clone());
                logins.collect::<Vec<_>>()
            })
        };

        // Signing in to one again sorts the accounts in memory; a restart reads the database's
        // order. Both ignore case.
        let accounts = load(cx);
        add(&accounts, "B", cx).await;
        assert_eq!(logins(&accounts, cx), ["a", "B"]);
        assert_eq!(logins(&load(cx), cx), ["a", "B"]);
    }

    #[gpui::test]
    async fn an_account_whose_token_is_gone_can_still_be_removed(cx: &mut TestAppContext) {
        let (accounts, store, db) = setup("removing_without_token", no_http(), cx).await;
        add(&accounts, "a", cx).await;
        // Deleted in Keychain Access, say.
        store.clear();
        accounts
            .update(cx, |accounts, cx| accounts.remove("a", cx))
            .await
            .unwrap();
        accounts.read_with(cx, |accounts, _| assert!(accounts.accounts().is_empty()));
        assert_eq!(db.accounts().unwrap(), []);
    }

    #[gpui::test]
    async fn removing_forgets_the_account_even_when_its_token_cant_be_deleted(
        cx: &mut TestAppContext,
    ) {
        let (accounts, store, db) = setup("removing_refused", no_http(), cx).await;
        add(&accounts, "a", cx).await;
        *store.refuse_deletes.lock() = true;
        let error = accounts
            .update(cx, |accounts, cx| accounts.remove("a", cx))
            .await
            .unwrap_err();
        // The account is gone, and the error says the token wasn't.
        assert!(
            format!("{error:#}").contains("couldn't delete its token"),
            "{error:#}"
        );
        accounts.read_with(cx, |accounts, _| assert!(accounts.accounts().is_empty()));
        assert_eq!(db.accounts().unwrap(), []);
    }

    async fn add_with(
        accounts: &Entity<Accounts>,
        login: &str,
        token: &str,
        source: AccountSource,
        cx: &mut TestAppContext,
    ) {
        accounts
            .update(cx, |accounts, cx| {
                accounts.add(user(login, 1), Token::new(token), source, cx)
            })
            .await
            .unwrap();
    }

    async fn client(
        accounts: &Entity<Accounts>,
        login: &str,
        cx: &mut TestAppContext,
    ) -> GithubClient {
        accounts
            .update(cx, |accounts, cx| accounts.client(login, cx))
            .await
            .unwrap()
    }

    const REPO: &str = "/repos/acme/app";

    #[gpui::test]
    async fn a_401_from_any_request_signs_only_that_account_out(cx: &mut TestAppContext) {
        let http = Arc::new(ByToken {
            sees: vec!["gho_B", "gho_NEW"],
            revoked: vec!["gho_A"],
            ..Default::default()
        });
        let (accounts, _, _) = setup("signed_out", http, cx).await;
        add_with(&accounts, "a", "gho_A", AccountSource::DeviceFlow, cx).await;
        add_with(&accounts, "b", "gho_B", AccountSource::DeviceFlow, cx).await;
        let a = client(&accounts, "A", cx).await;
        let b = client(&accounts, "b", cx).await;

        let error = a.get::<serde_json::Value>(REPO).await.unwrap_err();
        assert!(error.is_unauthorized(), "{error:?}");
        b.get::<serde_json::Value>(REPO).await.unwrap();
        accounts.read_with(cx, |accounts, _| {
            assert_eq!(accounts.signed_out("a"), Some(SignedOut::Refused));
            assert_eq!(accounts.signed_out("b"), None);
        });
        let result = accounts
            .update(cx, |accounts, cx| accounts.client("a", cx))
            .await;
        assert!(result.is_err());

        // Signing in again clears it, and clients made before pick up the new token.
        add_with(&accounts, "a", "gho_NEW", AccountSource::DeviceFlow, cx).await;
        accounts.read_with(cx, |accounts, _| assert!(!accounts.is_signed_out("a")));
        a.get::<serde_json::Value>(REPO).await.unwrap();
        assert_eq!(a.token().expose(), "gho_NEW");
    }

    /// A fake `gh` that prints the token in `token` next to it for `a`, counting its runs.
    struct FakeGh {
        dir: tempfile::TempDir,
    }

    impl FakeGh {
        fn new(token: &str) -> Self {
            use std::os::unix::fs::PermissionsExt as _;
            let dir = tempfile::tempdir().unwrap();
            let gh = dir.path().join("gh");
            std::fs::write(
                &gh,
                r#"#!/bin/sh
echo run >> "$(dirname "$0")/runs"
case "$*" in
  "auth token --hostname github.com --user a") cat "$(dirname "$0")/token" ;;
  *) exit 1 ;;
esac
"#,
            )
            .unwrap();
            std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
            std::fs::write(dir.path().join("token"), token).unwrap();
            Self { dir }
        }

        fn path(&self) -> std::path::PathBuf {
            self.dir.path().join("gh")
        }

        fn runs(&self) -> usize {
            std::fs::read_to_string(self.dir.path().join("runs"))
                .unwrap_or_default()
                .lines()
                .count()
        }
    }

    #[gpui::test]
    async fn a_401_reads_a_gh_accounts_token_from_gh_again(cx: &mut TestAppContext) {
        let http = Arc::new(ByToken {
            sees: vec!["gho_FRESH"],
            revoked: vec!["gho_OLD"],
            owners: vec![("gho_FRESH", "a")],
            ..Default::default()
        });
        let (accounts, store, _) = setup("gh_refresh", http, cx).await;
        let gh = FakeGh::new("gho_FRESH");
        accounts.update(cx, |accounts, _| accounts.set_gh_path(gh.path()));
        add_with(&accounts, "a", "gho_OLD", AccountSource::Gh, cx).await;
        let a = client(&accounts, "a", cx).await;
        let other = a.clone();

        // gh's short-lived token expired; gh hands out its refreshed one, and the request is
        // retried with it.
        a.get::<serde_json::Value>(REPO).await.unwrap();
        assert_eq!(gh.runs(), 1);
        assert_eq!(other.token().expose(), "gho_FRESH");
        assert_eq!(
            store.fake.state.lock().keys.get(&server_for("a")),
            Some(&b"gho_FRESH".to_vec())
        );
        accounts.read_with(cx, |accounts, _| assert!(!accounts.is_signed_out("a")));
    }

    #[gpui::test]
    async fn requests_refused_together_read_gh_once(cx: &mut TestAppContext) {
        let http = Arc::new(ByToken {
            sees: vec!["gho_FRESH"],
            revoked: vec!["gho_OLD"],
            owners: vec![("gho_FRESH", "a")],
            ..Default::default()
        });
        let (accounts, _, _) = setup("gh_refresh_together", http, cx).await;
        let gh = FakeGh::new("gho_FRESH");
        accounts.update(cx, |accounts, _| accounts.set_gh_path(gh.path()));
        add_with(&accounts, "a", "gho_OLD", AccountSource::Gh, cx).await;
        let a = client(&accounts, "a", cx).await;
        let other = a.clone();
        let (first, second) = futures::join!(
            a.get::<serde_json::Value>(REPO),
            other.get::<serde_json::Value>(REPO)
        );
        first.unwrap();
        second.unwrap();
        assert_eq!(gh.runs(), 1);
    }

    #[gpui::test]
    async fn a_gh_account_is_signed_out_when_gh_has_nothing_newer(cx: &mut TestAppContext) {
        let http = Arc::new(ByToken {
            revoked: vec!["gho_OLD"],
            ..Default::default()
        });
        let (accounts, _, _) = setup("gh_refresh_fails", http, cx).await;
        let gh = FakeGh::new("gho_OLD");
        accounts.update(cx, |accounts, _| accounts.set_gh_path(gh.path()));
        add_with(&accounts, "a", "gho_OLD", AccountSource::Gh, cx).await;
        let a = client(&accounts, "a", cx).await;

        let error = a.get::<serde_json::Value>(REPO).await.unwrap_err();
        assert!(error.is_unauthorized(), "{error:?}");
        accounts.read_with(cx, |accounts, _| assert!(accounts.is_signed_out("a")));
        // Signed out, the next refusal doesn't ask gh again.
        a.get::<serde_json::Value>(REPO).await.unwrap_err();
        assert_eq!(gh.runs(), 1);
    }

    #[gpui::test]
    async fn a_missing_token_signs_the_account_out(cx: &mut TestAppContext) {
        let (accounts, store, _) = setup("missing_token", no_http(), cx).await;
        accounts
            .update(cx, |accounts, cx| {
                accounts.add(
                    user("a", 1),
                    Token::new("gho_FAKE"),
                    AccountSource::DeviceFlow,
                    cx,
                )
            })
            .await
            .unwrap();
        store.clear();
        accounts.update(cx, |accounts, _| accounts.clients.clear());
        let result = accounts
            .update(cx, |accounts, cx| accounts.client("a", cx))
            .await;
        assert!(result.is_err());
        accounts.read_with(cx, |accounts, _| {
            assert_eq!(accounts.signed_out("a"), Some(SignedOut::TokenMissing))
        });
    }

    #[test]
    fn choosing_an_account() {
        let logins = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        let owner = |login: &str| Remembered {
            repo: None,
            owner: Some(login.into()),
        };
        assert_eq!(choose_account(&[], &owner("a")), AccountChoice::None);
        assert_eq!(
            choose_account(&logins(&["a"]), &owner("b")),
            AccountChoice::Account("a".into())
        );
        assert_eq!(
            choose_account(&logins(&["a", "b"]), &owner("B")),
            AccountChoice::Account("b".into())
        );
        assert_eq!(
            choose_account(&logins(&["a", "b"]), &owner("c")),
            AccountChoice::Ask(logins(&["a", "b"]))
        );
        assert_eq!(
            choose_account(&logins(&["a", "b"]), &Remembered::default()),
            AccountChoice::Ask(logins(&["a", "b"]))
        );
        // The repository's own account wins over its owner's, while it can see it.
        let both = Remembered {
            repo: Some("c".into()),
            owner: Some("a".into()),
        };
        assert_eq!(
            choose_account(&logins(&["a", "b", "c"]), &both),
            AccountChoice::Account("c".into())
        );
        assert_eq!(
            choose_account(&logins(&["a", "b"]), &both),
            AccountChoice::Account("a".into())
        );
        assert_eq!(both.chosen(), Some("c"));
        assert_eq!(owner("a").chosen(), Some("a"));
    }

    #[gpui::test]
    async fn the_account_that_can_see_the_repo_opens_it(cx: &mut TestAppContext) {
        let http = Arc::new(ByToken {
            sees: vec!["gho_WORK", "gho_HOME"],
            revoked: vec!["gho_REVOKED"],
            ..Default::default()
        });
        let (accounts, _, _) = setup("account_for_repo", http, cx).await;
        // In login order, so the accounts' order (when added, then login) doesn't depend on
        // which second each one is added in.
        for (login, token) in [
            ("home", "gho_HOME"),
            ("old", "gho_REVOKED"),
            ("other", "gho_OTHER"),
            ("work", "gho_WORK"),
        ] {
            accounts
                .update(cx, |accounts, cx| {
                    accounts.add(
                        user(login, 1),
                        Token::new(token),
                        AccountSource::DeviceFlow,
                        cx,
                    )
                })
                .await
                .unwrap();
        }
        let found = accounts
            .update(cx, |accounts, cx| {
                accounts.account_for_repo("acme", "app", |_| None, cx)
            })
            .await
            .unwrap();
        assert_eq!(
            found.choice,
            AccountChoice::Ask(vec!["home".into(), "work".into()])
        );
        // The revoked token's account is now signed out, which is worth saying.
        accounts.read_with(cx, |accounts, _| assert!(accounts.is_signed_out("old")));
        assert_eq!(problems(&found), [("old", "unauthorized")]);

        accounts
            .update(cx, |accounts, cx| {
                accounts.remember_account_for_owner("ACME", "home", cx)
            })
            .await
            .unwrap();
        let found = accounts
            .update(cx, |accounts, cx| {
                accounts.account_for_repo("acme", "app", |_| None, cx)
            })
            .await
            .unwrap();
        assert_eq!(found.choice, AccountChoice::Account("home".into()));
    }

    /// Each problem as (login, kind of error).
    fn problems(found: &RepoAccount) -> Vec<(&str, &'static str)> {
        found
            .problems
            .iter()
            .map(|(login, error)| {
                let kind = match error {
                    GithubError::Unauthorized { .. } => "unauthorized",
                    GithubError::RateLimited { .. } => "rate limited",
                    GithubError::SsoRequired { .. } => "sso",
                    GithubError::OAuthAppRestricted { .. } => "oauth restricted",
                    GithubError::Api { status: 502, .. } => "502",
                    GithubError::TokenUnavailable(_) => "token unavailable",
                    GithubError::Network(_) => "network",
                    _ => "other",
                };
                (login.as_str(), kind)
            })
            .collect()
    }

    const RATE_LIMITED: &[(&str, &str)] = &[
        ("x-ratelimit-remaining", "0"),
        ("x-ratelimit-reset", "1762419600"),
    ];
    const SSO: &[(&str, &str)] = &[(
        "x-github-sso",
        "required; url=https://github.com/orgs/acme/sso?authorization_request=X",
    )];

    #[gpui::test]
    async fn only_not_found_means_an_account_cant_see_a_repo(cx: &mut TestAppContext) {
        let http = Arc::new(ByToken {
            sees: vec!["gho_HOME"],
            answers: vec![
                (
                    "gho_WORK",
                    403,
                    RATE_LIMITED,
                    r#"{"message":"API rate limit exceeded"}"#,
                ),
                (
                    "gho_SSO",
                    403,
                    SSO,
                    r#"{"message":"Resource protected by organization SAML enforcement."}"#,
                ),
                ("gho_DOWN", 502, &[], "Bad gateway"),
            ],
            ..Default::default()
        });
        let (accounts, _, _) = setup("account_for_repo_problems", http, cx).await;
        // In login order, so the accounts' order (when added, then login) doesn't depend on
        // which second each one is added in.
        for (login, token) in [
            ("down", "gho_DOWN"),
            ("elsewhere", "gho_ELSEWHERE"),
            ("home", "gho_HOME"),
            ("sso", "gho_SSO"),
            ("work", "gho_WORK"),
        ] {
            add_with(&accounts, login, token, AccountSource::DeviceFlow, cx).await;
        }
        let account_for_repo = |cx: &mut TestAppContext| {
            accounts.update(cx, |accounts, cx| {
                accounts.account_for_repo("acme", "app", |_| None, cx)
            })
        };

        // Only home can see it, and the others' problems come along, in the accounts' order.
        let found = account_for_repo(cx).await.unwrap();
        assert_eq!(found.choice, AccountChoice::Account("home".into()));
        assert_eq!(
            problems(&found),
            [("down", "502"), ("sso", "sso"), ("work", "rate limited")]
        );
        let sso = &found.problems[1].1;
        assert_eq!(
            sso.help_link().unwrap().url(),
            "https://github.com/orgs/acme/sso?authorization_request=X"
        );

        // When the owner's account is the one that can't check, rozed doesn't quietly open the
        // repo with another.
        accounts
            .update(cx, |accounts, cx| {
                accounts.remember_account_for_owner("acme", "work", cx)
            })
            .await
            .unwrap();
        let found = account_for_repo(cx).await.unwrap();
        assert_eq!(found.choice, AccountChoice::Unavailable("work".into()));
    }

    async fn account_for_repo(accounts: &Entity<Accounts>, cx: &mut TestAppContext) -> RepoAccount {
        accounts
            .update(cx, |accounts, cx| {
                accounts.account_for_repo("acme", "app", |_| None, cx)
            })
            .await
            .unwrap()
    }

    #[gpui::test]
    async fn a_remembered_account_thats_signed_out_or_unreadable_is_unavailable(
        cx: &mut TestAppContext,
    ) {
        let http = Arc::new(ByToken {
            sees: vec!["gho_WORK", "gho_HOME"],
            ..Default::default()
        });
        let (accounts, store, db) = setup("account_for_repo_unavailable", http.clone(), cx).await;
        for (login, token) in [("home", "gho_HOME"), ("work", "gho_WORK")] {
            add_with(&accounts, login, token, AccountSource::DeviceFlow, cx).await;
        }
        accounts
            .update(cx, |accounts, cx| {
                accounts.remember_account_for_owner("acme", "work", cx)
            })
            .await
            .unwrap();
        let found = account_for_repo(&accounts, cx).await;
        assert_eq!(found.choice, AccountChoice::Account("work".into()));

        // Signed out, work isn't swapped for home, which can see the repository too.
        accounts.update(cx, |accounts, cx| {
            accounts.mark_signed_out("work", SignedOut::Refused, cx)
        });
        let found = account_for_repo(&accounts, cx).await;
        assert_eq!(found.choice, AccountChoice::Unavailable("work".into()));
        assert_eq!(problems(&found), [("work", "unauthorized")]);

        // After a restart, with work's token unreadable, as from a locked Keychain.
        let accounts = cx
            .update(|cx| Accounts::load(db.clone(), store.clone(), http.clone(), cx))
            .unwrap();
        store.refuse_reads.lock().push("work".into());
        let found = account_for_repo(&accounts, cx).await;
        assert_eq!(found.choice, AccountChoice::Unavailable("work".into()));
        assert_eq!(problems(&found), [("work", "token unavailable")]);
        accounts.read_with(cx, |accounts, _| assert!(!accounts.is_signed_out("work")));
    }

    #[gpui::test]
    async fn the_choice_is_read_under_the_repositorys_current_owner(cx: &mut TestAppContext) {
        // alice/tool was transferred to bigorg, and GitHub answers for it by its name now.
        let http = Arc::new(ByToken {
            sees: vec!["gho_WORK", "gho_HOME"],
            full_name: Some("BigOrg/tool"),
            ..Default::default()
        });
        let (accounts, _, _) = setup("account_for_repo_transferred", http, cx).await;
        for (login, token) in [("home", "gho_HOME"), ("work", "gho_WORK")] {
            add_with(&accounts, login, token, AccountSource::DeviceFlow, cx).await;
        }
        for (owner, login) in [("alice", "home"), ("bigorg", "work")] {
            accounts
                .update(cx, |accounts, cx| {
                    accounts.remember_account_for_owner(owner, login, cx)
                })
                .await
                .unwrap();
        }

        let found = accounts
            .update(cx, |accounts, cx| {
                accounts.account_for_repo("alice", "tool", |_| None, cx)
            })
            .await
            .unwrap();

        assert_eq!(
            (found.owner.as_str(), found.name.as_str()),
            ("BigOrg", "tool")
        );
        assert_eq!(found.choice, AccountChoice::Account("work".into()));
        assert_eq!(found.remembered.owner.as_deref(), Some("work"));
    }

    #[gpui::test]
    async fn a_repositorys_own_account_is_used_where_its_owners_cant_see_it(
        cx: &mut TestAppContext,
    ) {
        let http = Arc::new(ByToken {
            sees: vec!["gho_HOME", "gho_PERSONAL"],
            full_name: Some("acme/secret"),
            ..Default::default()
        });
        let (accounts, _, _) = setup("account_for_repo_per_repo", http, cx).await;
        for (login, token) in [
            ("home", "gho_HOME"),
            ("personal", "gho_PERSONAL"),
            ("work", "gho_WORK"),
        ] {
            add_with(&accounts, login, token, AccountSource::DeviceFlow, cx).await;
        }
        accounts
            .update(cx, |accounts, cx| {
                accounts.remember_account_for_owner("acme", "work", cx)
            })
            .await
            .unwrap();
        let account_for_secret = |cx: &mut TestAppContext| {
            accounts.update(cx, |accounts, cx| {
                accounts.account_for_repo("acme", "secret", |_| None, cx)
            })
        };

        // work, the owner's account, can't see it, and two others can.
        let found = account_for_secret(cx).await.unwrap();
        assert_eq!(
            found.choice,
            AccountChoice::Ask(vec!["home".into(), "personal".into()])
        );
        assert_eq!(found.remembered.owner.as_deref(), Some("work"));
        assert_eq!(found.remembered.repo, None);

        accounts
            .update(cx, |accounts, cx| {
                accounts.remember_account_for_repo("acme", "secret", "personal", cx)
            })
            .await
            .unwrap();
        let found = account_for_secret(cx).await.unwrap();
        assert_eq!(found.choice, AccountChoice::Account("personal".into()));
        // The owner's choice stays for its other repositories.
        accounts.read_with(cx, |accounts, _| {
            assert_eq!(
                accounts.remembered_account("acme").unwrap().as_deref(),
                Some("work")
            );
            assert_eq!(
                accounts.remembered("acme", "app").unwrap(),
                Remembered {
                    repo: None,
                    owner: Some("work".into())
                }
            );
        });

        // Signed out, the repository's own account isn't swapped for the other.
        accounts.update(cx, |accounts, cx| {
            accounts.mark_signed_out("personal", SignedOut::Refused, cx)
        });
        let found = account_for_secret(cx).await.unwrap();
        assert_eq!(found.choice, AccountChoice::Unavailable("personal".into()));
        assert_eq!(problems(&found), [("personal", "unauthorized")]);
    }

    #[gpui::test]
    async fn only_when_no_account_gets_an_answer_is_github_out_of_reach(cx: &mut TestAppContext) {
        let http = Arc::new(ByToken {
            sees: vec!["gho_HOME"],
            unreachable: vec!["gho_DOWN"],
            ..Default::default()
        });
        let (accounts, _, _) = setup("account_for_repo_unreachable", http, cx).await;
        add_with(&accounts, "down", "gho_DOWN", AccountSource::DeviceFlow, cx).await;

        // The one account's check got no answer.
        let error = account_for_repo_result(&accounts, cx).await.unwrap_err();
        let github = error.downcast_ref::<GithubError>().unwrap();
        assert!(github.is_unreachable(), "{error:#}");

        // With another account that got one, it's that account's problem.
        add_with(&accounts, "home", "gho_HOME", AccountSource::DeviceFlow, cx).await;
        let found = account_for_repo(&accounts, cx).await;
        assert_eq!(found.choice, AccountChoice::Account("home".into()));
        assert_eq!(problems(&found), [("down", "network")]);
    }

    async fn account_for_repo_result(
        accounts: &Entity<Accounts>,
        cx: &mut TestAppContext,
    ) -> Result<RepoAccount> {
        accounts
            .update(cx, |accounts, cx| {
                accounts.account_for_repo("acme", "app", |_| None, cx)
            })
            .await
    }

    #[gpui::test]
    async fn an_account_whose_token_cant_be_read_is_a_problem(cx: &mut TestAppContext) {
        let http = Arc::new(ByToken {
            sees: vec!["gho_HOME"],
            ..Default::default()
        });
        let (accounts, store, db) = setup("account_for_repo_unreadable", http.clone(), cx).await;
        for (login, token) in [("home", "gho_HOME"), ("work", "gho_WORK")] {
            add_with(&accounts, login, token, AccountSource::DeviceFlow, cx).await;
        }
        let accounts = cx
            .update(|cx| Accounts::load(db, store.clone(), http, cx))
            .unwrap();
        store.refuse_reads.lock().push("work".into());

        let found = account_for_repo(&accounts, cx).await;

        assert_eq!(found.choice, AccountChoice::Account("home".into()));
        assert_eq!(problems(&found), [("work", "token unavailable")]);
    }

    #[gpui::test]
    async fn each_check_waits_for_its_accounts_queue(cx: &mut TestAppContext) {
        let http = Arc::new(ByToken {
            sees: vec!["gho_HOME"],
            ..Default::default()
        });
        let (accounts, _, _) = setup("account_for_repo_queues", http.clone(), cx).await;
        for (login, token) in [("home", "gho_HOME"), ("work", "gho_WORK")] {
            add_with(&accounts, login, token, AccountSource::DeviceFlow, cx).await;
        }
        let sent = || http.sent.load(std::sync::atomic::Ordering::SeqCst);
        // work's queue is busy with a poll.
        let queue = crate::poll::RequestQueue::new(cx.executor(), std::time::Duration::ZERO);
        let (done, busy) = oneshot::channel::<()>();
        let poll = cx.background_spawn({
            let queue = queue.clone();
            async move { queue.read(|| busy).await }
        });
        cx.run_until_parked();

        let check = accounts.update(cx, |accounts, cx| {
            let queue = queue.clone();
            accounts.account_for_repo(
                "acme",
                "app",
                move |login| (login == "work").then(|| queue.clone()),
                cx,
            )
        });
        cx.run_until_parked();
        // home's check went ahead; work's waits its turn.
        assert_eq!(sent(), 1);

        done.send(()).unwrap();
        poll.await.ok();
        let found = check.await.unwrap();
        assert_eq!(sent(), 2);
        assert_eq!(found.choice, AccountChoice::Account("home".into()));
    }

    #[gpui::test]
    async fn names_that_arent_githubs_are_refused(cx: &mut TestAppContext) {
        let http = Arc::new(ByToken {
            sees: vec!["gho_A"],
            ..Default::default()
        });
        let (accounts, _, _) = setup("account_for_repo_names", http, cx).await;
        add_with(&accounts, "a", "gho_A", AccountSource::DeviceFlow, cx).await;
        for (owner, repo) in [
            ("acme", "app/../../user"),
            ("acme/app", "x"),
            ("acme", ".."),
            ("acme", "app?x=1"),
            ("", "app"),
        ] {
            let result = accounts
                .update(cx, |accounts, cx| {
                    accounts.account_for_repo(owner, repo, |_| None, cx)
                })
                .await;
            assert!(result.is_err(), "{owner}/{repo}");
        }
        for (owner, repo) in [("acme", "app.rs"), ("some_emu", "a-b_c.d")] {
            let result = accounts
                .update(cx, |accounts, cx| {
                    accounts.account_for_repo(owner, repo, |_| None, cx)
                })
                .await;
            assert!(result.is_ok(), "{owner}/{repo}");
        }
    }
}
