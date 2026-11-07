//! Vim's on by default: in the diff, Lo's keys reach Lo and vim's other keys reach vim.

use super::*;

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{Subscription, UpdateGlobal as _};

/// The name of the action each keystroke ran, or `None` for one that ran none, such as a
/// character vim waits for.
struct Ran {
    actions: Rc<RefCell<Vec<Option<String>>>>,
    _observing: Subscription,
}

impl Ran {
    fn record(cx: &mut VisualTestContext) -> Self {
        let actions: Rc<RefCell<Vec<Option<String>>>> = Rc::default();
        let _observing = cx.update(|_, cx| {
            let actions = actions.clone();
            cx.observe_keystrokes(move |event, _, _| {
                let action = event
                    .action
                    .as_ref()
                    .map(|action| action.name().to_string());
                actions.borrow_mut().push(action);
            })
        });
        Self {
            actions,
            _observing,
        }
    }

    /// What ran since the last call.
    fn take(&self) -> Vec<Option<String>> {
        std::mem::take(&mut self.actions.borrow_mut())
    }

    /// Types `keys` and returns the action the last keystroke ran.
    fn typing(&self, keys: &str, cx: &mut VisualTestContext) -> Option<String> {
        self.take();
        cx.simulate_keystrokes(keys);
        self.take().pop().flatten()
    }
}

fn pending(cx: &mut VisualTestContext) -> bool {
    cx.update(|window, _| window.has_pending_keystrokes())
}

/// The focused element's own context.
fn focused_context(cx: &mut VisualTestContext) -> gpui::KeyContext {
    cx.update(|window, _| window.context_stack().pop().unwrap_or_default())
}

fn vim_mode(cx: &mut VisualTestContext) -> Option<String> {
    focused_context(cx)
        .get("vim_mode")
        .map(|mode| mode.to_string())
}

/// A tab on fd#2212 with three files in its diff side by side, the diff focused.
async fn diff_with_vim(
    cx: &mut TestAppContext,
) -> (
    Entity<PrTab>,
    Entity<SplittableEditor>,
    &mut VisualTestContext,
) {
    let (workspace, cx) = workspace(cx).await;
    let tab = open_unloaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);
    let diff = show_test_diff(
        &tab,
        &workspace,
        &[
            ("src/a.rs", lines(30, &[]), lines(30, &[2, 25])),
            ("src/b.rs", lines(5, &[]), lines(5, &[3])),
            ("src/c.rs", lines(5, &[]), lines(5, &[4])),
        ],
        cx,
    )
    .await;
    tab.update_in(cx, |tab, window, cx| tab.focus_diff(window, cx));
    cx.run_until_parked();
    (tab, diff, cx)
}

/// The cursor's row and column in the diff's right side.
fn head(diff: &Entity<SplittableEditor>, cx: &mut VisualTestContext) -> (u32, u32) {
    let rhs = diff.read_with(cx, |diff, _| diff.rhs_editor().clone());
    rhs.update(cx, |editor, cx| {
        let head = editor
            .selections
            .newest::<language::Point>(&editor.display_snapshot(cx))
            .head();
        (head.row, head.column)
    })
}

#[gpui::test]
async fn los_keys_reach_lo_in_the_diff_with_vim_on(cx: &mut TestAppContext) {
    let (tab, diff, cx) = diff_with_vim(cx).await;
    let context = focused_context(cx);
    assert!(context.contains("PrDiff"), "{context:?}");
    assert_eq!(vim_mode(cx).as_deref(), Some("normal"));
    let ran = Ran::record(cx);

    // Each runs Lo's action at once, with no key left waiting.
    let check = |keys: &str, action: &str, cx: &mut VisualTestContext| {
        assert_eq!(ran.typing(keys, cx).as_deref(), Some(action), "{keys}");
        assert!(!pending(cx), "{keys} left keys waiting");
        assert!(
            focused_context(cx).contains("PrDiff"),
            "{keys} left the diff"
        );
    };
    check("] f", "review_ui::NextFile", cx);
    assert_eq!(selected(&tab, cx).as_deref(), Some("src/b.rs"));
    check("[ f", "review_ui::PreviousFile", cx);
    assert_eq!(selected(&tab, cx).as_deref(), Some("src/a.rs"));
    check("] c", "review_ui::NextChange", cx);
    let first = head(&diff, cx);
    check("] c", "review_ui::NextChange", cx);
    let second = head(&diff, cx);
    assert!(second.0 > first.0, "{first:?} then {second:?}");
    check("[ c", "review_ui::PreviousChange", cx);
    assert_eq!(head(&diff, cx), first);
    check("e", "review_ui::ToggleViewed", cx);
    for keys in ["alt-o", "alt-v", "alt-w", "alt-a"] {
        let action = match keys {
            "alt-o" => "review_ui::ToggleFileOrder",
            "alt-v" => "review_ui::ToggleDiffLayout",
            "alt-w" => "review_ui::ToggleWhitespace",
            _ => "review_ui::ExpandFile",
        };
        // Twice, back to how it was.
        check(keys, action, cx);
        check(keys, action, cx);
    }

    // From the left side too, where `tab` goes and comes back from.
    let on_left = |cx: &mut VisualTestContext| {
        cx.update(|window, cx| {
            let lhs = diff.read(cx).lhs_editor().unwrap().focus_handle(cx);
            lhs.is_focused(window)
        })
    };
    check("tab", "review_ui::SwitchSide", cx);
    assert!(on_left(cx));
    assert_eq!(vim_mode(cx).as_deref(), Some("normal"));
    check("] f", "review_ui::NextFile", cx);
    check("e", "review_ui::ToggleViewed", cx);
    check("tab", "review_ui::SwitchSide", cx);
    assert!(!on_left(cx));
}

#[gpui::test]
async fn vims_keys_move_and_select_in_the_diff(cx: &mut TestAppContext) {
    let (_, diff, cx) = diff_with_vim(cx).await;
    let ran = Ran::record(cx);
    let start = head(&diff, cx);
    assert_eq!(start, (0, 0));

    assert_eq!(ran.typing("j", cx).as_deref(), Some("vim::Down"));
    assert_eq!(head(&diff, cx), (1, 0));
    assert_eq!(ran.typing("k", cx).as_deref(), Some("vim::Up"));
    assert_eq!(head(&diff, cx), (0, 0));
    // "line 1": the word after "line".
    assert_eq!(ran.typing("w", cx).as_deref(), Some("vim::NextWordStart"));
    assert_eq!(head(&diff, cx), (0, 5));
    assert_eq!(
        ran.typing("b", cx).as_deref(),
        Some("vim::PreviousWordStart")
    );
    assert_eq!(head(&diff, cx), (0, 0));
    assert_eq!(
        ran.typing("shift-g", cx).as_deref(),
        Some("vim::EndOfDocument")
    );
    let end = head(&diff, cx);
    assert!(end.0 > 10, "{end:?}");
    assert_eq!(
        ran.typing("g g", cx).as_deref(),
        Some("vim::StartOfDocument")
    );
    assert_eq!(head(&diff, cx), (0, 0));

    assert_eq!(ran.typing("v", cx).as_deref(), Some("vim::ToggleVisual"));
    assert_eq!(vim_mode(cx).as_deref(), Some("visual"));
    ran.typing("j", cx);
    let rhs = diff.read_with(cx, |diff, _| diff.rhs_editor().clone());
    let selected = rhs.update(cx, |editor, cx| {
        !editor
            .selections
            .newest::<language::Point>(&editor.display_snapshot(cx))
            .is_empty()
    });
    assert!(selected);
    ran.typing("escape", cx);
    assert_eq!(vim_mode(cx).as_deref(), Some("normal"));
}

#[gpui::test]
async fn vim_keeps_lo_s_keys_while_it_waits_for_the_rest_of_a_command(cx: &mut TestAppContext) {
    let (_, diff, cx) = diff_with_vim(cx).await;
    let ran = Ran::record(cx);
    let lo_ran = |ran: &Ran| {
        ran.take()
            .into_iter()
            .flatten()
            .filter(|action| action.starts_with("review_ui::"))
            .collect::<Vec<_>>()
    };

    // `y e` yanks to the end of the word.
    cx.simulate_keystrokes("y e");
    assert_eq!(
        ran.take(),
        [
            Some("vim::PushYank".into()),
            Some("vim::NextWordEnd".into())
        ]
    );
    // `f e` finds the next "e", in "line".
    cx.simulate_keystrokes("f e");
    assert_eq!(lo_ran(&ran), [""; 0]);
    assert_eq!(head(&diff, cx), (0, 3));
    // `f ]` takes the bracket at once, without waiting for Lo's `] f`.
    ran.typing("0", cx);
    cx.simulate_keystrokes("f ]");
    assert!(!pending(cx));
    assert_eq!(lo_ran(&ran), [""; 0]);
    // A count goes with vim's `e`: the end of the second word.
    cx.simulate_keystrokes("2 e");
    assert_eq!(lo_ran(&ran), [""; 0]);
    assert_eq!(head(&diff, cx), (0, 5));
}

/// GPUI waits a second for the rest of a binding when the keys typed so far could also be text or
/// a binding of their own (`gpui::window::PENDING_INPUT_TIMEOUT`). In the diff, nothing binds `]`
/// alone and vim's normal mode takes no text, so `]` waits for its second key with no timeout, and
/// vim's own `]` sequences, none of which is `] f`, never make Lo's wait.
#[gpui::test]
async fn a_bracket_waits_for_lo_s_second_key(cx: &mut TestAppContext) {
    let (tab, _, cx) = diff_with_vim(cx).await;
    let ran = Ran::record(cx);
    for wait in [
        Duration::ZERO,
        Duration::from_millis(900),
        Duration::from_secs(2),
    ] {
        cx.simulate_keystrokes("]");
        assert!(pending(cx));
        cx.executor().advance_clock(wait);
        cx.run_until_parked();
        assert!(pending(cx), "after {wait:?}");
        assert_eq!(
            ran.typing("f", cx).as_deref(),
            Some("review_ui::NextFile"),
            "after {wait:?}"
        );
        assert!(!pending(cx));
        cx.simulate_keystrokes("[ f");
    }
    assert_eq!(selected(&tab, cx).as_deref(), Some("src/a.rs"));
}

#[gpui::test]
async fn los_keys_reach_lo_in_the_diff_with_vim_off(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx).await;
    cx.update(|_, cx| {
        settings::SettingsStore::update_global(cx, |store, cx| {
            let result = store.set_user_settings(r#"{ "vim_mode": false }"#, cx);
            assert_eq!(result.parse_status, settings::ParseStatus::Success);
        });
        cx.clear_key_bindings();
        crate::keymap::load(cx);
        crate::bind_keys(cx);
    });
    let tab = open_unloaded(&workspace, "https://github.com/sharkdp/fd/pull/2212", cx);
    show_test_diff(
        &tab,
        &workspace,
        &[
            ("src/a.rs", lines(5, &[]), lines(5, &[2])),
            ("src/b.rs", lines(5, &[]), lines(5, &[3])),
        ],
        cx,
    )
    .await;
    tab.update_in(cx, |tab, window, cx| tab.focus_diff(window, cx));
    cx.run_until_parked();
    assert_eq!(vim_mode(cx), None);
    let ran = Ran::record(cx);
    for (keys, action) in [
        ("] f", "review_ui::NextFile"),
        ("[ f", "review_ui::PreviousFile"),
        ("] c", "review_ui::NextChange"),
        ("[ c", "review_ui::PreviousChange"),
        ("e", "review_ui::ToggleViewed"),
        ("tab", "review_ui::SwitchSide"),
        ("tab", "review_ui::SwitchSide"),
        ("alt-o", "review_ui::ToggleFileOrder"),
        ("alt-w", "review_ui::ToggleWhitespace"),
    ] {
        assert_eq!(ran.typing(keys, cx).as_deref(), Some(action), "{keys}");
    }
    // Without vim, `j` isn't bound in the diff.
    assert_eq!(ran.typing("j", cx), None);
}
