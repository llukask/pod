//! The player bar along the bottom of the window (D1 annotation 5): what's
//! playing, the transport with its scrubber, and the speed, over the
//! show's blurred cover. It renders from the [`Player`] entity, so a
//! position tick re-renders the bar and nothing else, and it is hidden
//! while nothing is loaded.
//!
//! Beside the speed sits the output picker (spec 13.4): "This device" and
//! the server's Sonos groups. While a group plays the episode, the line
//! under the title says where, and the speed steps aside for the group's
//! volume: Sonos plays only at 1x.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::notification::{Notification, NotificationType};
use gpui_kit::component::popover::{Popover, PopoverState};
use gpui_kit::component::{Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, App, Bounds, ClickEvent, Context, Div, Entity, FontWeight, InteractiveElement as _,
    IntoElement, MouseButton, ParentElement as _, Pixels, Render, Role, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, TestSupportExt as _,
    WeakEntity, Window, canvas, div, px,
};
use pod_protocol::{EpisodeId, SonosPlayer, SonosPlayerState};

use super::icon::PodIcon;
use super::ui::{ROUND, art_shadow, icon, tabular};
use super::wash;
use crate::player::mpv::Failure;
use crate::player::{
    CycleSkipBack, CycleSkipForward, CycleSpeed, Player, PlayerHandle, SkipBack, SkipForward,
    SonosNotice, TogglePlayback,
};
use crate::shell::artwork::artwork;
use crate::sonos::{Remote, Target};
use crate::theme::{FONT_NARROW, mix, palette, size};

#[derive(Debug)]
pub struct PlayerBar {
    player: Entity<Player>,
    /// Where the scrubber's track was last painted, to turn a click into a
    /// position.
    track: Rc<Cell<Bounds<Pixels>>>,
    /// The same for the volume slider.
    volume_track: Rc<Cell<Bounds<Pixels>>>,
    /// The groups the output picker lists, fetched when it opens; `None`
    /// while they load.
    groups: Option<Vec<SonosPlayer>>,
    /// Keeps the picker's button while its menu is open, even when the
    /// list comes back empty.
    picker_open: bool,
    _listing: Option<Task<()>>,
    _notices: Subscription,
}

impl PlayerBar {
    /// Requires the [`PlayerHandle`] global.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let player = cx.global::<PlayerHandle>().0.clone();
        cx.observe(&player, |_, _, cx| cx.notify()).detach();
        let notices = cx.subscribe_in(
            &player,
            window,
            |_, _, SonosNotice(message): &SonosNotice, window, cx| {
                let message = message.clone();
                // The player emits this from inside its own update; the
                // notification list is the window root's.
                window.defer(cx, move |window, cx| {
                    window.push_notification(sonos_notification(message), cx);
                });
            },
        );
        Self {
            player,
            track: Rc::default(),
            volume_track: Rc::default(),
            groups: None,
            picker_open: false,
            _listing: None,
            _notices: notices,
        }
    }

    /// The picker opened or closed. Opening lists the groups afresh.
    fn picker_changed(&mut self, open: bool, cx: &mut Context<Self>) {
        self.picker_open = open;
        if open {
            self.groups = None;
            let listed = self.player.update(cx, |player, cx| player.list_players(cx));
            self._listing = Some(cx.spawn(async move |this, cx| {
                let groups = listed.await;
                let _ = this.update(cx, |this, cx| {
                    this.groups = Some(groups);
                    cx.notify();
                });
            }));
        } else {
            self._listing = None;
        }
        cx.notify();
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
        let remote = current.remote();
        let target = remote.map(|remote| remote.target.clone());
        let where_text = remote.map(where_label);
        let volume = remote
            .filter(|remote| !remote.is_connecting())
            .and_then(Remote::volume);
        let show_picker = player.sonos_available() || remote.is_some() || self.picker_open;
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
                    .child(match where_text {
                        Some(label) => where_line(label, cx).into_any_element(),
                        None => div()
                            .text_size(size::SMALL)
                            .text_color(p.muted)
                            .truncate()
                            .child(episode.podcast_title.clone())
                            .into_any_element(),
                    }),
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
                h_flex()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .justify_end()
                    .gap(px(10.))
                    .when_some(volume.zip(target.as_ref()), |this, (volume, target)| {
                        this.child(self.volume_slider(volume, &target.name, cx))
                    })
                    .when(show_picker, |this| {
                        this.child(self.output_picker(target.as_ref(), cx))
                    })
                    .when(target.is_none(), |this| {
                        this.child(
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
                                .on_click(|_, window, cx| {
                                    window.dispatch_action(Box::new(CycleSpeed), cx);
                                }),
                        )
                    }),
            )
            .into_any_element()
    }
}

impl PlayerBar {
    /// The episode's track. Clicking or dragging seeks.
    fn scrubber(
        &self,
        fraction: f32,
        duration_ms: Option<i64>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let player = self.player.clone();
        slider(
            "player-scrubber",
            fraction,
            self.track.clone(),
            duration_ms.is_some(),
            move |share, cx| {
                let Some(duration_ms) = duration_ms else {
                    return;
                };
                let target = (duration_ms as f64 * f64::from(share)) as i64;
                player.update(cx, |player, cx| player.seek(target, cx));
            },
            cx,
        )
        .flex_1()
    }

    /// The group's volume, in the speed's place while a group plays the
    /// episode. Dragging sends the value the pointer ends on.
    fn volume_slider(&self, volume: u8, name: &str, cx: &mut Context<Self>) -> impl IntoElement {
        let player = self.player.clone();
        slider(
            "player-volume",
            f32::from(volume) / 100.,
            self.volume_track.clone(),
            true,
            move |share, cx| {
                let volume = (share * 100.).round() as u8;
                player.update(cx, |player, cx| player.set_volume(volume, cx));
            },
            cx,
        )
        .w(px(132.))
        .flex_none()
        .aria_label(SharedString::from(format!("Volume on {name}")))
        .aria_value(SharedString::from(volume.to_string()))
        .test_support()
    }

    /// The speaker button and its menu: "This device" and each group,
    /// listed when the menu opens. In the accent while a group has the
    /// episode, since that is state.
    fn output_picker(&self, target: Option<&Target>, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let bar = cx.entity().downgrade();
        let trigger = Button::new("player-output")
            .ghost()
            .rounded(ROUND)
            .h(px(32.))
            .w(px(40.))
            .border_1()
            .map(|this| match target {
                Some(_) => this
                    .border_color(p.accent.opacity(0.55))
                    .bg(p.accent_soft)
                    .text_color(p.accent_ink),
                None => this.border_color(p.ink.opacity(0.22)).text_color(p.ink),
            })
            .accessibility_label("Play on")
            .child(icon(PodIcon::Speaker, px(18.)))
            .tooltip(match target {
                Some(target) => format!("Playing on {}", target.name),
                None => "Play on a speaker".to_owned(),
            });
        let changed = bar.clone();
        Popover::new("player-output-menu")
            // The bar sits along the bottom of the window, so the menu opens
            // upward, right-aligned with the button.
            .anchor(Anchor::BottomRight)
            .trigger(trigger)
            .on_open_change(move |open, _, cx| {
                let open = *open;
                let _ = changed.update(cx, |bar, cx| bar.picker_changed(open, cx));
            })
            .content(move |_, _, cx| {
                // Read at render time, so the menu follows the list and the
                // player.
                let Some(bar) = bar.upgrade() else {
                    return div().into_any_element();
                };
                let (groups, player) = {
                    let bar = bar.read(cx);
                    (bar.groups.clone(), bar.player.clone())
                };
                let chosen = player
                    .read(cx)
                    .current()
                    .and_then(|current| current.remote())
                    .map(|remote| remote.target.id.clone());
                let p = palette(cx);
                let menu = cx.entity().downgrade();
                let choose =
                    |target: Option<Target>| choose_output(menu.clone(), player.clone(), target);
                let local = output_item(
                    "output-local".into(),
                    "This device",
                    None,
                    chosen.is_none(),
                    cx,
                )
                .on_click(choose(None))
                .test_support();
                let groups = match groups {
                    None => vec![menu_note("Looking for speakers…", cx).into_any_element()],
                    Some(groups) if groups.is_empty() => {
                        vec![menu_note("No speakers found", cx).into_any_element()]
                    }
                    Some(groups) => groups
                        .iter()
                        .map(|group| {
                            let note = (group.state == SonosPlayerState::Other)
                                .then_some("Playing something else");
                            output_item(
                                format!("output-{}", group.id).into(),
                                &group.name,
                                note,
                                chosen.as_deref() == Some(group.id.as_str()),
                                cx,
                            )
                            .on_click(choose(Some(group.into())))
                            .test_support()
                            .into_any_element()
                        })
                        .collect(),
                };
                v_flex()
                    .id("output-menu")
                    .role(Role::Menu)
                    .aria_label("Play on")
                    .test_support()
                    .min_w(px(220.))
                    .gap(px(2.))
                    .text_color(p.ink)
                    .child(local)
                    .children(groups)
                    .into_any_element()
            })
    }
}

/// A thin track filled with the accent up to `fraction`, its thumb growing
/// under the pointer. Clicking or dragging calls `set` with the share of
/// the track under the pointer. `bounds` keeps where the track was last
/// painted.
fn slider(
    id: &'static str,
    fraction: f32,
    bounds: Rc<Cell<Bounds<Pixels>>>,
    enabled: bool,
    set: impl Fn(f32, &mut App) + 'static,
    cx: &App,
) -> Stateful<Div> {
    let p = palette(cx);
    let painted = bounds.clone();
    let set_at = Rc::new(move |x: Pixels, cx: &mut App| {
        let bounds = bounds.get();
        if !enabled || bounds.size.width <= px(0.) {
            return;
        }
        set(((x - bounds.left()) / bounds.size.width).clamp(0., 1.), cx);
    });
    let on_drag = set_at.clone();
    div()
        .id(id)
        .role(Role::Slider)
        .group(id)
        .relative()
        .min_w_0()
        .h(px(20.))
        .when(enabled, |this| this.cursor_pointer())
        .child(
            canvas(move |bounds, _, _| painted.set(bounds), |_, _, _, _| {})
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
        .when(enabled, |this| {
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
                    .group_hover(id, |this| this.opacity(1.)),
            )
        })
        .on_mouse_down(MouseButton::Left, move |event, _, cx| {
            set_at(event.position.x, cx);
        })
        .on_mouse_move(move |event, _, cx| {
            if event.pressed_button == Some(MouseButton::Left) {
                on_drag(event.position.x, cx);
            }
        })
}

// ==============================================================================
// Where it plays
// ==============================================================================

/// "Playing on Couch" while a group plays the episode, "Connecting to
/// Couch…" while it is being handed over.
fn where_label(remote: &Remote) -> String {
    if remote.is_connecting() {
        format!("Connecting to {}…", remote.target.name)
    } else {
        format!("Playing on {}", remote.target.name)
    }
}

/// The line under the title while a group has the episode, in accent ink.
fn where_line(label: String, cx: &App) -> impl IntoElement {
    let p = palette(cx);
    let label = SharedString::from(label);
    h_flex()
        .id("player-where")
        .aria_label(label.clone())
        .test_support()
        .min_w_0()
        .gap(px(5.))
        .text_size(size::SMALL)
        .font_weight(FontWeight::MEDIUM)
        .text_color(p.accent_ink)
        .child(div().flex_none().child(icon(PodIcon::Speaker, px(14.))))
        .child(div().min_w_0().truncate().child(label))
}

/// Closes the menu and plays on `target`, or with `None` here.
fn choose_output(
    menu: WeakEntity<PopoverState>,
    player: Entity<Player>,
    target: Option<Target>,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    move |_, window, cx| {
        let _ = menu.update(cx, |menu, cx| menu.dismiss(window, cx));
        player.update(cx, |player, cx| match target.clone() {
            Some(target) => player.hand_off(target, cx),
            None => player.take_back(cx),
        });
    }
}

/// One output in the picker's menu, with a check on the chosen one.
fn output_item(
    id: SharedString,
    name: &str,
    note: Option<&'static str>,
    checked: bool,
    cx: &App,
) -> Stateful<Div> {
    let p = palette(cx);
    let label = match note {
        Some(note) => format!("{name}, {}", note.to_lowercase()),
        None => name.to_owned(),
    };
    h_flex()
        .id(id)
        .role(Role::MenuItemRadio)
        .aria_label(SharedString::from(label))
        .aria_selected(checked)
        .justify_between()
        .gap(px(12.))
        .px(px(12.))
        .py(px(8.))
        .rounded(px(8.))
        .cursor_pointer()
        .hover(|this| this.bg(p.hover))
        .child(
            v_flex()
                .min_w_0()
                .child(
                    div()
                        .truncate()
                        .when(checked, |this| this.font_weight(FontWeight::SEMIBOLD))
                        .child(name.to_owned()),
                )
                .when_some(note, |this, note| {
                    this.child(div().text_size(size::SMALL).text_color(p.muted).child(note))
                }),
        )
        .when(checked, |this| {
            this.child(
                div()
                    .flex_none()
                    .text_color(p.accent)
                    .child(icon(PodIcon::Check, px(18.))),
            )
        })
}

fn menu_note(text: &'static str, cx: &App) -> Div {
    div()
        .px(px(12.))
        .py(px(8.))
        .text_size(size::SMALL)
        .text_color(palette(cx).muted)
        .child(text)
}

/// A failed Sonos request, which goes away by itself: the bar already
/// shows the state the episode is in.
fn sonos_notification(message: String) -> Notification {
    Notification::new()
        .with_type(NotificationType::Error)
        .content(move |_, _, _| {
            div()
                .id("sonos-notice")
                .aria_label(SharedString::from(message.clone()))
                .test_support()
                .child(message.clone())
                .into_any_element()
        })
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
    use std::time::Instant;

    use super::{clock, where_label};
    use crate::sonos::{Remote, Target};

    #[test]
    fn the_line_under_the_title_says_where_it_plays() {
        let couch = Target {
            id: "RINCON_COUCH".to_owned(),
            name: "Couch".to_owned(),
        };
        let remote = Remote::connecting(couch, 0, Instant::now());
        assert_eq!(where_label(&remote), "Connecting to Couch…");
    }

    #[test]
    fn clock_shows_hours_only_when_needed() {
        assert_eq!(clock(0), "0:00");
        assert_eq!(clock(245_999), "4:05");
        assert_eq!(clock(3_729_000), "1:02:09");
        assert_eq!(clock(-5), "0:00");
    }
}
