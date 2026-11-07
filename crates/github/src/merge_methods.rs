//! The merge method you last used in each repository (design §9, "Merging"): the merge panel
//! picks it first while the repository allows it
//! ([`review::merge::MergeSnapshot::preferred_method`]).
//!
//! It's a habit of the person at this Mac, so it's kept per repository, whichever account
//! merged. Like viewed records ([`crate::viewed::ViewedRecords`]), it isn't a cached answer:
//! clearing the cache keeps it.

use anyhow::Result;
use review::merge::MergeMethod;
use sqlez_macros::sql;

use crate::credentials::HOST;
use crate::db::LoDb;
use crate::pulls::RepoRef;

/// The method last used in each repository, in Lo's database.
#[derive(Clone)]
pub struct MergeMethods {
    db: LoDb,
}

impl MergeMethods {
    pub fn new(db: LoDb) -> Self {
        Self { db }
    }

    /// The method last used in `repo`, if any. One stored by a later Lo, which this one doesn't
    /// know, reads as none.
    pub fn preferred(&self, repo: &RepoRef) -> Result<Option<MergeMethod>> {
        let method = self
            .db
            .connection()
            .select_row_bound::<(&str, &str, &str), String>(sql!(
                SELECT method FROM merge_methods WHERE host = ?1 AND owner = ?2 AND repo = ?3
            ))?((HOST, &repo.owner, &repo.name))?;
        Ok(method.and_then(|method| method.parse().ok()))
    }

    /// Notes that `method` merged a pull request in `repo`.
    pub async fn remember(&self, repo: &RepoRef, method: MergeMethod) -> Result<()> {
        let row = (
            HOST.to_string(),
            repo.owner.clone(),
            repo.name.clone(),
            method.as_str().to_string(),
        );
        self.db
            .connection()
            .write(move |connection| {
                connection.exec_bound::<(String, String, String, String)>(sql!(
                    INSERT INTO merge_methods(host, owner, repo, method) VALUES (?1, ?2, ?3, ?4)
                    ON CONFLICT(host, owner, repo) DO UPDATE SET method = excluded.method
                ))?(row)
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_method_used_is_remembered_per_repository() {
        futures::executor::block_on(async {
            let methods = MergeMethods::new(LoDb::open_test("merge_methods").await);
            let shop = RepoRef::new("acme", "shop");
            let docs = RepoRef::new("acme", "docs");
            assert_eq!(methods.preferred(&shop).unwrap(), None);

            methods.remember(&shop, MergeMethod::Squash).await.unwrap();
            methods.remember(&docs, MergeMethod::Merge).await.unwrap();
            methods.remember(&shop, MergeMethod::Rebase).await.unwrap();

            // Owners and names are GitHub's, whose case doesn't matter.
            let shouted = RepoRef::new("ACME", "Shop");
            assert_eq!(
                methods.preferred(&shouted).unwrap(),
                Some(MergeMethod::Rebase)
            );
            assert_eq!(methods.preferred(&docs).unwrap(), Some(MergeMethod::Merge));
        });
    }

    #[test]
    fn a_method_lo_does_not_know_reads_as_none() {
        futures::executor::block_on(async {
            let db = LoDb::open_test("merge_methods_unknown").await;
            db.connection()
                .write(|connection| {
                    connection.exec(sql!(
                        INSERT INTO merge_methods(host, owner, repo, method)
                        VALUES ("github.com", "acme", "shop", "FAST_FORWARD")
                    ))?()
                })
                .await
                .unwrap();
            let methods = MergeMethods::new(db);
            assert_eq!(
                methods.preferred(&RepoRef::new("acme", "shop")).unwrap(),
                None
            );
        });
    }
}
