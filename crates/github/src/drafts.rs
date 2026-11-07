//! Text typed into comment boxes and not yet posted, kept on this Mac (design §11, "Unsent
//! text"): [`review::drafts::Drafts`] in Lo's database, saved as it changes and loaded at launch.
//!
//! Like viewed records ([`crate::viewed::ViewedRecords`]), drafts aren't cached answers: clearing
//! the cache keeps them, and removing the account removes its rows. Each row's comment box is
//! [`Place`] as JSON, written the same way each time, so it's part of the row's key.
//!
//! A draft is marked when it's sent ([`DraftStore::set_sent`]) and goes once it's posted. A write
//! that fails may still have posted ([`crate::GithubError::may_have_posted`]), and Lo may stop
//! before the answer comes, so a draft still marked sent at launch, or after such a failure, is
//! checked against your newest comments before the UI offers to send it again.
//!
//! Drafts aren't pruned by age: text typed on an open pull request is kept however old, since
//! losing it is worse than keeping it. A closed pull request's drafts go a month after it closed
//! ([`DraftStore::prune_closed`]), as its revision marks do, which also takes replies and edits
//! whose thread or comment was deleted.

use std::future::Future;
use std::time::{Duration, SystemTime};

use anyhow::{Context as _, Result};
use review::drafts::{DraftKey, Drafts, Place, PullRequestId};
use review::threads::{Line, Lines};
use serde::{Deserialize, Serialize};
use sqlez_macros::sql;

use crate::cache::{CacheEntry as _, from_secs, to_secs};
use crate::credentials::HOST;
use crate::db::LoDb;
use crate::pulls::PullSummary;
use crate::revision_marks::{CLOSED, STATE, UPDATED_AT};

/// Where a new thread's revision is in a row's comment box.
const THREAD_COMMIT: &str = "$.thread.commit";

/// The drafts in Lo's database.
#[derive(Clone)]
pub struct DraftStore {
    db: LoDb,
}

/// A draft marked sent ([`DraftStore::set_sent`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SentDraft {
    pub key: DraftKey,
    pub text: String,
    /// When it was sent.
    pub sent_at: SystemTime,
}

type Row = (String, String, String, i64, String, String, Option<i64>);

impl DraftStore {
    /// How long a closed pull request's drafts are kept after it closed.
    pub const KEPT_AFTER_CLOSING: Duration = Duration::from_secs(30 * 24 * 60 * 60);

    pub fn new(db: LoDb) -> Self {
        Self { db }
    }

    /// Every draft saved on this Mac, by any account. A row whose comment box Lo can't read, or
    /// wouldn't write the same way, as from another version, is left out and kept.
    pub fn load(&self) -> Result<Drafts> {
        Ok((self.rows()?.into_iter())
            .map(|(key, text, _)| (key, text))
            .collect())
    }

    /// The drafts marked sent, with their text, for the UI to check against your newest comments
    /// before it offers to send them again.
    pub fn sent(&self) -> Result<Vec<SentDraft>> {
        Ok((self.rows()?.into_iter())
            .filter_map(|(key, text, sent_at)| {
                Some(SentDraft {
                    key,
                    text,
                    sent_at: sent_at?,
                })
            })
            .collect())
    }

    fn rows(&self) -> Result<Vec<(DraftKey, String, Option<SystemTime>)>> {
        let rows = self.db.connection().select_bound::<&str, Row>(sql!(
            SELECT account, owner, repo, number, place, text, sent_at FROM drafts WHERE host = ?1
        ))?(HOST)?;
        Ok(rows
            .into_iter()
            .filter_map(|(account, owner, repo, number, place, text, sent_at)| {
                let place = (read_place(&place))
                    .inspect_err(|error| {
                        log::warn!("a draft's comment box Lo can't read: {error:#}")
                    })
                    .ok()?;
                let key = DraftKey {
                    account,
                    pull_request: PullRequestId {
                        owner,
                        repo,
                        number: u64::try_from(number).ok()?,
                    },
                    place,
                };
                Some((key, text, sent_at.map(from_secs)))
            })
            .collect())
    }

    /// Keeps `text` as the draft at `key`, changed at `at`, as [`Drafts::set`] keeps it: blank
    /// text removes the draft. Other text than it had clears its mark as sent.
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
                        sent_at = CASE WHEN text = excluded.text THEN sent_at ELSE NULL END,
                        text = excluded.text,
                        saved_at = excluded.saved_at
                ))?(row)
            })
            .await
    }

    /// Marks the draft at `key` sent at `at`, just before its write, or with `None` not sent, once
    /// the UI knows GitHub didn't take it. Posting it removes it ([`Self::remove`]).
    pub async fn set_sent(&self, key: &DraftKey, at: Option<SystemTime>) -> Result<()> {
        let row = (at.map(to_secs), columns(key)?);
        self.db
            .connection()
            .write(move |connection| {
                connection.exec_bound(sql!(
                    UPDATE drafts SET sent_at = ?1
                    WHERE host = ?2 AND account = ?3 AND owner = ?4 AND repo = ?5 AND number = ?6
                        AND place = ?7
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
            .select_bound::<(&str, &str, &str, i64, &str), String>(sql!(
                SELECT DISTINCT json_extract(place, ?5) FROM drafts
                WHERE host = ?1 AND owner = ?2 AND repo = ?3 AND number = ?4
                    AND json_extract(place, ?5) IS NOT NULL
            ))
            .and_then(|mut select| select((HOST, &pull.0, &pull.1, pull.2, THREAD_COMMIT)));
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
                                AND json_extract(place, ?6) = ?5
                            RETURNING number
                        ))?((
                            HOST,
                            pull.0.as_str(),
                            pull.1.as_str(),
                            pull.2,
                            commit.as_str(),
                            THREAD_COMMIT,
                        ))?;
                        removed += rows.len();
                    }
                    Ok(removed)
                })
                .await
        }
    }

    /// Forgets the drafts on every pull request closed before [`Self::KEPT_AFTER_CLOSING`] ago, at
    /// `now`, that were saved before then too, as [`crate::revision_marks::RevisionMarks::prune`]
    /// forgets marks: it's closed when every account's cached summary of it says so. A pull
    /// request with no summary in the cache, or one any account's summary says is open or was
    /// updated since, keeps its drafts.
    pub async fn prune_closed(&self, now: SystemTime) -> Result<()> {
        let cutoff = now - Self::KEPT_AFTER_CLOSING;
        let cutoff_secs = to_secs(cutoff);
        let cutoff = chrono::DateTime::<chrono::Utc>::from(cutoff)
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let bindings = (
            PullSummary::NAME,
            cutoff,
            STATE,
            CLOSED,
            UPDATED_AT,
            cutoff_secs,
        );
        self.db
            .connection()
            .write(move |connection| {
                connection.exec_bound(sql!(
                    DELETE FROM drafts WHERE saved_at < ?6 AND EXISTS (
                        SELECT pull.number FROM pr_cache AS pull
                        WHERE pull.host = drafts.host AND pull.owner = drafts.owner
                            AND pull.repo = drafts.repo AND pull.number = drafts.number
                            AND pull.query = ?1
                    ) AND NOT EXISTS (
                        SELECT pull.number FROM pr_cache AS pull
                        WHERE pull.host = drafts.host AND pull.owner = drafts.owner
                            AND pull.repo = drafts.repo AND pull.number = drafts.number
                            AND pull.query = ?1
                            AND (json_extract(pull.json, ?3) IS NOT ?4
                                OR json_extract(pull.json, ?5) >= ?2)
                    )
                ))?(bindings)
            })
            .await
    }
}

/// A draft's key as the table's columns, its login, owner and repository lowercased as
/// [`Drafts`] keeps them.
fn columns(key: &DraftKey) -> Result<(String, String, String, String, i64, String)> {
    let (owner, repo, number) = pull_request_columns(&key.pull_request);
    Ok((
        HOST.to_string(),
        key.account.to_ascii_lowercase(),
        owner,
        repo,
        number,
        write_place(&key.place)?,
    ))
}

fn pull_request_columns(pull_request: &PullRequestId) -> (String, String, i64) {
    (
        pull_request.owner.to_ascii_lowercase(),
        pull_request.repo.to_ascii_lowercase(),
        i64::try_from(pull_request.number).unwrap_or(i64::MAX),
    )
}

fn write_place(place: &Place) -> Result<String> {
    Ok(serde_json::to_string(&StoredPlace::from(place))?)
}

/// The comment box a row's `place` holds. It fails unless Lo would write that box the same way,
/// so a row is never read under a key that would miss it: removing a posted draft, or saving it
/// again, would leave the row behind.
fn read_place(text: &str) -> Result<Place> {
    let stored: StoredPlace = serde_json::from_str(text)?;
    let place = Place::try_from(stored)?;
    let written = write_place(&place)?;
    anyhow::ensure!(
        written == text,
        "Lo would write it as {written}, not as it's stored, {text}"
    );
    Ok(place)
}

/// [`Place`] as the table holds it: `{"thread": {"commit", "path", "lines"}}`,
/// `{"reply": {"thread"}}`, `{"edit": {"comment"}}`, `"summary"` or `"conversation"`. Its fields
/// are written in this order, so one box is always the same text, and it's part of the row's
/// key. So a field added later must be optional and left out when it's unset
/// (`skip_serializing_if`), or come with a migration that rewrites the rows' `place`: a row Lo
/// wouldn't write the same way is left out.
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

impl TryFrom<StoredPlace> for Place {
    type Error = anyhow::Error;

    /// The box, failing on a side Lo doesn't know rather than guessing one.
    fn try_from(place: StoredPlace) -> Result<Self> {
        let line = |line: StoredLine| -> Result<Line> {
            let side = (line.side.parse()).with_context(|| format!("side {:?}", line.side))?;
            Ok(Line::new(side, line.line))
        };
        Ok(match place {
            StoredPlace::Thread {
                commit,
                path,
                lines,
            } => Self::Thread {
                commit,
                path,
                lines: match lines {
                    Some(lines) => Some(Lines {
                        start: lines.start.map(line).transpose()?,
                        end: line(lines.end)?,
                    }),
                    None => None,
                },
            },
            StoredPlace::Reply { thread } => Self::Reply { thread },
            StoredPlace::Edit { comment } => Self::Edit { comment },
            StoredPlace::Summary => Self::Summary,
            StoredPlace::Conversation => Self::Conversation,
        })
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
    fn a_comment_box_lo_cant_read_or_would_write_otherwise_is_left_out_and_kept() {
        block_on(async {
            let (db, store) = store("drafts_unknown_place", &["bot"]).await;
            store
                .save(&key("bot", 23, Place::Summary), "Summary", at(1))
                .await
                .unwrap();
            let unknown_side = r#"{"thread":{"commit":"a3","path":"e11/review.txt","lines":{"start":null,"end":{"side":"MIDDLE","line":10}}}}"#;
            // Fields in another order: read as it stands, it would be saved and removed as
            // another row.
            let reordered = r#"{"thread":{"path":"e11/review.txt","commit":"a3","lines":null}}"#;
            for place in [r#"{"poll":{}}"#, unknown_side, reordered] {
                db.connection()
                    .write(move |connection| {
                        connection.exec_bound::<&str>(sql!(
                            INSERT INTO drafts(host, account, owner, repo, number, place, text, saved_at)
                            VALUES ("github.com", "bot", "rozed-dev", "e2e", 23, ?1, "?", 1)
                        ))?(place)
                    })
                    .await
                    .unwrap();
            }
            assert_eq!(store.load().unwrap().len(), 1);
            let kept = db
                .connection()
                .select::<i64>(sql!(SELECT count(*) FROM drafts))
                .unwrap()();
            assert_eq!(kept.unwrap(), [4]);
        });
    }

    #[test]
    fn a_draft_marked_sent_stays_so_until_its_text_changes() {
        block_on(async {
            let (_, store) = store("drafts_sent", &["bot"]).await;
            let line = key("bot", 23, thread("a3", Some(Lines::one(Line::right(10)))));
            let summary = key("bot", 23, Place::Summary);
            store.save(&line, "Off by one?", at(1)).await.unwrap();
            store.save(&summary, "Looks good.", at(1)).await.unwrap();
            assert_eq!(store.sent().unwrap(), []);

            store.set_sent(&line, Some(at(5))).await.unwrap();
            let sent = SentDraft {
                key: line.clone(),
                text: "Off by one?".into(),
                sent_at: at(5),
            };
            assert_eq!(store.sent().unwrap(), std::slice::from_ref(&sent));
            // Saving the same text again, as a box does when it's shown, keeps the mark.
            store.save(&line, "Off by one?", at(6)).await.unwrap();
            assert_eq!(store.sent().unwrap(), [sent]);
            // New text is a draft not yet sent.
            store.save(&line, "Off by two?", at(7)).await.unwrap();
            assert_eq!(store.sent().unwrap(), []);

            store.set_sent(&line, Some(at(8))).await.unwrap();
            store.set_sent(&line, None).await.unwrap();
            assert_eq!(store.sent().unwrap(), []);
            assert_eq!(store.load().unwrap().len(), 2);
        });
    }

    /// GitHub's summary of a pull request in `state`, last updated `days` days before `now`.
    fn summary(number: i64, state: &str, days: u64, now: SystemTime) -> crate::PullSummary {
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
    fn drafts_on_pull_requests_closed_a_month_ago_are_forgotten() {
        block_on(async {
            let (db, store) = store("drafts_prune_closed", &["bot", "other"]).await;
            let cache = PrCache::new(db);
            let now = at(100 * 86_400);
            let saved = now - Duration::from_secs(45 * 86_400);
            // Closed 40 days ago; closed 10 days ago; open but untouched for 40; never
            // summarized; closed as one account last saw it, but open as the other sees it now;
            // and closed 40 days ago but written in since.
            let pulls = [
                (1, vec![summary(1, "closed", 40, now)], saved),
                (2, vec![summary(2, "closed", 10, now)], saved),
                (3, vec![summary(3, "open", 40, now)], saved),
                (4, vec![], saved),
                (
                    5,
                    vec![summary(5, "closed", 40, now), summary(5, "open", 1, now)],
                    saved,
                ),
                (6, vec![summary(6, "closed", 40, now)], now),
            ];
            for (number, summaries, saved_at) in &pulls {
                let reply = Place::Reply {
                    thread: "PRRT_1".into(),
                };
                let number = u64::try_from(*number).unwrap();
                store
                    .save(&key("bot", number, reply), "A reply", *saved_at)
                    .await
                    .unwrap();
                let pull = crate::PullRef::new("rozed-dev", "e2e", i64::try_from(number).unwrap());
                for (summary, account) in summaries.iter().zip(["bot", "other"]) {
                    let target = pull.clone().into();
                    cache
                        .put(account, &target, summary, None, now)
                        .await
                        .unwrap();
                }
            }

            store.prune_closed(now).await.unwrap();

            let kept: Vec<u64> = (store.load().unwrap().iter())
                .map(|(key, _)| key.pull_request.number)
                .collect();
            assert_eq!(kept, [2, 3, 4, 5, 6]);
        });
    }
}
