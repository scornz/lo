//! GitHub's answers, cached per account in Lo's database (design §3 "Local data"): a reopened
//! PR shows at once, its first poll can still be a free `304`, and an offline Lo shows what it
//! last saw with how old it is (design §11 "Offline").
//!
//! Each row is one answer about one pull request, or about a repository as a whole, fetched with
//! one account. Rows carry the shape they were written in, so after an update changes a query, an
//! old row reads as missing and is fetched again rather than misread. The cache is the only table
//! that's ever cleared, and removing an account removes its rows.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};
use schemars::JsonSchema;
use serde::Serialize;
use serde::de::DeserializeOwned;
use sha2::{Digest as _, Sha256};
use sqlez_macros::sql;

use crate::client::API_VERSION;
use crate::credentials::HOST;
use crate::db::LoDb;
use crate::pulls::{ListedPulls, PullRef, PullSummary, Target};
use crate::queries::merged_pull_requests::{MAX_BRANCH_LOOKUPS, MERGED_PER_BRANCH};
use crate::queries::open_pull_requests::RECENTLY_UPDATED_MERGED;
use crate::queries::{
    MAX_OPEN_PULL_REQUESTS, OpenPullRequestList, PAGE_SIZE, PullRequestCheckData, PullRequestData,
    merged_pull_requests, open_pull_requests, pull_request, pull_request_check,
};

/// Something the cache can hold.
pub trait CacheEntry: Serialize + DeserializeOwned {
    /// The name its rows are stored under.
    const NAME: &'static str;

    /// Everything the shape of its stored JSON depends on: the JSON schema of a type Lo
    /// defines ([`schema_shape`]), the text of the GraphQL query a generated type comes from, and
    /// anything else that changes what a stored answer means.
    fn shape() -> String;

    /// The version its rows are written in: a hash of its [`Self::shape`]. A row in another
    /// version reads as missing, so a change to the shape can't be misread or kept forever
    /// behind an unchanged ETag.
    fn version() -> String {
        let hash = format!("{:x}", Sha256::digest(Self::shape().as_bytes()));
        hash[..16].to_string()
    }
}

/// The shape of a type Lo defines: its JSON schema, with keys sorted, and without
/// documentation or type names, which change nothing that's stored.
pub fn schema_shape<T: JsonSchema>() -> String {
    let mut schema = schemars::schema_for!(T).to_value();
    strip_annotations(&mut schema);
    canonical(&schema)
}

/// Removes the schema's titles and descriptions, leaving property names alone.
fn strip_annotations(schema: &mut serde_json::Value) {
    match schema {
        serde_json::Value::Object(map) => {
            map.remove("title");
            map.remove("description");
            for (key, value) in map.iter_mut() {
                match (key.as_str(), value) {
                    // Maps from a property's or a definition's name to its schema.
                    ("properties" | "$defs" | "definitions", serde_json::Value::Object(named)) => {
                        named.values_mut().for_each(strip_annotations);
                    }
                    (_, value) => strip_annotations(value),
                }
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(strip_annotations),
        _ => {}
    }
}

/// JSON with every object's keys sorted, whatever order the map kept them in.
fn canonical(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            let entries: Vec<String> = entries
                .into_iter()
                .map(|(key, value)| {
                    format!(
                        "{}:{}",
                        serde_json::Value::from(key.as_str()),
                        canonical(value)
                    )
                })
                .collect();
            format!("{{{}}}", entries.join(","))
        }
        serde_json::Value::Array(items) => {
            let items: Vec<String> = items.iter().map(canonical).collect();
            format!("[{}]", items.join(","))
        }
        other => other.to_string(),
    }
}

impl CacheEntry for PullSummary {
    const NAME: &'static str = "pull";

    fn shape() -> String {
        format!("REST {API_VERSION}\n{}", schema_shape::<Self>())
    }
}

impl CacheEntry for ListedPulls {
    const NAME: &'static str = "pulls";

    fn shape() -> String {
        format!(
            "REST {API_VERSION}, {PAGE_SIZE} a page\n{}",
            schema_shape::<Self>()
        )
    }
}

impl CacheEntry for PullRequestData {
    const NAME: &'static str = pull_request::OPERATION_NAME;

    fn shape() -> String {
        pull_request::QUERY.to_string()
    }
}

impl CacheEntry for PullRequestCheckData {
    const NAME: &'static str = pull_request_check::OPERATION_NAME;

    fn shape() -> String {
        pull_request_check::QUERY.to_string()
    }
}

impl CacheEntry for OpenPullRequestList {
    const NAME: &'static str = open_pull_requests::OPERATION_NAME;

    fn shape() -> String {
        format!(
            "{}\n{}\n{}\nat most {MAX_OPEN_PULL_REQUESTS}, {PAGE_SIZE} a page, with the \
             {RECENTLY_UPDATED_MERGED} merged updated most recently and {MERGED_PER_BRANCH} \
             merged from each of at most {MAX_BRANCH_LOOKUPS} branches",
            schema_shape::<Self>(),
            open_pull_requests::QUERY,
            merged_pull_requests::QUERY
        )
    }
}

/// A cached answer and when GitHub last confirmed it.
#[derive(Clone, Debug, PartialEq)]
pub struct Cached<T> {
    pub value: T,
    /// The ETag of the REST answer that tells whether this one changed.
    pub etag: Option<String>,
    pub fetched_at: SystemTime,
}

impl<T> Cached<T> {
    /// How old the answer is at `now`.
    pub fn age_at(&self, now: SystemTime) -> Duration {
        now.duration_since(self.fetched_at).unwrap_or_default()
    }

    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Cached<U> {
        Cached {
            value: f(self.value),
            etag: self.etag,
            fetched_at: self.fetched_at,
        }
    }
}

/// How many recent pull requests are kept.
pub const RECENT_PRS: usize = 100;

/// A pull request opened recently.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecentPr {
    pub pull: PullRef,
    /// Unknown until the pull request was fetched once.
    pub title: Option<String>,
    /// The account it was opened with.
    pub account: String,
    pub opened_at: SystemTime,
}

/// An answer encoded for the cache, from [`PrCache::row`].
pub struct Row {
    query: &'static str,
    version: String,
    json: String,
    etag: Option<String>,
}

/// The cache, over Lo's database. Clones share the connection.
#[derive(Clone)]
pub struct PrCache {
    db: LoDb,
}

pub(crate) fn to_secs(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

pub(crate) fn from_secs(secs: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(u64::try_from(secs).unwrap_or_default())
}

/// The columns that pick out a row.
fn key<T: CacheEntry>(
    account: &str,
    target: &Target,
) -> (String, String, String, String, i64, String) {
    let repo = target.repo();
    (
        HOST.to_string(),
        account.to_string(),
        repo.owner.clone(),
        repo.name.clone(),
        target.number(),
        T::NAME.to_string(),
    )
}

impl PrCache {
    pub fn new(db: LoDb) -> Self {
        Self { db }
    }

    pub(crate) fn db(&self) -> &LoDb {
        &self.db
    }

    /// `account`'s cached answer about `target`, if there's one in the current shape.
    pub fn get<T: CacheEntry>(&self, account: &str, target: &Target) -> Result<Option<Cached<T>>> {
        let repo = target.repo();
        let row = self.db.connection().select_row_bound::<
            (&str, &str, &str, &str, i64, &str),
            (String, String, Option<String>, i64),
        >(sql!(
            SELECT version, json, etag, fetched_at FROM pr_cache
            WHERE host = ?1 AND account = ?2 AND owner = ?3 AND repo = ?4 AND number = ?5
                AND query = ?6
        ))?((
            HOST,
            account,
            &repo.owner,
            &repo.name,
            target.number(),
            T::NAME,
        ))?;
        let Some((version, json, etag, fetched_at)) = row else {
            return Ok(None);
        };
        if version != T::version() {
            return Ok(None);
        }
        match serde_json::from_str(&json) {
            Ok(value) => Ok(Some(Cached {
                value,
                etag,
                fetched_at: from_secs(fetched_at),
            })),
            Err(error) => {
                // serde's message can quote the JSON, which holds private repositories' titles
                // and paths: log only what kind of error it was, and where.
                log::warn!(
                    "ignoring a cached {} for {target:?}: {:?} error at line {}, column {}",
                    T::NAME,
                    error.classify(),
                    error.line(),
                    error.column()
                );
                Ok(None)
            }
        }
    }

    /// Stores an answer, replacing any earlier one.
    pub async fn put<T: CacheEntry>(
        &self,
        account: &str,
        target: &Target,
        value: &T,
        etag: Option<&str>,
        fetched_at: SystemTime,
    ) -> Result<()> {
        let row = Self::row(value, etag)?;
        self.put_together(account, target, vec![row], fetched_at)
            .await
    }

    /// An answer ready for [`Self::put_together`].
    pub fn row<T: CacheEntry>(value: &T, etag: Option<&str>) -> Result<Row> {
        Ok(Row {
            query: T::NAME,
            version: T::version(),
            json: serde_json::to_string(value).context("encoding a cache entry")?,
            etag: etag.map(str::to_string),
        })
    }

    /// Stores answers about one target that only make sense together, such as details and the
    /// check they were fetched at: all of them, or, if any fails, none.
    pub async fn put_together(
        &self,
        account: &str,
        target: &Target,
        rows: Vec<Row>,
        fetched_at: SystemTime,
    ) -> Result<()> {
        let repo = target.repo();
        let key = (
            HOST.to_string(),
            account.to_string(),
            repo.owner.clone(),
            repo.name.clone(),
            target.number(),
        );
        let fetched_at = to_secs(fetched_at);
        self.db
            .connection()
            .write(move |connection| {
                connection.with_savepoint("put_together", || {
                    for row in rows {
                        let (host, account, owner, repo, number) = key.clone();
                        connection.exec_bound(sql!(
                            INSERT INTO pr_cache(
                                host, account, owner, repo, number, query, version, json, etag,
                                fetched_at
                            )
                            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                            ON CONFLICT(host, account, owner, repo, number, query) DO UPDATE SET
                                version = excluded.version,
                                json = excluded.json,
                                etag = excluded.etag,
                                fetched_at = excluded.fetched_at
                        ))?((
                            host,
                            account,
                            owner,
                            repo,
                            number,
                            row.query.to_string(),
                            row.version,
                            row.json,
                            row.etag,
                            fetched_at,
                        ))?;
                    }
                    Ok(())
                })
            })
            .await
    }

    /// Records that GitHub confirmed a cached answer is still current at `fetched_at`.
    pub async fn touch<T: CacheEntry>(
        &self,
        account: &str,
        target: &Target,
        fetched_at: SystemTime,
    ) -> Result<()> {
        let (key, version) = (key::<T>(account, target), T::version());
        let fetched_at = to_secs(fetched_at);
        self.db
            .connection()
            .write(move |connection| {
                connection.exec_bound(sql!(
                    UPDATE pr_cache SET fetched_at = ?8
                    WHERE host = ?1 AND account = ?2 AND owner = ?3 AND repo = ?4
                        AND number = ?5 AND query = ?6 AND version = ?7
                ))?((key, version, fetched_at))
            })
            .await
    }

    /// Replaces the value at `path` in a stored answer's JSON with the JSON `json`, and records
    /// that GitHub confirmed the answer at `fetched_at`.
    pub(crate) async fn update_json<T: CacheEntry>(
        &self,
        account: &str,
        target: &Target,
        path: &'static str,
        json: String,
        fetched_at: SystemTime,
    ) -> Result<()> {
        let (key, version) = (key::<T>(account, target), T::version());
        let fetched_at = to_secs(fetched_at);
        self.db
            .connection()
            .write(move |connection| {
                connection.exec_bound(sql!(
                    UPDATE pr_cache SET json = json_set(json, ?8, json(?9)), fetched_at = ?10
                    WHERE host = ?1 AND account = ?2 AND owner = ?3 AND repo = ?4
                        AND number = ?5 AND query = ?6 AND version = ?7
                ))?((key, version, path.to_string(), json, fetched_at))
            })
            .await
    }

    /// Forgets an answer.
    pub async fn remove<T: CacheEntry>(&self, account: &str, target: &Target) -> Result<()> {
        let key = key::<T>(account, target);
        self.db
            .connection()
            .write(move |connection| {
                connection.exec_bound(sql!(
                    DELETE FROM pr_cache
                    WHERE host = ?1 AND account = ?2 AND owner = ?3 AND repo = ?4
                        AND number = ?5 AND query = ?6
                ))?(key)
            })
            .await
    }

    /// Throws the whole cache away. Accounts, their owners and the recent PRs stay.
    pub async fn clear(&self) -> Result<()> {
        self.db
            .connection()
            .write(|connection| connection.exec(sql!(DELETE FROM pr_cache))?())
            .await
    }

    /// Notes that a pull request was opened, moving it to the top of the recent ones, and
    /// forgets all but the newest [`RECENT_PRS`]. A `None` title keeps the one already known.
    pub async fn record_recent(&self, recent: RecentPr) -> Result<()> {
        let row = (
            HOST.to_string(),
            recent.pull.repo.owner,
            recent.pull.repo.name,
            recent.pull.number,
            recent.title,
            recent.account,
            to_secs(recent.opened_at),
        );
        self.db
            .connection()
            .write(move |connection| {
                connection.exec_bound(sql!(
                    INSERT INTO recent_prs(host, owner, repo, number, title, account, opened_at)
                    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                    ON CONFLICT(host, owner, repo, number) DO UPDATE SET
                        owner = excluded.owner,
                        repo = excluded.repo,
                        title = coalesce(excluded.title, recent_prs.title),
                        account = excluded.account,
                        opened_at = excluded.opened_at
                ))?(row)?;
                connection.exec_bound(sql!(
                    DELETE FROM recent_prs WHERE rowid NOT IN (
                        SELECT rowid FROM recent_prs ORDER BY opened_at DESC, rowid DESC LIMIT ?1
                    )
                ))?(RECENT_PRS as i64)
            })
            .await
    }

    /// The most recently opened pull requests, newest first.
    pub fn recent(&self, limit: usize) -> Result<Vec<RecentPr>> {
        let rows = self
            .db
            .connection()
            .select_bound::<(&str, i64), (String, String, i64, Option<String>, String, i64)>(
                sql!(
                    SELECT owner, repo, number, title, account, opened_at FROM recent_prs
                    WHERE host = ?1 ORDER BY opened_at DESC, rowid DESC LIMIT ?2
                ),
            )?((HOST, i64::try_from(limit).unwrap_or(i64::MAX)))?;
        Ok(rows
            .into_iter()
            .map(
                |(owner, repo, number, title, account, opened_at)| RecentPr {
                    pull: PullRef::new(owner, repo, number),
                    title,
                    account,
                    opened_at: from_secs(opened_at),
                },
            )
            .collect())
    }

    /// Forgets that `pull` was opened under that name, once GitHub named its repository
    /// otherwise: after a rename or transfer, the pull request is listed once, by its name now.
    pub async fn forget_recent(&self, pull: &PullRef) -> Result<()> {
        let key = (
            HOST.to_string(),
            pull.repo.owner.clone(),
            pull.repo.name.clone(),
            pull.number,
        );
        self.db
            .connection()
            .write(move |connection| {
                connection.exec_bound(sql!(
                    DELETE FROM recent_prs WHERE host = ?1 AND owner = ?2 AND repo = ?3 AND number = ?4
                ))?(key)
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use futures::executor::block_on;
    use schemars::JsonSchema;
    use serde::Deserialize;

    use crate::db::{Account, AccountSource};
    use crate::poll::reads::Stamped;
    use crate::pulls::{PullBranch, RepoRef};
    use crate::push_history::PushHistory;
    use crate::queries::{StackLayerList, TrunkRuleList};

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

    async fn cache(name: &str) -> (PrCache, LoDb) {
        let db = LoDb::open_test(name).await;
        db.save_account(account("bot")).await.unwrap();
        db.save_account(account("other")).await.unwrap();
        (PrCache::new(db.clone()), db)
    }

    fn summary(title: &str) -> PullSummary {
        serde_json::from_value(serde_json::json!({
            "number": 1,
            "title": title,
            "state": "open",
            "draft": false,
            "merged_at": null,
            "updated_at": "2026-11-06T08:15:07Z",
            "head": {"ref": "stack-a", "sha": "b046244d"},
            "base": {"ref": "main", "sha": "6ce218eb"},
            "user": {"login": "ignored"}
        }))
        .unwrap()
    }

    fn pull(number: i64) -> Target {
        PullRef::new("rozed-dev", "playground", number).into()
    }

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn answers_round_trip() {
        block_on(async {
            let (cache, _) = cache("cache_round_trip").await;
            assert_eq!(cache.get::<PullSummary>("bot", &pull(1)).unwrap(), None);
            cache
                .put("bot", &pull(1), &summary("a"), Some("\"etag\""), at(1000))
                .await
                .unwrap();
            assert_eq!(
                cache.get::<PullSummary>("BOT", &pull(1)).unwrap(),
                Some(Cached {
                    value: summary("a"),
                    etag: Some("\"etag\"".into()),
                    fetched_at: at(1000),
                })
            );
            // Replacing it.
            cache
                .put("bot", &pull(1), &summary("b"), None, at(2000))
                .await
                .unwrap();
            let cached = cache.get::<PullSummary>("bot", &pull(1)).unwrap().unwrap();
            assert_eq!((cached.value.title.as_str(), cached.etag), ("b", None));
        });
    }

    #[test]
    fn graphql_answers_round_trip() {
        let http = crate::replay::ReplayHttpClient::cassette("graphql_pull_request_1");
        let client = crate::GithubClient::new(http, crate::Token::new("gho_FAKE"));
        block_on(async {
            let details = client
                .pull_request("rozed-dev", "playground", 1)
                .await
                .unwrap();
            let (cache, _) = cache("cache_graphql_round_trip").await;
            cache
                .put("bot", &pull(1), &details, None, at(1000))
                .await
                .unwrap();
            let cached = cache.get::<PullRequestData>("bot", &pull(1)).unwrap();
            assert_eq!(cached.unwrap().value, details);
        });
    }

    #[test]
    fn answers_are_per_account_target_and_query() {
        block_on(async {
            let (cache, _) = cache("cache_keys").await;
            cache
                .put("bot", &pull(1), &summary("bot's"), None, at(1000))
                .await
                .unwrap();
            let repo: Target = RepoRef::new("rozed-dev", "playground").into();
            assert_eq!(cache.get::<PullSummary>("other", &pull(1)).unwrap(), None);
            assert_eq!(cache.get::<PullSummary>("bot", &pull(2)).unwrap(), None);
            assert_eq!(cache.get::<PullSummary>("bot", &repo).unwrap(), None);
            assert_eq!(cache.get::<PullRequestData>("bot", &pull(1)).unwrap(), None);
            // Owners and names match case-insensitively, as on GitHub.
            let shouted: Target = PullRef::new("ROZED-DEV", "Playground", 1).into();
            assert!(cache.get::<PullSummary>("bot", &shouted).unwrap().is_some());
        });
    }

    #[test]
    fn an_answer_knows_its_age_and_a_touch_renews_it() {
        block_on(async {
            let (cache, _) = cache("cache_age").await;
            let three_hours_ago = at(1_800_000_000 - 3 * 3600);
            cache
                .put(
                    "bot",
                    &pull(1),
                    &summary("a"),
                    Some("\"e\""),
                    three_hours_ago,
                )
                .await
                .unwrap();
            let cached = cache.get::<PullSummary>("bot", &pull(1)).unwrap().unwrap();
            assert_eq!(
                cached.age_at(at(1_800_000_000)),
                Duration::from_secs(3 * 3600)
            );
            // A clock that went backwards gives no age rather than a panic.
            assert_eq!(cached.age_at(at(0)), Duration::ZERO);

            cache
                .touch::<PullSummary>("bot", &pull(1), at(1_800_000_000))
                .await
                .unwrap();
            let touched = cache.get::<PullSummary>("bot", &pull(1)).unwrap().unwrap();
            assert_eq!(touched.age_at(at(1_800_000_000)), Duration::ZERO);
            assert_eq!(
                (touched.value, touched.etag),
                (summary("a"), Some("\"e\"".into()))
            );
            // Touching what isn't there does nothing.
            cache
                .touch::<PullSummary>("bot", &pull(9), at(1))
                .await
                .unwrap();
            assert_eq!(cache.get::<PullSummary>("bot", &pull(9)).unwrap(), None);
        });
    }

    /// The same rows as [`PullSummary`], in a newer shape.
    #[derive(Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
    struct NewerSummary {
        title: String,
        mergeable: bool,
    }

    impl CacheEntry for NewerSummary {
        const NAME: &'static str = "pull";

        fn shape() -> String {
            schema_shape::<Self>()
        }
    }

    /// A shape whose version matches but whose JSON doesn't fit.
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Mismatched {
        count: u32,
    }

    impl CacheEntry for Mismatched {
        const NAME: &'static str = "pull";

        fn shape() -> String {
            PullSummary::shape()
        }
    }

    /// A summary as it might be written one day, and the same with a field added.
    #[derive(Serialize, Deserialize, JsonSchema)]
    struct Before {
        /// The title.
        title: String,
        head: PullBranch,
    }

    #[derive(Serialize, Deserialize, JsonSchema)]
    struct After {
        /// The title, documented differently.
        title: String,
        head: PullBranch,
        mergeable_state: Option<String>,
    }

    #[test]
    fn a_shape_is_its_fields_and_their_types() {
        assert_ne!(schema_shape::<Before>(), schema_shape::<After>());
        // Documentation isn't shape, and neither is the order of keys.
        #[derive(Serialize, Deserialize, JsonSchema)]
        #[schemars(rename = "Before")]
        struct Undocumented {
            title: String,
            head: PullBranch,
        }
        assert_eq!(schema_shape::<Before>(), schema_shape::<Undocumented>());
        // A field renamed in the JSON is a different shape.
        #[derive(Serialize, Deserialize, JsonSchema)]
        struct Renamed {
            #[serde(rename = "name")]
            title: String,
            head: PullBranch,
        }
        assert_ne!(schema_shape::<Before>(), schema_shape::<Renamed>());
    }

    /// Every version, as of the last change to what the cache stores. When this fails, a stored
    /// shape changed: rows written in the old one will read as missing and be fetched again.
    /// Check that's what the change means, then update the versions here. A read the poller keeps
    /// ([`crate::poll::CachedRead`]) is stored stamped. No two share a name, which would make
    /// one read the other's rows as missing.
    #[test]
    fn versions_follow_the_stored_shapes() {
        let names = [
            PullSummary::NAME,
            ListedPulls::NAME,
            PullRequestData::NAME,
            PullRequestCheckData::NAME,
            OpenPullRequestList::NAME,
            Stamped::<PushHistory>::NAME,
            Stamped::<StackLayerList>::NAME,
            TrunkRuleList::NAME,
        ];
        let unique: std::collections::BTreeSet<&str> = names.iter().copied().collect();
        assert_eq!(unique.len(), names.len(), "{names:?}");
        assert_eq!(
            [
                (PullSummary::NAME, PullSummary::version()),
                (ListedPulls::NAME, ListedPulls::version()),
                (PullRequestData::NAME, PullRequestData::version()),
                (PullRequestCheckData::NAME, PullRequestCheckData::version()),
                (OpenPullRequestList::NAME, OpenPullRequestList::version()),
                (
                    Stamped::<PushHistory>::NAME,
                    Stamped::<PushHistory>::version()
                ),
                (
                    Stamped::<StackLayerList>::NAME,
                    Stamped::<StackLayerList>::version()
                ),
                (TrunkRuleList::NAME, TrunkRuleList::version()),
            ],
            [
                ("pull", "b2f1fdde71669374".to_string()),
                ("pulls", "7c1cc8e8b3c05b05".to_string()),
                ("PullRequest", "2f6a282f3dfcbf15".to_string()),
                ("PullRequestCheck", "098cc23f0fe0c48d".to_string()),
                ("OpenPullRequests", "64bdb1e8ba14f7e0".to_string()),
                ("PushHistory", "1f9c5b1925f77e8b".to_string()),
                ("StackLayers", "a79d34dd50325fec".to_string()),
                ("TrunkRules", "22f61a7701923aef".to_string()),
            ]
        );
    }

    #[test]
    fn an_answer_in_another_shape_reads_as_missing() {
        block_on(async {
            let (cache, _) = cache("cache_versions").await;
            cache
                .put("bot", &pull(1), &summary("a"), None, at(1000))
                .await
                .unwrap();
            assert_eq!(cache.get::<NewerSummary>("bot", &pull(1)).unwrap(), None);
            assert_eq!(cache.get::<Mismatched>("bot", &pull(1)).unwrap(), None);
            // The newer shape replaces the row.
            let newer = NewerSummary {
                title: "a".into(),
                mergeable: true,
            };
            cache
                .put("bot", &pull(1), &newer, None, at(2000))
                .await
                .unwrap();
            assert_eq!(cache.get::<PullSummary>("bot", &pull(1)).unwrap(), None);
            assert_eq!(
                cache
                    .get::<NewerSummary>("bot", &pull(1))
                    .unwrap()
                    .unwrap()
                    .value,
                newer
            );
        });
    }

    /// A row the database refuses, once [`refuse_rows`] has run.
    #[derive(Serialize, Deserialize, JsonSchema)]
    struct Refused(u32);

    impl CacheEntry for Refused {
        const NAME: &'static str = "refused";

        fn shape() -> String {
            schema_shape::<Self>()
        }
    }

    async fn refuse_rows(db: &LoDb) {
        db.connection()
            .write(|connection| {
                connection.exec(
                    "CREATE TRIGGER refuse BEFORE INSERT ON pr_cache WHEN NEW.query = 'refused' \
                     BEGIN SELECT RAISE(ABORT, 'refused'); END",
                )?()
            })
            .await
            .unwrap();
    }

    #[test]
    fn answers_stored_together_are_all_stored_or_none() {
        block_on(async {
            let (cache, db) = cache("cache_together").await;
            cache
                .put("bot", &pull(1), &summary("old"), None, at(1000))
                .await
                .unwrap();
            refuse_rows(&db).await;
            let rows = vec![
                PrCache::row(&summary("new"), None).unwrap(),
                PrCache::row(&Refused(1), None).unwrap(),
            ];
            cache
                .put_together("bot", &pull(1), rows, at(2000))
                .await
                .unwrap_err();
            let kept = cache.get::<PullSummary>("bot", &pull(1)).unwrap().unwrap();
            assert_eq!(
                (kept.value.title.as_str(), kept.fetched_at),
                ("old", at(1000))
            );

            let rows = vec![PrCache::row(&summary("new"), Some("\"e\"")).unwrap()];
            cache
                .put_together("bot", &pull(1), rows, at(2000))
                .await
                .unwrap();
            let stored = cache.get::<PullSummary>("bot", &pull(1)).unwrap().unwrap();
            assert_eq!(
                (stored.value.title.as_str(), stored.etag.as_deref()),
                ("new", Some("\"e\""))
            );
        });
    }

    #[test]
    fn removing_forgets_one_answer() {
        block_on(async {
            let (cache, _) = cache("cache_remove").await;
            for number in [1, 2] {
                cache
                    .put("bot", &pull(number), &summary("a"), None, at(1000))
                    .await
                    .unwrap();
            }
            cache.remove::<PullSummary>("bot", &pull(1)).await.unwrap();
            assert_eq!(cache.get::<PullSummary>("bot", &pull(1)).unwrap(), None);
            assert!(cache.get::<PullSummary>("bot", &pull(2)).unwrap().is_some());
        });
    }

    #[test]
    fn clearing_the_cache_leaves_accounts_and_recent_prs_alone() {
        block_on(async {
            let (cache, db) = cache("cache_clear").await;
            db.set_owner_account(HOST.into(), "rozed-dev".into(), "bot".into())
                .await
                .unwrap();
            cache
                .put("bot", &pull(1), &summary("a"), None, at(1000))
                .await
                .unwrap();
            let recent = RecentPr {
                pull: PullRef::new("rozed-dev", "playground", 1),
                title: Some("a".into()),
                account: "bot".into(),
                opened_at: at(1000),
            };
            cache.record_recent(recent.clone()).await.unwrap();

            cache.clear().await.unwrap();
            assert_eq!(cache.get::<PullSummary>("bot", &pull(1)).unwrap(), None);
            assert_eq!(db.accounts().unwrap().len(), 2);
            assert_eq!(
                db.owner_account(HOST, "rozed-dev").unwrap().as_deref(),
                Some("bot")
            );
            assert_eq!(cache.recent(10).unwrap(), [recent]);
        });
    }

    #[test]
    fn removing_an_account_removes_its_answers() {
        block_on(async {
            let (cache, db) = cache("cache_account_removed").await;
            for login in ["bot", "other"] {
                cache
                    .put(login, &pull(1), &summary(login), None, at(1000))
                    .await
                    .unwrap();
            }
            db.delete_account(HOST.into(), "bot".into()).await.unwrap();
            assert_eq!(cache.get::<PullSummary>("bot", &pull(1)).unwrap(), None);
            assert!(
                cache
                    .get::<PullSummary>("other", &pull(1))
                    .unwrap()
                    .is_some()
            );
        });
    }

    #[test]
    fn recent_prs_keep_the_newest_and_go_with_their_account() {
        block_on(async {
            let (cache, db) = cache("cache_recent_bounded").await;
            for number in 1..=(RECENT_PRS as i64 + 5) {
                let account = if number % 2 == 0 { "bot" } else { "other" };
                cache
                    .record_recent(RecentPr {
                        pull: PullRef::new("rozed-dev", "playground", number),
                        title: None,
                        account: account.into(),
                        opened_at: at(number as u64),
                    })
                    .await
                    .unwrap();
            }
            let recent = cache.recent(1000).unwrap();
            assert_eq!(recent.len(), RECENT_PRS);
            assert_eq!(recent[0].pull.number, RECENT_PRS as i64 + 5);
            assert_eq!(recent.last().unwrap().pull.number, 6);

            db.delete_account(HOST.into(), "OTHER".into())
                .await
                .unwrap();
            assert!(
                cache
                    .recent(1000)
                    .unwrap()
                    .iter()
                    .all(|recent| recent.account == "bot")
            );
        });
    }

    #[test]
    fn recent_prs_are_newest_first() {
        block_on(async {
            let (cache, _) = cache("cache_recent").await;
            let recent = |number, title: Option<&str>, opened_at| RecentPr {
                pull: PullRef::new("rozed-dev", "playground", number),
                title: title.map(str::to_string),
                account: "bot".into(),
                opened_at: at(opened_at),
            };
            cache
                .record_recent(recent(1, Some("one"), 100))
                .await
                .unwrap();
            cache.record_recent(recent(2, None, 200)).await.unwrap();
            assert_eq!(
                cache.recent(10).unwrap(),
                [recent(2, None, 200), recent(1, Some("one"), 100)]
            );
            // Reopening moves it to the top and keeps the title it had.
            cache.record_recent(recent(1, None, 300)).await.unwrap();
            assert_eq!(cache.recent(1).unwrap(), [recent(1, Some("one"), 300)]);
        });
    }

    #[test]
    fn a_pull_request_opened_by_an_old_name_can_be_forgotten_under_it() {
        block_on(async {
            let (cache, _) = cache("cache_recent_forget").await;
            let recent = |repo: &str, number, opened_at| RecentPr {
                pull: PullRef::new("rozed-dev", repo, number),
                title: None,
                account: "bot".into(),
                opened_at: at(opened_at),
            };
            cache
                .record_recent(recent("playground", 2, 100))
                .await
                .unwrap();
            cache
                .record_recent(recent("renamed", 2, 200))
                .await
                .unwrap();
            cache
                .record_recent(recent("playground", 3, 300))
                .await
                .unwrap();

            cache
                .forget_recent(&PullRef::new("Rozed-Dev", "Playground", 2))
                .await
                .unwrap();

            assert_eq!(
                cache.recent(10).unwrap(),
                [recent("playground", 3, 300), recent("renamed", 2, 200)]
            );
        });
    }
}
