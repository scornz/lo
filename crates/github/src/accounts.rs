//! The signed-in accounts: their tokens, their database rows, and which one opens a PR.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, anyhow};
use credentials_provider::CredentialsProvider;
use futures::channel::oneshot;
use gpui::{App, AppContext as _, Context, Entity, Global, Task};
use http_client::HttpClient;

use crate::client::GithubClient;
use crate::credentials::{HOST, server_for};
use crate::db::{Account, AccountSource, RozedDb};
use crate::error::GithubError;
use crate::token::Token;
use crate::user::AuthenticatedUser;

/// Every account rozed is signed in to, as a GPUI model. Views observe it to redraw when an
/// account is added, removed or signed out.
pub struct Accounts {
    db: RozedDb,
    store: Arc<dyn CredentialsProvider>,
    http: Arc<dyn HttpClient>,
    accounts: Vec<Account>,
    /// Lowercased logins whose token GitHub refused, or whose token is missing.
    signed_out: HashSet<String>,
    /// Clients for accounts whose token has been read, by lowercased login.
    clients: HashMap<String, GithubClient>,
}

struct GlobalAccounts(Entity<Accounts>);

impl Global for GlobalAccounts {}

/// Which account to open an owner's PR with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountChoice {
    Account(String),
    /// Several accounts can see the repo and none was chosen for its owner yet: ask, then
    /// [`Accounts::remember_account_for_owner`].
    Ask(Vec<String>),
    /// No signed-in account can see it.
    None,
}

/// Picks among the accounts that can see a repo: the only one, else the one remembered for the
/// owner, else ask.
pub fn choose_account(visible: &[String], remembered: Option<&str>) -> AccountChoice {
    match visible {
        [] => AccountChoice::None,
        [only] => AccountChoice::Account(only.clone()),
        several => match remembered
            .and_then(|login| several.iter().find(|v| v.eq_ignore_ascii_case(login)))
        {
            Some(login) => AccountChoice::Account(login.clone()),
            None => AccountChoice::Ask(several.to_vec()),
        },
    }
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
    pub fn new(
        db: RozedDb,
        store: Arc<dyn CredentialsProvider>,
        http: Arc<dyn HttpClient>,
    ) -> Result<Self> {
        let accounts = db.accounts().context("reading accounts")?;
        Ok(Self {
            db,
            store,
            http,
            accounts,
            signed_out: HashSet::new(),
            clients: HashMap::new(),
        })
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
        self.signed_out.contains(&key(login))
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
        let http = self.http.clone();
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
                    this.clients.insert(login, GithubClient::new(http, token));
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
    pub fn mark_signed_out(&mut self, login: &str, cx: &mut Context<Self>) {
        let login = key(login);
        self.clients.remove(&login);
        if self.signed_out.insert(login) {
            cx.notify();
        }
    }

    /// Notes an error from a request made with `login`'s token, signing it out on a `401`.
    pub fn report_error(&mut self, login: &str, error: &GithubError, cx: &mut Context<Self>) {
        if error.is_unauthorized() {
            log::info!("GitHub refused {login}'s token; marking the account signed out");
            self.mark_signed_out(login, cx);
        }
    }

    /// A client for the account, reading its token from the store the first time.
    pub fn client(&mut self, login: &str, cx: &mut Context<Self>) -> Task<Result<GithubClient>> {
        let login_key = key(login);
        if self.signed_out.contains(&login_key) {
            return Task::ready(Err(anyhow!("{login} is signed out")));
        }
        if let Some(client) = self.clients.get(&login_key) {
            return Task::ready(Ok(client.clone()));
        }
        let Some(account) = self.account(login).cloned() else {
            return Task::ready(Err(anyhow!("no account {login}")));
        };
        let store = self.store.clone();
        let http = self.http.clone();
        cx.spawn(async move |this, cx| {
            let stored = store
                .read_credentials(&server_for(&account.login), cx)
                .await
                .context("reading the token")?;
            let Some((_, token)) = stored else {
                this.update(cx, |this, cx| this.mark_signed_out(&account.login, cx))?;
                return Err(anyhow!("{}'s token is missing", account.login));
            };
            let token =
                Token::new(String::from_utf8(token).context("the stored token isn't text")?);
            let client = GithubClient::new(http, token);
            this.update(cx, |this, _| {
                this.clients.insert(key(&account.login), client.clone())
            })?;
            Ok(client)
        })
    }

    /// Picks the account to open `owner/repo` with: the one that can see it; if several can, the
    /// one remembered for the owner; else [`AccountChoice::Ask`].
    pub fn account_for_repo(
        &mut self,
        owner: &str,
        repo: &str,
        cx: &mut Context<Self>,
    ) -> Task<Result<AccountChoice>> {
        let logins: Vec<String> = self
            .accounts
            .iter()
            .filter(|account| !self.is_signed_out(&account.login))
            .map(|account| account.login.clone())
            .collect();
        let remembered = match self.db.owner_account(HOST, owner) {
            Ok(remembered) => remembered,
            Err(error) => return Task::ready(Err(error)),
        };
        let path = format!("/repos/{owner}/{repo}");
        let clients: Vec<_> = logins
            .iter()
            .map(|login| (login.clone(), self.client(login, cx)))
            .collect();
        cx.spawn(async move |this, cx| {
            let mut visible = Vec::new();
            for (login, client) in clients {
                let Ok(client) = client.await else {
                    continue;
                };
                match client.get::<serde_json::Value>(&path).await {
                    Ok(_) => visible.push(login),
                    Err(error @ GithubError::Unauthorized { .. }) => {
                        this.update(cx, |this, cx| this.report_error(&login, &error, cx))?;
                    }
                    Err(GithubError::Network(error)) => return Err(error),
                    // Not found, or hidden by SSO or an org's OAuth restriction.
                    Err(_) => {}
                }
            }
            Ok(choose_account(&visible, remembered.as_deref()))
        })
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

    /// Answers `GET /repos/...` with 200 for the tokens in `sees`, 401 for `revoked`, else 404.
    struct ByToken {
        sees: Vec<&'static str>,
        revoked: Vec<&'static str>,
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
            let token = request
                .headers()
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("token "))
                .unwrap_or_default()
                .to_string();
            let status = if self.sees.contains(&token.as_str()) {
                200
            } else if self.revoked.contains(&token.as_str()) {
                401
            } else {
                404
            };
            Box::pin(async move {
                Ok(Response::builder()
                    .status(status)
                    .body(AsyncBody::from(r#"{"message":"x"}"#))
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
        let accounts = cx.new(|_| Accounts::new(db.clone(), store.clone(), http).unwrap());
        (accounts, store, db)
    }

    fn no_http() -> Arc<dyn HttpClient> {
        Arc::new(ByToken {
            sees: vec![],
            revoked: vec![],
        })
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
        let restarted = cx.new(|_| Accounts::new(db.clone(), store.clone(), no_http()).unwrap());
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

    #[gpui::test]
    async fn a_401_signs_only_that_account_out(cx: &mut TestAppContext) {
        let (accounts, _, _) = setup("signed_out", no_http(), cx).await;
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
        let error = GithubError::Unauthorized {
            message: "Bad credentials".into(),
        };
        accounts.update(cx, |accounts, cx| {
            accounts.report_error("A", &error, cx);
            accounts.report_error(
                "b",
                &GithubError::NotFound {
                    message: "Not Found".into(),
                    documentation_url: None,
                },
                cx,
            );
            assert!(accounts.is_signed_out("a"));
            assert!(!accounts.is_signed_out("b"));
        });
        let result = accounts
            .update(cx, |accounts, cx| accounts.client("a", cx))
            .await;
        assert!(result.is_err());
        // Signing in again clears it.
        accounts
            .update(cx, |accounts, cx| {
                accounts.add(
                    user("a", 1),
                    Token::new("gho_NEW"),
                    AccountSource::DeviceFlow,
                    cx,
                )
            })
            .await
            .unwrap();
        accounts.read_with(cx, |accounts, _| assert!(!accounts.is_signed_out("a")));
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
        accounts.read_with(cx, |accounts, _| assert!(accounts.is_signed_out("a")));
    }

    #[test]
    fn choosing_an_account() {
        let logins = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        assert_eq!(choose_account(&[], Some("a")), AccountChoice::None);
        assert_eq!(
            choose_account(&logins(&["a"]), Some("b")),
            AccountChoice::Account("a".into())
        );
        assert_eq!(
            choose_account(&logins(&["a", "b"]), Some("B")),
            AccountChoice::Account("b".into())
        );
        assert_eq!(
            choose_account(&logins(&["a", "b"]), Some("c")),
            AccountChoice::Ask(logins(&["a", "b"]))
        );
        assert_eq!(
            choose_account(&logins(&["a", "b"]), None),
            AccountChoice::Ask(logins(&["a", "b"]))
        );
    }

    #[gpui::test]
    async fn the_account_that_can_see_the_repo_opens_it(cx: &mut TestAppContext) {
        let http = Arc::new(ByToken {
            sees: vec!["gho_WORK", "gho_HOME"],
            revoked: vec!["gho_REVOKED"],
        });
        let (accounts, _, _) = setup("account_for_repo", http, cx).await;
        for (login, token) in [
            ("work", "gho_WORK"),
            ("home", "gho_HOME"),
            ("old", "gho_REVOKED"),
            ("other", "gho_OTHER"),
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
        let choice = accounts
            .update(cx, |accounts, cx| {
                accounts.account_for_repo("acme", "app", cx)
            })
            .await
            .unwrap();
        assert_eq!(
            choice,
            AccountChoice::Ask(vec!["home".into(), "work".into()])
        );
        // The revoked token's account is now signed out.
        accounts.read_with(cx, |accounts, _| assert!(accounts.is_signed_out("old")));

        accounts
            .update(cx, |accounts, cx| {
                accounts.remember_account_for_owner("ACME", "home", cx)
            })
            .await
            .unwrap();
        let choice = accounts
            .update(cx, |accounts, cx| {
                accounts.account_for_repo("acme", "app", cx)
            })
            .await
            .unwrap();
        assert_eq!(choice, AccountChoice::Account("home".into()));
    }
}
