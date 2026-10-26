//! The played and archive buttons at the end of an episode row, shared by
//! the inbox and the podcast page.

use gpui_kit::Styled as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{IconName, Selectable as _, Sizable as _, h_flex};
use gpui_kit::{App, IntoElement, ParentElement as _, SharedString};
use pod_protocol::EpisodeId;

use crate::sync::local_write::EpisodeChange;

/// Each button flips its flag and hands the change to `on_change`. The
/// inbox only lists unplayed, unarchived episodes, so there they mark played
/// and archive. Both pages bind `p` and `e` to the same changes.
pub fn actions(
    id: EpisodeId,
    played: bool,
    archived: bool,
    on_change: impl Fn(EpisodeChange, &mut App) + Clone + 'static,
) -> impl IntoElement {
    let on_archive = on_change.clone();
    h_flex()
        .gap_1()
        .child(
            Button::new(SharedString::from(format!("row-played-{id}")))
                .ghost()
                .xsmall()
                .icon(IconName::Check)
                .selected(played)
                .tooltip(if played {
                    "Mark unplayed (p)"
                } else {
                    "Mark played (p)"
                })
                .on_click(move |_, _, cx| on_change(EpisodeChange::Played(!played), cx)),
        )
        .child(
            Button::new(SharedString::from(format!("row-archive-{id}")))
                .ghost()
                .xsmall()
                .icon(gpui_kit::assets::IconName::Archive)
                .selected(archived)
                .tooltip(if archived {
                    "Unarchive (e)"
                } else {
                    "Archive (e)"
                })
                .on_click(move |_, _, cx| on_archive(EpisodeChange::Archived(!archived), cx)),
        )
}
