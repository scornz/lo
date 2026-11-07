//! The tab's header: the pull request's title, number and author, who the tab acts as and how
//! polling goes, and under it how far the tab has got (design §10 "Window").

use std::time::SystemTime;

use gpui::{AnyElement, App, Context, SharedString};
use ui::{Tooltip, prelude::*};

use super::{DiffSource, PrTab, Status};
use crate::account::Acting;
use crate::poll_state::{self, PollNote};

impl PrTab {
    /// The header's title: the pull request's, once known.
    pub fn title(&self) -> String {
        match (&self.header.title, &self.status) {
            (Some(title), _) => title.clone(),
            (None, Status::Failed(_)) => "Not opened".to_string(),
            (None, _) => "Loading…".to_string(),
        }
    }

    /// Who the header says the tab acts as.
    pub fn acting_text(&self) -> String {
        match &self.acting {
            Some(Acting::Account(login)) => format!("as {login}"),
            Some(Acting::Anonymous) => "not signed in".to_string(),
            None => String::new(),
        }
    }

    /// What the header says about how polling the pull request goes, if anything's wrong.
    pub fn poll_note(&self) -> Option<PollNote> {
        let login = self.acting.as_ref()?.login()?;
        self.watched.as_ref()?;
        poll_state::note(login, &self.poll, SystemTime::now())
    }

    pub(super) fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let number = self.link().number;
        let title = self.title();
        let acting = self.acting_text();
        let revision = self
            .revisions
            .as_ref()
            .and_then(|revisions| {
                let latest = revisions.latest()?;
                Some(format!(
                    "{} of {}",
                    revisions.label(&latest.head)?,
                    revisions.len()
                ))
            })
            .unwrap_or_else(|| "—".to_string());
        h_flex()
            .id("pr-header")
            .w_full()
            .flex_shrink_0()
            .px_3()
            .py_2()
            .gap_4()
            .justify_between()
            .border_b_1()
            .border_color(colors.border)
            .bg(colors.panel_background)
            .child(
                v_flex()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap_2()
                            .child(Label::new(title).size(LabelSize::Large).truncate())
                            .child(
                                Label::new(format!("#{number}"))
                                    .size(LabelSize::Large)
                                    .color(Color::Muted),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_1p5()
                            .child(
                                Label::new(self.repo_name())
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                            )
                            .children(self.header.author.as_ref().map(|author| {
                                Label::new(format!("by {author}"))
                                    .size(LabelSize::Small)
                                    .color(Color::Muted)
                            }))
                            .when(!acting.is_empty(), |this| {
                                this.child(
                                    Label::new("·").size(LabelSize::Small).color(Color::Muted),
                                )
                                .child(Label::new(acting).size(LabelSize::Small))
                            }),
                    )
                    .children(self.poll_note().map(render_poll_note)),
            )
            .child(
                h_flex()
                    .flex_shrink_0()
                    .gap_2()
                    // Placeholders until the merge panel and the revision picker arrive.
                    .child(chip("merge-status", "Merge status", "not checked yet", cx))
                    .child(chip("revision", "Revision", revision, cx)),
            )
    }

    pub(super) fn render_status(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let step = match &self.status {
            Status::Ready => None,
            Status::Loading(step) => Some((IconName::ArrowCircle, Color::Muted, step.clone())),
            Status::Offline(why) => Some((IconName::Warning, Color::Warning, why.clone())),
            Status::Failed(error) => Some((IconName::Warning, Color::Error, error.clone())),
        };
        let offline = matches!(self.status, Status::Offline(_));
        let github = self.diff_source == Some(DiffSource::GitHub);
        let whitespace = self.hide_whitespace;
        if step.is_none() && !github && !whitespace {
            return None;
        }
        Some(
            h_flex()
                .px_3()
                .py_1()
                .gap_2()
                .justify_between()
                .child(
                    h_flex()
                        .gap_2()
                        .children(step.map(|(icon, color, text)| {
                            h_flex()
                                .gap_2()
                                .child(Icon::new(icon).size(IconSize::Small).color(color))
                                .child(Label::new(text).size(LabelSize::Small).color(color))
                        }))
                        .when(offline, |this| this.children(self.render_actions(cx))),
                )
                .child(
                    h_flex()
                        .gap_3()
                        .when(whitespace, |this| {
                            this.child(
                                div()
                                    .id("whitespace-hidden")
                                    .child(
                                        Label::new("Whitespace changes hidden")
                                            .size(LabelSize::Small)
                                            .color(Color::Muted),
                                    )
                                    .tooltip(Tooltip::text(
                                        "Lines whose only change is to whitespace show as \
                                         unchanged, with the head's whitespace on both sides, \
                                         so text copied from the left side has the head's \
                                         whitespace too.",
                                    )),
                            )
                        })
                        .when(github, |this| {
                            this.child(
                                Label::new("GitHub's diff: changed lines only")
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                            )
                        }),
                )
                .into_any_element(),
        )
    }
}

/// The header's line about how polling goes, with GitHub's link to fix it if there's one.
fn render_poll_note(note: PollNote) -> impl IntoElement {
    let (icon, color) = match note.severity {
        Severity::Error => (IconName::XCircle, Color::Error),
        Severity::Warning => (IconName::Warning, Color::Warning),
        _ => (IconName::Info, Color::Muted),
    };
    h_flex()
        .id("poll-note")
        .min_w_0()
        .gap_1p5()
        .child(Icon::new(icon).size(IconSize::Small).color(color))
        .child(
            Label::new(note.text)
                .size(LabelSize::Small)
                .color(color)
                .truncate(),
        )
        .children(note.link.map(|link| {
            let url = link.url().to_string();
            Button::new("poll-note-link", link.label())
                .label_size(LabelSize::Small)
                .on_click(move |_, _, cx| cx.open_url(&url))
        }))
}

/// A placeholder in the header: a name and what it says.
fn chip(
    id: &'static str,
    name: &'static str,
    value: impl Into<SharedString>,
    cx: &App,
) -> impl IntoElement {
    h_flex()
        .id(id)
        .gap_1()
        .px_2()
        .py_0p5()
        .rounded_sm()
        .border_1()
        .border_color(cx.theme().colors().border_variant)
        .child(Label::new(name).size(LabelSize::XSmall).color(Color::Muted))
        .child(Label::new(value.into()).size(LabelSize::Small))
        .tooltip(Tooltip::text(format!("{name} isn't built yet")))
}
