//! `pod-server admin` commands for an operator without a UI.
//!
//! `main` parses the command line, connects and migrates, then calls one of
//! these with stdout as `out`. Each prints plain text and returns an error
//! for anything the operator has to fix, which `main` turns into exit code
//! 1.

use std::io::Write;

use color_eyre::eyre::{Result, WrapErr, bail};
use pod_protocol::{EpisodeId, Timestamp, UserId};
use sqlx::PgPool;

use crate::auth::{self, InvalidInput, Username};
use crate::db::SqlTimestamp;
use crate::ingest::{self, PurgeOutcome};

/// `purge-episode <episode_id>`: tombstones the episode so clients drop it
/// on their next pull. It stays purged until its publisher changes it (see
/// [`ingest::purge_episode`]).
pub async fn purge_episode(
    pool: &PgPool,
    episode_id: EpisodeId,
    out: &mut impl Write,
) -> Result<()> {
    let outcome = ingest::purge_episode(pool, episode_id)
        .await
        .context("purge the episode")?;
    match outcome {
        Some(PurgeOutcome::Purged { version }) => {
            writeln!(out, "purged (version {version})").context("write the purge result")?;
        }
        Some(PurgeOutcome::AlreadyDeleted) => {
            writeln!(out, "already deleted").context("write the purge result")?;
        }
        None => bail!("no such episode {episode_id}"),
    }
    Ok(())
}

/// `list-users`: one tab-separated line per account, by username, with the
/// number of sessions still valid at `now`. Tabs keep it easy to pipe into
/// `cut` or `awk`.
pub async fn list_users(pool: &PgPool, now: Timestamp, out: &mut impl Write) -> Result<()> {
    let users = sqlx::query!(
        r#"select u.id, u.username, u.created_at as "created_at: SqlTimestamp",
                  count(s.token_hash) filter (where s.expires_at > $1) as "sessions!"
           from users u left join session s on s.user_id = u.id
           group by u.id
           order by u.username"#,
        SqlTimestamp(now) as _
    )
    .fetch_all(pool)
    .await
    .context("list the users")?;
    writeln!(out, "id\tusername\tcreated_at\tsessions").context("write the header")?;
    for user in users {
        writeln!(
            out,
            "{}\t{}\t{}\t{}",
            UserId::from_uuid(user.id),
            user.username,
            user.created_at.0,
            user.sessions
        )
        .context("write a user line")?;
    }
    Ok(())
}

/// `reset-password <username>`: sets a new password under the register
/// rules and signs the user out everywhere.
///
/// The username is normalized as at login, so `Alice` finds `alice`.
pub async fn reset_password(
    pool: &PgPool,
    username: &str,
    password: String,
    out: &mut impl Write,
) -> Result<()> {
    let password = match auth::new_password(password) {
        Ok(password) => password,
        Err(InvalidInput(message)) => bail!(message),
    };
    // A name that breaks the username rules can't belong to an account.
    let Ok(username) = Username::parse(username) else {
        bail!("no such user {username}");
    };
    let Some(signed_out) = auth::set_password(pool, &username, password)
        .await
        .context("set the new password")?
    else {
        bail!("no such user {}", username.as_str());
    };
    let sessions = if signed_out == 1 {
        "session"
    } else {
        "sessions"
    };
    writeln!(
        out,
        "reset the password of {} and signed out {signed_out} {sessions}; \
         every device must log in again",
        username.as_str()
    )
    .context("write the reset result")?;
    Ok(())
}
