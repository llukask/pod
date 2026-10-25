//! The OpenAPI document for the endpoints whose bodies live in this crate.
//!
//! The Android client generates its API types from `openapi.json` at the
//! repository root; `tests/openapi.rs` keeps that file in step with
//! [`document`].

use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};

use crate::{AuthRequest, ErrorBody, MeResponse, SyncRequest, SyncResponse, TokenResponse};

/// The OpenAPI 3.1 document for `/v1/auth/*` and `POST /v1/sync`.
// TODO: add the read endpoints (spec 4.5) and the directory (spec 4.6) when
// their bodies move into this crate.
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
}
