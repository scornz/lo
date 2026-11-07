//! What Lo refuses vim: a shell, files on disk, links that aren't web pages, and typing in the
//! diff (`crate::vim_mode::refusals`).

use super::*;

use std::any::TypeId;
use std::cell::Cell;
use std::rc::Rc;

use gpui::DispatchPhase;

use crate::test_support::workspace_with_palette;
use crate::vim_mode::refusals::Refusal;
use crate::vim_mode::refusals::tests::{RefusalLog, refuse_nothing, vims_actions};

/// A tab on fd#2212 showing `first_line` atop a file, with the command palette, its diff focused.
async fn diff_with_palette<'a>(
    first_line: &str,
    cx: &'a mut TestAppContext,
) -> (
    Entity<Workspace>,
    Entity<PrTab>,
    Entity<SplittableEditor>,
    &'a mut VisualTestContext,
) {
    let (workspace, cx) = workspace_with_palette(cx).await;
    let tab = open_unloaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);
    let new = format!("{first_line}\n{}", lines(5, &[2]));
    let diff = show_test_diff(&tab, &workspace, &[("run.sh", lines(5, &[]), new)], cx).await;
    tab.update_in(cx, |tab, window, cx| tab.focus_diff(window, cx));
    cx.run_until_parked();
    (workspace, tab, diff, cx)
}

/// Types `command` in the command palette that vim's `:` opens, and runs it.
fn run_command(command: &str, cx: &mut VisualTestContext) {
    cx.simulate_keystrokes(":");
    cx.run_until_parked();
    cx.simulate_input(command);
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
}

fn refusals(cx: &mut VisualTestContext) -> Vec<Refusal> {
    cx.update(|_, cx| std::mem::take(&mut cx.default_global::<RefusalLog>().0))
}

fn vim_mode(cx: &mut VisualTestContext) -> Option<String> {
    cx.update(|window, _| {
        window
            .context_stack()
            .pop()
            .and_then(|context| context.get("vim_mode").map(|mode| mode.to_string()))
    })
}

fn items(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> usize {
    workspace.read_with(cx, |workspace, cx| {
        workspace.active_pane().read(cx).items_len()
    })
}

/// Counts the actions of type `type_id` that get past Lo's guard to `diff`'s right-hand editor,
/// where vim's handler would run them: a listener registered on the editor after Lo's, which
/// captures them before vim's handler does. While `stop` is set, it stops them there, so a test
/// can show they'd get there without running them.
fn count_reaching(
    diff: &Entity<SplittableEditor>,
    type_id: TypeId,
    stop: Rc<Cell<bool>>,
    cx: &mut VisualTestContext,
) -> Rc<Cell<usize>> {
    let count = Rc::new(Cell::new(0));
    let rhs = diff.read_with(cx, |diff, _| diff.rhs_editor().clone());
    let counted = count.clone();
    rhs.update(cx, |editor, _| {
        editor
            .register_action_renderer(move |_, window, _| {
                let (counted, stop) = (counted.clone(), stop.clone());
                window.on_action(type_id, move |_, phase, _, cx| {
                    if phase == DispatchPhase::Capture {
                        counted.set(counted.get() + 1);
                        if stop.get() {
                            cx.stop_propagation();
                        }
                    }
                });
            })
            .detach()
    });
    count
}

/// Waits a moment for a process that may have started, then says whether `path` exists.
fn created(path: &Path) -> bool {
    for _ in 0..20 {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    false
}

#[gpui::test]
async fn vim_runs_no_shell_command(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("ran");
    let touch = format!("touch {}", marker.display());
    let (workspace, _, diff, cx) = diff_with_palette(&touch, cx).await;
    let shell_exec = cx.update(|_, cx| vims_actions("!x", cx).remove(0));
    assert_eq!(shell_exec.name(), "vim::ShellExec");
    let reaching = count_reaching(&diff, shell_exec.as_any().type_id(), Rc::default(), cx);

    // The diff's first line through a shell: `!` isn't bound, so `s` and `h` are vim's motions.
    cx.simulate_keystrokes("! ! s h enter");
    assert_eq!(vim_mode(cx).as_deref(), Some("normal"));
    assert_eq!(items(&workspace, cx), 1);

    // `:` commands, each refused with a toast. Zed's vim takes `:w !cmd` for a file named
    // `!cmd`, and `:g` runs a command on each matching line.
    refusals(cx);
    for (command, refusal) in [
        (format!("!{touch}"), Refusal::Shell),
        ("!echo hi".into(), Refusal::Shell),
        (format!("r !{touch}"), Refusal::Shell),
        ("r !date".into(), Refusal::Shell),
        (".!sh".into(), Refusal::Shell),
        (format!("w !{touch}"), Refusal::Files),
        (format!("g/touch/!{touch}"), Refusal::Commands),
    ] {
        run_command(&command, cx);
        assert_eq!(refusals(cx), [refusal], ":{command}");
    }

    // Vim's own actions for them, dispatched in the diff as nothing in Lo does, stop at the editor.
    for query in [format!("!{touch}"), format!("r !{touch}"), ".!sh".into()] {
        let action = cx.update(|_, cx| vims_actions(&query, cx).remove(0));
        assert_eq!(action.name(), "vim::ShellExec");
        cx.update(|window, cx| window.dispatch_action(action, cx));
        cx.run_until_parked();
        assert_eq!(refusals(cx), [Refusal::Shell], "{query}");
    }

    assert_eq!(reaching.get(), 0, "a shell command reached vim");
    assert!(!created(&marker), "a shell ran {touch}");
}

/// Without Lo's refusals, the same commands reach vim's handler, so the test above would see
/// them. They stop just before it, as a shell running in a test isn't deterministic.
#[gpui::test]
async fn without_los_refusals_vim_would_run_a_shell(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("ran");
    let touch = format!("touch {}", marker.display());
    let (_, _, diff, cx) = diff_with_palette(&touch, cx).await;
    let shell_exec = cx.update(|_, cx| vims_actions("!x", cx).remove(0));
    let stop = Rc::new(Cell::new(true));
    let reaching = count_reaching(&diff, shell_exec.as_any().type_id(), stop.clone(), cx);
    cx.update(|_, cx| refuse_nothing(cx));
    refusals(cx);
    for command in [format!("r !{touch}"), ".!sh".into()] {
        run_command(&command, cx);
    }
    assert_eq!(reaching.get(), 2);
    // `:!` without a range asks the workspace for a terminal, which Lo never runs.
    stop.set(false);
    run_command(&format!("!{touch}"), cx);
    assert_eq!(reaching.get(), 3);
    assert_eq!(refusals(cx), [Refusal::Shell]);
    assert!(!created(&marker));
}

#[gpui::test]
async fn vim_opens_reads_and_writes_no_files(cx: &mut TestAppContext) {
    let (workspace, _, _, cx) = diff_with_palette("line 0", cx).await;
    for (command, refusal) in [
        ("w", Refusal::Files),
        ("w!", Refusal::Files),
        ("w out.txt", Refusal::Files),
        ("1,2w out.txt", Refusal::Files),
        ("wq", Refusal::Files),
        ("x", Refusal::Files),
        ("exi", Refusal::Files),
        ("up", Refusal::Files),
        ("wa", Refusal::Files),
        ("xa", Refusal::Files),
        ("wqa", Refusal::Files),
        ("e", Refusal::Files),
        ("e /etc/hosts", Refusal::Files),
        ("ex", Refusal::Files),
        ("tabe /etc/hosts", Refusal::Files),
        ("tabnew", Refusal::Files),
        ("new", Refusal::Files),
        ("vne", Refusal::Files),
        ("sp /etc/hosts", Refusal::Files),
        ("vs /etc/hosts", Refusal::Files),
        ("r", Refusal::Files),
        ("r /etc/hosts", Refusal::Files),
        ("rev", Refusal::Files),
        ("norm x", Refusal::Commands),
        ("g/line/w out.txt", Refusal::Commands),
        ("v/line/d", Refusal::Commands),
    ] {
        run_command(command, cx);
        assert_eq!(refusals(cx), [refusal], ":{command}");
        assert_eq!(
            items(&workspace, cx),
            1,
            ":{command} opened or closed something"
        );
    }
    // `ZZ` isn't bound.
    cx.simulate_keystrokes("shift-z shift-z");
    assert_eq!(items(&workspace, cx), 1);
    assert_eq!(refusals(cx), [Refusal::Files; 0]);

    // Vim's commands for Zed's panels, docs and keymap do nothing: Lo has none of them.
    for command in ["Explore", "Vexplore", "Git", "Debug", "AI", "h", "map"] {
        run_command(command, cx);
        assert_eq!(items(&workspace, cx), 1, ":{command}");
        assert_eq!(refusals(cx), [Refusal::Files; 0], ":{command}");
        assert_eq!(cx.opened_url(), None, ":{command}");
    }
    // And those for its terminal and other panels don't exist in Lo's build.
    for command in ["term", "Term", "Collab", "clist", "ls", "buffers"] {
        let actions = cx.update(|_, cx| vims_actions(command, cx));
        assert!(actions.is_empty(), ":{command} gives {actions:?}");
    }
}

#[gpui::test]
async fn g_x_opens_only_web_links(cx: &mut TestAppContext) {
    let (_, _, diff, cx) = diff_with_palette(
        "see https://example.com/a and file:///Applications/Calculator.app",
        cx,
    )
    .await;
    let rhs = diff.read_with(cx, |diff, _| diff.rhs_editor().clone());
    let put_cursor = |column: u32, cx: &mut VisualTestContext| {
        rhs.update_in(cx, |editor, window, cx| {
            editor.change_selections(Default::default(), window, cx, |selections| {
                selections.select_ranges([Point::new(0, column)..Point::new(0, column)])
            })
        });
    };
    put_cursor(10, cx);
    cx.simulate_keystrokes("g x");
    assert_eq!(cx.opened_url().as_deref(), Some("https://example.com/a"));
    assert_eq!(refusals(cx), [Refusal::Link; 0]);

    put_cursor(40, cx);
    cx.simulate_keystrokes("g x");
    assert_eq!(cx.opened_url().as_deref(), Some("https://example.com/a"));
    assert_eq!(refusals(cx), [Refusal::Link]);
}

#[gpui::test]
async fn the_diff_never_starts_typing_but_a_text_box_does(cx: &mut TestAppContext) {
    let (_, tab, _, cx) = diff_with_palette("line 0", cx).await;
    for keys in [
        "i", "a", "o", "shift-o", "shift-i", "shift-a", "s", "shift-s", "shift-c", "shift-r",
        "c w", "c c", "g i", "insert",
    ] {
        cx.simulate_keystrokes(keys);
        assert_eq!(vim_mode(cx).as_deref(), Some("normal"), "{keys}");
        cx.simulate_keystrokes("escape");
    }
    for keys in [
        "c",
        "s",
        "shift-i",
        "shift-a",
        "shift-r",
        "shift-s",
        "g shift-i",
        "g shift-a",
    ] {
        cx.simulate_keystrokes("v");
        cx.simulate_keystrokes(keys);
        assert_ne!(vim_mode(cx).as_deref(), Some("insert"), "v {keys}");
        assert_ne!(vim_mode(cx).as_deref(), Some("replace"), "v {keys}");
        cx.simulate_keystrokes("escape escape");
    }

    // A text box made for typing, such as a comment box, in the diff's place.
    let text_box = cx.update(|window, cx| {
        cx.new(|cx| crate::vim_mode::modal(Editor::auto_height(1, 10, window, cx)))
    });
    tab.update_in(cx, |tab, window, cx| {
        tab.show_middle(Middle::View(PartView::new(&text_box, cx)), window, cx)
    });
    cx.run_until_parked();
    assert_eq!(vim_mode(cx).as_deref(), Some("normal"));
    cx.simulate_keystrokes("i");
    assert_eq!(vim_mode(cx).as_deref(), Some("insert"));
    cx.simulate_input("typed");
    assert_eq!(
        text_box.read_with(cx, |editor, cx| editor.text(cx)),
        "typed"
    );
}

#[gpui::test]
async fn vims_shell_keys_hide_from_the_palette(cx: &mut TestAppContext) {
    let (_, _, _, cx) = diff_with_palette("line 0", cx).await;
    cx.update(|_, cx| {
        let filter = command_palette_hooks::CommandPaletteFilter::try_global(cx).unwrap();
        for name in ["vim::PushShellCommand", "vim::ShellCommand"] {
            let action = cx.build_action(name, None).unwrap();
            assert!(filter.is_hidden(action.as_ref()), "{name}");
        }
    });
}
