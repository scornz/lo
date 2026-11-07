//! What vim may not do in Lo (design §1): run a shell, open, read or write files on disk, open a
//! link that isn't a web page, or, in the diff, which is never editable, start typing.
//!
//! Vim's `:` commands come from the command palette, through vim's interceptor, which turns what's
//! typed into an action. Lo puts its own interceptor over vim's ([`wrap_vims_interceptor`]). It
//! gives vim no workspace, which vim uses only to list the folders on disk a path could complete
//! to, and swaps each command Lo refuses for [`RefusedCommand`], which says why. Under that, every
//! editor captures the refused actions before vim's handlers, which sit on the same editor, can
//! run them, whatever dispatched them ([`guard_editor`]). Vim's shell keys, `!` and visual `!`,
//! aren't bound at all (`crate::keymap`), and a workspace runs no terminal or task
//! ([`NoTerminals`]).
//!
//! Some of vim's command actions can't be built by name, so Lo learns their types from vim's own
//! interceptor: from a few commands it asks vim about when it wraps it, and from the commands that
//! pass through.

use std::any::TypeId;
use std::collections::{HashMap, HashSet};
use std::process::ExitStatus;

use command_palette_hooks::{
    CommandInterceptResult, CommandPaletteFilter, GlobalCommandPaletteInterceptor,
};
use editor::Editor;
use futures::FutureExt as _;
use gpui::{
    Action, App, AppContext as _, Context, DispatchPhase, Global, Task, WeakEntity, Window,
};
use workspace::notifications::NotificationId;
use workspace::{SaveIntent, Toast, Workspace};

use crate::links;
use crate::pr_tab::keys;
use crate::read_only::FileActions;

/// Why Lo refuses something vim asked for, as the toast says it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Refusal {
    /// Running a shell command: `:!`, `:r !`, a filter through `!`, or a terminal.
    Shell,
    /// Opening, reading or writing a file on disk: `:w`, `:e`, `:r`, `:sp`, `ZZ` and the like.
    Files,
    /// A command that runs other commands: `:g` on each matching line, and `:norm`.
    Commands,
    /// A link that isn't a web page, such as `file://`.
    Link,
}

impl Refusal {
    pub fn message(self) -> &'static str {
        match self {
            Self::Shell => "Lo doesn't run shell commands.",
            Self::Files => "Lo's diffs are read-only, so vim can't open, read or write files here.",
            Self::Commands => "Lo doesn't run vim's :g or :norm, which run other commands.",
            Self::Link => "Lo opens only http and https links.",
        }
    }
}

/// Vim's actions that run a shell. `vim::ShellExec` runs a `:!` command, through a terminal or
/// `Project::exec_in_shell`, its only caller; the other two start one from a key, `!`.
pub(crate) const SHELL_ACTIONS: &[&str] = &[
    "vim::ShellExec",
    "vim::PushShellCommand",
    "vim::ShellCommand",
];

/// Vim's command actions that open, read or write a file: `:w` and `:w <file>`, `:e <file>`,
/// `:tabe <file>`, `:r` and `:r <file>`, and `:sp <file>` and `:vs <file>`.
const FILE_ACTIONS: &[&str] = &[
    "vim::VimSave",
    "vim::VimEdit",
    "vim::VimRead",
    "vim::VimSplit",
];

/// Vim's command actions that run other commands.
const COMMAND_ACTIONS: &[&str] = &["vim::OnMatchingLines", "vim::VimNorm"];

/// Vim's actions that start insert or replace mode, which Lo refuses in the diff, where nothing
/// can be typed. Comment boxes keep them.
const INSERT_ACTIONS: &[&str] = &[
    "vim::InsertBefore",
    "vim::InsertAfter",
    "vim::InsertLineBelow",
    "vim::InsertLineAbove",
    "vim::InsertFirstNonWhitespace",
    "vim::InsertEndOfLine",
    "vim::InsertAtPrevious",
    "vim::VisualInsertFirstNonWhiteSpace",
    "vim::VisualInsertEndOfLine",
    "vim::Substitute",
    "vim::SubstituteLine",
    "vim::ChangeToEndOfLine",
    "vim::PushChange",
    "vim::ToggleReplace",
    "vim::SwitchToInsertMode",
    "vim::SwitchToReplaceMode",
];

/// Commands Lo asks vim's interceptor about when it wraps it, to learn the types of the refused
/// actions vim can't build by name.
const PROBES: &[&str] = &[
    "!x", "r !x", "w", "r", "e x", "sp x", "vs x", "norm x", "g/x/d",
];

/// Why Lo refuses `action`, if it does.
pub fn refusal(action: &dyn Action) -> Option<Refusal> {
    let name = action.name();
    if SHELL_ACTIONS.contains(&name) {
        Some(Refusal::Shell)
    } else if FILE_ACTIONS.contains(&name)
        || FileActions::names().contains(&name)
        || saves_on_close(action)
    {
        Some(Refusal::Files)
    } else if COMMAND_ACTIONS.contains(&name) {
        Some(Refusal::Commands)
    } else {
        None
    }
}

/// Whether `action` closes items saving them first, as `:wq`, `:x` and `ZZ` do. Nothing in Lo's
/// window has anything to save; the keys say "write".
pub fn saves_on_close(action: &dyn Action) -> bool {
    let saves = |intent: Option<SaveIntent>| {
        matches!(
            intent,
            Some(SaveIntent::Save | SaveIntent::SaveAll | SaveIntent::Overwrite)
        )
    };
    let any = action.as_any();
    if let Some(close) = any.downcast_ref::<workspace::CloseActiveItem>() {
        saves(close.save_intent)
    } else if let Some(close) = any.downcast_ref::<workspace::CloseAllItemsAndPanes>() {
        saves(close.save_intent)
    } else {
        false
    }
}

/// The refused actions' types, as Lo learns them.
#[derive(Default)]
struct Refused {
    everywhere: HashMap<TypeId, Refusal>,
    /// Refused only in the diff.
    in_diff: HashSet<TypeId>,
}

impl Global for Refused {}

/// Records `action`'s type if Lo refuses it, whatever its value. A close that saves first is
/// refused by its value, by the workspace (`crate::read_only`).
fn learn(action: &dyn Action, cx: &mut App) {
    if saves_on_close(action) {
        return;
    }
    if let Some(refusal) = refusal(action) {
        cx.default_global::<Refused>()
            .everywhere
            .insert(action.as_any().type_id(), refusal);
    }
}

/// A command Lo refuses, in place of vim's: running it says why.
#[derive(Clone, PartialEq, Action)]
#[action(namespace = review_ui, no_json, no_register)]
pub struct RefusedCommand {
    refusal: Refusal,
}

/// Sets the refusals up. Call it after `vim::init` and once the command palette is set up.
pub(super) fn init(cx: &mut App) {
    let mut shell = Vec::new();
    for name in SHELL_ACTIONS.iter().chain(INSERT_ACTIONS) {
        match cx.build_action(name, None) {
            Ok(action) => {
                let type_id = action.as_any().type_id();
                if INSERT_ACTIONS.contains(name) {
                    cx.default_global::<Refused>().in_diff.insert(type_id);
                } else {
                    learn(action.as_ref(), cx);
                    shell.push(type_id);
                }
            }
            // `vim::ShellExec` has no name to build it by; the probes learn it.
            Err(error) => log::debug!("can't build {name} to refuse it: {error}"),
        }
    }
    CommandPaletteFilter::update_global(cx, |filter, _| filter.hide_action_types(&shell));
    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace.register_action(|_, command: &RefusedCommand, window, cx| {
            refuse(command.refusal, window, cx)
        });
    })
    .detach();
    cx.observe_new(|editor: &mut Editor, _, cx| guard_editor(editor, cx))
        .detach();
}

/// Says why Lo refused something, in a toast.
pub fn refuse(refusal: Refusal, window: &mut Window, cx: &mut App) {
    log::info!("refused: {}", refusal.message());
    #[cfg(test)]
    cx.default_global::<tests::RefusalLog>().0.push(refusal);
    // Not now: this may run while the workspace is being updated.
    let workspace = Workspace::for_window(window, cx).map(|workspace| workspace.downgrade());
    cx.defer(move |cx| {
        let toast = Toast::new(
            NotificationId::unique::<RefusedCommand>(),
            refusal.message(),
        );
        if let Some(workspace) = workspace {
            workspace
                .update(cx, |workspace, cx| workspace.show_toast(toast, cx))
                .ok();
        }
    });
}

/// Captures the refused actions in `editor`, before vim's handlers on it, which act only as
/// actions bubble, can run them; in the diff, vim's actions that start typing too. Its
/// `editor::OpenUrl`, vim's `g x`, opens only web pages.
fn guard_editor(editor: &mut Editor, cx: &mut Context<Editor>) {
    let this = cx.entity().downgrade();
    editor
        .register_action_renderer(move |editor, window, cx| {
            let refused = cx.default_global::<Refused>();
            let everywhere: Vec<(TypeId, Refusal)> =
                refused.everywhere.iter().map(|(t, r)| (*t, *r)).collect();
            let in_diff: Vec<TypeId> = match keys::is_diff(editor) {
                true => refused.in_diff.iter().copied().collect(),
                false => Vec::new(),
            };
            for (type_id, refusal) in everywhere {
                window.on_action(type_id, move |_, phase, window, cx| {
                    if phase == DispatchPhase::Capture {
                        refuse(refusal, window, cx);
                        cx.stop_propagation();
                    }
                });
            }
            for type_id in in_diff {
                window.on_action(type_id, |_, phase, _, cx| {
                    if phase == DispatchPhase::Capture {
                        cx.stop_propagation();
                    }
                });
            }
            let this = this.clone();
            window.on_action(
                TypeId::of::<editor::actions::OpenUrl>(),
                move |_, phase, window, cx| {
                    if phase == DispatchPhase::Capture {
                        open_link(&this, window, cx);
                        cx.stop_propagation();
                    }
                },
            );
        })
        .detach();
}

/// `g x`: opens the link at `editor`'s cursor if it's a web page.
fn open_link(editor: &WeakEntity<Editor>, window: &mut Window, cx: &mut App) {
    let Some(link) = editor.read_with(cx, links::link_at_cursor).ok().flatten() else {
        return;
    };
    if !links::open_web_link(&link, cx) {
        refuse(Refusal::Link, window, cx);
    }
}

/// Puts Lo's interceptor over vim's, which vim sets when vim turns on. Call it each time it does.
pub(super) fn wrap_vims_interceptor(cx: &mut App) {
    let Some(vims) = cx.try_global::<GlobalCommandPaletteInterceptor>().cloned() else {
        return;
    };
    for probe in PROBES {
        let intercepted = intercept_with(&vims, probe, cx);
        cx.spawn(async move |cx| {
            let result = intercepted.await;
            cx.update(|cx| {
                for item in &result.results {
                    learn(item.action.as_ref(), cx);
                }
            })
        })
        .detach();
    }
    #[cfg(test)]
    cx.set_global(tests::VimsInterceptor(vims.clone()));
    // The palette awaits what this returns on a background thread, and on enter blocks the main
    // thread a moment for it, so it never waits on the main thread itself.
    GlobalCommandPaletteInterceptor::set(cx, move |query, _, cx| {
        let mut intercepted = intercept_with(&vims, query, cx);
        if let Some(mut result) = (&mut intercepted).now_or_never() {
            for item in &result.results {
                learn(item.action.as_ref(), cx);
            }
            refuse_results(&mut result);
            return Task::ready(result);
        }
        cx.background_spawn(async move {
            let mut result = intercepted.await;
            refuse_results(&mut result);
            result
        })
    });
}

/// What `interceptor` makes of `query`, without a workspace, so it lists no folders on disk.
/// `GlobalCommandPaletteInterceptor` runs only the global one, so `interceptor` is the global
/// while it runs.
fn intercept_with(
    interceptor: &GlobalCommandPaletteInterceptor,
    query: &str,
    cx: &mut App,
) -> Task<CommandInterceptResult> {
    let ours = cx.remove_global::<GlobalCommandPaletteInterceptor>();
    cx.set_global(interceptor.clone());
    let intercepted =
        GlobalCommandPaletteInterceptor::intercept(query, WeakEntity::new_invalid(), cx);
    cx.set_global(ours);
    intercepted.unwrap_or_else(|| Task::ready(CommandInterceptResult::default()))
}

/// Swaps each command in `result` that Lo refuses for [`RefusedCommand`].
fn refuse_results(result: &mut CommandInterceptResult) {
    for item in &mut result.results {
        if let Some(refusal) = refusal(item.action.as_ref()) {
            item.action = Box::new(RefusedCommand { refusal });
        }
    }
}

/// A workspace's terminals and tasks, which Lo never runs: vim's `:!` asks for one.
pub(crate) struct NoTerminals;

impl workspace::TerminalProvider for NoTerminals {
    fn spawn(
        &self,
        _: task::SpawnInTerminal,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<Option<anyhow::Result<ExitStatus>>> {
        refuse(Refusal::Shell, window, cx);
        Task::ready(None)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The refusals, for tests.
    #[derive(Default)]
    pub(crate) struct RefusalLog(pub(crate) Vec<Refusal>);

    impl Global for RefusalLog {}

    /// Vim's own interceptor, under Lo's.
    pub(crate) struct VimsInterceptor(pub(crate) GlobalCommandPaletteInterceptor);

    impl Global for VimsInterceptor {}

    /// The actions vim's own interceptor makes of `query`, as it does without Lo's.
    pub(crate) fn vims_actions(query: &str, cx: &mut App) -> Vec<Box<dyn Action>> {
        let vims = cx.global::<VimsInterceptor>().0.clone();
        let result = intercept_with(&vims, query, cx)
            .now_or_never()
            .unwrap_or_default();
        result.results.into_iter().map(|item| item.action).collect()
    }

    /// Turns every refusal off, as if Lo had none.
    pub(crate) fn refuse_nothing(cx: &mut App) {
        let vims = cx.global::<VimsInterceptor>().0.clone();
        cx.set_global(vims);
        cx.set_global(Refused::default());
    }
}
