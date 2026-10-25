//! Auth request and response bodies (spec section 4.1).

use std::fmt;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{DeviceId, Timestamp, UserId};

/// The body of `POST /v1/auth/register` and `POST /v1/auth/login`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AuthRequest {
    #[schema(examples("alice"))]
    pub username: String,
    pub password: String,
    /// Shown in the user's session list.
    #[schema(examples("Pixel 8"))]
    pub device_name: String,
}

/// Masks the password.
impl fmt::Debug for AuthRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthRequest")
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .field("device_name", &self.device_name)
            .finish()
    }
}

/// The response to a successful register or login. Each one starts a new
/// session with a new device id.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TokenResponse {
    /// Bearer token: 32 random bytes, base64url encoded.
    pub token: String,
    pub device_id: DeviceId,
    pub user_id: UserId,
    /// Sliding expiry: any request more than 24 hours after the last
    /// extension pushes it to 90 days from then.
    pub expires_at: Timestamp,
}

/// Masks the token.
impl fmt::Debug for TokenResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenResponse")
            .field("token", &"<redacted>")
            .field("device_id", &self.device_id)
            .field("user_id", &self.user_id)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// The response to `GET /v1/auth/me`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MeResponse {
    pub user_id: UserId,
    pub username: String,
    pub device_id: DeviceId,
}
