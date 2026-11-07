//! What the local copy's diff compares (design §6): two sides of a pull request in its clone, and
//! the files that changed between them.
//!
//! - The whole pull request compares where its head branched from its base with its head.
//! - One revision with another compares the old revision's head, or its changes replayed onto
//!   the new base, with the new head ([`Repo::compare_revisions`]). When the replay conflicts,
//!   every file it conflicted on is listed, with how the author resolved it, even one the same on
//!   both sides, as when the author dropped the base's change (decisions, "Revisions").
//! - A stack's layer compares the lower layer's head that it contains with its own.
//!
//! Each file's two sides are the blobs git listed for it, in the old side's tree and the new
//! side's. The constructors block while git runs; [`ShownComparison::spawn`] runs one on the
//! background executor.

use std::sync::Arc;

use gpui::{BackgroundExecutor, Task};
use repo_cache::{Change, ChangedFile, Error, LineCounts, Oid, Replay, Repo, Revision};

use crate::file_list::FileEntry;
use crate::pr_diff::LocalPullRequest;

/// Two sides of a pull request in its local copy, and the files that changed between them.
#[cfg_attr(test, derive(Clone))]
pub struct ShownComparison {
    /// The clone both sides are in. Holding it keeps the clone open, and a replayed tree pinned
    /// in it, while the diff shows.
    repo: Repo,
    /// The repository's canonical name, GitHub's, which starts each file's full path.
    repo_name: String,
    /// What the left side's files come from: a commit, or a tree replayed onto a new base.
    old: Oid,
    /// What the right side's files come from: one of the pull request's heads.
    new: Oid,
    files: Vec<ComparisonFile>,
    /// Why the `.gitattributes` files couldn't be read, when they couldn't.
    attributes_unread: Option<String>,
    kind: ComparisonKind,
}

/// One file of a [`ShownComparison`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComparisonFile {
    /// How it changed from the old side to the new. For a file the replay conflicted on that's
    /// the same on both sides, its version on both, unchanged.
    pub change: ChangedFile,
    /// Its `linguist-generated` attribute: in the new side's `.gitattributes`, or the old side's
    /// for a file the new side doesn't have (see `review::files::ChangedFile::linguist_generated`).
    pub generated: Option<bool>,
    /// For a file the replay conflicted on, which resolved a conflict, how the author resolved it
    /// ([`repo_cache::ComparedFile::resolution`]): what changed from the conflicted replay to the
    /// new head. `None` for any other file.
    pub resolution: Option<Vec<ChangedFile>>,
}

impl ComparisonFile {
    /// Whether the replay conflicted on the file, so the author resolved a conflict in it.
    pub fn conflicted(&self) -> bool {
        self.resolution.is_some()
    }
}

/// What a [`ShownComparison`] compares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComparisonKind {
    /// The whole pull request: the old side is where its head branched from its base.
    Full,
    /// What changed from one revision to another, the new one's head being the new side. The old
    /// side is the old revision's head, or its changes replayed onto the new revision's base, as
    /// `replay` says.
    Between {
        /// The old revision's head.
        from: Oid,
        replay: Replay,
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
    /// the clone already: a missing one is [`Error::NotFetched`], never a download. Blocks while
    /// git runs.
    pub fn between(
        pull: &LocalPullRequest,
        old: &Revision,
        new: &Revision,
    ) -> anyhow::Result<Self> {
        let comparison = pull.repo.compare_revisions(old, new)?;
        let mut changes = Vec::with_capacity(comparison.files.len());
        let mut resolutions = Vec::with_capacity(comparison.files.len());
        for file in comparison.files {
            let change = match file.change {
                Some(change) => change,
                // The same on both sides: the author kept their version over the base's.
                None => match unchanged(&pull.repo, &comparison.new, &file.path)? {
                    Some(change) => change,
                    None => {
                        log::warn!(
                            "{pull}: the replay conflicted on {}, which neither revision has",
                            file.display_path()
                        );
                        continue;
                    }
                },
            };
            changes.push(change);
            resolutions.push(file.resolution);
        }
        let (generated, attributes_unread) =
            linguist_generated(&pull.repo, &comparison.old, &comparison.new, &changes, pull);
        let files = (changes.into_iter().zip(generated).zip(resolutions))
            .map(|((change, generated), resolution)| ComparisonFile {
                change,
                generated,
                resolution,
            })
            .collect();
        Ok(Self {
            repo: pull.repo.clone(),
            repo_name: pull.name.clone(),
            old: comparison.old,
            new: comparison.new,
            files,
            attributes_unread,
            kind: ComparisonKind::Between {
                from: old.head.clone(),
                replay: comparison.replay,
            },
        })
    }

    /// `pull` as a layer of a stack: from `below`, the lower layer's head that `pull`'s head
    /// contains, to its head. `below` must be in the clone already: a missing one is
    /// [`Error::NotFetched`], never a download. Blocks while git runs.
    pub fn layer(pull: &LocalPullRequest, below: &Oid) -> anyhow::Result<Self> {
        if !pull.repo.has_commit(below)? {
            return Err(Error::NotFetched { oid: below.clone() }.into());
        }
        Self::heads(pull, below.clone(), ComparisonKind::Layer)
    }

    /// Runs `compare`, one of the constructors, on the background executor.
    pub fn spawn(
        pull: Arc<LocalPullRequest>,
        executor: &BackgroundExecutor,
        compare: impl FnOnce(&LocalPullRequest) -> anyhow::Result<Self> + Send + 'static,
    ) -> Task<anyhow::Result<Self>> {
        executor.spawn(async move { compare(&pull) })
    }

    /// `pull`'s head compared with `old`.
    fn heads(pull: &LocalPullRequest, old: Oid, kind: ComparisonKind) -> anyhow::Result<Self> {
        let changes = pull.repo.changed_files(&old, &pull.head)?;
        let (generated, attributes_unread) =
            linguist_generated(&pull.repo, &old, &pull.head, &changes, pull);
        let files = (changes.into_iter().zip(generated))
            .map(|(change, generated)| ComparisonFile {
                change,
                generated,
                resolution: None,
            })
            .collect();
        Ok(Self {
            repo: pull.repo.clone(),
            repo_name: pull.name.clone(),
            old,
            new: pull.head.clone(),
            files,
            attributes_unread,
            kind,
        })
    }

    /// The clone both sides are in.
    pub fn repo(&self) -> &Repo {
        &self.repo
    }

    /// The repository's canonical name, which starts each file's full path.
    pub fn repo_name(&self) -> &str {
        &self.repo_name
    }

    /// What the left side's files come from: a commit, or a tree replayed onto a new base.
    pub fn old_side(&self) -> &Oid {
        &self.old
    }

    /// What the right side's files come from: one of the pull request's heads.
    pub fn new_side(&self) -> &Oid {
        &self.new
    }

    /// Every file it lists, in git's order: what changed from the old side to the new, and every
    /// file the replay conflicted on.
    pub fn files(&self) -> &[ComparisonFile] {
        &self.files
    }

    /// Why the `.gitattributes` files couldn't be read, when they couldn't.
    pub fn attributes_unread(&self) -> Option<&str> {
        self.attributes_unread.as_deref()
    }

    pub fn kind(&self) -> &ComparisonKind {
        &self.kind
    }

    /// The file list's entry for each file, in git's order.
    pub fn entries(&self) -> Vec<FileEntry> {
        (self.files.iter())
            .map(|file| FileEntry::from_local(&file.change, file.generated))
            .collect()
    }
}

/// A file the same on both sides, as `rev` has it at `path`: listed as modified, with nothing
/// changed. `None` when `rev` has no file there.
fn unchanged(repo: &Repo, rev: &Oid, path: &[u8]) -> anyhow::Result<Option<ChangedFile>> {
    let Some(version) = repo.file_version(rev, path)? else {
        return Ok(None);
    };
    Ok(Some(ChangedFile {
        change: Change::Modified,
        old: Some(version.clone()),
        new: Some(version),
        binary: false,
        lines: Some(LineCounts {
            added: 0,
            deleted: 0,
        }),
    }))
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

    use std::time::{Instant, SystemTime};

    use editor::SplittableEditor;
    use gpui::{Entity, TestAppContext, VisualTestContext};
    use repo_cache::{Git, PullRequestRefs, RepoCache};
    use tempfile::TempDir;

    use crate::diff_decorations::BadgeStyle;
    use crate::pr_diff::tests::fixtures::{self, Entry, Fixture};
    use crate::pr_diff::{self, FileHeader, Trust};
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

    fn revision(base: &str, head: &str) -> Revision {
        Revision {
            base: base.parse().unwrap(),
            head: head.parse().unwrap(),
        }
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
            let tab = tab.handle(cx);
            cx.spawn_in(window, async move |_, cx| {
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

    fn side(path: &str, old: &str, new: &str) -> (String, Option<String>, String) {
        (path.to_string(), Some(old.to_string()), new.to_string())
    }

    /// What the diff's left side and its right side show, each as one text.
    fn editors(diff: &Entity<SplittableEditor>, cx: &mut VisualTestContext) -> (String, String) {
        diff.read_with(cx, |diff, cx| {
            let lhs = diff.lhs_editor().expect("the diff shows side by side");
            (lhs.read(cx).text(cx), diff.rhs_editor().read(cx).text(cx))
        })
    }

    fn paths(comparison: &ShownComparison) -> Vec<String> {
        (comparison.files().iter())
            .map(|file| file.change.display_path().into_owned())
            .collect()
    }

    /// Lines 1 to 8, with each of `changed` changed: line 1 as the base changes it, the others as
    /// an author does.
    fn eight(changed: &[usize]) -> Vec<u8> {
        (1..=8)
            .map(|line| match changed.contains(&line) {
                true if line == 1 => "1 base\n".to_string(),
                true => format!("{line} changed\n"),
                false => format!("{line}\n"),
            })
            .collect::<String>()
            .into_bytes()
    }

    fn text(bytes: Vec<u8>) -> String {
        String::from_utf8(bytes).unwrap()
    }

    /// Two revisions on either side of a rebase compare what the author changed between them,
    /// with the old side read from the old revision replayed onto the new base: what came with
    /// the base is on both sides, so it doesn't show as a change.
    #[gpui::test]
    async fn between_revisions_across_a_rebase_shows_only_the_authors_change(
        cx: &mut TestAppContext,
    ) {
        // Line 1 changes with the base, line 5 in r1, and line 8 in r2 too.
        let fixture = fixtures::empty();
        let old_base = fixture.commit(
            &[
                Entry::File("a.txt", &eight(&[])),
                Entry::File("base.txt", b"base 1\n"),
            ],
            None,
        );
        let r1 = fixture.commit(
            &[
                Entry::File("a.txt", &eight(&[5])),
                Entry::File("base.txt", b"base 1\n"),
            ],
            Some(&old_base),
        );
        let new_base = fixture.commit(
            &[
                Entry::File("a.txt", &eight(&[1])),
                Entry::File("base.txt", b"base 2\n"),
            ],
            Some(&old_base),
        );
        // r1 rebased onto the new base, and changed again.
        let r2 = fixture.commit(
            &[
                Entry::File("a.txt", &eight(&[1, 5, 8])),
                Entry::File("base.txt", b"base 2\n"),
            ],
            Some(&new_base),
        );
        fixture.set_ref("refs/heads/main", &new_base);
        fixture.set_ref("refs/pull/1/head", &r2);
        let (local, _clone) = copy(&fixture, "main", &[&r1]);

        let comparison =
            ShownComparison::between(&local, &revision(&old_base, &r1), &revision(&new_base, &r2))
                .unwrap();
        assert_eq!(
            comparison.kind(),
            &ComparisonKind::Between {
                from: r1.parse().unwrap(),
                replay: Replay::Clean,
            }
        );
        // The old side is the replay's tree, not r1's.
        assert_ne!(comparison.old_side().as_str(), r1);
        assert_eq!(comparison.new_side().as_str(), r2);
        assert!(comparison.files().iter().all(|file| !file.conflicted()));

        let (tab, cx) = shown_files::tests::tab(cx).await;
        let diff = show(&tab, local, comparison, cx);
        assert_eq!(
            sides(&diff, cx),
            [side(
                "a.txt",
                &text(eight(&[1, 5])),
                &text(eight(&[1, 5, 8]))
            )]
        );
        // Each side shows the lines around the change: line 8 as it was, and changed.
        let (left, right) = editors(&diff, cx);
        assert!(
            left.ends_with("7\n8\n") && right.ends_with("7\n8 changed\n"),
            "{left}|{right}"
        );
        assert_eq!(
            tab.read_with(cx, |tab, cx| tab.comparison(cx).unwrap().new_side().clone()),
            r2.parse::<Oid>().unwrap()
        );
    }

    /// Two revisions on the same base compare their heads, so everything the author changed
    /// between them shows, and nothing is replayed.
    #[gpui::test]
    async fn revisions_on_one_base_compare_their_heads(cx: &mut TestAppContext) {
        let fixture = fixtures::empty();
        let base = fixture.commit(&[Entry::File("a.txt", &eight(&[]))], None);
        let r1 = fixture.commit(&[Entry::File("a.txt", &eight(&[3]))], Some(&base));
        let r2 = fixture.commit(&[Entry::File("a.txt", &eight(&[3, 6]))], Some(&r1));
        fixture.set_ref("refs/heads/main", &base);
        fixture.set_ref("refs/pull/1/head", &r2);
        let (local, _clone) = copy(&fixture, "main", &[&r1]);

        let comparison =
            ShownComparison::between(&local, &revision(&base, &r1), &revision(&base, &r2)).unwrap();
        assert_eq!(
            comparison.kind(),
            &ComparisonKind::Between {
                from: r1.parse().unwrap(),
                replay: Replay::SameBase,
            }
        );
        assert_eq!(comparison.old_side().as_str(), r1);

        let (tab, cx) = shown_files::tests::tab(cx).await;
        let diff = show(&tab, local, comparison, cx);
        assert_eq!(
            sides(&diff, cx),
            [side("a.txt", &text(eight(&[3])), &text(eight(&[3, 6])))]
        );
    }

    /// When the base moved somewhere a rebase doesn't explain, as when the pull request was
    /// retargeted, the comparison is still of what the author changed, and says the base moved so.
    #[gpui::test]
    async fn a_retargeted_comparison_says_so(cx: &mut TestAppContext) {
        let fixture = fixtures::empty();
        let root = fixture.commit(
            &[
                Entry::File("a.txt", &eight(&[])),
                Entry::File("base.txt", b"root\n"),
            ],
            None,
        );
        // Two branches off the root; the pull request moves from one to the other.
        let old_base = fixture.commit(
            &[
                Entry::File("a.txt", &eight(&[])),
                Entry::File("base.txt", b"old branch\n"),
            ],
            Some(&root),
        );
        let new_base = fixture.commit(
            &[
                Entry::File("a.txt", &eight(&[])),
                Entry::File("base.txt", b"new branch\n"),
            ],
            Some(&root),
        );
        let r1 = fixture.commit(
            &[
                Entry::File("a.txt", &eight(&[4])),
                Entry::File("base.txt", b"old branch\n"),
            ],
            Some(&old_base),
        );
        let r2 = fixture.commit(
            &[
                Entry::File("a.txt", &eight(&[4, 7])),
                Entry::File("base.txt", b"new branch\n"),
            ],
            Some(&new_base),
        );
        fixture.set_ref("refs/heads/main", &new_base);
        fixture.set_ref("refs/pull/1/head", &r2);
        let (local, _clone) = copy(&fixture, "main", &[&r1]);

        let comparison =
            ShownComparison::between(&local, &revision(&old_base, &r1), &revision(&new_base, &r2))
                .unwrap();
        assert_eq!(
            comparison.kind(),
            &ComparisonKind::Between {
                from: r1.parse().unwrap(),
                replay: Replay::Retargeted,
            }
        );

        let (tab, cx) = shown_files::tests::tab(cx).await;
        let diff = show(&tab, local, comparison, cx);
        assert_eq!(
            sides(&diff, cx),
            [side("a.txt", &text(eight(&[4])), &text(eight(&[4, 7])))]
        );
    }

    /// A file the author renamed while rebasing shows as a rename, from its old path in the
    /// replayed old revision to its new path, with only the author's change.
    #[gpui::test]
    async fn a_rename_across_a_rebase_shows_as_a_rename(cx: &mut TestAppContext) {
        let fixture = fixtures::empty();
        let old_base = fixture.commit(
            &[
                Entry::File("src.txt", &eight(&[])),
                Entry::File("base.txt", b"base 1\n"),
            ],
            None,
        );
        let r1 = fixture.commit(
            &[
                Entry::File("src.txt", &eight(&[5])),
                Entry::File("base.txt", b"base 1\n"),
            ],
            Some(&old_base),
        );
        let new_base = fixture.commit(
            &[
                Entry::File("src.txt", &eight(&[])),
                Entry::File("base.txt", b"base 2\n"),
            ],
            Some(&old_base),
        );
        let r2 = fixture.commit(
            &[
                Entry::File("dst.txt", &eight(&[5, 8])),
                Entry::File("base.txt", b"base 2\n"),
            ],
            Some(&new_base),
        );
        fixture.set_ref("refs/heads/main", &new_base);
        fixture.set_ref("refs/pull/1/head", &r2);
        let (local, _clone) = copy(&fixture, "main", &[&r1]);

        let comparison =
            ShownComparison::between(&local, &revision(&old_base, &r1), &revision(&new_base, &r2))
                .unwrap();
        assert_eq!(paths(&comparison), ["dst.txt"]);
        let change = &comparison.files()[0].change;
        assert!(
            matches!(change.change, Change::Renamed { .. }),
            "{change:?}"
        );
        assert_eq!(
            FileHeader::of(change).renamed_from.as_deref(),
            Some("src.txt")
        );

        let (tab, cx) = shown_files::tests::tab(cx).await;
        let diff = show(&tab, local, comparison, cx);
        assert_eq!(
            sides(&diff, cx),
            [side("dst.txt", &text(eight(&[5])), &text(eight(&[5, 8])))]
        );
        assert_eq!(
            shown_files::tests::badge_label(&diff, "dst.txt", BadgeStyle::RenamedFrom, cx)
                .as_deref(),
            Some("src.txt")
        );
    }

    /// When replaying the old revision onto the new base conflicts, the comparison is of the
    /// heads, and lists every file the replay conflicted on with how the author resolved it, even
    /// one that's the same on both sides because the author dropped the base's change.
    #[gpui::test]
    async fn a_conflicted_rebase_lists_every_file_it_conflicted_on(cx: &mut TestAppContext) {
        let fixture = fixtures::empty();
        let old_base = fixture.commit(
            &[
                Entry::File("a.txt", b"a\n"),
                Entry::File("kept.txt", b"kept\n"),
                Entry::File("merged.txt", b"merged\n"),
            ],
            None,
        );
        let r1 = fixture.commit(
            &[
                Entry::File("a.txt", b"a\n"),
                Entry::File("kept.txt", b"kept by r1\n"),
                Entry::File("merged.txt", b"merged by r1\n"),
            ],
            Some(&old_base),
        );
        let new_base = fixture.commit(
            &[
                Entry::File("a.txt", b"a\n"),
                Entry::File("kept.txt", b"kept by the base\n"),
                Entry::File("merged.txt", b"merged by the base\n"),
            ],
            Some(&old_base),
        );
        // The author keeps r1's kept.txt, dropping the base's change, merges merged.txt, and
        // changes a.txt.
        let r2 = fixture.commit(
            &[
                Entry::File("a.txt", b"a changed\n"),
                Entry::File("kept.txt", b"kept by r1\n"),
                Entry::File("merged.txt", b"merged by both\n"),
            ],
            Some(&new_base),
        );
        fixture.set_ref("refs/heads/main", &new_base);
        fixture.set_ref("refs/pull/1/head", &r2);
        let (local, _clone) = copy(&fixture, "main", &[&r1]);

        let comparison =
            ShownComparison::between(&local, &revision(&old_base, &r1), &revision(&new_base, &r2))
                .unwrap();
        assert!(matches!(
            comparison.kind(),
            ComparisonKind::Between {
                replay: Replay::Conflicted { .. },
                ..
            }
        ));
        assert_eq!(comparison.old_side().as_str(), r1);
        assert_eq!(paths(&comparison), ["a.txt", "kept.txt", "merged.txt"]);
        let conflicted: Vec<_> = (comparison.files().iter())
            .map(|file| file.conflicted())
            .collect();
        assert_eq!(conflicted, [false, true, true]);
        // How the author resolved each: from the replay's markers to the new head.
        for file in &comparison.files()[1..] {
            let resolution = file.resolution.as_ref().unwrap();
            assert_eq!(resolution.len(), 1, "{file:?}");
            assert_eq!(resolution[0].path(), file.change.path());
        }
        // The file the same on both sides lists with nothing changed.
        let kept = &comparison.files()[1].change;
        assert_eq!(kept.old, kept.new);
        assert_eq!(comparison.entries().len(), 3);

        let (tab, cx) = shown_files::tests::tab(cx).await;
        let diff = show(&tab, local, comparison, cx);
        assert_eq!(
            sides(&diff, cx),
            [
                side("a.txt", "a\n", "a changed\n"),
                side("kept.txt", "kept by r1\n", "kept by r1\n"),
                side("merged.txt", "merged by r1\n", "merged by both\n"),
            ]
        );
        assert_eq!(
            shown_files::tests::collapsed_label(&diff, "kept.txt", cx).as_deref(),
            Some("no visible change")
        );
        assert_eq!(tab.read_with(cx, |tab, _| tab.files().len()), 3);
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
        assert_eq!(
            paths(&ShownComparison::full(&local).unwrap()),
            ["a.txt", "b.txt", "lower.txt"]
        );

        let comparison = ShownComparison::layer(&local, &lower.parse().unwrap()).unwrap();
        assert_eq!(comparison.kind(), &ComparisonKind::Layer);
        assert_eq!(comparison.old_side().as_str(), lower);
        assert_eq!(comparison.new_side().as_str(), upper);

        let (tab, cx) = shown_files::tests::tab(cx).await;
        let diff = show(&tab, local, comparison, cx);
        assert_eq!(
            sides(&diff, cx),
            [
                side("a.txt", "a lower\n", "a lower\na upper\n"),
                side("b.txt", "b\n", "b upper\n"),
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

    /// A lower layer's head the clone doesn't have is refused, never downloaded with its history.
    #[test]
    fn a_layer_over_a_head_the_clone_lacks_is_refused() {
        let fixture = fixtures::empty();
        let base = fixture.commit(&[Entry::File("a.txt", b"a\n")], None);
        let head = fixture.commit(&[Entry::File("a.txt", b"a head\n")], Some(&base));
        // On GitHub, but on no ref the clone fetches.
        let elsewhere = fixture.commit(&[Entry::File("a.txt", b"elsewhere\n")], Some(&base));
        fixture.set_ref("refs/heads/main", &base);
        fixture.set_ref("refs/pull/1/head", &head);
        fixture.set_ref("refs/heads/elsewhere", &elsewhere);
        let (local, _clone) = copy(&fixture, "main", &[]);

        let error = ShownComparison::layer(&local, &elsewhere.parse().unwrap())
            .err()
            .expect("the clone lacks it");
        assert!(
            matches!(
                error.downcast_ref::<Error>(),
                Some(Error::NotFetched { oid }) if oid.as_str() == elsewhere
            ),
            "{error:#}"
        );
        assert!(!local.repo.has_commit(&elsewhere.parse().unwrap()).unwrap());
    }

    /// The constructors run on the background executor through `spawn`.
    #[gpui::test]
    async fn a_comparison_is_made_in_the_background(cx: &mut TestAppContext) {
        let fixture = fixtures::empty();
        let base = fixture.commit(&[Entry::File("a.txt", b"a\n")], None);
        let head = fixture.commit(&[Entry::File("a.txt", b"a head\n")], Some(&base));
        fixture.set_ref("refs/heads/main", &base);
        fixture.set_ref("refs/pull/1/head", &head);
        let (local, _clone) = copy(&fixture, "main", &[]);
        let task = ShownComparison::spawn(Arc::new(local), &cx.executor(), ShownComparison::full);
        let comparison = task.await.unwrap();
        assert_eq!(paths(&comparison), ["a.txt"]);
    }
}
