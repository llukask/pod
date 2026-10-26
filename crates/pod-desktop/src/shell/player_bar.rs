//! The player bar along the bottom of the window (D1 annotation 5). It stays
//! visible on every route and renders from the [`Player`] entity, so a
//! position tick re-renders the bar and nothing else.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    Context, Entity, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div,
    px,
};

use crate::player::{
    CycleSkipBack, CycleSkipForward, CycleSpeed, Player, PlayerHandle, SkipBack, SkipForward,
    Source, TogglePlayback,
};
use crate::shell::artwork::artwork;

#[derive(Debug)]
pub struct PlayerBar {
    player: Entity<Player>,
}

impl PlayerBar {
    /// Requires the [`PlayerHandle`] global.
    pub fn new(cx: &mut Context<Self>) -> Self {
        let player = cx.global::<PlayerHandle>().0.clone();
        cx.observe(&player, |_, _, cx| cx.notify()).detach();
        Self { player }
    }
}

// TODO: the artwork is the `artwork.file` the episode's load joined in, so
// an image cached after that shows from the next load on.
impl Render for PlayerBar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bar = h_flex()
            .id("player-bar")
            .test_support()
            .flex_shrink_0()
            .h(px(64.))
            .px_4()
            .gap_4()
            .border_t_1()
            .border_color(cx.theme().border)
            .text_sm();
        let player = self.player.read(cx);
        let Some(current) = player.current() else {
            return bar
                .text_color(cx.theme().muted_foreground)
                .child("Nothing playing");
        };
        let settings = player.settings();
        let episode = &current.episode;
        let source = match episode.source {
            Source::Download(_) => "from download",
            Source::Stream(_) => "streaming",
        };
        let position_ms = current.position_ms();
        let remaining = current
            .duration_ms()
            .map(|duration_ms| format!("−{}", clock(duration_ms - position_ms)))
            .unwrap_or_default();
        let fraction = current
            .duration_ms()
            .filter(|ms| *ms > 0)
            .map_or(0.0, |ms| (position_ms as f32 / ms as f32).clamp(0.0, 1.0));
        let playing = current.is_playing();
        let muted = cx.theme().muted_foreground;

        bar.child(
            // As wide as the speed column, so the controls stay centred.
            h_flex()
                .w(px(280.))
                .gap_3()
                .child(artwork(
                    "player-artwork",
                    episode.artwork_file.as_deref(),
                    px(48.),
                    cx,
                ))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .id("player-title")
                                .aria_label(SharedString::from(episode.title.clone()))
                                .test_support()
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .truncate()
                                .child(episode.title.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .truncate()
                                .child(format!("{} · {source}", episode.podcast_title)),
                        ),
                ),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .items_center()
                .child(
                    h_flex()
                        .gap_3()
                        .child(skip_button(
                            "player-skip-back",
                            IconName::Undo,
                            settings.skip_back_ms,
                            "Skip back",
                            false,
                        ))
                        .child(
                            Button::new("player-toggle")
                                .primary()
                                .icon(if playing {
                                    IconName::Pause
                                } else {
                                    IconName::Play
                                })
                                .accessibility_label(if playing { "Pause" } else { "Play" })
                                .on_click(|_, _, cx| cx.dispatch_action(&TogglePlayback)),
                        )
                        .child(skip_button(
                            "player-skip-forward",
                            IconName::Redo,
                            settings.skip_forward_ms,
                            "Skip forward",
                            true,
                        )),
                )
                .child(
                    // TODO: drag the bar to seek, once a slider fits here.
                    h_flex()
                        .w_full()
                        .max_w(px(560.))
                        .gap_2()
                        .text_xs()
                        .text_color(muted)
                        .child(
                            div()
                                .id("player-position")
                                .aria_label(SharedString::from(clock(position_ms)))
                                .test_support()
                                .child(clock(position_ms)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .child(Progress::new("player-progress").value(fraction * 100.0)),
                        )
                        .child(div().id("player-remaining").child(remaining)),
                ),
        )
        .child(
            // TODO: volume and the sleep timer from D1.
            h_flex().w(px(280.)).justify_end().child(
                Button::new("player-speed")
                    .outline()
                    .small()
                    .label(format!("{}×", settings.speed))
                    .tooltip("Playback speed, on every device")
                    .on_click(|_, _, cx| cx.dispatch_action(&CycleSpeed)),
            ),
        )
    }
}

/// A skip button labelled with its interval. A secondary click steps the
/// interval, which is a synced setting.
fn skip_button(
    id: &'static str,
    icon: IconName,
    interval_ms: i64,
    tooltip: &'static str,
    forward: bool,
) -> impl IntoElement {
    let seconds = interval_ms / 1000;
    div()
        .on_mouse_down(MouseButton::Right, move |_, _, cx| {
            if forward {
                cx.dispatch_action(&CycleSkipForward);
            } else {
                cx.dispatch_action(&CycleSkipBack);
            }
        })
        .child(
            Button::new(id)
                .ghost()
                .icon(icon)
                .label(seconds.to_string())
                .tooltip(format!("{tooltip} {seconds} s (right-click to change)"))
                .on_click(move |_, _, cx| {
                    if forward {
                        cx.dispatch_action(&SkipForward);
                    } else {
                        cx.dispatch_action(&SkipBack);
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
