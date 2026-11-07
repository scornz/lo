//! Finds Lo's key bindings that fight. Where keys are typed, GPUI runs the binding whose context
//! holds deepest in the focused element's context stack, and of two as deep, the one bound later.
//! A binding whose keystrokes start another's waits to see whether the rest follow. So wherever
//! keys are typed in Lo's window, no two of Lo's bindings may both hold with the same keystrokes,
//! or with one's keystrokes starting the other's. Keystrokes are the same when one key press
//! types both, as GPUI matches them on a US layout: `}` is `shift-]`, and `ø` is `alt-o`.
//!
//! Tests in `review_ui` and `lo` read the context stack of each place their windows have, check
//! that every binding holds in one of them ([`unplaced`]), then check the places ([`clashes`]).
//!
//! Vim's keymap is Zed's, not Lo's, so its bindings may fight each other. Where one of vim's and
//! one of Lo's fight, each crate lists which wins, and [`with_vim`] checks the lists.

use std::collections::HashSet;

use gpui::{App, KeyBinding, KeyContext, Keystroke, Modifiers, OperatingSystem};

/// Where keys are typed in Lo's window: the context stack from the window's root to the focused
/// element.
pub struct Place {
    /// What the place is, for the clash's message.
    pub name: String,
    pub stack: Vec<KeyContext>,
}

/// Each pair of `bindings` that both hold somewhere in `places`, with the same keystrokes or with
/// one's starting the other's: a sentence naming the place, then each binding's keystrokes,
/// action, context and the depth it holds at.
pub fn clashes<'a>(
    bindings: impl IntoIterator<Item = &'a KeyBinding>,
    places: &[Place],
) -> Vec<String> {
    let bindings: Vec<&KeyBinding> = bindings.into_iter().collect();
    let mut clashes = Vec::new();
    for place in places {
        let holding: Vec<(&KeyBinding, usize)> = bindings
            .iter()
            .filter_map(|binding| Some((*binding, depth(binding, &place.stack)?)))
            .collect();
        for (index, (first, first_depth)) in holding.iter().enumerate() {
            for (second, second_depth) in &holding[index + 1..] {
                if starts(first, second) || starts(second, first) {
                    clashes.push(format!(
                        "In {}: {} at depth {first_depth}, and {} at depth {second_depth}",
                        place.name,
                        describe(first),
                        describe(second),
                    ));
                }
            }
        }
    }
    clashes
}

/// Each of `bindings` that holds in none of `places`, so [`clashes`] never compares it: a sentence
/// naming its keystrokes, action and context, to add a place where it holds.
pub fn unplaced<'a>(
    bindings: impl IntoIterator<Item = &'a KeyBinding>,
    places: &[Place],
) -> Vec<String> {
    bindings
        .into_iter()
        .filter(|binding| {
            places
                .iter()
                .all(|place| depth(binding, &place.stack).is_none())
        })
        .map(|binding| format!("No place has {}", describe(binding)))
        .collect()
}

/// Vim's bindings, as Lo binds them when vim is on: before Lo's own.
pub fn vim_bindings(cx: &App) -> Vec<KeyBinding> {
    crate::keymap::vim_bindings(cx)
}

/// Each pair of one of `lo`'s bindings and one of `vim`'s that both hold somewhere in `places`
/// with different actions, and the same keystrokes or one's starting the other's, unless the pair
/// is listed. Lo's bindings are bound after vim's, as Lo binds them, so Lo's wins unless vim's is
/// deeper with the same keystrokes. A shorter binding of Lo's runs at once, hiding vim's longer
/// one, and a longer one of Lo's makes vim's shorter one wait for more keys, so Lo's wins those
/// too.
///
/// `over` lists Lo's bindings that may win, by keystrokes and Lo's action; `under` lists vim's
/// that may win, by keystrokes and vim's action. Each entry must win somewhere in `places`, so a
/// list never names a fight that's gone. Vim's `null` bindings never hide Lo's, which bind
/// without a source, as a user's do.
pub fn with_vim(
    lo: &[KeyBinding],
    vim: &[KeyBinding],
    places: &[Place],
    over: &[(&'static str, &str)],
    under: &[(&'static str, &str)],
) -> Vec<String> {
    let entry = |binding: &KeyBinding| (presses(binding.keystrokes()), binding.action().name());
    let listed = |list: &[(&'static str, &str)]| -> Vec<(Vec<(Modifiers, String)>, String)> {
        list.iter()
            .map(|(keys, action)| {
                let keystrokes: Vec<Keystroke> = keys
                    .split(' ')
                    .map(|key| Keystroke::parse(key).expect("a keystroke"))
                    .collect();
                (keystrokes.iter().map(press).collect(), action.to_string())
            })
            .collect()
    };
    let (over, under) = (listed(over), listed(under));
    let vim: Vec<&KeyBinding> = vim
        .iter()
        .filter(|binding| {
            let action = binding.action();
            !gpui::is_no_action(action) && !gpui::is_unbind(action)
        })
        .collect();
    let mut used = HashSet::new();
    let mut fights = Vec::new();
    for place in places {
        let los = holding(lo.iter(), place);
        let vims = holding(vim.iter().copied(), place);
        for (lo, lo_depth) in &los {
            for (vim, vim_depth) in &vims {
                if !(starts(lo, vim) || starts(vim, lo)) || lo.action().partial_eq(vim.action()) {
                    continue;
                }
                let same_keys = lo.keystrokes().len() == vim.keystrokes().len();
                let vim_wins = same_keys && vim_depth > lo_depth;
                let (winner, list, whose) = if vim_wins {
                    (vim, &under, "UNDER_VIM")
                } else {
                    (lo, &over, "OVER_VIM")
                };
                let (keys, action) = entry(winner);
                match list
                    .iter()
                    .position(|(listed, name)| *listed == keys && name == action)
                {
                    Some(index) => {
                        used.insert((whose, index));
                    }
                    None => {
                        let (winner, loser) = match vim_wins {
                            true => (
                                format!("vim's {}", describe(vim)),
                                format!("Lo's {}", describe(lo)),
                            ),
                            false => (
                                format!("Lo's {}", describe(lo)),
                                format!("vim's {}", describe(vim)),
                            ),
                        };
                        fights.push(format!(
                            "In {}: {winner} wins over {loser} (depths {lo_depth} for Lo's, \
                             {vim_depth} for vim's), and {whose} doesn't list it",
                            place.name
                        ))
                    }
                }
            }
        }
    }
    for (whose, list) in [("OVER_VIM", &over), ("UNDER_VIM", &under)] {
        for (index, (_, action)) in list.iter().enumerate() {
            if !used.contains(&(whose, index)) {
                fights.push(format!(
                    "{whose} lists {action} at its keys, but it wins over nothing in any place"
                ));
            }
        }
    }
    fights
}

/// Each of `bindings` that holds in `place`, and how deep.
fn holding<'a>(
    bindings: impl Iterator<Item = &'a KeyBinding>,
    place: &Place,
) -> Vec<(&'a KeyBinding, usize)> {
    bindings
        .filter_map(|binding| Some((binding, depth(binding, &place.stack)?)))
        .collect()
}

/// The key and modifiers that press each of `keystrokes`.
fn presses(keystrokes: &[gpui::KeybindingKeystroke]) -> Vec<(Modifiers, String)> {
    keystrokes
        .iter()
        .map(|keystroke| press(keystroke.inner()))
        .collect()
}

/// How deep in `stack` `binding`'s context holds, if it does. A binding with no context holds
/// everywhere, deepest of all, as GPUI ranks it.
fn depth(binding: &KeyBinding, stack: &[KeyContext]) -> Option<usize> {
    match binding.predicate() {
        Some(predicate) => predicate.depth_of(stack),
        None => Some(stack.len()),
    }
}

/// Whether `second`'s keystrokes start with all of `first`'s, or are the same.
fn starts(first: &KeyBinding, second: &KeyBinding) -> bool {
    let (first, second) = (first.keystrokes(), second.keystrokes());
    first.len() <= second.len()
        && first
            .iter()
            .zip(second)
            .all(|(first, second)| press(first.inner()) == press(second.inner()))
}

/// Characters a US layout types with shift or option, by the key and modifier that type them.
const TYPED_WITH: &[(&str, &str, bool)] = &[
    // Shift, then the key.
    ("~", "`", false),
    ("!", "1", false),
    ("@", "2", false),
    ("#", "3", false),
    ("$", "4", false),
    ("%", "5", false),
    ("^", "6", false),
    ("&", "7", false),
    ("*", "8", false),
    ("(", "9", false),
    (")", "0", false),
    ("_", "-", false),
    ("+", "=", false),
    ("{", "[", false),
    ("}", "]", false),
    ("|", "\\", false),
    (":", ";", false),
    ("\"", "'", false),
    ("<", ",", false),
    (">", ".", false),
    ("?", "/", false),
    // Option, then the key.
    ("å", "a", true),
    ("∫", "b", true),
    ("ç", "c", true),
    ("∂", "d", true),
    ("ƒ", "f", true),
    ("©", "g", true),
    ("˙", "h", true),
    ("∆", "j", true),
    ("˚", "k", true),
    ("¬", "l", true),
    ("µ", "m", true),
    ("ø", "o", true),
    ("π", "p", true),
    ("œ", "q", true),
    ("®", "r", true),
    ("ß", "s", true),
    ("†", "t", true),
    ("√", "v", true),
    ("∑", "w", true),
    ("≈", "x", true),
    ("¥", "y", true),
    ("Ω", "z", true),
];

/// The key and modifiers that press `keystroke` on a US layout: a character typed with shift or
/// option is its key with that modifier.
fn press(keystroke: &Keystroke) -> (Modifiers, String) {
    let mut modifiers = keystroke.modifiers;
    match TYPED_WITH
        .iter()
        .find(|(typed, _, _)| *typed == keystroke.key)
    {
        Some((_, key, alt)) => {
            match alt {
                true => modifiers.alt = true,
                false => modifiers.shift = true,
            }
            (modifiers, key.to_string())
        }
        None => (modifiers, keystroke.key.clone()),
    }
}

/// `binding`'s keystrokes, action and context, as a message names them.
fn describe(binding: &KeyBinding) -> String {
    let keystrokes: Vec<String> = binding
        .keystrokes()
        .iter()
        .map(|keystroke| keystroke.inner().unparse(OperatingSystem::Mac))
        .collect();
    let context = binding
        .predicate()
        .map_or_else(|| "anywhere".to_string(), |predicate| predicate.to_string());
    format!(
        "`{}` for {} in `{context}`",
        keystrokes.join(" "),
        binding.action().name()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::pr_tab::keys::{DIFF_CONTEXT, KEY_CONTEXT, READING_CONTEXT};

    gpui::actions!(key_clashes_test, [First, Second, Third]);

    /// The diff and the file list, as the tab has them.
    fn places() -> Vec<Place> {
        let stack = |contexts: &[&str]| {
            contexts
                .iter()
                .map(|context| KeyContext::parse(context).unwrap())
                .collect()
        };
        vec![
            Place {
                name: "the diff".into(),
                stack: stack(&[
                    "Workspace",
                    "Pane",
                    "PrTab middle=diff",
                    "Editor mode=full multibuffer PrDiff",
                ]),
            },
            Place {
                name: "the file list".into(),
                stack: stack(&["Workspace", "Pane", "PrTab middle=diff", "PrFileList"]),
            },
        ]
    }

    #[test]
    fn a_binding_that_holds_in_no_place_is_unplaced() {
        let tree = KeyBinding::new("enter", First, Some("PrStackTree"));
        let diff = KeyBinding::new("e", Second, Some(DIFF_CONTEXT));
        assert_eq!(
            unplaced([&tree, &diff], &places()),
            ["No place has `enter` for key_clashes_test::First in `PrStackTree`"]
        );
    }

    #[test]
    fn bindings_that_hold_together_clash_on_the_same_keys_or_a_prefix() {
        let clash = |first: KeyBinding, second: KeyBinding| clashes([&first, &second], &places());
        // Different contexts that hold at the same depth: the one bound later wins.
        let tie = clash(
            KeyBinding::new("e", First, Some(KEY_CONTEXT)),
            KeyBinding::new("e", Second, Some(READING_CONTEXT)),
        );
        assert_eq!(tie.len(), 2, "{tie:#?}");
        assert!(tie[0].starts_with("In the diff: `e` for key_clashes_test::First in `PrTab`"));
        // A deeper binding hides a shallower one with the same keys.
        assert_eq!(
            clash(
                KeyBinding::new("alt-a", First, Some(READING_CONTEXT)),
                KeyBinding::new("alt-a", Second, Some(DIFF_CONTEXT)),
            )
            .len(),
            1
        );
        // `g` waits to see whether `f` follows.
        assert_eq!(
            clash(
                KeyBinding::new("g", First, Some(READING_CONTEXT)),
                KeyBinding::new("g f", Second, Some(READING_CONTEXT)),
            )
            .len(),
            2
        );
        // One key press types both.
        assert_eq!(
            clash(
                KeyBinding::new("}", First, Some(READING_CONTEXT)),
                KeyBinding::new("shift-]", Second, Some(READING_CONTEXT)),
            )
            .len(),
            2
        );
        assert_eq!(
            clash(
                KeyBinding::new("ø", First, Some(READING_CONTEXT)),
                KeyBinding::new("alt-o", Second, Some(READING_CONTEXT)),
            )
            .len(),
            2
        );
        // Bindings that never hold together, and keys that differ, don't clash.
        assert!(
            clash(
                KeyBinding::new("alt-v", First, Some(DIFF_CONTEXT)),
                KeyBinding::new("alt-v", Second, Some("PrFileList")),
            )
            .is_empty()
        );
        assert!(
            clash(
                KeyBinding::new("] f", First, Some(READING_CONTEXT)),
                KeyBinding::new("] c", Second, Some(READING_CONTEXT)),
            )
            .is_empty()
        );
    }

    #[test]
    fn lo_and_vim_fight_where_both_hold_unless_a_list_says_who_wins() {
        let fights = |lo: &[KeyBinding],
                      vim: &[KeyBinding],
                      over: &[(&'static str, &str)],
                      under: &[(&'static str, &str)]| {
            with_vim(lo, vim, &places(), over, under)
        };
        let vims_e = [KeyBinding::new("e", Second, Some("Editor"))];
        // As deep as vim's, Lo's wins, bound later.
        let los_e = [KeyBinding::new("e", First, Some("Editor"))];
        let unlisted = fights(&los_e, &vims_e, &[], &[]);
        assert_eq!(
            unlisted,
            [
                "In the diff: Lo's `e` for key_clashes_test::First in `Editor` wins over vim's `e` \
                 for key_clashes_test::Second in `Editor` (depths 4 for Lo's, 4 for vim's), and \
                 OVER_VIM doesn't list it"
            ]
        );
        let over = [("e", "key_clashes_test::First")];
        assert!(fights(&los_e, &vims_e, &over, &[]).is_empty());
        // Shallower, vim's wins.
        let tabs_e = [KeyBinding::new("e", First, Some("PrTab"))];
        let vim_wins = fights(&tabs_e, &vims_e, &over, &[]);
        assert_eq!(vim_wins.len(), 2, "{vim_wins:#?}");
        assert!(vim_wins[0].contains("vim's `e`"), "{vim_wins:#?}");
        assert!(vim_wins[1].starts_with("OVER_VIM lists"), "{vim_wins:#?}");
        let under = [("e", "key_clashes_test::Second")];
        assert!(fights(&tabs_e, &vims_e, &[], &under).is_empty());
        // Lo's shorter one runs at once, and its longer one makes vim's wait: Lo's wins either way.
        for (lo, vim) in [("g", "g g"), ("g g", "g")] {
            let lo = KeyBinding::new(lo, First, Some("PrTab"));
            let vim = KeyBinding::new(vim, Second, Some("Editor"));
            assert_eq!(fights(&[lo], &[vim], &[], &[]).len(), 1);
        }
        // The same action, vim's `null`, or keys that differ don't fight.
        let quiet = [
            KeyBinding::new("e", First, Some("Editor")),
            KeyBinding::new("e", gpui::NoAction, Some("Editor")),
            KeyBinding::new("w", Third, Some("Editor")),
        ];
        let lo = KeyBinding::new("e", First, Some("PrTab"));
        assert!(fights(&[lo], &quiet, &[], &[]).is_empty());
    }
}
