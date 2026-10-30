//! Each show's cover, blown up and heavily blurred, behind the surfaces
//! that belong to it: the player bar and the top of the show's page. This is
//! the design's one bold move; the rest of the interface stays quiet.
//!
//! The blurred images ([`crate::reconcile::artwork::wash`]) are made from
//! the cached artwork files on the tokio blocking pool, the first time a
//! view asks for one, and kept in memory for the session: they are 32 px
//! squares, one per show on screen. A view gets `None` until its image is
//! ready, and every window redraws once it is.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use color_eyre::eyre::{Result, WrapErr as _};
use gpui_kit::{
    AnyElement, App, AppContext as _, Entity, Global, ImageSource, IntoElement as _, ObjectFit,
    ParentElement as _, Pixels, RenderImage, Styled as _, StyledImage as _, div, img, relative,
};

use crate::reconcile::Reconciler;
use crate::reconcile::artwork;
use crate::runtime::Background;
use crate::theme::palette;

#[derive(Debug)]
enum Slot {
    Making,
    Ready(Arc<RenderImage>),
    /// The file didn't decode. Asking again would only fail again.
    Failed,
}

/// The washes made so far, by artwork file.
#[derive(Debug, Default)]
pub struct Washes {
    slots: HashMap<String, Slot>,
}

struct WashCache(Entity<Washes>);

impl Global for WashCache {}

/// The wash of the cached artwork `file`, if it's ready. Otherwise starts
/// making it, unless that already started. Requires the [`Reconciler`] and
/// [`Background`] globals.
pub fn image(file: &str, cx: &mut App) -> Option<Arc<RenderImage>> {
    let washes = match cx.try_global::<WashCache>() {
        Some(WashCache(washes)) => washes.clone(),
        None => {
            let washes = cx.new(|_| Washes::default());
            cx.set_global(WashCache(washes.clone()));
            washes
        }
    };
    if let Some(slot) = washes.read(cx).slots.get(file) {
        return match slot {
            Slot::Ready(image) => Some(image.clone()),
            Slot::Making | Slot::Failed => None,
        };
    }
    let path = cx.global::<Reconciler>().artwork_root().join(file);
    let made = cx
        .global::<Background>()
        .spawn(async move { tokio::task::spawn_blocking(move || make(path)).await });
    let file = file.to_owned();
    washes.update(cx, |washes, cx| {
        washes.slots.insert(file.clone(), Slot::Making);
        cx.spawn(async move |washes, cx| {
            let slot = match made.await {
                Ok(Ok(Ok(image))) => Slot::Ready(image),
                Ok(Ok(Err(error))) => {
                    tracing::info!(file, "making a wash failed: {error:#}");
                    Slot::Failed
                }
                Ok(Err(error)) | Err(error) => {
                    tracing::warn!(file, "the wash task failed: {error}");
                    Slot::Failed
                }
            };
            let _ = washes.update(cx, |washes, cx| {
                washes.slots.insert(file, slot);
                cx.refresh_windows();
            });
        })
        .detach();
    });
    None
}

/// Reads `path` and blurs it into a GPUI image (BGRA, as GPUI wants it).
fn make(path: PathBuf) -> Result<Arc<RenderImage>> {
    let body = std::fs::read(&path).context("read the artwork file")?;
    let mut wash = artwork::wash(&body).context("blur the artwork")?;
    for pixel in wash.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Ok(Arc::new(RenderImage::new(vec![image::Frame::new(wash)])))
}

/// The wash of `file` filling its parent, which must be `relative()` and
/// clip its overflow. The image is drawn larger than the parent, as the web
/// does, so its soft edges fall outside. `None` without a cached file or
/// before the wash is ready.
pub fn layer(file: Option<&str>, strength: f32, cx: &mut App) -> Option<AnyElement> {
    let image = image(file?, cx)?;
    let opacity = (palette(cx).wash_opacity + strength).min(1.);
    Some(
        div()
            .absolute()
            .left(relative(-0.3))
            .top(relative(-0.6))
            .w(relative(1.6))
            .h(relative(2.2))
            .opacity(opacity)
            .child(
                img(ImageSource::Render(image))
                    .size_full()
                    .object_fit(ObjectFit::Fill),
            )
            .into_any_element(),
    )
}

/// The wash behind the top `height` of a show's page, fading into the
/// background.
pub fn header_layer(file: Option<&str>, height: Pixels, cx: &mut App) -> Option<AnyElement> {
    let image = image(file?, cx)?;
    let p = palette(cx);
    Some(
        div()
            .absolute()
            .top_0()
            .left_0()
            .w_full()
            .h(height)
            .overflow_hidden()
            .child(
                div()
                    .absolute()
                    .left(relative(-0.3))
                    .top(relative(-0.6))
                    .w(relative(1.6))
                    .h(relative(2.2))
                    .opacity(p.wash_opacity)
                    .child(
                        img(ImageSource::Render(image))
                            .size_full()
                            .object_fit(ObjectFit::Fill),
                    ),
            )
            // The web masks the wash out from 30 % down; GPUI has no masks,
            // so the background is laid over it in a gradient instead.
            .child(div().absolute().size_full().bg(gpui_kit::linear_gradient(
                180.,
                gpui_kit::linear_color_stop(p.background.opacity(0.), 0.3),
                gpui_kit::linear_color_stop(p.background, 1.),
            )))
            .into_any_element(),
    )
}
