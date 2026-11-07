//! Where each file was marked viewed on this Mac (design §6 "Viewed marks").
//!
//! GitHub keeps one viewed mark per file and user, and resets it to "changed since viewed" when a
//! push changes the file, without saying since when. Lo notes the head on screen when it marked
//! each file viewed, so a file that changed since can open to what changed after that revision
//! (`review::revisions::viewed_file_comparison`). These records aren't cached answers: clearing
//! the cache keeps them, and removing the account removes them.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use sqlez_macros::sql;

use crate::credentials::HOST;
use crate::db::LoDb;
use crate::pulls::PullRef;

/// The heads at which an account marked a pull request's files viewed with Lo, in Lo's
/// database.
#[derive(Clone)]
pub struct ViewedRecords {
    db: LoDb,
}

impl ViewedRecords {
    pub fn new(db: LoDb) -> Self {
        Self { db }
    }

    /// The head `account` marked each of `pull`'s files viewed at, by path.
    pub fn heads(&self, account: &str, pull: &PullRef) -> Result<HashMap<String, String>> {
        let rows = self
            .db
            .connection()
            .select_bound::<(&str, &str, &str, &str, i64), (String, String)>(sql!(
                SELECT path, head FROM viewed_files
                WHERE host = ?1 AND account = ?2 AND owner = ?3 AND repo = ?4 AND number = ?5
            ))?((
            HOST,
            account,
            &pull.repo.owner,
            &pull.repo.name,
            pull.number,
        ))?;
        Ok(rows.into_iter().collect())
    }

    /// Notes that `account` marked `path` viewed at `head`, replacing any earlier record.
    pub async fn record(
        &self,
        account: &str,
        pull: &PullRef,
        path: &str,
        head: &str,
        at: SystemTime,
    ) -> Result<()> {
        let row = (
            key(account, pull, path),
            head.to_string(),
            at.duration_since(UNIX_EPOCH)
                .map(|since| i64::try_from(since.as_secs()).unwrap_or(i64::MAX))
                .unwrap_or_default(),
        );
        self.db
            .connection()
            .write(move |connection| {
                connection.exec_bound(sql!(
                    INSERT INTO viewed_files(host, account, owner, repo, number, path, head, viewed_at)
                    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                    ON CONFLICT(host, account, owner, repo, number, path) DO UPDATE SET
                        head = excluded.head,
                        viewed_at = excluded.viewed_at
                ))?(row)
            })
            .await
    }

    /// Forgets where `account` marked `path` viewed, once the mark is cleared.
    pub async fn forget(&self, account: &str, pull: &PullRef, path: &str) -> Result<()> {
        let key = key(account, pull, path);
        self.db
            .connection()
            .write(move |connection| {
                connection.exec_bound(sql!(
                    DELETE FROM viewed_files
                    WHERE host = ?1 AND account = ?2 AND owner = ?3 AND repo = ?4 AND number = ?5
                        AND path = ?6
                ))?(key)
            })
            .await
    }
}

/// The columns that pick out a file's record.
fn key(account: &str, pull: &PullRef, path: &str) -> (String, String, String, String, i64, String) {
    (
        HOST.to_string(),
        account.to_string(),
        pull.repo.owner.clone(),
        pull.repo.name.clone(),
        pull.number,
        path.to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use futures::executor::block_on;

    use crate::cache::PrCache;
    use crate::db::{Account, AccountSource};

    fn account(login: &str) -> Account {
        Account {
            host: HOST.into(),
            login: login.into(),
            user_id: 1,
            name: None,
            avatar_url: None,
            scopes: vec!["repo".into()],
            source: AccountSource::DeviceFlow,
            added_at: 1,
        }
    }

    async fn records(name: &str, logins: &[&str]) -> (LoDb, ViewedRecords) {
        let db = LoDb::open_test(name).await;
        for login in logins {
            db.save_account(account(login)).await.unwrap();
        }
        (db.clone(), ViewedRecords::new(db))
    }

    fn pr(owner: &str, repo: &str, number: i64) -> PullRef {
        PullRef::new(owner, repo, number)
    }

    fn heads(entries: &[(&str, &str)]) -> HashMap<String, String> {
        entries
            .iter()
            .map(|(path, head)| (path.to_string(), head.to_string()))
            .collect()
    }

    #[test]
    fn each_file_keeps_the_head_it_was_last_marked_viewed_at() {
        block_on(async {
            let (_, records) = records("viewed_heads", &["bot"]).await;
            let playground = pr("rozed-dev", "playground", 2);
            assert_eq!(records.heads("bot", &playground).unwrap(), heads(&[]));

            let now = SystemTime::now();
            records
                .record("bot", &playground, "a.rs", "h1", now)
                .await
                .unwrap();
            records
                .record("bot", &playground, "docs/b.md", "h1", now)
                .await
                .unwrap();
            records
                .record("bot", &playground, "a.rs", "h2", now)
                .await
                .unwrap();

            assert_eq!(
                records.heads("bot", &playground).unwrap(),
                heads(&[("a.rs", "h2"), ("docs/b.md", "h1")])
            );
            // Owners, repositories and logins match ignoring case, as on GitHub.
            assert_eq!(
                records
                    .heads("BOT", &pr("Rozed-Dev", "Playground", 2))
                    .unwrap()
                    .len(),
                2
            );
        });
    }

    #[test]
    fn records_belong_to_one_account_and_one_pull_request() {
        block_on(async {
            let (_, records) = records("viewed_scope", &["bot", "other"]).await;
            let now = SystemTime::now();
            records
                .record("bot", &pr("rozed-dev", "playground", 2), "a.rs", "h1", now)
                .await
                .unwrap();

            for (login, pull) in [
                ("other", pr("rozed-dev", "playground", 2)),
                ("bot", pr("rozed-dev", "playground", 1)),
                ("bot", pr("rozed-dev", "elsewhere", 2)),
                ("bot", pr("someone", "playground", 2)),
            ] {
                assert_eq!(records.heads(login, &pull).unwrap(), heads(&[]), "{pull}");
            }
        });
    }

    #[test]
    fn clearing_a_mark_forgets_its_record() {
        block_on(async {
            let (_, records) = records("viewed_forget", &["bot"]).await;
            let playground = pr("rozed-dev", "playground", 2);
            let now = SystemTime::now();
            for path in ["a.rs", "b.rs"] {
                records
                    .record("bot", &playground, path, "h1", now)
                    .await
                    .unwrap();
            }

            records.forget("bot", &playground, "a.rs").await.unwrap();
            // Forgetting a file with no record does nothing.
            records.forget("bot", &playground, "c.rs").await.unwrap();

            assert_eq!(
                records.heads("bot", &playground).unwrap(),
                heads(&[("b.rs", "h1")])
            );
        });
    }

    #[test]
    fn clearing_the_cache_keeps_them_and_removing_the_account_removes_them() {
        block_on(async {
            let (db, records) = records("viewed_lifetime", &["bot"]).await;
            let playground = pr("rozed-dev", "playground", 2);
            records
                .record("bot", &playground, "a.rs", "h1", SystemTime::now())
                .await
                .unwrap();

            PrCache::new(db.clone()).clear().await.unwrap();
            assert_eq!(records.heads("bot", &playground).unwrap().len(), 1);

            db.delete_account(HOST.into(), "bot".into()).await.unwrap();
            assert_eq!(records.heads("bot", &playground).unwrap(), heads(&[]));
        });
    }
}
