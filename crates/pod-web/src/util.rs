// ==============================================================================
// Formatting + small DOM helpers
// ==============================================================================

use chrono::{DateTime, Utc};
use pod_model::EpisodeWithProgress;
use wasm_bindgen::JsCast;

/// Format a duration in seconds as either a clock string ("1:02:03" / "12:34")
/// or a compact "1h 30m" / "45m" label, mirroring the JS `formatDuration`.
pub fn format_duration(secs: i64, compact: bool) -> String {
    if secs <= 0 {
        return "0:00".to_string();
    }
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if compact {
        if h > 0 {
            return format!("{}h {}m", h, m);
        }
        return format!("{}m", m);
    }
    if h > 0 {
        format!("{}:{:02}:{:02}", h, m, s)
    } else {
        format!("{}:{:02}", m, s)
    }
}

/// Render a `DateTime<Utc>` as e.g. "Apr 25, 2026" (en-US locale).
///
/// We delegate to `Date.prototype.toLocaleDateString` via JS rather than
/// pulling chrono's heavyweight locale support into the wasm bundle.
pub fn format_date(dt: &DateTime<Utc>) -> String {
    let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_str(&dt.to_rfc3339()));
    let opts = js_sys::Object::new();
    let set = |k: &str, v: &str| {
        let _ = js_sys::Reflect::set(
            &opts,
            &wasm_bindgen::JsValue::from_str(k),
            &wasm_bindgen::JsValue::from_str(v),
        );
    };
    set("month", "short");
    set("day", "numeric");
    set("year", "numeric");

    // Date.prototype.toLocaleDateString(locales?, options?)
    let f = js_sys::Reflect::get(&date, &wasm_bindgen::JsValue::from_str("toLocaleDateString"))
        .ok()
        .and_then(|v| v.dyn_into::<js_sys::Function>().ok());
    if let Some(f) = f {
        if let Ok(out) = f.call2(
            &date,
            &wasm_bindgen::JsValue::from_str("en-US"),
            &opts.into(),
        ) {
            if let Some(s) = out.as_string() {
                return s;
            }
        }
    }
    // Fallback: ISO date prefix.
    dt.format("%Y-%m-%d").to_string()
}

pub fn format_date_opt(dt: &Option<DateTime<Utc>>) -> String {
    match dt {
        Some(d) => format_date(d),
        None => String::new(),
    }
}

/// Strip HTML tags by dumping into a detached `<div>` and reading textContent.
/// Matches the JS frontend's `stripHtml` helper.
pub fn strip_html(html: &str) -> String {
    if html.is_empty() {
        return String::new();
    }
    let doc = match web_sys::window().and_then(|w| w.document()) {
        Some(d) => d,
        None => return html.to_string(),
    };
    let div = match doc.create_element("div") {
        Ok(d) => d,
        Err(_) => return html.to_string(),
    };
    div.set_inner_html(html);
    div.text_content().unwrap_or_default()
}

/// Sanitize HTML using DOMPurify (loaded from CDN in `index.html`) and
/// rewrite all `<a>` tags to open in a new tab. Falls back to escaping the
/// raw text if DOMPurify is unavailable.
pub fn sanitize_html(html: &str) -> String {
    if html.is_empty() {
        return String::new();
    }

    let window = match web_sys::window() {
        Some(w) => w,
        None => return strip_html(html),
    };

    let purify = js_sys::Reflect::get(&window, &wasm_bindgen::JsValue::from_str("DOMPurify"))
        .ok()
        .filter(|v| !v.is_undefined() && !v.is_null());
    let Some(purify) = purify else {
        return strip_html(html);
    };

    let opts = js_sys::Object::new();
    let _ = js_sys::Reflect::set(
        &opts,
        &wasm_bindgen::JsValue::from_str("FORBID_TAGS"),
        &js_sys::Array::of2(
            &wasm_bindgen::JsValue::from_str("style"),
            &wasm_bindgen::JsValue::from_str("form"),
        )
        .into(),
    );
    let _ = js_sys::Reflect::set(
        &opts,
        &wasm_bindgen::JsValue::from_str("FORBID_ATTR"),
        &js_sys::Array::of1(&wasm_bindgen::JsValue::from_str("style")).into(),
    );

    let sanitize = js_sys::Reflect::get(&purify, &wasm_bindgen::JsValue::from_str("sanitize"))
        .ok()
        .and_then(|v| v.dyn_into::<js_sys::Function>().ok());
    let Some(sanitize) = sanitize else {
        return strip_html(html);
    };

    let cleaned = sanitize
        .call2(&purify, &wasm_bindgen::JsValue::from_str(html), &opts.into())
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();

    // Rewrite anchors to open in a new tab. Done here rather than via a
    // DOMPurify hook because we want a plain string back for use with
    // `inner_html=`.
    let doc = match window.document() {
        Some(d) => d,
        None => return cleaned,
    };
    let tmp = match doc.create_element("div") {
        Ok(d) => d,
        Err(_) => return cleaned,
    };
    tmp.set_inner_html(&cleaned);
    if let Ok(anchors) = tmp.query_selector_all("a[href]") {
        for i in 0..anchors.length() {
            if let Some(node) = anchors.item(i) {
                if let Ok(el) = node.dyn_into::<web_sys::Element>() {
                    let _ = el.set_attribute("target", "_blank");
                    let _ = el.set_attribute("rel", "noopener noreferrer");
                }
            }
        }
    }
    tmp.inner_html()
}

/// Returns true if the episode should be considered "done" — either
/// explicitly marked or progressed past 95% of duration.
pub fn is_episode_done(item: &EpisodeWithProgress) -> bool {
    if item.done {
        return true;
    }
    let dur = item.episode.audio_duration as i64;
    if dur <= 0 {
        return false;
    }
    let prog = item.progress.unwrap_or(0) as i64;
    (prog * 100 / dur) >= 95
}

/// Inline SVG markup for the play / pause icons. Identical to the JS frontend.
pub const ICON_PLAY: &str =
    r#"<svg viewBox="0 0 24 24" fill="currentColor"><polygon points="7,4 21,12 7,20"/></svg>"#;
pub const ICON_PAUSE: &str = r#"<svg viewBox="0 0 24 24" fill="currentColor"><rect x="5" y="4" width="5" height="16"/><rect x="14" y="4" width="5" height="16"/></svg>"#;
