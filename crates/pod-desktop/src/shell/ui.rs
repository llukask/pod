//! The small pieces every page is built from, drawn to the design brief:
//! pill buttons, round icon buttons, the progress-ring play button, the
//! logo, page titles and empty states.

use std::f32::consts::TAU;
use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, BoxShadow, ClickEvent, Div, ElementId, FontFeatures, FontWeight,
    InteractiveElement as _, ParentElement as _, PathBuilder, Pixels, Role, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, canvas, div, point,
    px, relative, svg,
};

use super::icon::PodIcon;
use crate::theme::{self, FONT_DISPLAY, Palette, palette, size};

/// What `.test_support()` turns an element into: the element itself, or
/// in tests a wrapper that registers it for UI queries and forwards its
/// styling.
pub type Observed<E> = gpui_kit::base::ObservedElement<E>;

/// Fully round: pills and circles.
pub const ROUND: Pixels = px(999.);

/// Tabular figures, so times and durations line up and don't jitter.
pub fn tabular() -> FontFeatures {
    FontFeatures(Arc::new(vec![("tnum".into(), 1)]))
}

/// `icon` at `size`, in the text colour.
pub fn icon(icon: PodIcon, size: Pixels) -> Icon {
    Icon::new(icon).with_size(size)
}

// ==============================================================================
// Buttons
// ==============================================================================

/// The common shape: a pill 38 px high, its label medium weight. The
/// colours come from gpui-component's variants, which [`crate::theme`]
/// points at Pod's palette. (Its custom variant draws the background at a
/// fifth of its strength, too pale for a primary action.)
fn pill_base(button: Button) -> Button {
    button
        .rounded(ROUND)
        .h(px(38.))
        .px(px(16.))
        .font_weight(FontWeight::MEDIUM)
}

/// A secondary action: surface pill with a hairline border.
pub fn pill(id: impl Into<ElementId>, _: &App) -> Button {
    pill_base(Button::new(id))
}

/// The one primary action of a pane: accent pill.
pub fn primary_pill(id: impl Into<ElementId>, _: &App) -> Button {
    pill_base(Button::new(id).primary()).font_weight(FontWeight::SEMIBOLD)
}

/// A quiet action: no border, muted until hovered.
pub fn quiet_pill(id: impl Into<ElementId>, cx: &App) -> Button {
    pill_base(Button::new(id).ghost()).text_color(palette(cx).muted)
}

/// A destructive action that is the obvious next step, as leaving a feed
/// that is gone.
pub fn danger_pill(id: impl Into<ElementId>, _: &App) -> Button {
    pill_base(Button::new(id).danger()).font_weight(FontWeight::SEMIBOLD)
}

/// A round 36 px icon button, muted until hovered.
pub fn icon_button(id: impl Into<ElementId>, glyph: PodIcon, cx: &App) -> Button {
    Button::new(id)
        .ghost()
        .rounded(ROUND)
        .size(px(36.))
        .text_color(palette(cx).muted)
        .child(icon(glyph, px(20.)))
}

// ==============================================================================
// The play button with its progress ring
// ==============================================================================

/// What an episode's play button shows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ring {
    /// How much has been heard, 0 to 1.
    pub fraction: f32,
    pub played: bool,
    /// The episode is the one in the player: the button fills with the
    /// accent.
    pub current: bool,
    /// The player is playing it: the button shows pause.
    pub playing: bool,
}

impl Ring {
    /// The ring for an episode heard to `position_ms` of `duration_ms`.
    pub fn new(position_ms: i64, duration_ms: Option<i64>, played: bool) -> Ring {
        let fraction = match duration_ms {
            _ if played => 1.,
            Some(duration) if duration > 0 => (position_ms as f32 / duration as f32).clamp(0., 1.),
            _ => 0.,
        };
        Ring {
            fraction,
            played,
            current: false,
            playing: false,
        }
    }

    /// The share of the circle drawn. A started episode shows at least a
    /// short arc, which reads as progress; a sliver under that reads as a
    /// stray dot.
    pub fn shown(&self) -> f32 {
        if self.fraction > 0. {
            self.fraction.max(0.06)
        } else {
            0.
        }
    }
}

/// The round play button: a thin ring around it fills clockwise from 12
/// o'clock with how much of the episode has been heard. `label` names the
/// episode for screen readers ("Play <title>").
pub fn play_button(
    id: impl Into<ElementId>,
    ring: Ring,
    diameter: Pixels,
    label: &str,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Observed<Stateful<Div>> {
    let p = palette(cx);
    let (track, fill) = if ring.current {
        (None, p.on_accent.opacity(0.7))
    } else if ring.played {
        (Some(p.line), p.faint)
    } else {
        (Some(p.line), p.accent)
    };
    let shown = ring.shown();
    let glyph = if ring.playing {
        PodIcon::Pause
    } else {
        PodIcon::Play
    };
    let verb = if ring.playing { "Pause" } else { "Play" };
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(SharedString::from(format!("{verb} {label}")))
        .test_support()
        .relative()
        .flex_none()
        .size(diameter)
        .rounded(ROUND)
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .text_color(if ring.current { p.on_accent } else { p.ink })
        .when(ring.current, |this| this.bg(p.accent))
        .when(!ring.current, |this| this.hover(|this| this.bg(p.hover)))
        .when_some(track, |this, track| {
            this.child(
                div()
                    .absolute()
                    .inset(px(1.))
                    .rounded(ROUND)
                    .border_2()
                    .border_color(track),
            )
        })
        .when(shown > 0., |this| {
            this.child(
                canvas(
                    |_, _, _| (),
                    move |bounds, (), window, _| {
                        let center = bounds.center();
                        let radius = bounds.size.width / 2. - px(2.);
                        let steps = ((shown * 96.).ceil() as usize).max(2);
                        let mut path = PathBuilder::stroke(px(2.));
                        for step in 0..=steps {
                            // Clockwise from 12 o'clock.
                            let angle = TAU * shown * step as f32 / steps as f32 - TAU / 4.;
                            let at = point(
                                center.x + radius * angle.cos(),
                                center.y + radius * angle.sin(),
                            );
                            if step == 0 {
                                path.move_to(at);
                            } else {
                                path.line_to(at);
                            }
                        }
                        if let Ok(path) = path.build() {
                            window.paint_path(path, fill);
                        }
                    },
                )
                .absolute()
                .size_full(),
            )
        })
        .child(
            // The play triangle's weight sits left of its box; nudged, it
            // looks centred.
            div()
                .when(!ring.playing, |this| this.ml(px(1.5)))
                .child(icon(glyph, diameter * 0.43)),
        )
        .on_click(on_click)
}

// ==============================================================================
// Brand, headings, empty states
// ==============================================================================

/// The pea-pod logo: the pod on an accent square, its peas cut out in the
/// accent.
pub fn logo(size: Pixels, cx: &App) -> Div {
    let p = palette(cx);
    let layer = |glyph: PodIcon, color| {
        svg()
            .path(gpui_kit::assets::IconNamed::path(glyph))
            .absolute()
            .size_full()
            .text_color(color)
    };
    div()
        .relative()
        .flex_none()
        .size(size)
        .rounded(size * 0.25)
        .bg(p.accent)
        .child(layer(PodIcon::LogoPod, p.on_accent))
        .child(layer(PodIcon::LogoPeas, p.accent))
}

/// A page's title, which tests read as `page-title`.
pub fn page_title(text: impl Into<SharedString>) -> Observed<Stateful<Div>> {
    let text = text.into();
    div()
        .id("page-title")
        .aria_label(text.clone())
        .test_support()
        .font_family(FONT_DISPLAY)
        .text_size(size::PAGE)
        .font_weight(FontWeight::BOLD)
        .line_height(relative(1.1))
        .child(text)
}

/// A dialog's title, in the display face.
pub fn dialog_title(text: impl Into<SharedString>) -> Div {
    div()
        .font_family(FONT_DISPLAY)
        .text_size(size::TITLE)
        .font_weight(FontWeight::BOLD)
        .line_height(relative(1.2))
        .child(text.into())
}

/// "Today", "Account": a small semibold muted heading.
pub fn small_heading(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .text_size(size::SMALL)
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(palette(cx).muted)
        .child(text.into())
}

/// A key or chord, as the search field's `Ctrl K`.
pub fn kbd(text: &'static str, cx: &App) -> Div {
    let p = palette(cx);
    div()
        .flex_none()
        .px(px(7.))
        .py(px(1.))
        .rounded(px(6.))
        .border_1()
        .border_b_2()
        .border_color(p.line)
        .bg(p.surface)
        .text_size(px(12.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(p.muted)
        .child(text)
}

/// An empty page or list: an icon, a title in the display face, one muted
/// line, and the action that fills it.
pub fn empty_state(
    id: impl Into<ElementId>,
    glyph: PodIcon,
    title: &'static str,
    line: impl Into<SharedString>,
    action: Option<AnyElement>,
    cx: &App,
) -> Observed<Stateful<Div>> {
    let p = palette(cx);
    v_flex()
        .id(id)
        .test_support()
        .w_full()
        .items_center()
        .gap(px(6.))
        .px(px(24.))
        .py(px(72.))
        .text_color(p.faint)
        .child(icon(glyph, px(36.)))
        .child(
            div()
                .mt(px(8.))
                .font_family(FONT_DISPLAY)
                .text_size(size::TITLE)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(p.ink)
                .child(title),
        )
        .child(div().max_w(px(420.)).text_center().child(line.into()))
        .when_some(action, |this, action| {
            this.child(div().mt(px(16.)).child(action))
        })
}

/// A segmented control, as the show page's All / Unplayed: `options` are
/// `(id, label, active)`, and `on_select` gets the chosen index.
pub fn segmented(
    options: &[(&'static str, &'static str, bool)],
    on_select: impl Fn(usize, &mut Window, &mut App) + Clone + 'static,
    cx: &App,
) -> Div {
    let p = palette(cx);
    h_flex()
        .flex_none()
        .p(px(3.))
        .rounded(ROUND)
        .bg(p.hover)
        .children(
            options
                .iter()
                .enumerate()
                .map(|(index, &(id, label, active))| {
                    let on_select = on_select.clone();
                    div()
                        .id(id)
                        .role(Role::Button)
                        .aria_label(label)
                        .aria_selected(active)
                        .test_support()
                        .h(px(30.))
                        .px(px(16.))
                        .flex()
                        .items_center()
                        .rounded(ROUND)
                        .text_size(size::SMALL)
                        .font_weight(FontWeight::MEDIUM)
                        .cursor_pointer()
                        .map(|this| {
                            if active {
                                this.bg(p.surface)
                                    .text_color(p.ink)
                                    .shadow(vec![soft_shadow(p, 0.1, px(1.), px(2.))])
                            } else {
                                this.text_color(p.muted)
                                    .hover(|this| this.text_color(p.ink))
                            }
                        })
                        .child(label)
                        .on_click(move |_, window, cx| on_select(index, window, cx))
                }),
        )
}

/// A soft drop shadow `blur` wide, `y` down, at `alpha` of black.
pub fn soft_shadow(_: &Palette, alpha: f32, y: Pixels, blur: Pixels) -> BoxShadow {
    BoxShadow {
        color: gpui_kit::black().opacity(alpha),
        offset: point(px(0.), y),
        blur_radius: blur,
        spread_radius: px(0.),
        inset: false,
    }
}

/// The shadow under large cover art and lifted cards.
pub fn art_shadow(cx: &App) -> Vec<BoxShadow> {
    let p = palette(cx);
    vec![
        BoxShadow {
            color: gpui_kit::black().opacity(if p.is_dark { 0.4 } else { 0.08 }),
            offset: point(px(0.), px(1.)),
            blur_radius: px(2.),
            spread_radius: px(0.),
            inset: false,
        },
        BoxShadow {
            color: p.art_shadow,
            offset: point(px(0.), px(8.)),
            blur_radius: px(24.),
            spread_radius: px(-8.),
            inset: false,
        },
    ]
}

/// The shadow under dialogs, menus and the sign-in card.
pub fn pop_shadow(cx: &App) -> Vec<BoxShadow> {
    let dark = theme::palette(cx).is_dark;
    vec![
        BoxShadow {
            color: gpui_kit::black().opacity(if dark { 0.4 } else { 0.08 }),
            offset: point(px(0.), px(2.)),
            blur_radius: px(6.),
            spread_radius: px(0.),
            inset: false,
        },
        BoxShadow {
            color: gpui_kit::black().opacity(if dark { 0.8 } else { 0.3 }),
            offset: point(px(0.), px(16.)),
            blur_radius: px(40.),
            spread_radius: px(-12.),
            inset: false,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::Ring;

    #[test]
    fn the_ring_shows_progress_with_a_minimum_arc_and_fills_once_played() {
        assert_eq!(Ring::new(0, Some(1000), false).shown(), 0.);
        assert_eq!(Ring::new(1, Some(1000), false).shown(), 0.06);
        assert_eq!(Ring::new(500, Some(1000), false).shown(), 0.5);
        assert_eq!(Ring::new(0, None, true).shown(), 1.);
        assert_eq!(Ring::new(2000, Some(1000), false).shown(), 1.);
        assert_eq!(Ring::new(10, None, false).shown(), 0., "no length, no ring");
    }
}
