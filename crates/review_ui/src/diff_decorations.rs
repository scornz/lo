//! What features draw on the diff (design §5 "The diff view"): badges in each file's header, and
//! anything else on the diff's editors, such as a changed image's before and after.
//!
//! Each feature registers a [`DiffDecorator`] in [`crate::pr_diff::DECORATORS`], one line, and
//! never edits the header or the code that builds the diff. Its decorations go on again whenever
//! what they sit on is rebuilt ([`Redecorate`]):
//!
//! - after each file is added to the diff, or shows other rows (`alt-a`);
//! - after the diff goes side by side or inline, or its left side is built again, as `alt-v` and a
//!   pane growing wide enough do;
//! - after `alt-w` hides changes to whitespace alone, or shows them again;
//! - when the local copy's diff replaces GitHub's, or another copy's;
//! - once every file is in;
//! - when the decorator's own data changes, as it asks ([`refresh`]).
//!
//! **When decorators run.** A run is never made while it's asked for: what's asked is noted, and
//! the decorators run once the update that asked is over, before the window draws, with every
//! request for the diff made meanwhile merged into one run. So a decorator can read the tab, its
//! pull request and any other entity while it runs; nothing is being updated then. A run asked
//! for while decorators run comes after, unless that run covers it already.
//!
//! **What a decorator mustn't do.** `decorate` draws on the editors (highlights, blocks); it
//! mustn't rebuild the diff, by adding files or showing other rows, which would ask for another
//! run, and another.
//!
//! Each diff's right-hand editor keeps its own decorators in a [`DiffDecorations`] addon, made
//! with the diff for the pull request it shows ([`DiffContext`]), so decorations never outlive
//! their diff.

use std::any::{Any, TypeId};
use std::sync::Arc;

use editor::{Addon, SplittableEditor};
use gpui::{App, Entity, EntityId, SharedString, Window};
use language::BufferId;

use crate::pr_diff::FileHeaders;
use crate::pr_tab::{ExpandFile, PullHandle};
use crate::shown_comparison::ShownComparison;
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
    /// Every file is in the diff: the last was added, or the diff that replaced another had them
    /// all.
    Loaded,
    /// The decorator's own data changed ([`refresh`]): only it runs, for every file.
    Refresh,
}

/// A feature that draws on the diff.
pub(crate) trait DiffDecorator: Any {
    /// The badges `file`, at `path`, gets in its header, in the order they show. Asked again
    /// whenever the decorators run, and when what a file's header says changes.
    fn badges(&self, _path: &str, _file: &ShownFile, _cx: &App) -> Vec<Badge> {
        Vec::new()
    }

    /// Puts the decorator's decorations on `diff`'s editors again, for what `run` says was
    /// rebuilt. It may read anything, and mustn't rebuild the diff (see the module's docs). The
    /// badges of the files it covers are asked for after every decorator ran.
    fn decorate(
        &mut self,
        _diff: &Entity<SplittableEditor>,
        _run: &Redecorate,
        _window: &mut Window,
        _cx: &mut App,
    ) {
    }
}

/// What a feature's decorator is made with: the diff's pull request and what it compares.
#[derive(Clone)]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the features that read it, threads and moves, come later"
    )
)]
pub(crate) struct DiffContext {
    /// The pull request the diff is of. Read it, or its tab, while decorating: nothing is being
    /// updated then.
    pub(crate) pull: PullHandle,
    /// What the local copy's diff compares; `None` for GitHub's diff.
    pub(crate) comparison: Option<Arc<ShownComparison>>,
}

/// Makes a feature's decorator for a new diff. It can make or find the entities it needs with
/// `cx`, such as its own state for the pull request.
pub(crate) type NewDecorator = fn(&DiffContext, &mut App) -> Box<dyn DiffDecorator>;

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
    /// What they run for next, once the update that asked is over.
    pending: Pending,
    /// Whether a run is due, so asking again only adds to it.
    due: bool,
    /// What the run under way covers, while one is: a request it covers is dropped.
    running: Option<Pending>,
}

/// What a diff's decorators are asked to run for, merged.
#[derive(Clone, Debug, Default)]
struct Pending {
    /// Each file added or showing other rows, in the order asked.
    files: Vec<String>,
    split: bool,
    whitespace: bool,
    replaced: bool,
    loaded: bool,
    /// Decorators that asked to run alone, by their type.
    refresh: Vec<TypeId>,
    /// Files whose headers have something new to say.
    badges: Vec<String>,
}

impl Pending {
    fn add(&mut self, run: Redecorate) {
        match run {
            Redecorate::File(path) => push_new(&mut self.files, path),
            Redecorate::Split => self.split = true,
            Redecorate::Whitespace => self.whitespace = true,
            Redecorate::Replaced => self.replaced = true,
            Redecorate::Loaded => self.loaded = true,
            Redecorate::Refresh => {}
        }
    }

    /// Whether a run of `self` does what `run` asks.
    fn covers(&self, run: &Redecorate) -> bool {
        match run {
            Redecorate::File(path) => self.split || self.files.contains(path),
            Redecorate::Split => self.split,
            Redecorate::Whitespace => self.whitespace,
            Redecorate::Replaced => self.replaced,
            Redecorate::Loaded => self.loaded,
            Redecorate::Refresh => false,
        }
    }

    /// The runs, in order: rebuilt sides first, which cover every file, else each file, then
    /// what changed the whole diff.
    fn runs(&self) -> Vec<Redecorate> {
        let mut runs = Vec::new();
        if self.split {
            runs.push(Redecorate::Split);
        } else {
            runs.extend(self.files.iter().cloned().map(Redecorate::File));
        }
        for (asked, run) in [
            (self.whitespace, Redecorate::Whitespace),
            (self.replaced, Redecorate::Replaced),
            (self.loaded, Redecorate::Loaded),
        ] {
            if asked {
                runs.push(run);
            }
        }
        runs
    }

    /// The files whose badges the run asks for again: `None` for every file.
    fn badge_paths(&self) -> Option<Vec<String>> {
        let every = self.split
            || self.whitespace
            || self.replaced
            || self.loaded
            || !self.refresh.is_empty();
        if every {
            return None;
        }
        let mut paths = self.files.clone();
        for path in &self.badges {
            push_new(&mut paths, path.clone());
        }
        Some(paths)
    }
}

fn push_new<T: PartialEq>(items: &mut Vec<T>, item: T) {
    if !items.contains(&item) {
        items.push(item);
    }
}

impl DiffDecorations {
    /// The decorators `new` makes for a diff in `context`, and in tests, those they add.
    pub(crate) fn new(context: &DiffContext, new: &[NewDecorator], cx: &mut App) -> Self {
        let mut decorators: Vec<_> = new.iter().map(|new| new(context, cx)).collect();
        #[cfg(test)]
        {
            let added = cx
                .try_global::<TestDecorators>()
                .map(|added| added.0.clone())
                .unwrap_or_default();
            decorators.extend(added.iter().map(|new| new(context, cx)));
        }
        Self {
            decorators: Some(std::mem::take(&mut decorators)),
            lhs: None,
            pending: Pending::default(),
            due: false,
            running: None,
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

/// Asks `diff`'s decorators to run for `run`, once the update asking is over (see the module's
/// docs).
pub(crate) fn redecorate(
    diff: &Entity<SplittableEditor>,
    run: Redecorate,
    window: &mut Window,
    cx: &mut App,
) {
    ask(diff, window, cx, |pending| pending.add(run));
}

/// Asks `D`, one of `diff`'s decorators, to run alone, for every file, as when its own data
/// changed: new threads came, or moves were found.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "features that refresh come later")
)]
pub(crate) fn refresh<D: DiffDecorator>(
    diff: &Entity<SplittableEditor>,
    window: &mut Window,
    cx: &mut App,
) {
    ask(diff, window, cx, |pending| {
        push_new(&mut pending.refresh, TypeId::of::<D>())
    });
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
    ask(diff, window, cx, |pending| {
        for path in paths {
            push_new(&mut pending.badges, path.clone());
        }
    });
}

/// Asks `diff`'s decorators to run for the split when it split again since they last ran, as
/// `alt-v` and a pane growing wide enough make it. Cheap otherwise: it runs on every change to the
/// diff.
pub(crate) fn sync_split(diff: &Entity<SplittableEditor>, window: &mut Window, cx: &mut App) {
    let lhs = diff.read(cx).lhs_editor().map(|lhs| lhs.entity_id());
    let rhs = diff.read(cx).rhs_editor().read(cx);
    let stale = rhs.addon::<DiffDecorations>().is_some_and(|decorations| {
        decorations.lhs != lhs && !decorations.pending.split && decorations.running.is_none()
    });
    if stale {
        redecorate(diff, Redecorate::Split, window, cx);
    }
}

/// Notes what `add` asks of `diff`'s decorators, and has them run once the update asking is
/// over, unless a run is due already. What the run under way covers is dropped.
fn ask(
    diff: &Entity<SplittableEditor>,
    window: &mut Window,
    cx: &mut App,
    add: impl FnOnce(&mut Pending),
) {
    let rhs = diff.read(cx).rhs_editor().clone();
    let schedule = rhs.update(cx, |rhs, _| {
        let decorations = rhs.addon_mut::<DiffDecorations>()?;
        let mut asked = Pending::default();
        add(&mut asked);
        if let Some(running) = &decorations.running {
            asked
                .files
                .retain(|path| !running.covers(&Redecorate::File(path.clone())));
            asked.split &= !running.split;
            asked.whitespace &= !running.whitespace;
            asked.replaced &= !running.replaced;
            asked.loaded &= !running.loaded;
        }
        let pending = &mut decorations.pending;
        for path in asked.files {
            push_new(&mut pending.files, path);
        }
        pending.split |= asked.split;
        pending.whitespace |= asked.whitespace;
        pending.replaced |= asked.replaced;
        pending.loaded |= asked.loaded;
        for decorator in asked.refresh {
            push_new(&mut pending.refresh, decorator);
        }
        for path in asked.badges {
            push_new(&mut pending.badges, path);
        }
        Some(!std::mem::replace(&mut decorations.due, true))
    });
    if schedule != Some(true) {
        return;
    }
    let diff = diff.downgrade();
    window.defer(cx, move |window, cx| {
        if let Some(diff) = diff.upgrade() {
            run_due(&diff, window, cx);
        }
    });
}

/// Runs `diff`'s decorators for what they were asked since they last ran, then asks them for the
/// badges of the files it covers. When the diff split again since they last ran, they run for
/// the split, which covers every file.
fn run_due(diff: &Entity<SplittableEditor>, window: &mut Window, cx: &mut App) {
    let lhs = diff.read(cx).lhs_editor().map(|lhs| lhs.entity_id());
    let rhs = diff.read(cx).rhs_editor().clone();
    let taken = rhs.update(cx, |rhs, _| {
        let decorations = rhs.addon_mut::<DiffDecorations>()?;
        let decorators = decorations.decorators.take()?;
        decorations.due = false;
        let mut pending = std::mem::take(&mut decorations.pending);
        if decorations.lhs != lhs {
            pending.split = true;
            decorations.lhs = lhs;
        }
        decorations.running = Some(pending.clone());
        Some((decorators, pending))
    });
    let Some((mut decorators, pending)) = taken else {
        return;
    };
    for run in pending.runs() {
        for decorator in &mut decorators {
            decorator.decorate(diff, &run, window, cx);
        }
    }
    for wanted in &pending.refresh {
        for decorator in &mut decorators {
            if (decorator.as_ref() as &dyn Any).type_id() == *wanted {
                decorator.decorate(diff, &Redecorate::Refresh, window, cx);
            }
        }
    }
    let badges = badges_of(diff, &decorators, pending.badge_paths().as_deref(), cx);
    write_badges(diff, badges, cx);
    rhs.update(cx, |rhs, _| {
        if let Some(decorations) = rhs.addon_mut::<DiffDecorations>() {
            decorations.decorators = Some(decorators);
            decorations.running = None;
        }
    });
}

/// The badges `decorators` give `diff`'s files at `paths`, or every file.
fn badges_of(
    diff: &Entity<SplittableEditor>,
    decorators: &[Box<dyn DiffDecorator>],
    paths: Option<&[String]>,
    cx: &App,
) -> Vec<(BufferId, Vec<Badge>)> {
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
pub(crate) fn collapsed(_: &DiffContext, _: &mut App) -> Box<dyn DiffDecorator> {
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
pub(crate) fn change_notes(_: &DiffContext, _: &mut App) -> Box<dyn DiffDecorator> {
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
pub(crate) fn warnings(_: &DiffContext, _: &mut App) -> Box<dyn DiffDecorator> {
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
pub(crate) fn renamed_from(_: &DiffContext, _: &mut App) -> Box<dyn DiffDecorator> {
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

    use crate::pr_diff::Warning;
    use crate::pr_diff::tests::fixtures::Entry;
    use crate::shown_files::tests::{
        badge_label, changed, github_file, lines, select, show, show_github, tab,
    };

    /// What each test decorator ran for, and how many files its pull request listed then.
    #[derive(Default)]
    struct Runs(Vec<(&'static str, Redecorate, usize)>);

    impl gpui::Global for Runs {}

    /// Notes each run with what it reads of the pull request and its tab, and gives every file a
    /// badge.
    struct Recorder {
        name: &'static str,
        context: DiffContext,
        /// What it asks for while it runs.
        asks: fn(&Entity<SplittableEditor>, &Redecorate, &mut Window, &mut App),
    }

    impl DiffDecorator for Recorder {
        fn badges(&self, path: &str, _: &ShownFile, cx: &App) -> Vec<Badge> {
            // The tab can be read while badges are asked for.
            let tab = self.context.pull.tab().upgrade().unwrap();
            let _ = tab.read(cx).hides_whitespace();
            vec![Badge::note(format!("{} {path}", self.name))]
        }

        fn decorate(
            &mut self,
            diff: &Entity<SplittableEditor>,
            run: &Redecorate,
            window: &mut Window,
            cx: &mut App,
        ) {
            // Reading the pull request, and so its tab, would panic were the tab being updated.
            let files = (self.context.pull)
                .read_with(cx, |pull, _| pull.files().len())
                .unwrap();
            // It was made with what its diff compares: the local copy's, or GitHub's, nothing.
            let compares = shown_files::read(diff, cx, |files| files.comparison.clone()).unwrap();
            assert_eq!(
                self.context.comparison.as_ref().map(Arc::as_ptr),
                compares.as_ref().map(Arc::as_ptr)
            );
            (self.asks)(diff, run, window, cx);
            cx.default_global::<Runs>()
                .0
                .push((self.name, run.clone(), files));
        }
    }

    fn recorder(context: &DiffContext, _: &mut App) -> Box<dyn DiffDecorator> {
        Box::new(Recorder {
            name: "recorded",
            context: context.clone(),
            asks: |_, _, _, _| {},
        })
    }

    /// A decorator that asks for runs while it runs: for the file it's decorating again, and to
    /// run alone once every file is in.
    struct Asker(Recorder);

    impl DiffDecorator for Asker {
        fn badges(&self, path: &str, file: &ShownFile, cx: &App) -> Vec<Badge> {
            self.0.badges(path, file, cx)
        }

        fn decorate(
            &mut self,
            diff: &Entity<SplittableEditor>,
            run: &Redecorate,
            window: &mut Window,
            cx: &mut App,
        ) {
            self.0.decorate(diff, run, window, cx)
        }
    }

    fn asker(context: &DiffContext, _: &mut App) -> Box<dyn DiffDecorator> {
        Box::new(Asker(Recorder {
            name: "asker",
            context: context.clone(),
            asks: |diff, run, window, cx| match run {
                Redecorate::File(path) => {
                    redecorate(diff, Redecorate::File(path.clone()), window, cx)
                }
                Redecorate::Loaded => refresh::<Asker>(diff, window, cx),
                _ => {}
            },
        }))
    }

    /// The runs since the last call, of the decorator `name`, with the files its pull request
    /// listed.
    fn runs(name: &str, cx: &mut VisualTestContext) -> Vec<(Redecorate, usize)> {
        cx.update(|_, cx| {
            let runs = std::mem::take(&mut cx.default_global::<Runs>().0);
            (runs.into_iter())
                .filter(|(of, _, _)| *of == name)
                .map(|(_, run, files)| (run, files))
                .collect()
        })
    }

    /// [`runs`], without the files.
    fn reasons(name: &str, cx: &mut VisualTestContext) -> Vec<Redecorate> {
        runs(name, cx).into_iter().map(|(run, _)| run).collect()
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

    /// A feature's decorator runs again whenever what it decorates is rebuilt, once for each
    /// rebuild, and reads its pull request as it runs. Its badges, with the header's own, stay
    /// through each.
    #[gpui::test]
    async fn decorators_run_again_whenever_the_diff_is_rebuilt(cx: &mut TestAppContext) {
        let (tab, cx) = tab(cx).await;
        cx.update(|_, cx| cx.set_global(TestDecorators(vec![recorder])));

        // GitHub's diff: its one file, whose adding made the left side, then all of it.
        let github = show_github(&tab, vec![github_file("a.txt", "a", 10)], cx);
        assert_eq!(
            reasons("recorded", cx),
            [Redecorate::Split, Redecorate::Loaded]
        );
        assert_eq!(badges(&github, cx), [["recorded a.txt"]]);

        // The local copy's diff: each file as it's added out of sight, the first making the left
        // side, then once as it replaces GitHub's, with every file in.
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
        let built = runs("recorded", cx);
        assert_eq!(
            built.iter().map(|(run, _)| run.clone()).collect::<Vec<_>>(),
            [
                Redecorate::Split,
                file("a.txt"),
                file("ws.txt"),
                Redecorate::Replaced,
                Redecorate::Loaded
            ]
        );
        // By then the pull request lists the local copy's files.
        assert_eq!(built[3], (Redecorate::Replaced, 3));
        let shown = vec![
            vec!["lockfile".to_string(), "recorded Cargo.lock".to_string()],
            vec!["recorded a.txt".to_string()],
            vec!["recorded ws.txt".to_string()],
        ];
        assert_eq!(badges(&diff, cx), shown);

        // alt-v, inline and back, each a run for the split, reading the pull request.
        select(&diff, "a.txt", cx);
        cx.simulate_keystrokes("alt-v");
        cx.run_until_parked();
        assert_eq!(runs("recorded", cx), [(Redecorate::Split, 3)]);
        assert_eq!(badges(&diff, cx), shown);
        cx.simulate_keystrokes("alt-v");
        cx.run_until_parked();
        assert_eq!(runs("recorded", cx), [(Redecorate::Split, 3)]);
        assert_eq!(badges(&diff, cx), shown);

        // alt-w: each file, as its rows change, then all of them.
        let whitespace = |cx: &mut VisualTestContext| {
            let mut runs = reasons("recorded", cx);
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

        // alt-a, showing a file whole: a run for that file, reading the pull request.
        select(&diff, "a.txt", cx);
        cx.simulate_keystrokes("alt-a");
        cx.run_until_parked();
        assert_eq!(runs("recorded", cx), [(file("a.txt"), 3)]);
        assert_eq!(badges(&diff, cx), shown);
    }

    /// What's asked of a diff's decorators within one update merges into one run, after it. What
    /// a decorator asks while it runs comes after, unless the run covers it, so asking for the
    /// file it's decorating doesn't loop. A decorator can run alone, and a file's badges alone can
    /// be asked for again.
    #[gpui::test]
    async fn runs_merge_and_a_decorator_can_ask_for_more(cx: &mut TestAppContext) {
        let (tab, cx) = tab(cx).await;
        cx.update(|_, cx| cx.set_global(TestDecorators(vec![recorder, asker])));
        let (diff, _copy) = show(
            &tab,
            &[
                Entry::File("a.txt", lines("a", 20).as_bytes()),
                Entry::File("b.txt", lines("b", 20).as_bytes()),
            ],
            &[
                Entry::File("a.txt", changed("a", 20, 5).as_bytes()),
                Entry::File("b.txt", changed("b", 20, 5).as_bytes()),
            ],
            cx,
        );
        // Once every file was in, the asker ran alone, as it asked.
        assert_eq!(reasons("asker", cx).last(), Some(&Redecorate::Refresh),);

        cx.update(|window, cx| {
            redecorate(&diff, file("a.txt"), window, cx);
            redecorate(&diff, file("a.txt"), window, cx);
            redecorate(&diff, Redecorate::Whitespace, window, cx);
            // Nothing runs while it's asked.
            assert_eq!(cx.default_global::<Runs>().0.len(), 0);
        });
        cx.run_until_parked();
        // The asker asked for a.txt again while decorating it, which the run covered.
        assert_eq!(
            runs("recorded", cx),
            [(file("a.txt"), 2), (Redecorate::Whitespace, 2)]
        );

        cx.update(|window, cx| refresh::<Asker>(&diff, window, cx));
        cx.run_until_parked();
        let ran = cx.update(|_, cx| std::mem::take(&mut cx.default_global::<Runs>().0));
        assert_eq!(ran, [("asker", Redecorate::Refresh, 2)]);

        // A warning in a.txt's header: its badges again, and no run.
        let buffer = cx.update(|_, cx| {
            shown_files::read(&diff, cx, |files| files.get("a.txt").unwrap().buffer_id).unwrap()
        });
        cx.update(|window, cx| {
            let warning = Warning {
                label: "careful".into(),
                detail: "why".into(),
            };
            shown_files::set_warning(&diff, "a.txt", buffer, warning, window, cx)
        });
        cx.run_until_parked();
        assert_eq!(
            badge_label(&diff, "a.txt", BadgeStyle::Warning, cx).as_deref(),
            Some("careful")
        );
        assert_eq!(reasons("recorded", cx), []);
    }
}
