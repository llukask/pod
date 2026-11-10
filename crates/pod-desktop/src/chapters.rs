//! An episode's chapters in the detail pane (spec 17.4).
//!
//! The server resolves chapters on demand, so the desktop asks
//! `GET /v1/episodes/{id}/chapters` when an episode's detail opens and keeps
//! the answer in memory for the session. No table stores them, so an
//! offline desktop shows none, and a failed request shows nothing either.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use color_eyre::eyre::{Result, WrapErr, bail};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, FontWeight, Global, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Task,
    TestSupportExt as _, div, px,
};
use pod_protocol::{Chapter, ChapterList, EpisodeId};

use crate::runtime::Background;
use crate::shell::ui::tabular;
use crate::store::Store;
use crate::sync::engine::SyncEngine;
use crate::sync::session::load_server;
use crate::theme::{palette, size};

/// The server gives up on slow sources after 25 s (spec 17.2), so this
/// leaves it room to answer.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Chapters fetched this session, by episode. Only answers are kept: a
/// failure is asked again when the episode's detail opens again.
#[derive(Debug, Default)]
struct Loaded(HashMap<EpisodeId, Rc<Vec<Chapter>>>);

impl Global for Loaded {}

/// A view's request for one episode's chapters. Dropping it, on the next
/// selection, cancels a request still running.
#[derive(Debug)]
pub struct Request {
    episode: EpisodeId,
    _task: Task<()>,
}

/// `episode`'s chapters if this session has them. Otherwise starts a request
/// unless `request` already holds one for `episode`, and notifies the view
/// once it answers.
pub fn chapters<V: 'static>(
    episode: EpisodeId,
    request: &mut Option<Request>,
    cx: &mut Context<V>,
) -> Option<Rc<Vec<Chapter>>> {
    if let Some(chapters) = cx
        .try_global::<Loaded>()
        .and_then(|loaded| loaded.0.get(&episode))
    {
        return Some(chapters.clone());
    }
    if request
        .as_ref()
        .is_some_and(|request| request.episode == episode)
    {
        return None;
    }
    let (Some(engine), Some(background)) =
        (cx.try_global::<SyncEngine>(), cx.try_global::<Background>())
    else {
        return None;
    };
    let store = cx.global::<Store>().clone();
    let http = engine.http().clone();
    let fetched = background.spawn(fetch(store, http, episode));
    let task = cx.spawn(async move |this, cx| {
        let chapters = match fetched.await {
            Ok(Ok(chapters)) => chapters,
            Ok(Err(error)) => {
                tracing::info!("loading episode {episode}'s chapters failed: {error:#}");
                return;
            }
            Err(error) => {
                tracing::warn!("the chapter request stopped: {error}");
                return;
            }
        };
        let _ = this.update(cx, |_, cx| {
            cx.default_global::<Loaded>()
                .0
                .insert(episode, Rc::new(chapters));
            cx.notify();
        });
    });
    *request = Some(Request {
        episode,
        _task: task,
    });
    None
}

async fn fetch(store: Store, http: reqwest::Client, episode: EpisodeId) -> Result<Vec<Chapter>> {
    let Some((server_url, token)) = store.read(load_server).await.context("read the account")?
    else {
        bail!("signed out");
    };
    let url = format!(
        "{}/v1/episodes/{episode}/chapters",
        server_url.trim_end_matches('/')
    );
    let response = http
        .get(url)
        .bearer_auth(token)
        .timeout(TIMEOUT)
        .send()
        .await
        .context("send the chapter request")?;
    let status = response.status();
    if !status.is_success() {
        bail!("the server answered {status}");
    }
    let body = response
        .bytes()
        .await
        .context("read the chapter response")?;
    let list: ChapterList = serde_json::from_slice(&body).context("parse the chapter list")?;
    Ok(list.chapters)
}

/// A chapter's start as the list shows it: `M:SS`, or `H:MM:SS` from an
/// hour on.
pub fn start_label(start_ms: i64) -> String {
    let seconds = start_ms.max(0) / 1000;
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// The chapter that holds `position_ms`: the last one starting at or before
/// it. `None` before the first chapter starts.
pub fn current(chapters: &[Chapter], position_ms: i64) -> Option<usize> {
    chapters
        .iter()
        .rposition(|chapter| chapter.start_ms <= position_ms)
}

/// The detail's "Chapters" section for `episode`, or `None` without
/// chapters. A row plays the episode from its chapter's start; while the
/// episode is in the player, the chapter it is in shows in accent ink.
// TODO: previous and next chapter buttons in the player bar, the current
// chapter's title in it, and chapter links and images (spec 17.4).
pub fn section(episode: EpisodeId, chapters: &[Chapter], cx: &App) -> Option<AnyElement> {
    if chapters.is_empty() {
        return None;
    }
    let p = palette(cx);
    let playing = crate::player::now_playing(cx)
        .filter(|(id, _, _)| *id == episode)
        .and_then(|(_, _, position_ms)| current(chapters, position_ms));
    let rows = chapters.iter().enumerate().map(|(index, chapter)| {
        let is_current = playing == Some(index);
        let start = chapter.start_ms;
        h_flex()
            .id(SharedString::from(format!("chapter-{index}")))
            .aria_label(SharedString::from(if is_current {
                format!("{} (playing)", chapter.title)
            } else {
                chapter.title.clone()
            }))
            .test_support()
            .gap(px(14.))
            .px(px(8.))
            .py(px(6.))
            .rounded(px(8.))
            .cursor_pointer()
            .hover(|this| this.bg(p.hover))
            .child(
                div()
                    .flex_none()
                    .w(px(64.))
                    .text_size(size::SMALL)
                    .text_color(p.muted)
                    .font_features(tabular())
                    .child(start_label(start)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(if is_current { p.accent_ink } else { p.ink })
                    .when(is_current, |this| this.font_weight(FontWeight::SEMIBOLD))
                    .child(chapter.title.clone()),
            )
            .on_click(move |_, _, cx| crate::player::play_at(episode, start, cx))
    });
    Some(
        v_flex()
            .id("detail-chapters")
            .test_support()
            .gap(px(2.))
            .child(
                div()
                    .px(px(8.))
                    .pb(px(6.))
                    .text_size(size::SMALL)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.muted)
                    .child("Chapters"),
            )
            .children(rows)
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chapter(start_ms: i64) -> Chapter {
        Chapter {
            start_ms,
            title: format!("At {start_ms}"),
            url: None,
        }
    }

    #[test]
    fn starts_read_as_minutes_or_hours() {
        assert_eq!(start_label(0), "0:00");
        assert_eq!(start_label(754_500), "12:34");
        assert_eq!(start_label(3_600_000), "1:00:00");
        assert_eq!(start_label(4_323_000), "1:12:03");
    }

    #[test]
    fn the_current_chapter_is_the_last_that_started() {
        let chapters = [chapter(5_000), chapter(60_000), chapter(120_000)];
        assert_eq!(current(&chapters, 0), None);
        assert_eq!(current(&chapters, 5_000), Some(0));
        assert_eq!(current(&chapters, 119_999), Some(1));
        assert_eq!(current(&chapters, 600_000), Some(2));
    }
}
