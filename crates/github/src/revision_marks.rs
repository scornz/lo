//! What rozed saw of each pull request's revisions on this Mac (design §6, "Revisions").
//!
//! - **Seen heads:** every head an account's poller saw, and when it first saw it. They're one of
//!   the sources a pull request's revisions are built from (`review::revisions::PushRecord::
//!   Polled`), the only one for a run of pushes no other source saw.
//! - **Last seen here:** the head of the newest revision on screen when an account last left a
//!   pull request, one of the two markers the revision picker and the default comparison start
//!   from.
//!
//! Like viewed records ([`crate::viewed::ViewedRecords`]), these aren't cached answers: clearing
//! the cache keeps them, and removing the account removes its rows. A pull request's rows go once
//! it has been closed for [`RevisionMarks::KEPT_AFTER_CLOSING`] ([`RevisionMarks::prune`]).

use std::time::{Duration, SystemTime};

use anyhow::Result;
use sqlez_macros::sql;

use crate::cache::{CacheEntry as _, from_secs, to_secs};
use crate::credentials::HOST;
use crate::db::RozedDb;
use crate::pulls::{PullRef, PullSummary};

/// Where a cached summary says whether a pull request is closed, and when it was last updated.
const STATE: &str = "$.state";
const CLOSED: &str = "closed";
const UPDATED_AT: &str = "$.updated_at";

/// A head a poller saw on a pull request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeenHead {
    /// The head commit's SHA.
    pub head: String,
    /// When a poller first saw it.
    pub first_seen: SystemTime,
}

/// Seen heads and "last seen here", in rozed's database.
#[derive(Clone)]
pub struct RevisionMarks {
    db: RozedDb,
}

impl RevisionMarks {
    /// How long a pull request's rows are kept after it closed.
    pub const KEPT_AFTER_CLOSING: Duration = Duration::from_secs(30 * 24 * 60 * 60);

    pub fn new(db: RozedDb) -> Self {
        Self { db }
    }

    /// Forgets the seen heads and "last seen here" of every pull request closed before
    /// [`Self::KEPT_AFTER_CLOSING`] ago, at `now`, as any account's cached summary of it says.
    /// A closed pull request's summary is updated when it closes or later, so its update time
    /// stands for when it closed. A pull request with no summary in the cache keeps its rows.
    pub async fn prune(&self, now: SystemTime) -> Result<()> {
        let cutoff = chrono::DateTime::<chrono::Utc>::from(now - Self::KEPT_AFTER_CLOSING)
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let summary = PullSummary::NAME;
        self.db
            .connection()
            .write(move |connection| {
                connection.with_savepoint("prune_revision_marks", || {
                    connection.exec_bound(sql!(
                        DELETE FROM seen_heads WHERE EXISTS (
                            SELECT pull.number FROM pr_cache AS pull
                            WHERE pull.host = seen_heads.host AND pull.owner = seen_heads.owner
                                AND pull.repo = seen_heads.repo
                                AND pull.number = seen_heads.number AND pull.query = ?1
                                AND json_extract(pull.json, ?3) = ?4
                                AND json_extract(pull.json, ?5) < ?2
                        )
                    ))?((summary, cutoff.clone(), STATE, CLOSED, UPDATED_AT))?;
                    connection.exec_bound(sql!(
                        DELETE FROM last_seen WHERE EXISTS (
                            SELECT pull.number FROM pr_cache AS pull
                            WHERE pull.host = last_seen.host AND pull.owner = last_seen.owner
                                AND pull.repo = last_seen.repo
                                AND pull.number = last_seen.number AND pull.query = ?1
                                AND json_extract(pull.json, ?3) = ?4
                                AND json_extract(pull.json, ?5) < ?2
                        )
                    ))?((summary, cutoff, STATE, CLOSED, UPDATED_AT))
                })
            })
            .await
    }

    /// Every head any account's poller saw on `pull`, with when one first saw it, earliest
    /// first. A head is a fact about the pull request, whichever account saw it.
    pub fn seen_heads(&self, pull: &PullRef) -> Result<Vec<SeenHead>> {
        let rows = self
            .db
            .connection()
            .select_bound::<(&str, &str, &str, i64), (String, i64)>(sql!(
                SELECT head, min(first_seen) AS first_seen FROM seen_heads
                WHERE host = ?1 AND owner = ?2 AND repo = ?3 AND number = ?4
                GROUP BY head
                ORDER BY first_seen, head
            ))?((HOST, &pull.repo.owner, &pull.repo.name, pull.number))?;
        Ok(rows
            .into_iter()
            .map(|(head, first_seen)| SeenHead {
                head,
                first_seen: from_secs(first_seen),
            })
            .collect())
    }

    /// Notes that `account`'s poller saw `head` on `pull` at `at`. Seeing it again keeps the
    /// earliest time.
    pub async fn record_seen(
        &self,
        account: &str,
        pull: &PullRef,
        head: &str,
        at: SystemTime,
    ) -> Result<()> {
        let row = (key(account, pull), head.to_string(), to_secs(at));
        self.db
            .connection()
            .write(move |connection| {
                connection.exec_bound(sql!(
                    INSERT INTO seen_heads(host, account, owner, repo, number, head, first_seen)
                    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                    ON CONFLICT(host, account, owner, repo, number, head) DO UPDATE SET
                        first_seen = min(seen_heads.first_seen, excluded.first_seen)
                ))?(row)
            })
            .await
    }

    /// The head of the newest revision on screen when `account` last left `pull`, if it ever
    /// did.
    pub fn last_seen(&self, account: &str, pull: &PullRef) -> Result<Option<String>> {
        self.db
            .connection()
            .select_row_bound::<(&str, &str, &str, &str, i64), String>(sql!(
                SELECT head FROM last_seen
                WHERE host = ?1 AND account = ?2 AND owner = ?3 AND repo = ?4 AND number = ?5
            ))?((
            HOST,
            account,
            &pull.repo.owner,
            &pull.repo.name,
            pull.number,
        ))
    }

    /// Notes that `account` left `pull` at `at` with the revision whose head is `head` the
    /// newest on screen, replacing what it noted before.
    pub async fn set_last_seen(
        &self,
        account: &str,
        pull: &PullRef,
        head: &str,
        at: SystemTime,
    ) -> Result<()> {
        let row = (key(account, pull), head.to_string(), to_secs(at));
        self.db
            .connection()
            .write(move |connection| {
                connection.exec_bound(sql!(
                    INSERT INTO last_seen(host, account, owner, repo, number, head, seen_at)
                    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                    ON CONFLICT(host, account, owner, repo, number) DO UPDATE SET
                        head = excluded.head,
                        seen_at = excluded.seen_at
                ))?(row)
            })
            .await
    }
}

/// The columns that pick out an account's rows about a pull request.
fn key(account: &str, pull: &PullRef) -> (String, String, String, String, i64) {
    (
        HOST.to_string(),
        account.to_string(),
        pull.repo.owner.clone(),
        pull.repo.name.clone(),
        pull.number,
    )
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use futures::executor::block_on;

    use super::*;
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

    async fn marks(name: &str, logins: &[&str]) -> (RozedDb, RevisionMarks) {
        let db = RozedDb::open_test(name).await;
        for login in logins {
            db.save_account(account(login)).await.unwrap();
        }
        (db.clone(), RevisionMarks::new(db))
    }

    fn pr(owner: &str, repo: &str, number: i64) -> PullRef {
        PullRef::new(owner, repo, number)
    }

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_790_000_000 + secs)
    }

    fn seen(head: &str, secs: u64) -> SeenHead {
        SeenHead {
            head: head.into(),
            first_seen: at(secs),
        }
    }

    #[test]
    fn a_head_keeps_when_it_was_first_seen() {
        block_on(async {
            let (_, marks) = marks("seen_first", &["bot"]).await;
            let e2e = pr("rozed-dev", "e2e", 16);
            assert_eq!(marks.seen_heads(&e2e).unwrap(), []);

            marks.record_seen("bot", &e2e, "h2", at(20)).await.unwrap();
            marks.record_seen("bot", &e2e, "h1", at(10)).await.unwrap();
            marks.record_seen("bot", &e2e, "h2", at(30)).await.unwrap();
            // A clock that went backwards, or a write that came late, still keeps the earliest.
            marks.record_seen("bot", &e2e, "h1", at(5)).await.unwrap();

            assert_eq!(
                marks.seen_heads(&e2e).unwrap(),
                [seen("h1", 5), seen("h2", 20)]
            );
            // Owners and repositories match ignoring case, as on GitHub.
            assert_eq!(
                marks.seen_heads(&pr("Rozed-Dev", "E2E", 16)).unwrap().len(),
                2
            );
        });
    }

    #[test]
    fn heads_any_account_saw_count_once_each() {
        block_on(async {
            let (_, marks) = marks("seen_accounts", &["bot", "other"]).await;
            let e2e = pr("rozed-dev", "e2e", 16);
            marks.record_seen("bot", &e2e, "h1", at(10)).await.unwrap();
            marks.record_seen("other", &e2e, "h1", at(8)).await.unwrap();
            marks
                .record_seen("other", &e2e, "h2", at(20))
                .await
                .unwrap();
            marks
                .record_seen("bot", &pr("rozed-dev", "e2e", 14), "x1", at(1))
                .await
                .unwrap();

            assert_eq!(
                marks.seen_heads(&e2e).unwrap(),
                [seen("h1", 8), seen("h2", 20)]
            );
        });
    }

    #[test]
    fn last_seen_is_per_account_and_pull_request() {
        block_on(async {
            let (_, marks) = marks("last_seen", &["bot", "other"]).await;
            let e2e = pr("rozed-dev", "e2e", 16);
            assert_eq!(marks.last_seen("bot", &e2e).unwrap(), None);

            marks
                .set_last_seen("bot", &e2e, "h2", at(10))
                .await
                .unwrap();
            marks
                .set_last_seen("bot", &e2e, "h3", at(20))
                .await
                .unwrap();

            assert_eq!(marks.last_seen("BOT", &e2e).unwrap().as_deref(), Some("h3"));
            assert_eq!(marks.last_seen("other", &e2e).unwrap(), None);
            assert_eq!(
                marks.last_seen("bot", &pr("rozed-dev", "e2e", 14)).unwrap(),
                None
            );
        });
    }

    /// GitHub's summary of a pull request in `state`, last updated `days` days before `now`.
    fn summary(number: i64, state: &str, days: u64, now: SystemTime) -> PullSummary {
        let updated =
            chrono::DateTime::<chrono::Utc>::from(now - Duration::from_secs(days * 86_400));
        serde_json::from_value(serde_json::json!({
            "number": number,
            "title": "t",
            "state": state,
            "updated_at": updated.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "head": {"ref": "h", "sha": "h1"},
            "base": {"ref": "main", "sha": "m1"},
        }))
        .unwrap()
    }

    #[test]
    fn pull_requests_closed_a_month_ago_are_forgotten() {
        block_on(async {
            let (db, marks) = marks("marks_prune", &["bot"]).await;
            let cache = PrCache::new(db);
            let now = at(100 * 86_400);
            // Closed 40 days ago, closed 10 days ago, open but untouched for 40, and never
            // summarized.
            let pulls = [
                (
                    pr("rozed-dev", "e2e", 1),
                    Some(summary(1, "closed", 40, now)),
                ),
                (
                    pr("rozed-dev", "e2e", 2),
                    Some(summary(2, "closed", 10, now)),
                ),
                (pr("rozed-dev", "e2e", 3), Some(summary(3, "open", 40, now))),
                (pr("rozed-dev", "e2e", 4), None),
            ];
            for (pull, summary) in &pulls {
                marks.record_seen("bot", pull, "h1", now).await.unwrap();
                marks.set_last_seen("bot", pull, "h1", now).await.unwrap();
                if let Some(summary) = summary {
                    let target = pull.clone().into();
                    cache.put("bot", &target, summary, None, now).await.unwrap();
                }
            }

            marks.prune(now).await.unwrap();

            let kept: Vec<i64> = (pulls.iter())
                .filter(|(pull, _)| !marks.seen_heads(pull).unwrap().is_empty())
                .map(|(pull, _)| pull.number)
                .collect();
            assert_eq!(kept, [2, 3, 4]);
            let last_seen: Vec<i64> = (pulls.iter())
                .filter(|(pull, _)| marks.last_seen("bot", pull).unwrap().is_some())
                .map(|(pull, _)| pull.number)
                .collect();
            assert_eq!(last_seen, [2, 3, 4]);
        });
    }

    #[test]
    fn clearing_the_cache_keeps_them_and_removing_the_account_removes_them() {
        block_on(async {
            let (db, marks) = marks("marks_lifetime", &["bot", "other"]).await;
            let e2e = pr("rozed-dev", "e2e", 16);
            marks.record_seen("bot", &e2e, "h1", at(10)).await.unwrap();
            marks
                .record_seen("other", &e2e, "h2", at(20))
                .await
                .unwrap();
            marks
                .set_last_seen("bot", &e2e, "h2", at(30))
                .await
                .unwrap();

            PrCache::new(db.clone()).clear().await.unwrap();
            assert_eq!(marks.seen_heads(&e2e).unwrap().len(), 2);
            assert!(marks.last_seen("bot", &e2e).unwrap().is_some());

            db.delete_account(HOST.into(), "bot".into()).await.unwrap();
            assert_eq!(marks.seen_heads(&e2e).unwrap(), [seen("h2", 20)]);
            assert_eq!(marks.last_seen("bot", &e2e).unwrap(), None);
        });
    }
}
