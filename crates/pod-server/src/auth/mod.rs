//! Accounts and sessions (spec 2.3 and 4.1): input rules, password and
//! token handling, and the session table.
//!
//! The HTTP handlers in [`crate::http::auth`] only translate between the
//! wire and these functions. Every function that depends on the time takes
//! `now` from the caller, so tests can move it.

mod password;
mod token;

use color_eyre::eyre::{Report, Result, WrapErr};
use pod_protocol::{AuthRequest, DeviceId, Timestamp, UserId};
use sqlx::{PgConnection, PgPool};

use crate::db::SqlTimestamp;
pub use password::Password;
pub use token::{SessionToken, TokenHash};

// ==============================================================================
// Input rules
// ==============================================================================

// The spec doesn't define username or password rules. These are the
// smallest set that keeps usernames unambiguous and bounds the work a
// request can cause.

const MAX_USERNAME_CHARS: usize = 64;
const MIN_PASSWORD_CHARS: usize = 8;
/// Argon2 accepts any length, but there is no reason to feed it megabytes.
const MAX_PASSWORD_BYTES: usize = 1024;
const MAX_DEVICE_NAME_CHARS: usize = 100;

/// A request field that breaks the input rules. The message is shown to
/// the client in a `bad_request` error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidInput(pub &'static str);

/// A username: trimmed, lowercased, 1 to 64 of `a-z`, `0-9`, `.`, `_`, `-`.
///
/// Lowercasing makes `Alice` and `alice` the same account, which also
/// absorbs a phone keyboard's automatic capital. The ASCII-only alphabet
/// rules out look-alike Unicode names without a normalization step.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Username(String);

impl Username {
    pub fn parse(input: &str) -> Result<Self, InvalidInput> {
        let username = input.trim().to_ascii_lowercase();
        if username.is_empty() || username.len() > MAX_USERNAME_CHARS {
            return Err(InvalidInput("Username must be 1 to 64 characters long."));
        }
        if !username.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
        }) {
            return Err(InvalidInput(
                "Username may only contain letters a-z, digits, '.', '_' and '-'.",
            ));
        }
        Ok(Self(username))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The client-chosen label of a session, trimmed, 1 to 100 characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceName(String);

impl DeviceName {
    pub fn parse(input: &str) -> Result<Self, InvalidInput> {
        let name = input.trim();
        if name.is_empty() || name.chars().count() > MAX_DEVICE_NAME_CHARS {
            return Err(InvalidInput(
                "Device name must be 1 to 100 characters long.",
            ));
        }
        Ok(Self(name.to_owned()))
    }
}

/// A password chosen for an account, at register or by an admin reset:
/// 8 characters to 1024 bytes.
pub fn new_password(password: String) -> Result<Password, InvalidInput> {
    // Characters, not bytes, so a passphrase in a non-Latin script isn't
    // held to a stricter minimum.
    if password.chars().count() < MIN_PASSWORD_CHARS || password.len() > MAX_PASSWORD_BYTES {
        return Err(InvalidInput(
            "Password must be at least 8 characters and at most 1024 bytes long.",
        ));
    }
    Ok(Password::new(password))
}

/// A validated register request.
#[derive(Debug, Clone)]
pub struct NewAccount {
    pub username: Username,
    pub password: Password,
    pub device_name: DeviceName,
}

impl NewAccount {
    pub fn parse(request: AuthRequest) -> Result<Self, InvalidInput> {
        let username = Username::parse(&request.username)?;
        let password = new_password(request.password)?;
        let device_name = DeviceName::parse(&request.device_name)?;
        Ok(Self {
            username,
            password,
            device_name,
        })
    }
}

/// A login request. Only the device name is held to the input rules; a
/// username or password that breaks them simply matches no account, which
/// keeps login working for accounts made under older rules.
#[derive(Debug, Clone)]
pub struct LoginAttempt {
    /// `None` when the username can't exist under the current rules.
    pub username: Option<Username>,
    pub password: Password,
    pub device_name: DeviceName,
}

impl LoginAttempt {
    pub fn parse(request: AuthRequest) -> Result<Self, InvalidInput> {
        Ok(Self {
            username: Username::parse(&request.username).ok(),
            password: Password::new(request.password),
            device_name: DeviceName::parse(&request.device_name)?,
        })
    }
}

// ==============================================================================
// Sessions
// ==============================================================================

const HOUR_MS: i64 = 60 * 60 * 1000;
/// Sessions live 90 days from their last extension (spec 2.3).
const SESSION_TTL_MS: i64 = 90 * 24 * HOUR_MS;
/// A request extends the session once the last extension is more than this
/// old (spec 2.3).
const EXTENSION_INTERVAL_MS: i64 = 24 * HOUR_MS;
/// `last_seen` is only rewritten once it is this stale. Writing it on every
/// request would turn each read into a row update; a session list doesn't
/// need finer resolution.
const LAST_SEEN_RESOLUTION_MS: i64 = 5 * 60 * 1000;

fn plus_ms(at: Timestamp, ms: i64) -> Timestamp {
    Timestamp::from_epoch_ms(at.as_epoch_ms() + ms)
        .expect("server clock readings plus 90 days stay inside jiff's range")
}

/// A session created by register or login.
#[derive(Debug, Clone)]
pub struct NewSession {
    pub token: SessionToken,
    pub user_id: UserId,
    pub device_id: DeviceId,
    pub expires_at: Timestamp,
}

/// The session behind a valid bearer token.
#[derive(Debug, Clone)]
pub struct AuthSession {
    pub user_id: UserId,
    pub device_id: DeviceId,
    pub username: String,
    /// Identifies the session row, for logout.
    pub token_hash: TokenHash,
}

/// The new `last_seen` and `expires_at` of a session that a request at
/// `now` touched, or `None` when neither needs a write.
///
/// The schema has no "last extension" column. Every extension sets
/// `expires_at` to that moment plus the TTL, so the last extension is
/// `expires_at - 90 days`.
fn touch(
    now: Timestamp,
    last_seen: Timestamp,
    expires_at: Timestamp,
) -> Option<(Timestamp, Timestamp)> {
    let last_extension = plus_ms(expires_at, -SESSION_TTL_MS);
    let extend = now.as_epoch_ms() - last_extension.as_epoch_ms() > EXTENSION_INTERVAL_MS;
    let seen_is_stale = now.as_epoch_ms() - last_seen.as_epoch_ms() >= LAST_SEEN_RESOLUTION_MS;
    if !extend && !seen_is_stale {
        return None;
    }
    let expires_at = if extend {
        plus_ms(now, SESSION_TTL_MS)
    } else {
        expires_at
    };
    Some((now, expires_at))
}

/// Inserts a session with a fresh token and device id.
async fn create_session(
    conn: &mut PgConnection,
    user_id: UserId,
    device_name: &DeviceName,
    now: Timestamp,
) -> Result<NewSession> {
    let token = SessionToken::generate();
    let device_id = DeviceId::new_v7();
    let expires_at = plus_ms(now, SESSION_TTL_MS);
    sqlx::query!(
        "insert into session
             (token_hash, user_id, device_id, device_name, created_at, last_seen, expires_at)
         values ($1, $2, $3, $4, $5, $5, $6)",
        &token.hash().0[..],
        user_id.as_uuid(),
        device_id.as_uuid(),
        device_name.0,
        SqlTimestamp(now) as _,
        SqlTimestamp(expires_at) as _,
    )
    .execute(conn)
    .await
    .context("insert the session")?;
    Ok(NewSession {
        token,
        user_id,
        device_id,
        expires_at,
    })
}

// ==============================================================================
// Operations
// ==============================================================================

#[derive(Debug)]
pub enum RegisterError {
    UsernameTaken,
    Internal(Report),
}

impl From<Report> for RegisterError {
    fn from(report: Report) -> Self {
        Self::Internal(report)
    }
}

/// Creates the account and its first session in one transaction, so a
/// client never ends up with an account it holds no token for.
pub async fn register(
    pool: &PgPool,
    account: NewAccount,
    now: Timestamp,
) -> Result<NewSession, RegisterError> {
    let password_hash = password::hash(account.password)
        .await
        .context("hash the new password")?;
    let user_id = UserId::new_v7();

    let mut tx = pool
        .begin()
        .await
        .context("begin the register transaction")?;
    // The unique constraint, not a prior lookup, decides a taken name, so
    // two concurrent registrations can't both win.
    let inserted = sqlx::query!(
        "insert into users (id, username, password_hash, created_at)
         values ($1, $2, $3, $4)
         on conflict (username) do nothing",
        user_id.as_uuid(),
        account.username.as_str(),
        password_hash,
        SqlTimestamp(now) as _,
    )
    .execute(&mut *tx)
    .await
    .context("insert the user")?;
    if inserted.rows_affected() == 0 {
        return Err(RegisterError::UsernameTaken);
    }
    let session = create_session(&mut tx, user_id, &account.device_name, now)
        .await
        .context("create the first session")?;
    tx.commit().await.context("commit the registration")?;
    Ok(session)
}

#[derive(Debug)]
pub enum LoginError {
    /// Unknown username or wrong password; deliberately not told apart.
    InvalidCredentials,
    Internal(Report),
}

impl From<Report> for LoginError {
    fn from(report: Report) -> Self {
        Self::Internal(report)
    }
}

/// Checks the password and starts a new session with a new device id.
pub async fn login(
    pool: &PgPool,
    attempt: LoginAttempt,
    now: Timestamp,
) -> Result<NewSession, LoginError> {
    // A username or password outside the input rules can't match any
    // account. Answering early leaks nothing, because the rules are public.
    let Some(username) = attempt.username else {
        return Err(LoginError::InvalidCredentials);
    };
    if attempt.password.as_str().len() > MAX_PASSWORD_BYTES {
        return Err(LoginError::InvalidCredentials);
    }

    let user = sqlx::query!(
        "select id, password_hash from users where username = $1",
        username.as_str()
    )
    .fetch_optional(pool)
    .await
    .context("look up the user")?;
    let (user_id, stored_hash) = match user {
        Some(user) => (Some(UserId::from_uuid(user.id)), Some(user.password_hash)),
        None => (None, None),
    };
    let matches = password::verify(attempt.password, stored_hash)
        .await
        .context("check the password")?;
    let Some(user_id) = user_id.filter(|_| matches) else {
        return Err(LoginError::InvalidCredentials);
    };

    let mut conn = pool.acquire().await.context("acquire a connection")?;
    Ok(
        create_session(&mut conn, user_id, &attempt.device_name, now)
            .await
            .context("create the login session")?,
    )
}

/// Loads the unexpired session for `token` and, per [`touch`], slides its
/// expiry and updates `last_seen`. `None` means the token is unknown or
/// expired.
///
/// Expired rows stay until the daily cleanup job deletes them.
pub async fn authenticate(
    pool: &PgPool,
    token: &SessionToken,
    now: Timestamp,
) -> Result<Option<AuthSession>> {
    let token_hash = token.hash();
    let Some(row) = sqlx::query!(
        r#"select s.user_id, s.device_id, u.username,
                  s.last_seen as "last_seen: SqlTimestamp",
                  s.expires_at as "expires_at: SqlTimestamp"
           from session s join users u on u.id = s.user_id
           where s.token_hash = $1 and s.expires_at > $2"#,
        &token_hash.0[..],
        SqlTimestamp(now) as _,
    )
    .fetch_optional(pool)
    .await
    .context("look up the session")?
    else {
        return Ok(None);
    };

    // Two concurrent requests may both write here. They write nearly the
    // same values, so the later one winning is harmless.
    if let Some((last_seen, expires_at)) = touch(now, row.last_seen.0, row.expires_at.0) {
        sqlx::query!(
            "update session set last_seen = $2, expires_at = $3 where token_hash = $1",
            &token_hash.0[..],
            SqlTimestamp(last_seen) as _,
            SqlTimestamp(expires_at) as _,
        )
        .execute(pool)
        .await
        .context("update the session's last_seen and expiry")?;
    }

    Ok(Some(AuthSession {
        user_id: UserId::from_uuid(row.user_id),
        device_id: DeviceId::from_uuid(row.device_id),
        username: row.username,
        token_hash,
    }))
}

/// Deletes the session. Deleting one that is already gone is fine, so a
/// retried logout converges on the same state.
pub async fn logout(pool: &PgPool, token_hash: TokenHash) -> Result<()> {
    sqlx::query!(
        "delete from session where token_hash = $1",
        &token_hash.0[..]
    )
    .execute(pool)
    .await
    .context("delete the session")?;
    Ok(())
}

/// Replaces the user's password and deletes all of their sessions, so every
/// device has to log in with the new password. Returns the number of
/// sessions deleted, or `None` when no user has this name.
///
/// Both writes share a transaction: a session must not outlive the reset
/// that was meant to end it.
pub async fn set_password(
    pool: &PgPool,
    username: &Username,
    password: Password,
) -> Result<Option<u64>> {
    let password_hash = password::hash(password)
        .await
        .context("hash the new password")?;
    let mut tx = pool
        .begin()
        .await
        .context("begin the password reset transaction")?;
    let Some(user_id) = sqlx::query_scalar!(
        "update users set password_hash = $2 where username = $1 returning id",
        username.as_str(),
        password_hash
    )
    .fetch_optional(&mut *tx)
    .await
    .context("update the password hash")?
    else {
        return Ok(None);
    };
    let signed_out = sqlx::query!("delete from session where user_id = $1", user_id)
        .execute(&mut *tx)
        .await
        .context("delete the user's sessions")?
        .rows_affected();
    tx.commit().await.context("commit the password reset")?;
    Ok(Some(signed_out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: i64) -> Timestamp {
        Timestamp::from_epoch_ms(ms).expect("small offsets are in range")
    }

    const T0: i64 = 1_800_000_000_000;

    #[test]
    fn touch_extends_only_after_24_hours() {
        let expires_at = at(T0 + SESSION_TTL_MS);
        let just_under = at(T0 + EXTENSION_INTERVAL_MS);
        assert_eq!(
            touch(just_under, at(T0), expires_at),
            Some((just_under, expires_at)),
            "exactly 24 h is not more than 24 h"
        );
        let just_over = at(T0 + EXTENSION_INTERVAL_MS + 1);
        assert_eq!(
            touch(just_over, at(T0), expires_at),
            Some((
                just_over,
                at(T0 + EXTENSION_INTERVAL_MS + 1 + SESSION_TTL_MS)
            ))
        );
    }

    #[test]
    fn touch_skips_the_write_while_last_seen_is_fresh() {
        let expires_at = at(T0 + SESSION_TTL_MS);
        assert_eq!(
            touch(at(T0 + LAST_SEEN_RESOLUTION_MS - 1), at(T0), expires_at),
            None
        );
        let stale = at(T0 + LAST_SEEN_RESOLUTION_MS);
        assert_eq!(touch(stale, at(T0), expires_at), Some((stale, expires_at)));
    }

    #[test]
    fn usernames_are_normalized_and_restricted() {
        assert_eq!(
            Username::parse("  Alice.B_9-x ").map(|name| name.0),
            Ok("alice.b_9-x".to_owned())
        );
        for input in [
            "",
            "   ",
            "al ice",
            "ålice",
            "alice@example",
            &"a".repeat(65),
        ] {
            assert!(Username::parse(input).is_err(), "{input:?}");
        }
        assert!(Username::parse(&"a".repeat(64)).is_ok());
    }

    fn request(username: &str, password: &str, device_name: &str) -> AuthRequest {
        AuthRequest {
            username: username.into(),
            password: password.into(),
            device_name: device_name.into(),
        }
    }

    #[test]
    fn new_accounts_follow_the_password_and_device_rules() {
        assert!(NewAccount::parse(request("alice", "12345678", "Pixel 8")).is_ok());
        assert!(NewAccount::parse(request("alice", "ääääääää", "Pixel 8")).is_ok());
        for (password, device_name) in [
            ("1234567", "Pixel 8"),
            (&"x".repeat(MAX_PASSWORD_BYTES + 1), "Pixel 8"),
            ("12345678", " "),
            ("12345678", &"x".repeat(MAX_DEVICE_NAME_CHARS + 1)),
        ] {
            assert!(
                NewAccount::parse(request("alice", password, device_name)).is_err(),
                "{device_name:?}"
            );
        }
    }

    #[test]
    fn login_accepts_any_username_and_password_shape() {
        let attempt = LoginAttempt::parse(request("not a name!", "1", "Pixel 8"))
            .expect("only the device name is checked");
        assert_eq!(attempt.username, None);
        assert!(LoginAttempt::parse(request("alice", "1", "")).is_err());
    }
}
