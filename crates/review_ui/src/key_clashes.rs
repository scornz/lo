//! Finds Lo's key bindings that fight. Where keys are typed, GPUI runs the binding whose context
//! holds deepest in the focused element's context stack, and of two as deep, the one bound later.
//! A binding whose keystrokes start another's waits to see whether the rest follow. So wherever
//! keys are typed in Lo's window, no two of Lo's bindings may both hold with the same keystrokes,
//! or with one's keystrokes starting the other's. Tests in `review_ui` and `lo` check the places
//! their windows have, with every binding Lo makes there.

use gpui::{KeyBinding, KeyContext, OperatingSystem};

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

/// How deep in `stack` `binding`'s context holds, if it does. A binding with no context holds
/// everywhere.
fn depth(binding: &KeyBinding, stack: &[KeyContext]) -> Option<usize> {
    match binding.predicate() {
        Some(predicate) => predicate.depth_of(stack),
        None => Some(0),
    }
}

/// Whether `second`'s keystrokes start with all of `first`'s, or are the same.
fn starts(first: &KeyBinding, second: &KeyBinding) -> bool {
    let (first, second) = (first.keystrokes(), second.keystrokes());
    first.len() <= second.len() && second[..first.len()] == *first
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
                stack: stack(&["Workspace", "Pane", "PrTab", "Editor mode=full multibuffer"]),
            },
            Place {
                name: "the file list".into(),
                stack: stack(&["Workspace", "Pane", "PrTab", "PrFileList"]),
            },
        ]
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
