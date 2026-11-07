//! A fake GitHub for the tab's tests: its REST API, answering per token, and its git server, real
//! bare repositories served over `file://`. With them, tests open pull requests the way Lo
//! does, through the GitHub layer, the local copy and the diff.

use gpui::{Entity, TestAppContext, VisualTestContext};
use project::{FakeFs, Project};
use workspace::{MultiWorkspace, Workspace};

mod git;
mod github;

pub use git::GitServer;
pub use github::{Answer, FakeGithub, FakePull, WriteAnswer, anonymous, sign_in, sign_in_as};

/// Sets the app up as Lo does: the command palette, the editor, Zed's keymap with the tab's
/// keys over it, and Lo's changes to every workspace.
pub fn init(cx: &mut TestAppContext) {
    crate::trust::tests::init_test(cx);
    cx.update(|cx| {
        command_palette_hooks::init(cx);
        editor::init(cx);
        crate::init_workspaces(cx);
    });
}

/// A window like Lo's: a workspace in a `MultiWorkspace`, which renders the workspace's
/// actions.
pub async fn workspace(cx: &mut TestAppContext) -> (Entity<Workspace>, &mut VisualTestContext) {
    init(cx);
    let fs = FakeFs::new(cx.executor());
    // A repository elsewhere on disk, which nothing in Lo should open.
    fs.insert_tree(
        "/elsewhere",
        serde_json::json!({
            "Cargo.toml": "[package]\nname = \"elsewhere\"\n",
            "src": { "main.rs": "fn main() {}\n" },
        }),
    )
    .await;
    let project = Project::test(fs, [], cx).await;
    let (multi_workspace, cx) =
        cx.add_window_view(|window, cx| MultiWorkspace::test_new(project, window, cx));
    let workspace =
        multi_workspace.read_with(cx, |multi_workspace, _| multi_workspace.workspace().clone());
    (workspace, cx)
}
