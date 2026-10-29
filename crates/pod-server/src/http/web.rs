//! The web client's static files (spec 10, step 5). pod-server serves the
//! built bundle itself, so a deployment is one process behind one origin and
//! the client calls the API with relative URLs.

use std::path::Path;

use axum::Router;
use axum::http::{HeaderValue, header};
use tower_http::services::{ServeDir, ServeFile};
use tower_http::set_header::SetResponseHeaderLayer;

/// The page's Content-Security-Policy. The client inserts show-note HTML
/// (spec 2.2), so scripts and styles come only from the bundle, never inline.
/// Artwork and audio live on publishers' hosts, plain HTTP included for feeds
/// on a LAN. API calls stay on this origin.
const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; script-src 'self'; style-src 'self'; \
     img-src 'self' https: http: data:; media-src 'self' https: http:; connect-src 'self'; \
     object-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

/// Vite names everything under `assets/` after its content hash, so a cached
/// copy can never be stale.
const IMMUTABLE: &str = "public, max-age=31536000, immutable";

/// Every other file, `index.html` above all, is revalidated on each load so a
/// deploy reaches open tabs on their next navigation.
const REVALIDATE: &str = "no-cache";

/// Serves `dir`: `assets/` as is (a missing asset is a 404), and every other
/// path as the file it names or else `index.html`, so the client's own routes
/// load on a reload or a shared link.
pub fn routes(dir: &Path) -> Router {
    let assets = Router::new()
        .fallback_service(ServeDir::new(dir.join("assets")))
        .layer(cache_control(IMMUTABLE));
    let pages = Router::new()
        .fallback_service(ServeDir::new(dir).fallback(ServeFile::new(dir.join("index.html"))))
        .layer(cache_control(REVALIDATE));
    Router::new()
        .nest("/assets", assets)
        .fallback_service(pages)
        .layer(SetResponseHeaderLayer::overriding(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(CONTENT_SECURITY_POLICY),
        ))
}

fn cache_control(value: &'static str) -> SetResponseHeaderLayer<HeaderValue> {
    SetResponseHeaderLayer::overriding(header::CACHE_CONTROL, HeaderValue::from_static(value))
}
