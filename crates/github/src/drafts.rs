//! Text typed into comment boxes and not yet posted, kept on this Mac (design §11, "Unsent
//! text"): [`review::drafts::Drafts`] in Lo's database, saved as it changes and loaded at launch.
//!
//! Like viewed records ([`crate::viewed::ViewedRecords`]), drafts aren't cached answers: clearing
//! the cache keeps them, and removing the account removes its rows. Each row's comment box is
//! [`Place`] as JSON, written the same way each time, so it's part of the row's key.

use std::future::Future;
use std::time::SystemTime;

use anyhow::Result;
use review::drafts::{DraftKey, Drafts, Place, PullRequestId};
use review::threads::{DiffSide, Line, Lines};
use serde::{Deserialize, Serialize};
use sqlez_macros::sql;

use crate::cache::to_secs;
use crate::credentials::HOST;
use crate::db::LoDb;

/// The drafts in Lo's database.
#[derive(Clone)]
pub struct DraftStore {
    db: LoDb,
}

impl DraftStore {
    pub fn new(db: LoDb) -> Self {
        Self { db }
    }

    /// Every draft saved on this Mac, by any account. A row whose comment box Lo can't read, as
    /// from a later version, is left out and kept.
    pub fn load(&self) -> Result<Drafts> {
        let rows = self
            .db
            .connection()
            .select_bound::<&str, (String, String, String, i64, String, String)>(sql!(
                SELECT account, owner, repo, number, place, text FROM drafts WHERE host = ?1
            ))?(HOST)?;
        Ok(rows
            .into_iter()
            .filter_map(|(account, owner, repo, number, place, text)| {
                let place = (serde_json::from_str::<StoredPlace>(&place))
                    .inspect_err(|error| log::warn!("a draft's comment box Lo can't read: {error}"))
                    .ok()?;
                let key = DraftKey {
                    account,
                    pull_request: PullRequestId {
                        owner,
                        repo,
                        number: u64::try_from(number).ok()?,
                    },
                    place: place.into(),
                };
                Some((key, text))
            })
            .collect())
    }

    /// Keeps `text` as the draft at `key`, changed at `at`, as [`Drafts::set`] keeps it: blank
    /// text removes the draft.
    pub async fn save(&self, key: &DraftKey, text: &str, at: SystemTime) -> Result<()> {
        if text.trim().is_empty() {
            return self.remove(key).await;
        }
        let row = (columns(key)?, text.to_string(), to_secs(at));
        self.db
            .connection()
            .write(move |connection| {
                connection.exec_bound(sql!(
                    INSERT INTO drafts(host, account, owner, repo, number, place, text, saved_at)
                    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                    ON CONFLICT(host, account, owner, repo, number, place) DO UPDATE SET
                        text = excluded.text,
                        saved_at = excluded.saved_at
                ))?(row)
            })
            .await
    }

    /// Removes the draft at `key`, as [`Drafts::take`] does once its comment is posted or its box
    /// is closed.
    pub async fn remove(&self, key: &DraftKey) -> Result<()> {
        let key = columns(key)?;
        self.db
            .connection()
            .write(move |connection| {
                connection.exec_bound(sql!(
                    DELETE FROM drafts
                    WHERE host = ?1 AND account = ?2 AND owner = ?3 AND repo = ?4 AND number = ?5
                        AND place = ?6
                ))?(key)
            })
            .await
    }

    /// Removes the new threads' drafts on `pull_request`, by any account, written on revisions
    /// that `exists` says are gone, as [`Drafts::prune`] does. The returned future removes them
    /// and says how many it removed.
    pub fn prune<E: FnMut(&str) -> bool>(
        &self,
        pull_request: &PullRequestId,
        mut exists: E,
    ) -> impl Future<Output = Result<usize>> + use<E> {
        let pull = pull_request_columns(pull_request);
        let commits = self
            .db
            .connection()
            .select_bound::<(&str, &str, &str, i64), String>(sql!(
                SELECT DISTINCT json_extract(place, "$.thread.commit") FROM drafts
                WHERE host = ?1 AND owner = ?2 AND repo = ?3 AND number = ?4
                    AND json_extract(place, "$.thread.commit") IS NOT NULL
            ))
            .and_then(|mut select| select((HOST, &pull.0, &pull.1, pull.2)));
        let gone = commits.map(|commits| {
            (commits.into_iter())
                .filter(|commit| !exists(commit))
                .collect::<Vec<_>>()
        });
        let db = self.db.clone();
        async move {
            let gone = gone?;
            if gone.is_empty() {
                return Ok(0);
            }
            db.connection()
                .write(move |connection| {
                    let mut removed = 0;
                    for commit in gone {
                        let rows = connection.select_bound::<_, i64>(sql!(
                            DELETE FROM drafts
                            WHERE host = ?1 AND owner = ?2 AND repo = ?3 AND number = ?4
                                AND json_extract(place, "$.thread.commit") = ?5
                            RETURNING number
                        ))?((
                            HOST,
                            pull.0.as_str(),
                            pull.1.as_str(),
                            pull.2,
                            commit.as_str(),
                        ))?;
                        removed += rows.len();
                    }
                    Ok(removed)
                })
                .await
        }
    }
}

/// A draft's key as the table's columns, its login, owner and repository lowercased as
/// [`Drafts`] keeps them.
fn columns(key: &DraftKey) -> Result<(String, String, String, String, i64, String)> {
    let (owner, repo, number) = pull_request_columns(&key.pull_request);
    let place = serde_json::to_string(&StoredPlace::from(&key.place))?;
    Ok((
        HOST.to_string(),
        key.account.to_ascii_lowercase(),
        owner,
        repo,
        number,
        place,
    ))
}

fn pull_request_columns(pull_request: &PullRequestId) -> (String, String, i64) {
    (
        pull_request.owner.to_ascii_lowercase(),
        pull_request.repo.to_ascii_lowercase(),
        i64::try_from(pull_request.number).unwrap_or(i64::MAX),
    )
}

/// [`Place`] as the table holds it: `{"thread": {"commit", "path", "lines"}}`,
/// `{"reply": {"thread"}}`, `{"edit": {"comment"}}`, `"summary"` or `"conversation"`. Its fields
/// are written in this order, so one box is always the same text.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StoredPlace {
    Thread {
        commit: String,
        path: String,
        /// `None` for a file-level comment.
        lines: Option<StoredLines>,
    },
    Reply {
        thread: String,
    },
    Edit {
        comment: String,
    },
    Summary,
    Conversation,
}

/// [`Lines`], with GitHub's names for the sides.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct StoredLines {
    start: Option<StoredLine>,
    end: StoredLine,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct StoredLine {
    side: String,
    line: u32,
}

impl From<&Place> for StoredPlace {
    fn from(place: &Place) -> Self {
        let line = |line: Line| StoredLine {
            side: line.side.as_str().to_string(),
            line: line.number,
        };
        match place {
            Place::Thread {
                commit,
                path,
                lines,
            } => Self::Thread {
                commit: commit.clone(),
                path: path.clone(),
                lines: lines.map(|lines| StoredLines {
                    start: lines.start.map(line),
                    end: line(lines.end),
                }),
            },
            Place::Reply { thread } => Self::Reply {
                thread: thread.clone(),
            },
            Place::Edit { comment } => Self::Edit {
                comment: comment.clone(),
            },
            Place::Summary => Self::Summary,
            Place::Conversation => Self::Conversation,
        }
    }
}

impl From<StoredPlace> for Place {
    fn from(place: StoredPlace) -> Self {
        // A side Lo doesn't know reads as the head's, as a thread's does.
        let line =
            |line: StoredLine| Line::new(line.side.parse().unwrap_or(DiffSide::Right), line.line);
        match place {
            StoredPlace::Thread {
                commit,
                path,
                lines,
            } => Self::Thread {
                commit,
                path,
                lines: lines.map(|lines| Lines {
                    start: lines.start.map(line),
                    end: line(lines.end),
                }),
            },
            StoredPlace::Reply { thread } => Self::Reply { thread },
            StoredPlace::Edit { comment } => Self::Edit { comment },
            StoredPlace::Summary => Self::Summary,
            StoredPlace::Conversation => Self::Conversation,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::{Duration, UNIX_EPOCH};

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

    async fn store(name: &str, logins: &[&str]) -> (LoDb, DraftStore) {
        let db = LoDb::open_test(name).await;
        for login in logins {
            db.save_account(account(login)).await.unwrap();
        }
        (db.clone(), DraftStore::new(db))
    }

    fn pull_request(number: u64) -> PullRequestId {
        PullRequestId {
            owner: "rozed-dev".into(),
            repo: "e2e".into(),
            number,
        }
    }

    fn key(account: &str, number: u64, place: Place) -> DraftKey {
        DraftKey {
            account: account.into(),
            pull_request: pull_request(number),
            place,
        }
    }

    fn thread(commit: &str, lines: Option<Lines>) -> Place {
        Place::Thread {
            commit: commit.into(),
            path: "e11/review.txt".into(),
            lines,
        }
    }

    /// One of each comment box, with a range from the base's side to the head's.
    fn every_place() -> Vec<Place> {
        vec![
            thread("a3", Some(Lines::one(Line::right(10)))),
            thread("a3", Some(Lines::range(Line::left(10), Line::right(11)))),
            thread("a3", None),
            Place::Reply {
                thread: "PRRT_1".into(),
            },
            Place::Edit {
                comment: "PRRC_1".into(),
            },
            Place::Summary,
            Place::Conversation,
        ]
    }

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn every_comment_box_round_trips() {
        block_on(async {
            let (_, store) = store("drafts_round_trip", &["bot"]).await;
            let mut drafts = Drafts::new();
            for (index, place) in every_place().into_iter().enumerate() {
                let key = key("bot", 23, place);
                let text = format!("Draft {index}");
                drafts.set(key.clone(), &text);
                store.save(&key, &text, at(1)).await.unwrap();
            }
            assert_eq!(store.load().unwrap(), drafts);
        });
    }

    #[test]
    fn a_comment_box_is_written_the_same_way_each_time() {
        let range = thread("a3", Some(Lines::range(Line::left(10), Line::right(11))));
        assert_eq!(
            serde_json::to_string(&StoredPlace::from(&range)).unwrap(),
            r#"{"thread":{"commit":"a3","path":"e11/review.txt","lines":{"start":{"side":"LEFT","line":10},"end":{"side":"RIGHT","line":11}}}}"#
        );
        assert_eq!(
            serde_json::to_string(&StoredPlace::from(&Place::Summary)).unwrap(),
            r#""summary""#
        );
    }

    #[test]
    fn saving_again_replaces_the_text_and_blank_text_removes_it() {
        block_on(async {
            let (_, store) = store("drafts_save", &["bot"]).await;
            let summary = key("bot", 23, Place::Summary);
            store.save(&summary, "Looks", at(1)).await.unwrap();
            store.save(&summary, "Looks good.", at(2)).await.unwrap();
            // Logins, owners and repositories ignore case, as on GitHub.
            let shouted = DraftKey {
                account: "BOT".into(),
                pull_request: PullRequestId {
                    owner: "Rozed-Dev".into(),
                    repo: "E2E".into(),
                    number: 23,
                },
                place: Place::Summary,
            };
            assert_eq!(store.load().unwrap().get(&shouted), Some("Looks good."));
            assert_eq!(store.load().unwrap().len(), 1);

            store.save(&shouted, " \n", at(3)).await.unwrap();
            assert!(store.load().unwrap().is_empty());
        });
    }

    #[test]
    fn a_posted_drafts_row_is_removed() {
        block_on(async {
            let (_, store) = store("drafts_remove", &["bot"]).await;
            let reply = key(
                "bot",
                23,
                Place::Reply {
                    thread: "PRRT_1".into(),
                },
            );
            store.save(&reply, "Agreed.", at(1)).await.unwrap();
            store
                .save(&key("bot", 23, Place::Summary), "Summary", at(1))
                .await
                .unwrap();

            store.remove(&reply).await.unwrap();
            // Removing one that isn't there does nothing.
            store.remove(&reply).await.unwrap();

            let left: Vec<_> = (store.load().unwrap().iter())
                .map(|(key, _)| key.place.clone())
                .collect();
            assert_eq!(left, [Place::Summary]);
        });
    }

    #[test]
    fn pruning_removes_new_threads_drafts_on_revisions_that_are_gone() {
        block_on(async {
            let (_, store) = store("drafts_prune", &["bot", "other"]).await;
            let line = || Some(Lines::one(Line::right(10)));
            for (key, text) in [
                (key("bot", 23, thread("a1", line())), "Gone"),
                (key("other", 23, thread("a1", None)), "Another's, gone"),
                (key("bot", 23, thread("a2", line())), "Kept"),
                (key("bot", 23, Place::Summary), "Summary"),
                (key("bot", 24, thread("a1", line())), "On #24"),
            ] {
                store.save(&key, text, at(1)).await.unwrap();
            }
            let mut asked = Vec::new();
            let removed = store
                .prune(&pull_request(23), |commit| {
                    asked.push(commit.to_string());
                    commit != "a1"
                })
                .await
                .unwrap();
            assert_eq!(removed, 2);
            asked.sort();
            assert_eq!(asked, ["a1", "a2"]);
            let mut left: Vec<String> = (store.load().unwrap().iter())
                .map(|(_, text)| text.to_string())
                .collect();
            left.sort();
            assert_eq!(left, ["Kept", "On #24", "Summary"]);
            assert_eq!(store.prune(&pull_request(23), |_| false).await.unwrap(), 1);
        });
    }

    #[test]
    fn clearing_the_cache_keeps_them_and_removing_the_account_removes_them() {
        block_on(async {
            let (db, store) = store("drafts_lifetime", &["bot"]).await;
            store
                .save(&key("bot", 23, Place::Conversation), "Thanks!", at(1))
                .await
                .unwrap();

            PrCache::new(db.clone()).clear().await.unwrap();
            assert_eq!(store.load().unwrap().len(), 1);

            db.delete_account(HOST.into(), "bot".into()).await.unwrap();
            assert!(store.load().unwrap().is_empty());
        });
    }

    #[test]
    fn a_comment_box_lo_cant_read_is_left_out() {
        block_on(async {
            let (db, store) = store("drafts_unknown_place", &["bot"]).await;
            store
                .save(&key("bot", 23, Place::Summary), "Summary", at(1))
                .await
                .unwrap();
            db.connection()
                .write(|connection| {
                    connection.exec_bound::<&str>(sql!(
                        INSERT INTO drafts(host, account, owner, repo, number, place, text, saved_at)
                        VALUES ("github.com", "bot", "rozed-dev", "e2e", 23, ?1, "?", 1)
                    ))?(r#"{"poll":{}}"#)
                })
                .await
                .unwrap();
            assert_eq!(store.load().unwrap().len(), 1);
        });
    }
}
