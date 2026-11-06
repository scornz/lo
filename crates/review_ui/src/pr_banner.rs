//! Banners above a pull request's diff: what you should know about what it shows, such as a head
//! that isn't the one GitHub reports, or a checkout that failed (design §11).
//!
//! Each tab keeps its pull request's banners in a [`Banners`] entity and shows them under its
//! header, whichever diff it shows.

use gpui::{Context, IntoElement};
use ui::{Banner, Label, prelude::*};

/// What a banner is about. A pull request shows at most one banner of each kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BannerKind {
    /// The head shown isn't the one GitHub's API reports.
    HeadMismatch,
    /// The trusted checkout is on its way.
    CheckingOut,
    /// The trusted checkout failed, so everything shows from git.
    CheckoutFailed,
    /// The header shows what rozed saved about the pull request, not what GitHub says now.
    Saved,
}

/// One banner: its kind, how serious it is, and what it says.
#[derive(Clone, Debug, PartialEq)]
pub struct BannerMessage {
    pub kind: BannerKind,
    pub severity: Severity,
    pub text: SharedString,
}

/// A pull request's banners, in the order they were shown.
#[derive(Default)]
pub struct Banners {
    messages: Vec<BannerMessage>,
}

impl Banners {
    /// Shows `text` as the banner of `kind`, replacing any it had.
    pub fn show(
        &mut self,
        kind: BannerKind,
        severity: Severity,
        text: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        let message = BannerMessage {
            kind,
            severity,
            text: text.into(),
        };
        match self.messages.iter_mut().find(|shown| shown.kind == kind) {
            Some(shown) => *shown = message,
            None => self.messages.push(message),
        }
        cx.notify();
    }

    /// Hides the banner of `kind`, if one shows.
    pub fn hide(&mut self, kind: BannerKind, cx: &mut Context<Self>) {
        self.messages.retain(|shown| shown.kind != kind);
        cx.notify();
    }

    pub fn messages(&self) -> &[BannerMessage] {
        &self.messages
    }
}

/// The banners, one under another, or nothing when there are none.
pub fn render(messages: &[BannerMessage]) -> Option<impl IntoElement> {
    (!messages.is_empty()).then(|| {
        v_flex()
            .w_full()
            .px_2()
            .py_1()
            .gap_1()
            .children(messages.iter().map(|message| {
                Banner::new()
                    .severity(message.severity)
                    .wrap_content(true)
                    .child(Label::new(message.text.clone()).size(LabelSize::Small))
            }))
    })
}
