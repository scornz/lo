//! What features draw on the diff (design §5 "The diff view"): badges in each file's header, and
//! anything else on the diff's editors, such as a changed image's before and after.
//!
//! Each feature registers a [`DiffDecorator`] in [`crate::pr_diff::DECORATORS`], one line, and
//! never edits the header or the code that builds the diff. Its decorations go on again whenever
//! what they sit on is rebuilt ([`Redecorate`]):
//!
//! - after each file is added to the diff, or shows other rows;
//! - after the diff goes side by side or inline, or its left side is built again, as `alt-v` and a
//!   pane growing wide enough do;
//! - after `alt-w` hides changes to whitespace alone, or shows them again;
//! - when the local copy's diff replaces GitHub's, or an older copy's.
//!
//! Each diff's right-hand editor keeps its own decorators in a [`DiffDecorations`] addon, made
//! with the diff, so decorations never outlive their diff.

use editor::{Addon, SplittableEditor};
use gpui::{App, Entity, EntityId, SharedString, WeakEntity, Window};
use language::BufferId;

use crate::pr_diff::FileHeaders;
use crate::pr_tab::{ExpandFile, PrTab};
use crate::shown_files::{self, ShownFile};

/// What the decorators run for: what was rebuilt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Redecorate {
    /// The file at this path was added to the diff, or replaced, as when a large file loads, or
    /// shows other rows, as when `alt-a` shows it whole.
    File(String),
    /// The diff went side by side or inline, or its left side was built again: decorations go on
    /// again on both sides, for every file.
    Split,
    /// Changes to whitespace alone were hidden or shown again, which changed files' hunks. Each
    /// file whose rows changed had its own [`Redecorate::File`] first.
    Whitespace,
    /// The diff replaced another in the tab, as the local copy's replaces GitHub's, with every
    /// file in it.
    Replaced,
}

/// A feature that draws on the diff.
pub(crate) trait DiffDecorator: 'static {
    /// The badges `file`, at `path`, gets in its header, in the order they show. Asked again
    /// whenever the decorators run for the file, and when what its header says changes.
    fn badges(&self, _path: &str, _file: &ShownFile, _cx: &App) -> Vec<Badge> {
        Vec::new()
    }

    /// Puts the decorator's decorations on `diff`'s editors again, for what `run` says was
    /// rebuilt. The badges of the files it covers are asked for after every decorator ran.
    fn decorate(
        &mut self,
        _diff: &Entity<SplittableEditor>,
        _run: &Redecorate,
        _window: &mut Window,
        _cx: &mut App,
    ) {
    }
}

/// Makes a feature's decorator for a new diff in `tab`.
pub(crate) type NewDecorator = fn(&WeakEntity<PrTab>) -> Box<dyn DiffDecorator>;

/// A note in a file's header, right of its status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Badge {
    pub(crate) label: SharedString,
    /// What hovering it says, when there's more to say.
    pub(crate) detail: Option<SharedString>,
    pub(crate) style: BadgeStyle,
}

/// How a badge looks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BadgeStyle {
    /// Outlined, as why a file is collapsed is.
    Outlined,
    /// Muted text.
    Note,
    /// In the warning colour, after a warning sign.
    Warning,
    /// A path in the buffer's font and an arrow, right before the file's own path, as a renamed
    /// file's old path is.
    RenamedFrom,
}

impl Badge {
    pub(crate) fn outlined(
        label: impl Into<SharedString>,
        detail: impl Into<SharedString>,
    ) -> Self {
        Self {
            label: label.into(),
            detail: Some(detail.into()),
            style: BadgeStyle::Outlined,
        }
    }

    pub(crate) fn note(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            detail: None,
            style: BadgeStyle::Note,
        }
    }

    pub(crate) fn warning(label: impl Into<SharedString>, detail: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            detail: Some(detail.into()),
            style: BadgeStyle::Warning,
        }
    }

    pub(crate) fn renamed_from(path: impl Into<SharedString>) -> Self {
        Self {
            label: path.into(),
            detail: None,
            style: BadgeStyle::RenamedFrom,
        }
    }
}

/// A diff's decorators, which its right-hand editor keeps.
pub(crate) struct DiffDecorations {
    /// Each feature's, in the order its badges show. `None` while they run.
    decorators: Option<Vec<Box<dyn DiffDecorator>>>,
    /// The left-hand editor they last ran with, to tell when the diff splits again.
    lhs: Option<EntityId>,
}

impl DiffDecorations {
    /// The decorators `new` makes, for a diff in `tab`, and in tests, those they add.
    pub(crate) fn new(tab: &WeakEntity<PrTab>, new: &[NewDecorator], cx: &App) -> Self {
        let decorators = new.iter().map(|new| new(tab)).collect::<Vec<_>>();
        #[cfg(test)]
        let decorators = {
            let mut decorators = decorators;
            let added = cx.try_global::<TestDecorators>();
            decorators.extend(
                added
                    .iter()
                    .flat_map(|added| added.0.iter().map(|new| new(tab))),
            );
            decorators
        };
        #[cfg(not(test))]
        let _ = cx;
        Self {
            decorators: Some(decorators),
            lhs: None,
        }
    }
}

/// Decorators tests add to every new diff, after Lo's own.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct TestDecorators(pub(crate) Vec<NewDecorator>);

#[cfg(test)]
impl gpui::Global for TestDecorators {}

impl Addon for DiffDecorations {
    fn to_any(&self) -> &dyn std::any::Any {
        self
    }

    fn to_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

/// Runs `diff`'s decorators for `run`, then asks them for the badges of the files it covers.
/// When the diff split again since they last ran, they run for [`Redecorate::Split`] first, which
/// covers every file, then for `run` unless it's only a file's. A run asked for while they run
/// waits until they're done.
pub(crate) fn redecorate(
    diff: &Entity<SplittableEditor>,
    run: Redecorate,
    window: &mut Window,
    cx: &mut App,
) {
    let lhs = diff.read(cx).lhs_editor().map(|lhs| lhs.entity_id());
    let rhs = diff.read(cx).rhs_editor().clone();
    let taken = rhs.update(cx, |rhs, _| {
        let decorations = rhs.addon_mut::<DiffDecorations>()?;
        let decorators = decorations.decorators.take();
        let split = decorators.is_some() && decorations.lhs != lhs;
        if decorators.is_some() {
            decorations.lhs = lhs;
        }
        Some((decorators, split))
    });
    let (mut decorators, split) = match taken {
        // A diff without decorations, as some tests build.
        None => return,
        Some((None, _)) => {
            let diff = diff.downgrade();
            window.defer(cx, move |window, cx| {
                if let Some(diff) = diff.upgrade() {
                    redecorate(&diff, run, window, cx)
                }
            });
            return;
        }
        Some((Some(decorators), split)) => (decorators, split),
    };
    let runs = match (split, run) {
        (false, run) => vec![run],
        (true, Redecorate::File(_) | Redecorate::Split) => vec![Redecorate::Split],
        (true, run) => vec![Redecorate::Split, run],
    };
    for run in &runs {
        for decorator in &mut decorators {
            decorator.decorate(diff, run, window, cx);
        }
    }
    let paths = match runs.as_slice() {
        [Redecorate::File(path)] => Some(vec![path.clone()]),
        _ => None,
    };
    set_badges(diff, &decorators, paths.as_deref(), cx);
    rhs.update(cx, |rhs, _| {
        if let Some(decorations) = rhs.addon_mut::<DiffDecorations>() {
            decorations.decorators = Some(decorators);
        }
    });
}

/// Runs `diff`'s decorators for [`Redecorate::Split`] when it split again since they last ran,
/// as `alt-v` and a pane growing wide enough make it. Cheap otherwise: it runs on every change to
/// the diff.
pub(crate) fn sync_split(diff: &Entity<SplittableEditor>, window: &mut Window, cx: &mut App) {
    let lhs = diff.read(cx).lhs_editor().map(|lhs| lhs.entity_id());
    let rhs = diff.read(cx).rhs_editor().read(cx);
    let ran_with = rhs
        .addon::<DiffDecorations>()
        .filter(|decorations| decorations.decorators.is_some())
        .map(|decorations| decorations.lhs);
    if ran_with.is_some_and(|ran_with| ran_with != lhs) {
        redecorate(diff, Redecorate::Split, window, cx);
    }
}

/// Asks `diff`'s decorators again for the badges of the files at `paths`, whose headers have
/// something new to say, such as a large file's now that it's open.
pub(crate) fn refresh_badges(
    diff: &Entity<SplittableEditor>,
    paths: &[String],
    window: &mut Window,
    cx: &mut App,
) {
    if paths.is_empty() {
        return;
    }
    let rhs = diff.read(cx).rhs_editor().read(cx);
    let running = rhs
        .addon::<DiffDecorations>()
        .is_some_and(|decorations| decorations.decorators.is_none());
    if running {
        // The run under way asks for them when it's done, unless it's another file's.
        let (diff, paths) = (diff.downgrade(), paths.to_vec());
        window.defer(cx, move |window, cx| {
            if let Some(diff) = diff.upgrade() {
                refresh_badges(&diff, &paths, window, cx)
            }
        });
        return;
    }
    let badges = badges_of(diff, None, Some(paths), cx);
    write_badges(diff, badges, cx);
}

/// Asks `decorators` for the badges of `diff`'s files at `paths`, or of every file, and puts them
/// in their headers.
fn set_badges(
    diff: &Entity<SplittableEditor>,
    decorators: &[Box<dyn DiffDecorator>],
    paths: Option<&[String]>,
    cx: &mut App,
) {
    let badges = badges_of(diff, Some(decorators), paths, cx);
    write_badges(diff, badges, cx);
}

/// The badges of `diff`'s files at `paths`, or of every file, from `decorators`, or from the diff's
/// own when they aren't running.
fn badges_of(
    diff: &Entity<SplittableEditor>,
    decorators: Option<&[Box<dyn DiffDecorator>]>,
    paths: Option<&[String]>,
    cx: &App,
) -> Vec<(BufferId, Vec<Badge>)> {
    let rhs = diff.read(cx).rhs_editor().read(cx);
    let Some(decorators) = decorators.or_else(|| {
        let decorations = rhs.addon::<DiffDecorations>()?;
        decorations.decorators.as_deref()
    }) else {
        return Vec::new();
    };
    shown_files::read(diff, cx, |files| {
        files
            .iter()
            .filter(|(path, _)| paths.is_none_or(|paths| paths.contains(path)))
            .map(|(path, file)| {
                let badges = (decorators.iter())
                    .flat_map(|decorator| decorator.badges(path, file, cx))
                    .collect();
                (file.buffer_id, badges)
            })
            .collect()
    })
    .unwrap_or_default()
}

/// Puts `badges` in their files' headers, in place of what they had.
fn write_badges(
    diff: &Entity<SplittableEditor>,
    badges: Vec<(BufferId, Vec<Badge>)>,
    cx: &mut App,
) {
    let rhs = diff.read(cx).rhs_editor().clone();
    rhs.update(cx, |rhs, cx| {
        if let Some(headers) = rhs.addon_mut::<FileHeaders>() {
            for (buffer, badges) in badges {
                headers.set_badges(buffer, badges);
            }
        }
        cx.notify();
    });
}

/// The key that opens or loads a collapsed file, as headers name it: the one the keymap binds,
/// whatever has focus.
fn expand_key(cx: &App) -> String {
    let keymap = cx.key_bindings();
    let keymap = keymap.borrow();
    keymap
        .bindings_for_action(&ExpandFile)
        .last()
        .map(|binding| ui::text_for_keybinding_keystrokes(binding.keystrokes(), cx))
        .unwrap_or_else(|| "alt-a".to_string())
}

/// Says why a file is collapsed: it's generated or a lockfile, it's large or too large to show,
/// every change in it is to whitespace, which is hidden, or nothing in its text that shows
/// changed.
pub(crate) fn collapsed(_: &WeakEntity<PrTab>) -> Box<dyn DiffDecorator> {
    struct Collapsed;
    impl DiffDecorator for Collapsed {
        fn badges(&self, _: &str, file: &ShownFile, cx: &App) -> Vec<Badge> {
            let Some(collapsed) = &file.collapsed else {
                return Vec::new();
            };
            let key = expand_key(cx);
            vec![Badge::outlined(
                collapsed.label(&key),
                collapsed.detail(&key),
            )]
        }
    }
    Box::new(Collapsed)
}

/// Says what the status doesn't about a file's change: a change of mode or type, a submodule, a
/// name or bytes that aren't UTF-8, line endings alone, or what it was copied from.
pub(crate) fn change_notes(_: &WeakEntity<PrTab>) -> Box<dyn DiffDecorator> {
    struct ChangeNotes;
    impl DiffDecorator for ChangeNotes {
        fn badges(&self, _: &str, file: &ShownFile, _: &App) -> Vec<Badge> {
            file.notes.iter().cloned().map(Badge::note).collect()
        }
    }
    Box::new(ChangeNotes)
}

/// Warns that a file shows other than as it should: Lo couldn't show it, it's GitHub's patch as
/// text, GitHub's diff leaves it out, or the checkout's copy differs from the head's.
pub(crate) fn warnings(_: &WeakEntity<PrTab>) -> Box<dyn DiffDecorator> {
    struct Warnings;
    impl DiffDecorator for Warnings {
        fn badges(&self, _: &str, file: &ShownFile, _: &App) -> Vec<Badge> {
            (file.warning.iter())
                .map(|warning| Badge::warning(warning.label.clone(), warning.detail.clone()))
                .collect()
        }
    }
    Box::new(Warnings)
}

/// Shows a renamed file's old path before its new one.
pub(crate) fn renamed_from(_: &WeakEntity<PrTab>) -> Box<dyn DiffDecorator> {
    struct RenamedFrom;
    impl DiffDecorator for RenamedFrom {
        fn badges(&self, _: &str, file: &ShownFile, _: &App) -> Vec<Badge> {
            file.renamed_from
                .iter()
                .cloned()
                .map(Badge::renamed_from)
                .collect()
        }
    }
    Box::new(RenamedFrom)
}

#[cfg(test)]
mod tests {
    use super::*;

    use gpui::{TestAppContext, VisualTestContext};

    use crate::pr_diff::tests::fixtures::Entry;
    use crate::shown_files::tests::{
        badge_label, changed, github_file, lines, select, show, show_github, tab,
    };

    /// What the test decorator ran for.
    #[derive(Default)]
    struct Runs(Vec<Redecorate>);

    impl gpui::Global for Runs {}

    /// Notes each run, and gives every file a badge.
    fn recorder(_: &WeakEntity<PrTab>) -> Box<dyn DiffDecorator> {
        struct Recorder;
        impl DiffDecorator for Recorder {
            fn badges(&self, path: &str, _: &ShownFile, _: &App) -> Vec<Badge> {
                vec![Badge::note(format!("decorated {path}"))]
            }

            fn decorate(
                &mut self,
                _: &Entity<SplittableEditor>,
                run: &Redecorate,
                _: &mut Window,
                cx: &mut App,
            ) {
                cx.default_global::<Runs>().0.push(run.clone());
            }
        }
        Box::new(Recorder)
    }

    fn runs(cx: &mut VisualTestContext) -> Vec<Redecorate> {
        cx.update(|_, cx| std::mem::take(&mut cx.default_global::<Runs>().0))
    }

    fn file(path: &str) -> Redecorate {
        Redecorate::File(path.into())
    }

    /// Every file's badges, label by label.
    fn badges(diff: &Entity<SplittableEditor>, cx: &mut VisualTestContext) -> Vec<Vec<String>> {
        diff.read_with(cx, |diff, cx| {
            let rhs = diff.rhs_editor().read(cx);
            let headers = rhs.addon::<FileHeaders>().unwrap();
            let files = rhs.addon::<shown_files::ShownFiles>().unwrap();
            let mut paths = files.paths();
            paths.sort();
            paths
                .iter()
                .map(|path| {
                    let buffer = files.get(path).unwrap().buffer_id;
                    (headers.badges(buffer).iter())
                        .map(|badge| badge.label.to_string())
                        .collect()
                })
                .collect()
        })
    }

    /// A feature's decorator runs again whenever what it decorates is rebuilt, and its badges,
    /// with the header's own, stay through each.
    #[gpui::test]
    async fn decorators_run_again_whenever_the_diff_is_rebuilt(cx: &mut TestAppContext) {
        let (tab, cx) = tab(cx).await;
        cx.update(|_, cx| cx.set_global(TestDecorators(vec![recorder])));

        // Each file of GitHub's diff, as it's added. The first made the left side.
        let github = show_github(&tab, vec![github_file("a.txt", "a", 10)], cx);
        assert_eq!(runs(cx), [Redecorate::Split]);
        assert_eq!(badges(&github, cx), [["decorated a.txt"]]);

        // The local copy's diff, each file out of sight, then as it replaces GitHub's.
        let (diff, _copy) = show(
            &tab,
            &[
                Entry::File("Cargo.lock", b"a\n"),
                Entry::File("a.txt", lines("a", 20).as_bytes()),
                Entry::File("ws.txt", b"a b\n"),
            ],
            &[
                Entry::File("Cargo.lock", b"b\n"),
                Entry::File("a.txt", changed("a", 20, 10).as_bytes()),
                Entry::File("ws.txt", b"a  b\n"),
            ],
            cx,
        );
        let mut built = runs(cx);
        assert_eq!(built.first(), Some(&Redecorate::Split));
        assert_eq!(built.pop(), Some(Redecorate::Replaced));
        assert!(
            built[1..]
                .iter()
                .all(|run| matches!(run, Redecorate::File(_)))
        );
        let shown = vec![
            vec!["lockfile".to_string(), "decorated Cargo.lock".to_string()],
            vec!["decorated a.txt".to_string()],
            vec!["decorated ws.txt".to_string()],
        ];
        assert_eq!(badges(&diff, cx), shown);

        // alt-v, inline and back.
        select(&diff, "a.txt", cx);
        cx.simulate_keystrokes("alt-v");
        cx.run_until_parked();
        assert_eq!(runs(cx), [Redecorate::Split]);
        assert_eq!(badges(&diff, cx), shown);
        cx.simulate_keystrokes("alt-v");
        cx.run_until_parked();
        assert_eq!(runs(cx), [Redecorate::Split]);
        assert_eq!(badges(&diff, cx), shown);

        // alt-w: each file, as its rows change, then all of them.
        let whitespace = |cx: &mut VisualTestContext| {
            let mut runs = runs(cx);
            assert_eq!(runs.pop(), Some(Redecorate::Whitespace));
            runs.sort_by_key(|run| format!("{run:?}"));
            runs
        };
        let each = [file("Cargo.lock"), file("a.txt"), file("ws.txt")];
        cx.simulate_keystrokes("alt-w");
        cx.run_until_parked();
        assert_eq!(whitespace(cx), each);
        assert_eq!(
            badge_label(&diff, "ws.txt", BadgeStyle::Outlined, cx).as_deref(),
            Some("whitespace only")
        );
        assert_eq!(badges(&diff, cx)[..2], shown[..2]);
        cx.simulate_keystrokes("alt-w");
        cx.run_until_parked();
        assert_eq!(whitespace(cx), each);
        assert_eq!(badges(&diff, cx), shown);

        // alt-a, showing a file whole.
        select(&diff, "a.txt", cx);
        cx.simulate_keystrokes("alt-a");
        cx.run_until_parked();
        assert_eq!(runs(cx), [file("a.txt")]);
        assert_eq!(badges(&diff, cx), shown);
    }
}
