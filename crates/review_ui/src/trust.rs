//! Which folders Lo trusts to run language servers in.
//!
//! Lo never runs a repository's code unless the user trusts the repository (design §1). Until
//! milestone 10's trust prompt, trusting is the `--trust` flag. Without it Lo makes no checkout,
//! so a language server has nowhere to run. With it, Lo trusts the pull request's checkout and
//! nothing else, and Zed's worktree trust enforces that: Zed's project starts no language server
//! in a worktree until the worktree is trusted.
//!
//! Trust never carries over between runs: Lo starts Zed's trust store empty and empties the
//! table where Zed's workspace saves trusted paths. Trust starts language servers and nothing else:
//! a repository's own Zed settings never apply, trusted or not (see [`crate::local_project`]).

use std::path::Path;

use anyhow::Context as _;
use gpui::{App, Entity};
use project::Project;
use project::trusted_worktrees::{self, PathTrust, TrustedWorktrees};
use project::worktree_store::WorktreeStoreEvent;
use workspace::WorkspaceDb;

/// Sets up Zed's worktree trust with nothing trusted, after emptying the table of paths Zed's
/// workspace saved as trusted. Call it before creating any project: a project only tracks trust
/// when the store exists as it's created.
pub fn init(cx: &mut App) {
    forget_saved_trust(cx);
    trusted_worktrees::init(Default::default(), cx);
}

/// Empties the `trusted_worktrees` table in Lo's database. Zed's workspace saves every path
/// trusted in a run there, and Zed reads it back at startup. Lo never reads it, and empties it
/// so no later run, or later code, can inherit a pull request's trust.
fn forget_saved_trust(cx: &mut App) {
    let cleared = cx
        .foreground_executor()
        .block_on(WorkspaceDb::global(cx).clear_trusted_worktrees());
    if let Err(error) = cleared {
        log::error!("can't empty Zed's table of trusted folders: {error:#}");
    }
}

/// Trusts the folder `checkout`, and every folder under it, in `project`, so language servers
/// start there.
///
/// Zed also trusts every single-file worktree once a folder is trusted. Lo never opens one
/// itself, so from here on it restricts any visible single-file worktree the project gets, such as
/// a file dropped on the window.
pub fn trust_checkout(
    project: &Entity<Project>,
    checkout: &Path,
    cx: &mut App,
) -> anyhow::Result<()> {
    let trusted_worktrees =
        TrustedWorktrees::try_get_global(cx).context("worktree trust isn't set up")?;
    let worktree_store = project.read(cx).worktree_store();
    cx.subscribe(&worktree_store, {
        let trusted_worktrees = trusted_worktrees.clone();
        move |worktree_store, event, cx| {
            let WorktreeStoreEvent::WorktreeAdded(worktree) = event else {
                return;
            };
            let worktree = worktree.read(cx);
            if !worktree.is_visible() || !worktree.is_single_file() {
                return;
            }
            log::warn!(
                "not trusting {}: Lo trusts only the pull request's checkout",
                worktree.abs_path().display()
            );
            let restricted = [PathTrust::Worktree(worktree.id())].into_iter().collect();
            trusted_worktrees.update(cx, |trusted_worktrees, cx| {
                trusted_worktrees.restrict(worktree_store.downgrade(), restricted, cx)
            });
        }
    })
    .detach();
    trusted_worktrees.update(cx, |trusted_worktrees, cx| {
        trusted_worktrees.trust(
            &worktree_store,
            [PathTrust::AbsPath(checkout.to_path_buf())]
                .into_iter()
                .collect(),
            cx,
        );
    });
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    use std::path::PathBuf;

    use gpui::{TestAppContext, VisualTestContext};
    use project::{FakeFs, WorktreeId};
    use settings::SettingsStore;
    use workspace::Workspace;

    pub(crate) fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings = SettingsStore::test(cx);
            cx.set_global(settings);
            cx.set_global(db::AppDatabase::test_new());
            release_channel::init(semver::Version::new(0, 0, 0), cx);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            init(cx);
        });
    }

    const CHECKOUTS: &str = "/cache/checkouts/github.com/owner/repo";

    fn checkout(name: &str) -> PathBuf {
        Path::new(CHECKOUTS).join(name)
    }

    /// Whether the worktree at `root` in `project` is trusted.
    pub(crate) fn is_trusted(
        project: &Entity<Project>,
        root: &Path,
        cx: &mut TestAppContext,
    ) -> bool {
        let (worktree_store, worktree_id) = project.read_with(cx, |project, cx| {
            (project.worktree_store(), worktree_id(project, root, cx))
        });
        let trusted_worktrees = cx.update(|cx| TrustedWorktrees::try_get_global(cx).unwrap());
        trusted_worktrees.update(cx, |trusted_worktrees, cx| {
            trusted_worktrees.can_trust(&worktree_store, worktree_id, cx)
        })
    }

    fn worktree_id(project: &Project, root: &Path, cx: &App) -> WorktreeId {
        project
            .worktrees(cx)
            .find(|worktree| worktree.read(cx).abs_path().as_ref() == root)
            .unwrap_or_else(|| panic!("no worktree at {}", root.display()))
            .read(cx)
            .id()
    }

    /// A project with a worktree for each of `roots`, on a disk with three checkouts side by side
    /// and a file elsewhere.
    async fn project(roots: &[PathBuf], cx: &mut TestAppContext) -> Entity<Project> {
        let fs = FakeFs::new(cx.executor());
        let main_rs = serde_json::json!({ "main.rs": "fn main() {}" });
        fs.insert_tree(
            CHECKOUTS,
            serde_json::json!({
                "pull-1": main_rs.clone(),
                "pull-12": main_rs.clone(),
                "pull-2": main_rs,
            }),
        )
        .await;
        fs.insert_tree(
            "/elsewhere",
            serde_json::json!({ "lib.rs": "pub fn f() {}" }),
        )
        .await;
        Project::test_with_worktree_trust(fs, roots.iter().map(PathBuf::as_path), cx).await
    }

    async fn add_worktree(project: &Entity<Project>, path: &Path, cx: &mut TestAppContext) {
        project
            .update(cx, |project, cx| {
                project.find_or_create_worktree(path, true, cx)
            })
            .await
            .unwrap();
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn nothing_is_trusted_at_first(cx: &mut TestAppContext) {
        init_test(cx);
        let project = project(&[checkout("pull-1")], cx).await;
        assert!(!is_trusted(&project, &checkout("pull-1"), cx));
    }

    #[gpui::test]
    async fn only_the_checkout_is_trusted(cx: &mut TestAppContext) {
        init_test(cx);
        let roots = [
            checkout("pull-1"),
            // Siblings, one whose name starts with the checkout's.
            checkout("pull-12"),
            checkout("pull-2"),
            // The folder that holds them all.
            PathBuf::from(CHECKOUTS),
        ];
        let project = project(&roots, cx).await;

        cx.update(|cx| trust_checkout(&project, &checkout("pull-1"), cx))
            .unwrap();

        assert!(is_trusted(&project, &checkout("pull-1"), cx));
        assert!(!is_trusted(&project, &checkout("pull-12"), cx));
        assert!(!is_trusted(&project, &checkout("pull-2"), cx));
        assert!(!is_trusted(&project, Path::new(CHECKOUTS), cx));
    }

    /// Why Lo restricts single-file worktrees: Zed itself trusts every one once a folder is
    /// trusted, even a file nowhere near it.
    #[gpui::test]
    async fn zed_trusts_any_single_file_once_a_folder_is_trusted(cx: &mut TestAppContext) {
        init_test(cx);
        let elsewhere = PathBuf::from("/elsewhere/lib.rs");
        let project = project(&[checkout("pull-1")], cx).await;
        cx.update(|cx| {
            let worktree_store = project.read(cx).worktree_store();
            let trusted = [PathTrust::AbsPath(checkout("pull-1"))]
                .into_iter()
                .collect();
            TrustedWorktrees::try_get_global(cx)
                .unwrap()
                .update(cx, |trusted_worktrees, cx| {
                    trusted_worktrees.trust(&worktree_store, trusted, cx)
                })
        });

        add_worktree(&project, &elsewhere, cx).await;

        assert!(is_trusted(&project, &elsewhere, cx));
    }

    #[gpui::test]
    async fn single_files_stay_untrusted_after_trusting_a_checkout(cx: &mut TestAppContext) {
        init_test(cx);
        let elsewhere = PathBuf::from("/elsewhere/lib.rs");
        let project = project(&[checkout("pull-1")], cx).await;
        cx.update(|cx| trust_checkout(&project, &checkout("pull-1"), cx))
            .unwrap();

        add_worktree(&project, &elsewhere, cx).await;

        assert!(!is_trusted(&project, &elsewhere, cx));
        assert!(is_trusted(&project, &checkout("pull-1"), cx));
    }

    #[gpui::test]
    async fn trust_saved_by_zeds_workspace_is_forgotten_at_startup(cx: &mut TestAppContext) {
        init_test(cx);
        let project = project(&[checkout("pull-1")], cx).await;
        // Zed's workspace saves trusted paths a moment after each change.
        let window = cx.add_window(|window, cx| Workspace::test_new(project.clone(), window, cx));
        let _cx = VisualTestContext::from_window(*window, cx);
        cx.update(|cx| trust_checkout(&project, &checkout("pull-1"), cx))
            .unwrap();
        cx.executor()
            .advance_clock(workspace::SERIALIZATION_THROTTLE_TIME * 2);
        cx.run_until_parked();
        let saved = |cx: &mut TestAppContext| {
            cx.update(|cx| WorkspaceDb::global(cx).fetch_trusted_worktrees().unwrap())
                .into_values()
                .flatten()
                .collect::<Vec<_>>()
        };
        assert_eq!(saved(cx), vec![checkout("pull-1")]);

        // The next run starts here.
        cx.update(forget_saved_trust);

        assert_eq!(saved(cx), Vec::<PathBuf>::new());
    }
}
