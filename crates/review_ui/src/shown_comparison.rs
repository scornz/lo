//! What the local copy's diff compares (design §6): two sides of a pull request in its clone, and
//! the files that changed between them.
//!
//! - The whole pull request compares where its head branched from its base with its head.
//! - One revision with another compares the old revision's head, or its changes replayed onto
//!   the new base, with the new head ([`Repo::compare_revisions`]).
//! - A stack's layer compares the lower layer's head that it contains with its own.
//!
//! Each file's two sides are the blobs git listed for it, in the old side's tree and the new
//! side's.

use repo_cache::{ChangedFile, Oid, Replay, Repo, Revision};

use crate::file_list::FileEntry;
use crate::pr_diff::LocalPullRequest;

/// Two sides of a pull request in its local copy, and the files that changed between them.
#[cfg_attr(test, derive(Clone))]
pub struct ShownComparison {
    /// The clone both sides are in. Holding it keeps the clone open, and a replayed tree pinned
    /// in it, while the diff shows.
    pub repo: Repo,
    /// The repository's canonical name, GitHub's, which starts each file's full path.
    pub repo_name: String,
    /// What the left side's files come from: a commit, or a tree replayed onto a new base.
    pub old: Oid,
    /// What the right side's files come from: one of the pull request's heads.
    pub new: Oid,
    /// What changed from `old` to `new`, in git's order.
    pub files: Vec<ChangedFile>,
    /// Each file's `linguist-generated` attribute, in the same order: in `new`'s `.gitattributes`,
    /// or `old`'s for a file `new` doesn't have (see `review::files::ChangedFile::linguist_generated`).
    pub generated: Vec<Option<bool>>,
    /// Why the `.gitattributes` files couldn't be read, when they couldn't.
    pub attributes_unread: Option<String>,
    pub kind: ComparisonKind,
}

/// What a [`ShownComparison`] compares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComparisonKind {
    /// The whole pull request: the old side is where its head branched from its base.
    Full,
    /// What changed from one revision to another. The old side is the old revision's head, or
    /// its changes replayed onto the new revision's base, as `replay` says.
    Between {
        /// The old revision's head.
        from: Oid,
        /// The new revision's head, the new side.
        to: Oid,
        replay: Replay,
        /// Every path the replay conflicted on, each of which resolved a conflict, whether or not
        /// it changed from one side to the other. Empty unless the replay conflicted.
        conflicted: Vec<Vec<u8>>,
    },
    /// A stack's layer, compared with the layer below it: the old side is the lower layer's head
    /// that this layer contains.
    Layer,
}

impl ShownComparison {
    /// The whole of `pull`, from where its head branched from its base. Lists the files and reads
    /// their attributes, which can download them. Blocks while git runs.
    pub fn full(pull: &LocalPullRequest) -> anyhow::Result<Self> {
        Self::heads(pull, pull.base.clone(), ComparisonKind::Full)
    }

    /// What changed in `pull` from revision `old` to revision `new`, so only what its author
    /// changed shows, even across a rebase ([`Repo::compare_revisions`]). Every commit must be in
    /// the clone already. Blocks while git runs.
    pub fn between(
        pull: &LocalPullRequest,
        old: &Revision,
        new: &Revision,
    ) -> anyhow::Result<Self> {
        let comparison = pull.repo.compare_revisions(old, new)?;
        let conflicted = (comparison.files.iter())
            .filter(|file| file.conflicted())
            .map(|file| file.path.clone())
            .collect();
        // A file the replay conflicted on that's the same on both sides has nothing to compare.
        let files: Vec<ChangedFile> = (comparison.files.into_iter())
            .filter_map(|file| file.change)
            .collect();
        let (generated, attributes_unread) =
            linguist_generated(&pull.repo, &comparison.old, &comparison.new, &files, pull);
        Ok(Self {
            repo: pull.repo.clone(),
            repo_name: pull.name.clone(),
            old: comparison.old,
            new: comparison.new,
            files,
            generated,
            attributes_unread,
            kind: ComparisonKind::Between {
                from: old.head.clone(),
                to: new.head.clone(),
                replay: comparison.replay,
                conflicted,
            },
        })
    }

    /// `pull` as a layer of a stack: from `below`, the lower layer's head that `pull`'s head
    /// contains, to its head. Blocks while git runs.
    pub fn layer(pull: &LocalPullRequest, below: &Oid) -> anyhow::Result<Self> {
        Self::heads(pull, below.clone(), ComparisonKind::Layer)
    }

    /// `pull`'s head compared with `old`.
    fn heads(pull: &LocalPullRequest, old: Oid, kind: ComparisonKind) -> anyhow::Result<Self> {
        let files = pull.repo.changed_files(&old, &pull.head)?;
        let (generated, attributes_unread) =
            linguist_generated(&pull.repo, &old, &pull.head, &files, pull);
        Ok(Self {
            repo: pull.repo.clone(),
            repo_name: pull.name.clone(),
            old,
            new: pull.head.clone(),
            files,
            generated,
            attributes_unread,
            kind,
        })
    }

    /// The file list's entry for each file, in git's order.
    pub fn entries(&self) -> Vec<FileEntry> {
        self.files
            .iter()
            .zip(&self.generated)
            .map(|(file, generated)| FileEntry::from_local(file, *generated))
            .collect()
    }
}

/// Each of `files`' `linguist-generated` attribute in `new`'s `.gitattributes`, or `old`'s for
/// a file `new` doesn't have, which git reads from the commit or tree. When they can't be read, no
/// file counts as generated by attribute, though lockfiles still do by name, and the second value
/// says why.
fn linguist_generated(
    repo: &Repo,
    old: &Oid,
    new: &Oid,
    files: &[ChangedFile],
    link: &dyn std::fmt::Display,
) -> (Vec<Option<bool>>, Option<String>) {
    let mut generated = vec![None; files.len()];
    let mut unread = None;
    for (rev, deleted) in [(new, false), (old, true)] {
        let (indices, paths): (Vec<usize>, Vec<&[u8]>) = files
            .iter()
            .enumerate()
            .filter(|(_, file)| file.new.is_none() == deleted)
            .map(|(index, file)| (index, file.path()))
            .unzip();
        if paths.is_empty() {
            continue;
        }
        match repo.attribute(rev, "linguist-generated", &paths) {
            Ok(attributes) => {
                for (index, attribute) in indices.into_iter().zip(attributes) {
                    generated[index] = attribute.as_bool();
                }
            }
            Err(error) => {
                log::warn!(
                    "{link}: can't read which files .gitattributes marks generated: {error}"
                );
                unread.get_or_insert_with(|| error.to_string());
            }
        }
    }
    (generated, unread)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;
    use std::time::{Instant, SystemTime};

    use editor::SplittableEditor;
    use gpui::{Entity, TestAppContext, VisualTestContext};
    use repo_cache::{Git, PullRequestRefs, RepoCache};
    use tempfile::TempDir;

    use crate::pr_diff::tests::fixtures::{self, Entry, Fixture};
    use crate::pr_diff::{self, Trust};
    use crate::pr_tab::PrTab;
    use crate::shown_files;

    /// Pull request #1 of `fixture`, which merges into `base_branch`, fetched into a clone of its
    /// own, with `heads` fetched too, as older revisions are.
    fn copy(fixture: &Fixture, base_branch: &str, heads: &[&str]) -> (LocalPullRequest, TempDir) {
        let dir = TempDir::new().unwrap();
        let cache = RepoCache::new(dir.path(), Git::find().unwrap())
            .unwrap()
            .with_remote_base(fixture.base_url());
        let repo = cache.open("owner", "repo", None).unwrap();
        let fetched = repo
            .fetch_pull(&PullRequestRefs {
                number: 1,
                head: None,
                base_branch: base_branch.into(),
                base: None,
            })
            .unwrap();
        let heads: Vec<Oid> = heads.iter().map(|head| head.parse().unwrap()).collect();
        assert_eq!(repo.fetch_commits(1, &heads).unwrap(), []);
        let base = repo
            .merge_base(&fetched.head, &fetched.base_tip)
            .unwrap()
            .unwrap();
        let local = LocalPullRequest {
            repo,
            owner: "owner".into(),
            name: "repo".into(),
            number: 1,
            base,
            head: fetched.head,
            head_source: fetched.head_source,
        };
        (local, dir)
    }

    /// Shows `comparison` of `local` in `tab`, as opening a pull request does, and returns the
    /// diff.
    fn show(
        tab: &Entity<PrTab>,
        local: LocalPullRequest,
        comparison: ShownComparison,
        cx: &mut VisualTestContext,
    ) -> Entity<SplittableEditor> {
        let (local, comparison) = (Arc::new(local), Arc::new(comparison));
        tab.update_in(cx, |tab, window, cx| {
            let (workspace, link) = (tab.workspace().clone(), tab.link().clone());
            cx.spawn_in(window, async move |tab, cx| {
                let title = format!("{link}: test");
                pr_diff::show_local_copy(
                    tab,
                    workspace,
                    link,
                    title,
                    local,
                    comparison,
                    SystemTime::now(),
                    Trust::Untrusted,
                    Instant::now(),
                    cx,
                )
                .await
                .unwrap()
            })
            .detach();
        });
        cx.run_until_parked();
        tab.read_with(cx, |tab, _| tab.diff().unwrap().clone())
    }

    /// Each file the diff shows, by path, with the text of its two sides: `None` for a side it
    /// doesn't have.
    fn sides(
        diff: &Entity<SplittableEditor>,
        cx: &mut VisualTestContext,
    ) -> Vec<(String, Option<String>, String)> {
        cx.update(|_, cx| {
            shown_files::read(diff, cx, |files| {
                let mut sides: Vec<_> = (files.paths().into_iter())
                    .map(|path| {
                        let file = files.get(&path).unwrap();
                        let old = file.base.clone().unwrap().map(|old| old.to_string());
                        (path, old, file.buffer.read(cx).text())
                    })
                    .collect();
                sides.sort();
                sides
            })
            .unwrap()
        })
    }

    /// What the diff's left side and its right side show, each as one text.
    fn editors(diff: &Entity<SplittableEditor>, cx: &mut VisualTestContext) -> (String, String) {
        diff.read_with(cx, |diff, cx| {
            let lhs = diff.lhs_editor().expect("the diff shows side by side");
            (lhs.read(cx).text(cx), diff.rhs_editor().read(cx).text(cx))
        })
    }

    /// Two revisions on either side of a rebase compare what the author changed between them,
    /// with the old side read from the old revision replayed onto the new base: what came with
    /// the base is on both sides, so it doesn't show as a change.
    #[gpui::test]
    async fn between_revisions_across_a_rebase_shows_only_the_authors_change(
        cx: &mut TestAppContext,
    ) {
        // Line 1 changes with the base, line 5 in r1, and line 8 in r2 too.
        let a = |changed: &[usize]| -> Vec<u8> {
            (1..=8)
                .map(|line| match changed.contains(&line) {
                    true if line == 1 => "1 base\n".to_string(),
                    true => format!("{line} changed\n"),
                    false => format!("{line}\n"),
                })
                .collect::<String>()
                .into_bytes()
        };
        let fixture = fixtures::empty();
        let old_base = fixture.commit(
            &[
                Entry::File("a.txt", &a(&[])),
                Entry::File("base.txt", b"base 1\n"),
            ],
            None,
        );
        let r1 = fixture.commit(
            &[
                Entry::File("a.txt", &a(&[5])),
                Entry::File("base.txt", b"base 1\n"),
            ],
            Some(&old_base),
        );
        let new_base = fixture.commit(
            &[
                Entry::File("a.txt", &a(&[1])),
                Entry::File("base.txt", b"base 2\n"),
            ],
            Some(&old_base),
        );
        // r1 rebased onto the new base, and changed again.
        let r2 = fixture.commit(
            &[
                Entry::File("a.txt", &a(&[1, 5, 8])),
                Entry::File("base.txt", b"base 2\n"),
            ],
            Some(&new_base),
        );
        fixture.set_ref("refs/heads/main", &new_base);
        fixture.set_ref("refs/pull/1/head", &r2);
        let (local, _clone) = copy(&fixture, "main", &[&r1]);
        let revision = |base: &str, head: &str| Revision {
            base: base.parse().unwrap(),
            head: head.parse().unwrap(),
        };

        let comparison =
            ShownComparison::between(&local, &revision(&old_base, &r1), &revision(&new_base, &r2))
                .unwrap();
        assert_eq!(
            comparison.kind,
            ComparisonKind::Between {
                from: r1.parse().unwrap(),
                to: r2.parse().unwrap(),
                replay: Replay::Clean,
                conflicted: Vec::new(),
            }
        );
        // The old side is the replay's tree, not r1's.
        assert_ne!(comparison.old.as_str(), r1);
        assert_eq!(comparison.new.as_str(), r2);

        let (tab, cx) = shown_files::tests::tab(cx).await;
        let diff = show(&tab, local, comparison, cx);
        let text = |changed| String::from_utf8(a(changed)).unwrap();
        assert_eq!(
            sides(&diff, cx),
            [("a.txt".to_string(), Some(text(&[1, 5])), text(&[1, 5, 8]))]
        );
        // Each side shows the lines around the change: line 8 as it was, and changed.
        let (left, right) = editors(&diff, cx);
        assert!(
            left.ends_with("7\n8\n") && right.ends_with("7\n8 changed\n"),
            "{left}|{right}"
        );
        assert_eq!(
            tab.read_with(cx, |tab, cx| tab.comparison(cx).unwrap().new.clone()),
            r2.parse::<Oid>().unwrap()
        );
    }

    /// A stack's layer compares its head with the lower layer's, so neither side shows what the
    /// lower layer changed.
    #[gpui::test]
    async fn a_layer_shows_only_its_own_change_over_the_layer_below(cx: &mut TestAppContext) {
        let fixture = fixtures::empty();
        let base = fixture.commit(
            &[Entry::File("a.txt", b"a\n"), Entry::File("b.txt", b"b\n")],
            None,
        );
        let lower = fixture.commit(
            &[
                Entry::File("a.txt", b"a lower\n"),
                Entry::File("b.txt", b"b\n"),
                Entry::File("lower.txt", b"lower\n"),
            ],
            Some(&base),
        );
        let upper = fixture.commit(
            &[
                Entry::File("a.txt", b"a lower\na upper\n"),
                Entry::File("b.txt", b"b upper\n"),
                Entry::File("lower.txt", b"lower\n"),
            ],
            Some(&lower),
        );
        fixture.set_ref("refs/heads/main", &base);
        fixture.set_ref("refs/pull/1/head", &upper);
        let (local, _clone) = copy(&fixture, "main", &[]);
        // The whole pull request, against main, has the lower layer's change too.
        let full: Vec<_> = ShownComparison::full(&local)
            .unwrap()
            .files
            .iter()
            .map(|file| file.display_path().into_owned())
            .collect();
        assert_eq!(full, ["a.txt", "b.txt", "lower.txt"]);

        let comparison = ShownComparison::layer(&local, &lower.parse().unwrap()).unwrap();
        assert_eq!(comparison.kind, ComparisonKind::Layer);
        assert_eq!(comparison.old.as_str(), lower);
        assert_eq!(comparison.new.as_str(), upper);

        let (tab, cx) = shown_files::tests::tab(cx).await;
        let diff = show(&tab, local, comparison, cx);
        assert_eq!(
            sides(&diff, cx),
            [
                (
                    "a.txt".to_string(),
                    Some("a lower\n".to_string()),
                    "a lower\na upper\n".to_string()
                ),
                (
                    "b.txt".to_string(),
                    Some("b\n".to_string()),
                    "b upper\n".to_string()
                ),
            ]
        );
        let (left, right) = editors(&diff, cx);
        assert!(
            left.contains("a lower") && right.contains("a upper"),
            "{left}\n{right}"
        );
        assert_eq!(
            tab.read_with(cx, |tab, _| tab.files().len()),
            2,
            "the file list lists the layer's files"
        );
    }
}
