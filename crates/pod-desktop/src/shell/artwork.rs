//! Artwork from the cache (spec 7.5), on a muted square that doubles as the
//! placeholder while an image isn't cached or doesn't decode.
//!
//! Callers pass the `artwork.file` their query joined in, never a URL: GPUI
//! fetches a URL source over HTTP from the render path, and a cached file
//! is what makes artwork work offline.

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, ElementId, InteractiveElement as _, IntoElement, ObjectFit, ParentElement as _, Pixels,
    StatefulInteractiveElement as _, Styled as _, StyledImage as _, TestSupportExt as _, div, img,
    px,
};

use crate::reconcile::Reconciler;
use crate::reconcile::artwork::MAX_SIDE;

/// A `size` square showing the cached image `file`, if any.
pub fn artwork(
    id: impl Into<ElementId>,
    file: Option<&str>,
    size: Pixels,
    cx: &App,
) -> impl IntoElement {
    debug_assert!(
        size * 2. <= px(MAX_SIDE as f32),
        "a {size:?} view needs a larger MAX_SIDE"
    );
    let path = file.map(|file| cx.global::<Reconciler>().artwork_root().join(file));
    div()
        .id(id.into())
        // Tests read which file an element shows from its accessibility
        // value; the image itself is decorative.
        .when(cfg!(test), |this| {
            this.aria_value(
                path.as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default(),
            )
        })
        .test_support()
        .flex_none()
        .size(size)
        .rounded(px(4.))
        .overflow_hidden()
        .bg(cx.theme().muted)
        .when_some(path, |this, path| {
            this.child(img(path).size_full().object_fit(ObjectFit::Cover))
        })
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use color_eyre::eyre::WrapErr as _;
    use gpui_kit::TestAppContext;
    use gpui_kit::test::TestWindowExt as _;
    use pod_protocol::Timestamp;
    use rusqlite::params;

    use super::*;
    use crate::app::tests::{TestApp, signed_in};
    use crate::download::tests::{ensure_server, image_requests};
    use crate::net::NetworkState;

    const PODCAST: &str = "a1000000-0000-5000-8000-0000000000a1";
    const EPISODE: &str = "e0000000-0000-5000-8000-0000000000a1";

    /// The issue's acceptance: after a pass cached the artwork, the sidebar
    /// and the inbox show the cached files with the network off, and
    /// rendering fetches nothing.
    #[gpui_kit::test]
    fn the_sidebar_and_inbox_show_cached_artwork_offline(cx: &mut TestAppContext) {
        let server = ensure_server();
        let podcast_art = format!("{server}/image/ui-podcast");
        let episode_art = format!("{server}/image/ui-episode");
        let seed_urls = (podcast_art.clone(), episode_art.clone());
        let app = TestApp::open(cx, move |transaction| {
            signed_in(transaction)?;
            transaction
                .execute(
                    "insert into podcast (id, feed_url, status, title, version, artwork_url)
                     values (?1, 'https://feeds.example.com/a', 'ok', 'Art Show', 1, ?2)",
                    params![PODCAST, seed_urls.0],
                )
                .context("insert the podcast")?;
            transaction
                .execute(
                    "insert into subscription (podcast_id, feed_url, subscribed, subscribed_at,
                         subscribed_dev)
                     values (?1, 'https://feeds.example.com/a', 1, 0, 'device')",
                    [PODCAST],
                )
                .context("insert the subscription")?;
            transaction
                .execute(
                    "insert into episode (id, podcast_id, title, summary_html, content_html,
                         published_at, audio_url, audio_type, version, artwork_url)
                     values (?1, ?2, 'Painted', '', '', ?3, 'https://cdn.example.com/1.mp3',
                         'audio/mpeg', 1, ?4)",
                    params![
                        EPISODE,
                        PODCAST,
                        Timestamp::now().as_epoch_ms(),
                        seed_urls.1
                    ],
                )
                .context("insert the episode")?;
            Ok(())
        });

        let reconciler = cx.update(|cx| cx.global::<Reconciler>().clone());
        app.runtime
            .block_on(reconciler.pass())
            .expect("the pass runs");
        let deadline = Instant::now() + Duration::from_secs(20);
        while app.read(|connection| {
            connection
                .query_row("select count(*) from artwork", [], |row| {
                    row.get::<_, i64>(0)
                })
                .context("count the artwork rows")
        }) < 2
        {
            assert!(Instant::now() < deadline, "the artwork is cached");
            std::thread::sleep(Duration::from_millis(20));
        }
        app.network.send_replace(NetworkState {
            connected: false,
            metered: false,
        });

        let root = reconciler.artwork_root();
        let cached = |url: &str| {
            root.join(crate::reconcile::artwork::file_name(url))
                .display()
                .to_string()
        };
        for (id, url) in [
            (format!("podcast-artwork-{PODCAST}"), &podcast_art),
            (format!("inbox-artwork-{EPISODE}"), &episode_art),
            ("detail-artwork".to_owned(), &episode_art),
        ] {
            let expected = cached(url);
            app.wait_until(cx, &format!("{id} shows {expected}"), |window| {
                window
                    .try_find(id.clone())
                    .is_some_and(|element| element.value() == Some(expected.as_str()))
            });
        }
        assert_eq!(image_requests("ui-podcast"), 1);
        assert_eq!(image_requests("ui-episode"), 1);
    }
}
