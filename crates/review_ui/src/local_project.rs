//! The Zed project rozed shows a pull request in, set up so a repository can't configure rozed or
//! run anything on its own:
//!
//! - **No repository settings.** Zed applies a trusted worktree's `.zed/settings.json` over every
//!   global settings layer, including the one rozed forces, so a repository could pick its own
//!   language server binary. rozed's project never applies a worktree's own settings, tasks or
//!   debug scenarios (`.zed/`, `.vscode/tasks.json`, `.vscode/launch.json`), trusted or not.
//!   `.editorconfig` files still apply: they set only indentation, line endings and line length.
//! - **No login shell in the checkout.** Without an environment of its own, Zed's project learns
//!   each worktree's by running the user's login shell, interactively, `cd`'d into the worktree,
//!   so the shell's directory hooks (direnv, mise, nvm and the like) run on the pull request's
//!   files. rozed hands it rozed's own environment instead, and Zed then captures none.
//! - **No Recent Items.** Zed adds every visible worktree to macOS's Recent Items, and a cached
//!   checkout doesn't belong there. On some desktops the update also kept rozed from quitting.

use std::collections::HashMap;

use gpui::{App, Entity};
use project::{LocalProjectFlags, Project};
use workspace::AppState;

/// A new local project for rozed's window, set up with [`configure`].
pub fn new(app_state: &AppState, cx: &mut App) -> Entity<Project> {
    let project = Project::local(
        app_state.client.clone(),
        app_state.node_runtime.clone(),
        app_state.user_store.clone(),
        app_state.languages.clone(),
        app_state.fs.clone(),
        Some(environment().into_iter().collect()),
        LocalProjectFlags {
            // Zed's worktree trust: no language server until rozed trusts the worktree (see
            // `crate::trust`).
            init_worktree_trust: true,
            watch_global_configs: true,
        },
        cx,
    );
    configure(&project, cx);
    project
}

/// rozed's own environment, for the project's language servers and git. Variables that aren't
/// UTF-8 are left out.
fn environment() -> HashMap<String, String> {
    std::env::vars_os()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
        .collect()
}

/// Stops `project` applying its worktrees' own settings, tasks and debug scenarios, and adding
/// them to macOS's Recent Items. Call it before the project gets any worktree.
pub fn configure(project: &Entity<Project>, cx: &mut App) {
    project.update(cx, |project, cx| {
        project
            .settings_observer()
            .update(cx, |settings_observer, _| {
                settings_observer.ignore_worktree_settings_files()
            });
        project.worktree_store().update(cx, |worktree_store, _| {
            worktree_store.keep_out_of_recent_documents()
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::RefCell;
    use std::path::Path;
    use std::rc::Rc;

    use gpui::TestAppContext;
    use language::LanguageServerName;
    use project::FakeFs;
    use project::project_settings::{ProjectSettings, SettingsObserverEvent};
    use settings::{Settings as _, SettingsLocation};
    use util::rel_path::RelPath;

    use crate::trust;

    /// A repository that tries to configure the app: its own rust-analyzer binary, a tab size, a
    /// task and a debug scenario, in Zed's files and VS Code's.
    async fn repository_with_zed_settings(cx: &mut TestAppContext) -> std::sync::Arc<FakeFs> {
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            "/checkout",
            serde_json::json!({
                ".zed": {
                    "settings.json": r#"{
                        "lsp": { "rust-analyzer": { "binary": { "path": "./evil" } } },
                        "tab_size": 7
                    }"#,
                    "tasks.json": r#"[{ "label": "evil", "command": "./evil" }]"#,
                    "debug.json": r#"[{ "label": "evil", "adapter": "CodeLLDB", "request": "launch", "program": "./evil" }]"#,
                },
                ".vscode": {
                    "tasks.json": r#"{ "version": "2.0.0", "tasks": [{ "label": "evil", "type": "shell", "command": "./evil" }] }"#,
                },
                "evil": "#!/bin/sh\necho owned\n",
                "main.rs": "fn main() {}",
            }),
        )
        .await;
        fs
    }

    struct Opened {
        rust_analyzer_path: Option<String>,
        /// The settings observer's events: which of the repository's files it applied.
        applied: Vec<String>,
    }

    /// Opens `/checkout` in a new project, configured as rozed's when `configure_project` is set,
    /// and trusts it.
    async fn open_trusted(configure_project: bool, cx: &mut TestAppContext) -> Opened {
        trust::tests::init_test(cx);
        let fs = repository_with_zed_settings(cx).await;
        let project = Project::test_with_worktree_trust(fs, [], cx).await;
        if configure_project {
            cx.update(|cx| configure(&project, cx));
        }
        let applied = Rc::new(RefCell::new(Vec::new()));
        let settings_observer =
            project.read_with(cx, |project, _| project.settings_observer().clone());
        cx.update(|cx| {
            let applied = applied.clone();
            cx.subscribe(&settings_observer, move |_, event, _| {
                let kind = match event {
                    SettingsObserverEvent::LocalSettingsUpdated(_) => "settings",
                    SettingsObserverEvent::LocalTasksUpdated(_) => "tasks",
                    SettingsObserverEvent::LocalDebugScenariosUpdated(_) => "debug",
                    // rozed's own tasks.json and debug.json, not the repository's.
                    SettingsObserverEvent::GlobalTasksUpdated(_)
                    | SettingsObserverEvent::GlobalDebugScenariosUpdated(_) => return,
                };
                applied.borrow_mut().push(kind.to_string());
            })
            .detach()
        });
        cx.update(|cx| trust::trust_checkout(&project, Path::new("/checkout"), cx))
            .unwrap();
        let (worktree, _) = project
            .update(cx, |project, cx| {
                project.find_or_create_worktree("/checkout", true, cx)
            })
            .await
            .unwrap();
        cx.run_until_parked();
        assert!(trust::tests::is_trusted(
            &project,
            Path::new("/checkout"),
            cx
        ));

        let worktree_id = worktree.read_with(cx, |worktree, _| worktree.id());
        let rust_analyzer_path = cx.update(|cx| {
            let location = SettingsLocation {
                worktree_id,
                path: RelPath::empty(),
            };
            ProjectSettings::get(Some(location), cx)
                .lsp
                .get(&LanguageServerName::new_static("rust-analyzer"))
                .and_then(|settings| settings.binary.as_ref()?.path.clone())
        });
        let mut applied = applied.borrow().clone();
        applied.sort();
        applied.dedup();
        Opened {
            rust_analyzer_path,
            applied,
        }
    }

    /// Why rozed configures its project: Zed applies a trusted repository's own settings.
    #[gpui::test]
    async fn zed_applies_a_trusted_repositorys_own_settings(cx: &mut TestAppContext) {
        let opened = open_trusted(false, cx).await;
        assert_eq!(opened.rust_analyzer_path.as_deref(), Some("./evil"));
        assert_eq!(opened.applied, ["debug", "settings", "tasks"]);
    }

    #[gpui::test]
    async fn a_trusted_repositorys_own_settings_never_apply(cx: &mut TestAppContext) {
        let opened = open_trusted(true, cx).await;
        assert_eq!(opened.rust_analyzer_path, None);
        assert_eq!(opened.applied, Vec::<String>::new());
    }

    #[gpui::test]
    async fn checkouts_stay_out_of_recent_items(cx: &mut TestAppContext) {
        trust::tests::init_test(cx);
        let project = Project::test_with_worktree_trust(FakeFs::new(cx.executor()), [], cx).await;
        let adds = |cx: &mut TestAppContext| {
            project.read_with(cx, |project, cx| {
                project.worktree_store().read(cx).adds_to_recent_documents()
            })
        };
        assert!(adds(cx), "Zed's default changed");
        cx.update(|cx| configure(&project, cx));
        assert!(!adds(cx));
    }

    #[test]
    fn the_environment_is_los_own() {
        let environment = environment();
        assert_eq!(
            environment.get("PATH").map(String::as_str),
            std::env::var("PATH").ok().as_deref()
        );
    }
}
