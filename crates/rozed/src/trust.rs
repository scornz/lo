//! Which folders rozed trusts to run language servers and apply `.zed/settings.json` for.
//!
//! rozed never runs a repository's code unless the user trusts the repository (design §1). Zed's
//! worktree trust enforces that: with its trust store in place, Zed's project starts no language
//! server and applies no repository settings in a worktree until the worktree is trusted. rozed
//! starts the store empty, without the folders Zed's own database remembers, so nothing is trusted
//! until rozed trusts it with [`trust_path`].

use std::path::Path;

use gpui::{App, Entity};
use project::Project;
use project::trusted_worktrees::{self, PathTrust, TrustedWorktrees};

/// Sets up Zed's worktree trust with nothing trusted. Call it before creating any project: a
/// project only tracks trust when the store exists as it's created.
pub fn init(cx: &mut App) {
    trusted_worktrees::init(Default::default(), cx);
}

/// Trusts `path` and every folder under it in `project`, now and for worktrees added later, so
/// their language servers start and their `.zed/settings.json` applies.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the pull request view calls it for its checkout")
)]
pub fn trust_path(project: &Entity<Project>, path: &Path, cx: &mut App) {
    let Some(trusted_worktrees) = TrustedWorktrees::try_get_global(cx) else {
        log::error!(
            "can't trust {}: worktree trust isn't set up",
            path.display()
        );
        return;
    };
    let worktree_store = project.read(cx).worktree_store();
    trusted_worktrees.update(cx, |trusted_worktrees, cx| {
        trusted_worktrees.trust(
            &worktree_store,
            [PathTrust::AbsPath(path.to_path_buf())]
                .into_iter()
                .collect(),
            cx,
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    use gpui::TestAppContext;
    use language::language_settings::LanguageSettings;
    use project::FakeFs;
    use settings::SettingsStore;

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings = SettingsStore::test(cx);
            cx.set_global(settings);
            release_channel::init(semver::Version::new(0, 0, 0), cx);
            init(cx);
        });
    }

    #[gpui::test]
    async fn a_repository_s_settings_apply_only_once_it_is_trusted(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            "/repo",
            serde_json::json!({
                ".zed": { "settings.json": r#"{ "tab_size": 7 }"# },
                "main.rs": "fn main() {}",
            }),
        )
        .await;
        let project = Project::test_with_worktree_trust(fs, [Path::new("/repo")], cx).await;
        let buffer = project
            .update(cx, |project, cx| {
                project.open_local_buffer("/repo/main.rs", cx)
            })
            .await
            .unwrap();
        cx.run_until_parked();
        let tab_size = |cx: &mut TestAppContext| {
            cx.update(|cx| {
                LanguageSettings::for_buffer(buffer.read(cx), cx)
                    .tab_size
                    .get()
            })
        };
        assert_ne!(
            tab_size(cx),
            7,
            "an untrusted repository's settings applied"
        );

        cx.update(|cx| trust_path(&project, Path::new("/repo"), cx));
        cx.run_until_parked();
        assert_eq!(
            tab_size(cx),
            7,
            "a trusted repository's settings didn't apply"
        );
    }

    #[gpui::test]
    async fn nothing_is_trusted_at_first(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree("/repo", serde_json::json!({ "main.rs": "fn main() {}" }))
            .await;
        let project = Project::test_with_worktree_trust(fs, [Path::new("/repo")], cx).await;
        let (worktree_store, worktree_id) = project.read_with(cx, |project, cx| {
            let worktree_store = project.worktree_store();
            let worktree_id = project.worktrees(cx).next().unwrap().read(cx).id();
            (worktree_store, worktree_id)
        });
        let trusted_worktrees = cx.update(|cx| TrustedWorktrees::try_get_global(cx).unwrap());
        let can_trust = |cx: &mut TestAppContext| {
            trusted_worktrees.update(cx, |trusted_worktrees, cx| {
                trusted_worktrees.can_trust(&worktree_store, worktree_id, cx)
            })
        };
        assert!(!can_trust(cx));
        cx.update(|cx| trust_path(&project, Path::new("/repo"), cx));
        assert!(can_trust(cx));
    }
}
