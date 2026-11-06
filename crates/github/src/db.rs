//! rozed's own SQLite database, `rozed.sqlite`, through Zed's `sqlez`.
//!
//! It's a separate file from Zed's `db.sqlite`, and unlike Zed's it never falls back to an
//! in-memory database: it holds durable state, such as which account to use for each owner, and
//! losing that silently is worse than an error. Migrations are append-only: `sqlez` refuses to
//! open a database whose recorded migration text no longer matches.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use sqlez::bindable::{Bind, Column, StaticColumnCount};
use sqlez::domain::Domain;
use sqlez::statement::Statement;
use sqlez::thread_safe_connection::ThreadSafeConnection;
use sqlez_macros::sql;

/// rozed's database connection. Reads run on the calling thread; writes are queued to one
/// background thread.
#[derive(Clone)]
pub struct RozedDb(ThreadSafeConnection);

impl Domain for RozedDb {
    const NAME: &str = "rozed";
    const MIGRATIONS: &[&str] = &[
        sql!(
            // One row per signed-in account. Tokens aren't here: they're in the token store.
            CREATE TABLE accounts(
                host TEXT NOT NULL,
                login TEXT NOT NULL COLLATE NOCASE,
                user_id INTEGER NOT NULL,
                name TEXT,
                avatar_url TEXT,
                // Comma-separated, as GitHub's x-oauth-scopes header lists them.
                scopes TEXT NOT NULL,
                // "device_flow" or "gh".
                source TEXT NOT NULL,
                // Seconds since the Unix epoch.
                added_at INTEGER NOT NULL,
                PRIMARY KEY(host, login)
            ) STRICT;

            // The account to open an owner's PRs with, when several accounts can see them.
            CREATE TABLE owner_accounts(
                host TEXT NOT NULL,
                owner TEXT NOT NULL COLLATE NOCASE,
                login TEXT NOT NULL COLLATE NOCASE,
                PRIMARY KEY(host, owner),
                FOREIGN KEY(host, login) REFERENCES accounts(host, login) ON DELETE CASCADE
            ) STRICT;
        ),
        sql!(
            // GitHub's answers, cached per account: a reopened PR shows at once, its first poll can
            // still be a free 304, and offline rozed shows what it last saw. The only table that's
            // ever cleared; removing an account removes its rows.
            CREATE TABLE pr_cache(
                host TEXT NOT NULL,
                account TEXT NOT NULL COLLATE NOCASE,
                owner TEXT NOT NULL COLLATE NOCASE,
                repo TEXT NOT NULL COLLATE NOCASE,
                // The pull request's number, or 0 for an answer about the whole repository.
                number INTEGER NOT NULL,
                // What was asked: a GraphQL operation's name, or a name for a REST endpoint.
                query TEXT NOT NULL,
                // The shape the JSON was written in. A row in another shape reads as missing.
                version TEXT NOT NULL,
                json TEXT NOT NULL,
                // The ETag of the REST answer that tells whether this one changed.
                etag TEXT,
                // When GitHub last confirmed it, in seconds since the Unix epoch.
                fetched_at INTEGER NOT NULL,
                PRIMARY KEY(host, account, owner, repo, number, query),
                FOREIGN KEY(host, account) REFERENCES accounts(host, login) ON DELETE CASCADE
            ) STRICT;

            // The pull requests opened most recently, and the account each was opened with.
            CREATE TABLE recent_prs(
                host TEXT NOT NULL,
                owner TEXT NOT NULL COLLATE NOCASE,
                repo TEXT NOT NULL COLLATE NOCASE,
                number INTEGER NOT NULL,
                // Unknown until the pull request was fetched once.
                title TEXT,
                account TEXT NOT NULL COLLATE NOCASE,
                // Seconds since the Unix epoch.
                opened_at INTEGER NOT NULL,
                PRIMARY KEY(host, owner, repo, number)
            ) STRICT;
        ),
        sql!(
            // Where each file was marked viewed on this Mac: the head on screen when rozed marked
            // it, per account. Not cached data, so clearing the cache keeps it; removing the
            // account removes its rows.
            CREATE TABLE viewed_files(
                host TEXT NOT NULL,
                account TEXT NOT NULL COLLATE NOCASE,
                owner TEXT NOT NULL COLLATE NOCASE,
                repo TEXT NOT NULL COLLATE NOCASE,
                number INTEGER NOT NULL,
                // The file's path in the pull request, as GitHub names it.
                path TEXT NOT NULL,
                // The head commit's SHA.
                head TEXT NOT NULL,
                // Seconds since the Unix epoch.
                viewed_at INTEGER NOT NULL,
                PRIMARY KEY(host, account, owner, repo, number, path),
                FOREIGN KEY(host, account) REFERENCES accounts(host, login) ON DELETE CASCADE
            ) STRICT;
        ),
    ];
}

const DB_INITIALIZE_QUERY: &str = sql!(
    PRAGMA journal_mode=WAL;
    PRAGMA busy_timeout=500;
    PRAGMA synchronous=NORMAL;
);

const CONNECTION_INITIALIZE_QUERY: &str = sql!(
    PRAGMA foreign_keys=TRUE;
);

/// Where rozed's database lives: next to Zed's, in `0-dev` for debug builds and `0-stable` for
/// release builds, so a debug build's accounts never mix with a release build's. Their tokens are
/// kept apart the same way ([`crate::credentials::server_for`]).
pub fn default_path() -> PathBuf {
    let scope = match crate::credentials::Build::current() {
        crate::credentials::Build::Debug => "dev",
        crate::credentials::Build::Release => "stable",
    };
    paths::database_dir()
        .join(format!("0-{scope}"))
        .join("rozed.sqlite")
}

impl RozedDb {
    /// Opens or creates the database at `path`, running its migrations.
    pub async fn open(path: &Path) -> Result<Self> {
        let dir = path.parent().context("the database path has no folder")?;
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let uri = path.to_string_lossy();
        let connection = ThreadSafeConnection::builder::<Self>(&uri, true)
            .with_db_initialization_query(DB_INITIALIZE_QUERY)
            .with_connection_initialize_query(CONNECTION_INITIALIZE_QUERY)
            .build()
            .await
            .with_context(|| format!("opening {}", path.display()))?;
        let db = Self(connection);
        db.check_file(path)?;
        Ok(db)
    }

    pub(crate) fn connection(&self) -> &ThreadSafeConnection {
        &self.0
    }

    /// `sqlez` opens an in-memory database when it can't open the file. Refuse that.
    fn check_file(&self, path: &Path) -> Result<()> {
        let files = self
            .0
            .select::<(i64, String, String)>("PRAGMA database_list")?()?;
        let main = files
            .into_iter()
            .find(|(_, name, _)| name == "main")
            .map(|(_, _, file)| file)
            .unwrap_or_default();
        let canonical = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
        if main.is_empty() || canonical(Path::new(&main)) != canonical(path) {
            bail!(
                "couldn't open {} (SQLite opened {:?} instead)",
                path.display(),
                main
            );
        }
        Ok(())
    }

    /// An in-memory database for tests, unique to `name`, with writes run synchronously.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn open_test(name: &str) -> Self {
        Self(
            ThreadSafeConnection::builder::<Self>(name, false)
                .with_db_initialization_query(DB_INITIALIZE_QUERY)
                .with_connection_initialize_query(CONNECTION_INITIALIZE_QUERY)
                .with_write_queue_constructor(sqlez::thread_safe_connection::locking_queue())
                .build()
                .await
                .expect("opening a test database"),
        )
    }

    pub fn accounts(&self) -> Result<Vec<Account>> {
        self.0.select::<Account>(sql!(
            SELECT host, login, user_id, name, avatar_url, scopes, source, added_at
            FROM accounts ORDER BY added_at, login
        ))?()
    }

    /// Adds an account, or updates it if the login is already there.
    pub async fn save_account(&self, account: Account) -> Result<()> {
        self.0
            .write(move |connection| {
                connection.exec_bound::<Account>(sql!(
                    INSERT INTO accounts(host, login, user_id, name, avatar_url, scopes, source, added_at)
                    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                    ON CONFLICT(host, login) DO UPDATE SET
                        login = excluded.login,
                        user_id = excluded.user_id,
                        name = excluded.name,
                        avatar_url = excluded.avatar_url,
                        scopes = excluded.scopes,
                        source = excluded.source
                ))?(account)
            })
            .await
    }

    /// Removes an account and the owners it was chosen for.
    pub async fn delete_account(&self, host: String, login: String) -> Result<()> {
        self.0
            .write(move |connection| {
                connection.with_savepoint("delete_account", || {
                    // Its owners and cached answers go with it, by their foreign keys.
                    connection.exec_bound::<(String, String)>(sql!(
                        DELETE FROM accounts WHERE host = ?1 AND login = ?2
                    ))?((host.clone(), login.clone()))?;
                    connection.exec_bound::<(String, String)>(sql!(
                        DELETE FROM recent_prs WHERE host = ?1 AND account = ?2
                    ))?((host, login))
                })
            })
            .await
    }

    /// The account remembered for an owner, if any.
    pub fn owner_account(&self, host: &str, owner: &str) -> Result<Option<String>> {
        self.0.select_row_bound::<(&str, &str), String>(sql!(
            SELECT login FROM owner_accounts WHERE host = ?1 AND owner = ?2
        ))?((host, owner))
    }

    pub async fn set_owner_account(
        &self,
        host: String,
        owner: String,
        login: String,
    ) -> Result<()> {
        self.0
            .write(move |connection| {
                connection.exec_bound::<(String, String, String)>(sql!(
                    INSERT INTO owner_accounts(host, owner, login) VALUES (?1, ?2, ?3)
                    ON CONFLICT(host, owner) DO UPDATE SET owner = excluded.owner, login = excluded.login
                ))?((host, owner, login))
            })
            .await
    }
}

/// How an account was added.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountSource {
    /// rozed's own sign-in, GitHub's device flow.
    DeviceFlow,
    /// Imported from the `gh` CLI.
    Gh,
}

impl AccountSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::DeviceFlow => "device_flow",
            Self::Gh => "gh",
        }
    }

    fn parse(source: &str) -> Result<Self> {
        match source {
            "device_flow" => Ok(Self::DeviceFlow),
            "gh" => Ok(Self::Gh),
            other => bail!("unknown account source {other:?}"),
        }
    }
}

/// A signed-in account, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub host: String,
    pub login: String,
    pub user_id: u64,
    pub name: Option<String>,
    pub avatar_url: Option<String>,
    pub scopes: Vec<String>,
    pub source: AccountSource,
    /// Seconds since the Unix epoch.
    pub added_at: i64,
}

type AccountRow = (
    String,
    String,
    i64,
    Option<String>,
    Option<String>,
    String,
    String,
    i64,
);

impl StaticColumnCount for Account {
    fn column_count() -> usize {
        8
    }
}

impl Bind for Account {
    fn bind(&self, statement: &Statement, start_index: i32) -> Result<i32> {
        let row: AccountRow = (
            self.host.clone(),
            self.login.clone(),
            i64::try_from(self.user_id).context("user id out of range")?,
            self.name.clone(),
            self.avatar_url.clone(),
            self.scopes.join(", "),
            self.source.as_str().to_string(),
            self.added_at,
        );
        row.bind(statement, start_index)
    }
}

impl Column for Account {
    fn column(statement: &mut Statement, start_index: i32) -> Result<(Self, i32)> {
        let ((host, login, user_id, name, avatar_url, scopes, source, added_at), next) =
            AccountRow::column(statement, start_index)?;
        Ok((
            Self {
                host,
                login,
                user_id: u64::try_from(user_id).context("negative user id")?,
                name,
                avatar_url,
                scopes: crate::user::parse_scopes(Some(&scopes)),
                source: AccountSource::parse(&source)?,
                added_at,
            },
            next,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn account(login: &str, added_at: i64) -> Account {
        Account {
            host: "github.com".into(),
            login: login.into(),
            user_id: 1,
            name: Some(format!("{login}'s name")),
            avatar_url: None,
            scopes: vec!["read:org".into(), "repo".into(), "workflow".into()],
            source: AccountSource::DeviceFlow,
            added_at,
        }
    }

    #[test]
    fn accounts_round_trip_and_update_in_place() {
        futures::executor::block_on(async {
            let db = RozedDb::open_test("accounts_round_trip").await;
            assert_eq!(db.accounts().unwrap(), []);
            db.save_account(account("b", 2)).await.unwrap();
            db.save_account(account("a", 1)).await.unwrap();
            assert_eq!(db.accounts().unwrap(), [account("a", 1), account("b", 2)]);

            let mut renamed = account("A", 5);
            renamed.source = AccountSource::Gh;
            renamed.scopes = vec!["repo".into()];
            db.save_account(renamed).await.unwrap();
            let accounts = db.accounts().unwrap();
            assert_eq!(accounts.len(), 2);
            // Logins are case-insensitive; the update keeps when it was first added.
            assert_eq!(accounts[0].login, "A");
            assert_eq!(accounts[0].added_at, 1);
            assert_eq!(accounts[0].source, AccountSource::Gh);
            assert_eq!(accounts[0].scopes, ["repo"]);
        });
    }

    #[test]
    fn removing_an_account_forgets_its_owners() {
        futures::executor::block_on(async {
            let db = RozedDb::open_test("owner_accounts").await;
            db.save_account(account("a", 1)).await.unwrap();
            db.save_account(account("b", 2)).await.unwrap();
            let set = |owner: &str, login: &str| {
                db.set_owner_account("github.com".into(), owner.into(), login.into())
            };
            set("acme", "a").await.unwrap();
            set("Other", "b").await.unwrap();
            assert_eq!(
                db.owner_account("github.com", "ACME").unwrap().as_deref(),
                Some("a")
            );
            set("acme", "b").await.unwrap();
            assert_eq!(
                db.owner_account("github.com", "acme").unwrap().as_deref(),
                Some("b")
            );
            db.delete_account("github.com".into(), "B".into())
                .await
                .unwrap();
            assert_eq!(db.owner_account("github.com", "acme").unwrap(), None);
            assert_eq!(db.owner_account("github.com", "other").unwrap(), None);
            assert_eq!(db.accounts().unwrap(), [account("a", 1)]);
        });
    }

    #[test]
    fn opens_a_file_and_keeps_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("0-dev/rozed.sqlite");
        futures::executor::block_on(async {
            let db = RozedDb::open(&path).await.unwrap();
            db.save_account(account("a", 1)).await.unwrap();
        });
        assert!(path.exists());
    }

    /// The first migration, as existing databases applied it. sqlez refuses to open a database
    /// whose applied migrations' text differs, so step 0 must never change.
    const SHIPPED_STEP_0: &str = sql!(
        // One row per signed-in account. Tokens aren't here: they're in the token store.
        CREATE TABLE accounts(
            host TEXT NOT NULL,
            login TEXT NOT NULL COLLATE NOCASE,
            user_id INTEGER NOT NULL,
            name TEXT,
            avatar_url TEXT,
            // Comma-separated, as GitHub's x-oauth-scopes header lists them.
            scopes TEXT NOT NULL,
            // "device_flow" or "gh".
            source TEXT NOT NULL,
            // Seconds since the Unix epoch.
            added_at INTEGER NOT NULL,
            PRIMARY KEY(host, login)
        ) STRICT;

        // The account to open an owner's PRs with, when several accounts can see them.
        CREATE TABLE owner_accounts(
            host TEXT NOT NULL,
            owner TEXT NOT NULL COLLATE NOCASE,
            login TEXT NOT NULL COLLATE NOCASE,
            PRIMARY KEY(host, owner),
            FOREIGN KEY(host, login) REFERENCES accounts(host, login) ON DELETE CASCADE
        ) STRICT;
    );

    /// rozed's database as it was before the cache.
    struct BeforeTheCache;

    impl Domain for BeforeTheCache {
        const NAME: &str = RozedDb::NAME;
        const MIGRATIONS: &[&str] = &[SHIPPED_STEP_0];
    }

    #[test]
    fn the_first_migration_never_changes() {
        assert_eq!(RozedDb::MIGRATIONS[0], SHIPPED_STEP_0);
    }

    #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    struct Note(u32);

    impl crate::cache::CacheEntry for Note {
        const NAME: &'static str = "note";

        fn shape() -> String {
            "a number".into()
        }
    }

    #[test]
    fn upgrades_a_database_from_before_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rozed.sqlite");
        futures::executor::block_on(async {
            let old =
                ThreadSafeConnection::builder::<BeforeTheCache>(&path.to_string_lossy(), true)
                    .with_db_initialization_query(DB_INITIALIZE_QUERY)
                    .with_connection_initialize_query(CONNECTION_INITIALIZE_QUERY)
                    .build()
                    .await
                    .unwrap();
            old.write(|connection| {
                connection.exec_bound::<Account>(sql!(
                    INSERT INTO accounts(host, login, user_id, name, avatar_url, scopes, source, added_at)
                    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                ))?(account("a", 1))
            })
            .await
            .unwrap();
            drop(old);

            let db = RozedDb::open(&path).await.unwrap();
            assert_eq!(db.accounts().unwrap(), [account("a", 1)]);
            let cache = crate::cache::PrCache::new(db);
            let target = crate::PullRef::new("o", "r", 1).into();
            cache
                .put("a", &target, &Note(1), None, std::time::SystemTime::now())
                .await
                .unwrap();
            assert_eq!(
                cache.get::<Note>("a", &target).unwrap().unwrap().value,
                Note(1)
            );
        });
    }

    /// rozed's database as it was before viewed files were recorded: the accounts and the cache.
    struct BeforeViewedFiles;

    impl Domain for BeforeViewedFiles {
        const NAME: &str = RozedDb::NAME;
        const MIGRATIONS: &[&str] = &[RozedDb::MIGRATIONS[0], RozedDb::MIGRATIONS[1]];
    }

    #[test]
    fn upgrades_a_database_from_before_viewed_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rozed.sqlite");
        futures::executor::block_on(async {
            let old =
                ThreadSafeConnection::builder::<BeforeViewedFiles>(&path.to_string_lossy(), true)
                    .with_db_initialization_query(DB_INITIALIZE_QUERY)
                    .with_connection_initialize_query(CONNECTION_INITIALIZE_QUERY)
                    .build()
                    .await
                    .unwrap();
            old.write(|connection| {
                connection.exec_bound::<Account>(sql!(
                    INSERT INTO accounts(host, login, user_id, name, avatar_url, scopes, source, added_at)
                    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                ))?(account("a", 1))
            })
            .await
            .unwrap();
            drop(old);

            let db = RozedDb::open(&path).await.unwrap();
            assert_eq!(db.accounts().unwrap(), [account("a", 1)]);
            let records = crate::viewed::ViewedRecords::new(db);
            let pull = crate::PullRef::new("o", "r", 1);
            records
                .record("a", &pull, "a.rs", "h1", std::time::SystemTime::now())
                .await
                .unwrap();
            assert_eq!(
                records
                    .heads("a", &pull)
                    .unwrap()
                    .get("a.rs")
                    .map(String::as_str),
                Some("h1")
            );
        });
    }

    #[test]
    fn refuses_to_fall_back_to_memory() {
        let dir = tempfile::tempdir().unwrap();
        // A folder where the file should be: SQLite can't open it.
        let path = dir.path().join("rozed.sqlite");
        std::fs::create_dir(&path).unwrap();
        let Err(error) = futures::executor::block_on(RozedDb::open(&path)) else {
            panic!("opened a folder as a database");
        };
        // sqlez fell back to memory, and rozed's own check refused that.
        let message = format!("{error:#}");
        assert!(
            message.starts_with(&format!("couldn't open {}", path.display())),
            "{message}"
        );
        assert!(message.contains("SQLite opened"), "{message}");
    }
}
