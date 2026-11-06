//! Banners above a pull request's diff: what you should know about what it shows, such as a head
//! that isn't the one GitHub reports, or a checkout that failed (design §11).
//!
//! Each pull request's diff keeps its banners in a [`Banners`] entity, which an addon on its editor
//! points at. [`BannerBar`], a row in every pane's toolbar, shows the banners of the pane's active
//! pull request, and hides when there are none.

use std::any::Any;

use editor::{Addon, Editor};
use gpui::{Context, Entity, EventEmitter, Render, Subscription, Window};
use ui::{Banner, Label, prelude::*};
use workspace::{ItemHandle, ToolbarItemEvent, ToolbarItemLocation, ToolbarItemView};

/// What a banner is about. A pull request shows at most one banner of each kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BannerKind {
    /// The head shown isn't the one GitHub's API reports.
    HeadMismatch,
    /// The trusted checkout is on its way.
    CheckingOut,
    /// The trusted checkout failed, so everything shows from git.
    CheckoutFailed,
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

/// The addon on a pull request's editor that points at its banners.
pub struct BannersAddon(pub Entity<Banners>);

impl Addon for BannersAddon {
    fn to_any(&self) -> &dyn Any {
        self
    }
}

/// The toolbar row that shows the active pull request's banners.
#[derive(Default)]
pub struct BannerBar {
    banners: Option<Entity<Banners>>,
    _observing: Option<Subscription>,
}

impl BannerBar {
    fn location(&self, cx: &App) -> ToolbarItemLocation {
        match &self.banners {
            Some(banners) if !banners.read(cx).messages().is_empty() => {
                ToolbarItemLocation::Secondary
            }
            _ => ToolbarItemLocation::Hidden,
        }
    }
}

impl EventEmitter<ToolbarItemEvent> for BannerBar {}

impl ToolbarItemView for BannerBar {
    fn set_active_pane_item(
        &mut self,
        active_pane_item: Option<&dyn ItemHandle>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> ToolbarItemLocation {
        // A pull request's `SplittableEditor` acts as its right side's editor, which holds the
        // addon.
        self.banners = active_pane_item
            .and_then(|item| item.act_as::<Editor>(cx))
            .and_then(|editor| {
                editor
                    .read(cx)
                    .addon::<BannersAddon>()
                    .map(|addon| addon.0.clone())
            });
        self._observing = self.banners.as_ref().map(|banners| {
            cx.observe(banners, |bar, _, cx| {
                cx.emit(ToolbarItemEvent::ChangeLocation(bar.location(cx)));
                cx.notify();
            })
        });
        self.location(cx)
    }
}

impl Render for BannerBar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let messages = self
            .banners
            .as_ref()
            .map(|banners| banners.read(cx).messages().to_vec())
            .unwrap_or_default();
        v_flex()
            .w_full()
            .gap_1()
            .children(messages.into_iter().map(|message| {
                Banner::new()
                    .severity(message.severity)
                    .wrap_content(true)
                    .child(Label::new(message.text).size(LabelSize::Small))
            }))
    }
}
