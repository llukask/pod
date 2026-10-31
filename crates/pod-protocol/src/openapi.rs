//! The OpenAPI document for the endpoints whose bodies live in this crate.
//!
//! The Android client generates its API types from `openapi.json` at the
//! repository root; `tests/openapi.rs` keeps that file in step with
//! [`document`].

use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};

use crate::{
    AuthRequest, DirectorySearchResponse, EpisodePage, ErrorBody, InboxPage, MeResponse, PodcastId,
    PodcastItem, PodcastList, SettingsResponse, SonosCommand, SonosPlayRequest, SonosPlayer,
    SonosPlayersResponse, SonosReleased, SyncRequest, SyncResponse, TokenResponse,
};

/// The OpenAPI 3.1 document for `/v1/auth/*`, `POST /v1/sync`, the read
/// endpoints, `GET /v1/directory/search` and `/v1/sonos/*`.
pub fn document() -> utoipa::openapi::OpenApi {
    ApiDoc::openapi()
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Pod",
        description = "Self-hosted podcast sync. The spec in docs/spec.md is authoritative; \
                       this document describes the wire shapes.",
    ),
    paths(
        endpoints::register,
        endpoints::login,
        endpoints::logout,
        endpoints::me,
        endpoints::sync,
        endpoints::podcasts,
        endpoints::podcast,
        endpoints::podcast_episodes,
        endpoints::inbox,
        endpoints::settings,
        endpoints::directory_search,
        endpoints::sonos_players,
        endpoints::sonos_play,
        endpoints::sonos_command,
        endpoints::sonos_release,
    ),
    modifiers(&BearerToken),
)]
struct ApiDoc;

struct BearerToken;

impl Modify for BearerToken {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        openapi
            .components
            .get_or_insert_with(Default::default)
            .add_security_scheme(
                "bearer",
                SecurityScheme::Http(HttpBuilder::new().scheme(HttpAuthScheme::Bearer).build()),
            );
    }
}

/// Bodiless stand-ins that only carry the `#[utoipa::path]` declarations;
/// the handlers live in pod-server.
#[allow(dead_code)]
mod endpoints {
    use super::*;

    #[utoipa::path(
        post,
        path = "/v1/auth/register",
        tag = "auth",
        request_body = AuthRequest,
        responses(
            (status = 200, description = "Account and session created", body = TokenResponse),
            (status = 400, description = "`bad_request`", body = ErrorBody),
            (status = 403, description = "`registration_disabled`", body = ErrorBody),
            (status = 409, description = "`username_taken`", body = ErrorBody),
            (status = 429, description = "`rate_limited`, with `Retry-After`", body = ErrorBody),
            (status = 500, description = "`internal`", body = ErrorBody),
        ),
    )]
    fn register() {}

    #[utoipa::path(
        post,
        path = "/v1/auth/login",
        tag = "auth",
        request_body = AuthRequest,
        responses(
            (status = 200, description = "Session created with a new device id", body = TokenResponse),
            (status = 400, description = "`bad_request`", body = ErrorBody),
            (status = 401, description = "`invalid_credentials`", body = ErrorBody),
            (status = 429, description = "`rate_limited`, with `Retry-After`", body = ErrorBody),
            (status = 500, description = "`internal`", body = ErrorBody),
        ),
    )]
    fn login() {}

    #[utoipa::path(
        post,
        path = "/v1/auth/logout",
        tag = "auth",
        responses(
            (status = 204, description = "Session deleted"),
            (status = 401, description = "`unauthenticated`", body = ErrorBody),
            (status = 500, description = "`internal`", body = ErrorBody),
        ),
        security(("bearer" = [])),
    )]
    fn logout() {}

    #[utoipa::path(
        get,
        path = "/v1/auth/me",
        tag = "auth",
        responses(
            (status = 200, description = "The session's user and device", body = MeResponse),
            (status = 401, description = "`unauthenticated`", body = ErrorBody),
            (status = 500, description = "`internal`", body = ErrorBody),
        ),
        security(("bearer" = [])),
    )]
    fn me() {}

    #[utoipa::path(
        post,
        path = "/v1/sync",
        tag = "sync",
        request_body = SyncRequest,
        responses(
            (status = 200, description = "Mutation results and scope changes. A malformed \
                mutation is a `rejected` result, not an error.", body = SyncResponse),
            (status = 400, description = "`bad_request`: the envelope is broken, for example a \
                mutation without a valid `id`", body = ErrorBody),
            (status = 401, description = "`unauthenticated`", body = ErrorBody),
            (status = 413, description = "`batch_too_large`: more than 200 mutations", body = ErrorBody),
            (status = 429, description = "`rate_limited`, with `Retry-After`", body = ErrorBody),
            (status = 500, description = "`internal`", body = ErrorBody),
        ),
        security(("bearer" = [])),
    )]
    fn sync() {}

    #[utoipa::path(
        get,
        path = "/v1/podcasts",
        tag = "read",
        responses(
            (status = 200, description = "Every subscribed podcast, including `pending` \
                ones", body = PodcastList),
            (status = 401, description = "`unauthenticated`", body = ErrorBody),
            (status = 500, description = "`internal`", body = ErrorBody),
        ),
        security(("bearer" = [])),
    )]
    fn podcasts() {}

    #[utoipa::path(
        get,
        path = "/v1/podcasts/{id}",
        tag = "read",
        params(("id" = PodcastId, Path, description = "The podcast id.")),
        responses(
            (status = 200, description = "The podcast", body = PodcastItem),
            (status = 400, description = "`bad_request`: `id` is not a UUID", body = ErrorBody),
            (status = 401, description = "`unauthenticated`", body = ErrorBody),
            (status = 404, description = "`not_found`: the user isn't subscribed to it",
                body = ErrorBody),
            (status = 500, description = "`internal`", body = ErrorBody),
        ),
        security(("bearer" = [])),
    )]
    fn podcast() {}

    #[utoipa::path(
        get,
        path = "/v1/podcasts/{id}/episodes",
        tag = "read",
        params(
            ("id" = PodcastId, Path, description = "The podcast id."),
            ("cursor" = Option<String>, Query, description = "`next_cursor` of the previous \
                page; absent for the first page."),
            ("limit" = Option<u32>, Query, description = "Items wanted, default 50, clamped \
                to 1..=200."),
        ),
        responses(
            (status = 200, description = "One page of the podcast's episodes with the \
                user's state", body = EpisodePage),
            (status = 400, description = "`bad_request`: `id` is not a UUID or `limit` is not \
                a number; `invalid_cursor`: `cursor` doesn't decode", body = ErrorBody),
            (status = 401, description = "`unauthenticated`", body = ErrorBody),
            (status = 404, description = "`not_found`: the user isn't subscribed to it",
                body = ErrorBody),
            (status = 500, description = "`internal`", body = ErrorBody),
        ),
        security(("bearer" = [])),
    )]
    fn podcast_episodes() {}

    #[utoipa::path(
        get,
        path = "/v1/inbox",
        tag = "read",
        params(
            ("cursor" = Option<String>, Query, description = "`next_cursor` of the previous \
                page; absent for the first page."),
            ("limit" = Option<u32>, Query, description = "Items wanted, default 50, clamped \
                to 1..=200."),
        ),
        responses(
            (status = 200, description = "One page of the inbox (spec section 6)",
                body = InboxPage),
            (status = 400, description = "`bad_request`: `limit` is not a number; \
                `invalid_cursor`: `cursor` doesn't decode", body = ErrorBody),
            (status = 401, description = "`unauthenticated`", body = ErrorBody),
            (status = 500, description = "`internal`", body = ErrorBody),
        ),
        security(("bearer" = [])),
    )]
    fn inbox() {}

    #[utoipa::path(
        get,
        path = "/v1/settings",
        tag = "read",
        responses(
            (status = 200, description = "Every synced setting, defaults filled in",
                body = SettingsResponse),
            (status = 401, description = "`unauthenticated`", body = ErrorBody),
            (status = 500, description = "`internal`", body = ErrorBody),
        ),
        security(("bearer" = [])),
    )]
    fn settings() {}

    #[utoipa::path(
        get,
        path = "/v1/directory/search",
        tag = "directory",
        params(
            ("q" = String, Query, description = "Search term. Must not be blank."),
            ("limit" = Option<u32>, Query, description = "Results wanted, default 25, \
                clamped to 1..=50."),
        ),
        responses(
            (status = 200, description = "Matching podcasts, cached for an hour per \
                normalized term", body = DirectorySearchResponse),
            (status = 400, description = "`bad_request`: `q` is missing or blank, or \
                `limit` is not a number", body = ErrorBody),
            (status = 401, description = "`unauthenticated`", body = ErrorBody),
            (status = 500, description = "`internal`", body = ErrorBody),
            (status = 502, description = "`upstream_unavailable`: the directory failed or \
                timed out", body = ErrorBody),
        ),
        security(("bearer" = [])),
    )]
    fn directory_search() {}

    #[utoipa::path(
        get,
        path = "/v1/sonos/players",
        tag = "sonos",
        responses(
            (status = 200, description = "Every Sonos group, empty when the server has no \
                `sonos_hosts`", body = SonosPlayersResponse),
            (status = 401, description = "`unauthenticated`", body = ErrorBody),
            (status = 500, description = "`internal`", body = ErrorBody),
            (status = 502, description = "`upstream_unavailable`: no configured speaker \
                answered", body = ErrorBody),
        ),
        security(("bearer" = [])),
    )]
    fn sonos_players() {}

    #[utoipa::path(
        post,
        path = "/v1/sonos/players/{id}/play",
        tag = "sonos",
        params(("id" = String, Path, description = "The player id, its coordinator's UUID.")),
        request_body = SonosPlayRequest,
        responses(
            (status = 200, description = "The player with the new session", body = SonosPlayer),
            (status = 400, description = "`bad_request`", body = ErrorBody),
            (status = 401, description = "`unauthenticated`", body = ErrorBody),
            (status = 404, description = "`not_found`: the player isn't a current coordinator, \
                the episode isn't readable, or Sonos is off", body = ErrorBody),
            (status = 500, description = "`internal`", body = ErrorBody),
            (status = 502, description = "`upstream_unavailable`: the speaker didn't answer \
                or refused", body = ErrorBody),
        ),
        security(("bearer" = [])),
    )]
    fn sonos_play() {}

    #[utoipa::path(
        post,
        path = "/v1/sonos/players/{id}/command",
        tag = "sonos",
        params(("id" = String, Path, description = "The player id, its coordinator's UUID.")),
        request_body = SonosCommand,
        responses(
            (status = 200, description = "The player after the command", body = SonosPlayer),
            (status = 400, description = "`bad_request`", body = ErrorBody),
            (status = 401, description = "`unauthenticated`", body = ErrorBody),
            (status = 404, description = "`not_found`: the player isn't a current coordinator, \
                or Sonos is off", body = ErrorBody),
            (status = 409, description = "`no_session`: the user has no session on the \
                player (any command but `volume`)", body = ErrorBody),
            (status = 500, description = "`internal`", body = ErrorBody),
            (status = 502, description = "`upstream_unavailable`: the speaker didn't answer \
                or refused", body = ErrorBody),
        ),
        security(("bearer" = [])),
    )]
    fn sonos_command() {}

    #[utoipa::path(
        post,
        path = "/v1/sonos/players/{id}/release",
        tag = "sonos",
        params(("id" = String, Path, description = "The player id, its coordinator's UUID.")),
        responses(
            (status = 200, description = "The session as it was when released", body = SonosReleased),
            (status = 401, description = "`unauthenticated`", body = ErrorBody),
            (status = 404, description = "`not_found`: the player isn't a current coordinator, \
                or Sonos is off", body = ErrorBody),
            (status = 409, description = "`no_session`: the user has no session on the \
                player", body = ErrorBody),
            (status = 500, description = "`internal`", body = ErrorBody),
            (status = 502, description = "`upstream_unavailable`: the speaker didn't answer \
                or refused", body = ErrorBody),
        ),
        security(("bearer" = [])),
    )]
    fn sonos_release() {}
}
