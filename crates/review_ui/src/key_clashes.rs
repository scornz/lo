//! Finds Lo's key bindings that fight. Where keys are typed, GPUI runs the binding whose context
//! holds deepest in the focused element's context stack, and of two as deep, the one bound later.
//! A binding whose keystrokes start another's waits to see whether the rest follow. So wherever
//! keys are typed in Lo's window, no two of Lo's bindings may both hold with the same keystrokes,
//! or with one's keystrokes starting the other's. Keystrokes are the same when one key press
//! types both, as GPUI matches them on a US layout: `}` is `shift-]`, and `ø` is `alt-o`.
//!
//! Tests in `review_ui` and `lo` read the context stack of each place their windows have, check
//! that every binding holds in one of them ([`unplaced`]), then check the places ([`clashes`]).

use gpui::{KeyBinding, KeyContext, Keystroke, Modifiers, OperatingSystem};

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

    gpui::actions!(key_clashes_test, [First, Second]);

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
                    "Editor mode=full multibuffer",
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
}
