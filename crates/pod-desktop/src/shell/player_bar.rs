//! The player bar along the bottom of the window (D1 annotation 5): what's
//! playing, the transport with its scrubber, and the speed, over the
//! show's blurred cover. It renders from the [`Player`] entity, so a
//! position tick re-renders the bar and nothing else, and it is hidden
//! while nothing is loaded.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, Bounds, Context, Entity, FontWeight, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, Pixels, Render, Role, SharedString, StatefulInteractiveElement as _,
    Styled as _, TestSupportExt as _, Window, canvas, div, px,
};
use pod_protocol::EpisodeId;

use super::icon::PodIcon;
use super::ui::{ROUND, art_shadow, icon, tabular};
use super::wash;
use crate::player::mpv::Failure;
use crate::player::{
    CycleSkipBack, CycleSkipForward, CycleSpeed, Player, PlayerHandle, SkipBack, SkipForward,
    TogglePlayback,
};
use crate::shell::artwork::artwork;
use crate::theme::{FONT_NARROW, mix, palette, size};

#[derive(Debug)]
pub struct PlayerBar {
    player: Entity<Player>,
    /// Where the scrubber's track was last painted, to turn a click into a
    /// position.
    track: Rc<Cell<Bounds<Pixels>>>,
}

impl PlayerBar {
    /// Requires the [`PlayerHandle`] global.
    pub fn new(cx: &mut Context<Self>) -> Self {
        let player = cx.global::<PlayerHandle>().0.clone();
        cx.observe(&player, |_, _, cx| cx.notify()).detach();
        Self {
            player,
            track: Rc::default(),
        }
    }
}

// TODO: the artwork is the `artwork.file` the episode's load joined in, so
// an image cached after that shows from the next load on.
impl Render for PlayerBar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let player = self.player.read(cx);
        let Some(current) = player.current() else {
            return div().into_any_element();
        };
        let settings = player.settings();
        let episode = current.episode.clone();
        let position_ms = current.position_ms();
        let duration_ms = current.duration_ms().filter(|ms| *ms > 0);
        let remaining = duration_ms
            .map(|duration_ms| format!("−{}", clock(duration_ms - position_ms)))
            .unwrap_or_default();
        let fraction =
            duration_ms.map_or(0.0, |ms| (position_ms as f32 / ms as f32).clamp(0.0, 1.0));
        let playing = current.is_playing();
        let failure = current.failure();
        let wash = wash::layer(episode.artwork_file.as_deref(), 0., cx);

        let now_playing = h_flex()
            .flex_1()
            .min_w_0()
            .gap(px(14.))
            .child(
                artwork(
                    "player-artwork",
                    episode.artwork_file.as_deref(),
                    Some(&episode.podcast_title),
                    px(52.),
                    cx,
                )
                .shadow(art_shadow(cx)),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .id("player-title")
                            .aria_label(SharedString::from(episode.title.clone()))
                            .test_support()
                            .font_weight(FontWeight::SEMIBOLD)
                            .truncate()
                            .child(episode.title.clone()),
                    )
                    .child(
                        div()
                            .text_size(size::SMALL)
                            .text_color(p.muted)
                            .truncate()
                            .child(episode.podcast_title.clone()),
                    ),
            );

        let controls = h_flex()
            .justify_center()
            .gap(px(18.))
            .child(skip_button(
                "player-skip-back",
                PodIcon::SkipBack,
                settings.skip_back_ms,
                "Skip back",
                false,
                cx,
            ))
            .child(
                // Ink, not the accent: the accent marks state, and this is
                // the bar's one control that matters most.
                div()
                    .id("player-toggle")
                    .role(Role::Button)
                    .aria_label(if playing { "Pause" } else { "Play" })
                    .test_support()
                    .size(px(44.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(ROUND)
                    .bg(p.ink)
                    .text_color(p.background)
                    .cursor_pointer()
                    .hover(|this| this.bg(mix(p.ink, p.background, 0.15)))
                    .active(|this| this.bg(mix(p.ink, p.background, 0.25)))
                    .child(div().when(!playing, |this| this.ml(px(2.))).child(icon(
                        if playing {
                            PodIcon::Pause
                        } else {
                            PodIcon::Play
                        },
                        px(20.),
                    )))
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(TogglePlayback), cx)),
            )
            .child(skip_button(
                "player-skip-forward",
                PodIcon::SkipForward,
                settings.skip_forward_ms,
                "Skip forward",
                true,
                cx,
            ));

        let timeline = match failure {
            Some(failure) => failure_row(failure, episode.id, cx).into_any_element(),
            None => {
                let clock_text = |id: &'static str, text: String, align_right: bool| {
                    let text = SharedString::from(text);
                    div()
                        .id(id)
                        .aria_label(text.clone())
                        .test_support()
                        .flex_none()
                        .min_w(px(52.))
                        .font_family(FONT_NARROW)
                        .text_size(px(12.))
                        .font_weight(FontWeight::MEDIUM)
                        .font_features(tabular())
                        .text_color(p.muted)
                        .when(align_right, |this| this.text_right())
                        .child(text)
                };
                h_flex()
                    .w_full()
                    .gap(px(10.))
                    .child(clock_text("player-position", clock(position_ms), true))
                    .child(self.scrubber(fraction, duration_ms, cx))
                    .child(clock_text("player-remaining", remaining, false))
                    .into_any_element()
            }
        };

        h_flex()
            .id("player-bar")
            .test_support()
            .relative()
            .overflow_hidden()
            .flex_shrink_0()
            .min_h(px(88.))
            .px(px(24.))
            .py(px(12.))
            .gap(px(24.))
            .border_t_1()
            .border_color(p.line)
            .bg(p.surface)
            .children(wash)
            // The sides share what the transport leaves equally, so the
            // transport stays centred.
            .child(div().relative().flex_1().min_w_0().child(now_playing))
            .child(
                v_flex()
                    .relative()
                    .flex_shrink(1.)
                    .w(px(600.))
                    .min_w(px(280.))
                    .gap(px(4.))
                    .child(controls)
                    .child(timeline),
            )
            .child(
                h_flex().relative().flex_1().min_w_0().justify_end().child(
                    Button::new("player-speed")
                        .ghost()
                        .rounded(ROUND)
                        .h(px(32.))
                        .min_w(px(56.))
                        .px(px(12.))
                        .border_1()
                        .border_color(p.ink.opacity(0.22))
                        .font_weight(FontWeight::SEMIBOLD)
                        .label(format!("{}×", settings.speed))
                        .tooltip("Playback speed, on every device")
                        .on_click(|_, window, cx| window.dispatch_action(Box::new(CycleSpeed), cx)),
                ),
            )
            .into_any_element()
    }
}

impl PlayerBar {
    /// A thin track filled with the accent up to `fraction`, its thumb
    /// growing under the pointer. Clicking or dragging seeks.
    fn scrubber(
        &self,
        fraction: f32,
        duration_ms: Option<i64>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let p = palette(cx);
        let track = self.track.clone();
        let seek_to = {
            let track = self.track.clone();
            let player = self.player.clone();
            move |x: Pixels, cx: &mut App| {
                let (Some(duration_ms), bounds) = (duration_ms, track.get()) else {
                    return;
                };
                if bounds.size.width <= px(0.) {
                    return;
                }
                let share = ((x - bounds.left()) / bounds.size.width).clamp(0., 1.);
                let target = (duration_ms as f64 * f64::from(share)) as i64;
                player.update(cx, |player, cx| player.seek(target, cx));
            }
        };
        let on_drag = seek_to.clone();
        div()
            .id("player-scrubber")
            .group("scrubber")
            .relative()
            .flex_1()
            .min_w_0()
            .h(px(20.))
            .when(duration_ms.is_some(), |this| this.cursor_pointer())
            .child(
                canvas(move |bounds, _, _| track.set(bounds), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            )
            .child(
                div()
                    .absolute()
                    .top(px(8.))
                    .left_0()
                    .right_0()
                    .h(px(4.))
                    .rounded(px(2.))
                    .bg(p.ink.opacity(0.16))
                    .child(
                        div()
                            .h_full()
                            .w(gpui_kit::relative(fraction))
                            .rounded(px(2.))
                            .bg(p.accent),
                    ),
            )
            .when(duration_ms.is_some(), |this| {
                this.child(
                    div()
                        .absolute()
                        .top(px(4.))
                        .left(gpui_kit::relative(fraction))
                        .ml(px(-6.))
                        .size(px(12.))
                        .rounded(ROUND)
                        .bg(p.ink)
                        .opacity(0.)
                        .group_hover("scrubber", |this| this.opacity(1.)),
                )
            })
            .on_mouse_down(MouseButton::Left, move |event, _, cx| {
                seek_to(event.position.x, cx);
            })
            .on_mouse_move(move |event, _, cx| {
                if event.pressed_button == Some(MouseButton::Left) {
                    on_drag(event.position.x, cx);
                }
            })
    }
}

/// What the bar says in place of the timeline after a failed play.
fn failure_message(failure: Failure) -> &'static str {
    match failure {
        Failure::NoAudioOutput => "Couldn't play: no audio output",
        Failure::Unloadable => "Couldn't load the episode",
        Failure::Other => "Couldn't play the episode",
    }
}

fn failure_row(failure: Failure, episode: EpisodeId, cx: &App) -> impl IntoElement {
    let p = palette(cx);
    let message = failure_message(failure);
    h_flex()
        .justify_center()
        .gap(px(8.))
        .text_size(size::SMALL)
        .child(
            div()
                .id("player-failure")
                .aria_label(message)
                .test_support()
                .text_color(p.danger)
                .child(message),
        )
        .child(
            Button::new("player-retry")
                .ghost()
                .small()
                .rounded(ROUND)
                .label("Retry")
                .on_click(move |_, _, cx| crate::player::play(episode, cx)),
        )
}

/// A circular-arrow skip button with its interval printed inside. A
/// secondary click steps the interval, which is a synced setting.
fn skip_button(
    id: &'static str,
    glyph: PodIcon,
    interval_ms: i64,
    tooltip: &'static str,
    forward: bool,
    cx: &App,
) -> impl IntoElement {
    let p = palette(cx);
    let seconds = interval_ms / 1000;
    div()
        .on_mouse_down(MouseButton::Right, move |_, window, cx| {
            if forward {
                window.dispatch_action(Box::new(CycleSkipForward), cx);
            } else {
                window.dispatch_action(Box::new(CycleSkipBack), cx);
            }
        })
        .child(
            Button::new(id)
                .ghost()
                .rounded(ROUND)
                .size(px(40.))
                .text_color(p.ink)
                .accessibility_label(format!("{tooltip} {seconds} s"))
                .child(
                    div()
                        .relative()
                        .size(px(28.))
                        .child(icon(glyph, px(28.)))
                        .child(
                            div()
                                .absolute()
                                .inset_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .pt(px(1.))
                                .text_size(px(9.))
                                .font_weight(FontWeight::BOLD)
                                .font_features(tabular())
                                .child(seconds.to_string()),
                        ),
                )
                .tooltip(format!("{tooltip} {seconds} s (right-click to change)"))
                .on_click(move |_, window, cx| {
                    if forward {
                        window.dispatch_action(Box::new(SkipForward), cx);
                    } else {
                        window.dispatch_action(Box::new(SkipBack), cx);
                    }
                }),
        )
}

/// "4:05" or "1:02:09".
fn clock(ms: i64) -> String {
    let seconds = ms.max(0) / 1000;
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::clock;

    #[test]
    fn clock_shows_hours_only_when_needed() {
        assert_eq!(clock(0), "0:00");
        assert_eq!(clock(245_999), "4:05");
        assert_eq!(clock(3_729_000), "1:02:09");
        assert_eq!(clock(-5), "0:00");
    }
}
