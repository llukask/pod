// ==============================================================================
// Tiny components shared between views
// ==============================================================================

use leptos::prelude::*;

/// `<img>` with a music-note placeholder if the URL is empty or the image
/// fails to load. Mirrors `imgWithPlaceholder` from the JS frontend.
#[component]
pub fn ImageWithPlaceholder(
    #[prop(into)] src: Signal<String>,
    size: u32,
    #[prop(default = "")] class: &'static str,
) -> impl IntoView {
    let failed = RwSignal::new(false);

    let style = format!("width:{px}px;height:{px}px;flex-shrink:0", px = size);

    let style_a = style.clone();
    let style_b = style.clone();

    view! {
        <Show
            when=move || !failed.get() && !src.get().is_empty()
            fallback=move || view! {
                <div class="img-placeholder" style=style_b.clone()>
                    "♫"
                </div>
            }>
            <img
                class=class
                src=move || src.get()
                alt=""
                loading="lazy"
                style=style_a.clone()
                on:error=move |_| failed.set(true) />
        </Show>
    }
}
