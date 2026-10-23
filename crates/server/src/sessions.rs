//! A person's own Floatplane and Sauce+ sessions, kept for their devices.
//!
//! - In: a device after its sign-in (`PUT /api/v1/sessions/{source}`), or a
//!   paste on the website. The server checks it with the service first.
//! - Out: only to that person's own tokens, inside `/sync`, and only when
//!   the device's fingerprint differs. Never rendered, logged or listed.
//! - Expiry: a device reports it with the fingerprint it used; an old
//!   fingerprint is ignored (that device just had a stale copy). The
//!   checker asks the services every [`CHECK_EVERY`].

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use slopstream_core::fingerprint;
use slopstream_core::services::{self, Service, Site};
use slopstream_core::wire::{SessionInfo, SessionStatus};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::AppState;
use crate::crypto::Secrets;
use crate::error::ApiError;
use crate::library::now_ms;
use crate::sites::{self, Check};

pub const CHECK_EVERY: Duration = Duration::from_secs(6 * 3600);

fn aad(user: Uuid, source: &str) -> Vec<u8> {
    [user.as_bytes().as_slice(), b"/", source.as_bytes()].concat()
}

pub fn site_of(source: &str) -> Result<(&'static Service, &'static Site), ApiError> {
    let svc = services::by_key(source).ok_or(ApiError::NotFound)?;
    let site = svc.site.as_ref().ok_or_else(|| ApiError::bad(format!("{} has no sign-in", svc.label)))?;
    Ok((svc, site))
}

fn status_str(s: SessionStatus) -> &'static str {
    match s {
        SessionStatus::Ok => "ok",
        SessionStatus::Expired => "expired",
        SessionStatus::Unchecked => "unchecked",
    }
}

fn parse_status(s: &str) -> SessionStatus {
    match s {
        "ok" => SessionStatus::Ok,
        "expired" => SessionStatus::Expired,
        _ => SessionStatus::Unchecked,
    }
}

/// Checks the session with the service and stores it. A refusal is an
/// error; a check that could not happen stores it as unchecked, since the
/// device that sent it had signed in with it.
pub async fn put(
    st: &AppState,
    user: Uuid,
    source: &str,
    value: &str,
    username: Option<&str>,
) -> Result<SessionStatus, ApiError> {
    let (svc, site) = site_of(source)?;
    let value = services::session_value(value, site.session_cookie)
        .ok_or_else(|| ApiError::bad(format!("that does not look like a {} session", site.session_cookie)))?;
    let (status, username, checked) =
        match sites::check(&st.http, &st.site_base(site), site.session_cookie, &value).await {
            Check::Valid { username } => (SessionStatus::Ok, Some(username), true),
            Check::Rejected => return Err(ApiError::bad(format!("{} says this session is signed out", svc.label))),
            Check::Unknown(why) => {
                tracing::info!(source, why, "stored a session the server could not check");
                (SessionStatus::Unchecked, username.map(|u| u.chars().take(100).collect()), false)
            }
        };
    let sealed = st.secrets.seal(&value, &aad(user, source));
    sqlx::query(
        "INSERT INTO service_sessions (user_id, source, ciphertext, fingerprint, username, status, checked_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, CASE WHEN $7 THEN now() END, $8)
         ON CONFLICT (user_id, source) DO UPDATE SET ciphertext = $3, fingerprint = $4, username = $5, status = $6,
           checked_at = CASE WHEN $7 THEN now() END, updated_at = $8",
    )
    .bind(user)
    .bind(source)
    .bind(sealed)
    .bind(fingerprint(&value))
    .bind(username)
    .bind(status_str(status))
    .bind(checked)
    .bind(now_ms())
    .execute(&st.pool)
    .await?;
    Ok(status)
}

async fn value_of(pool: &PgPool, secrets: &Secrets, user: Uuid, source: &str) -> Result<Option<String>, ApiError> {
    let row: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT ciphertext FROM service_sessions WHERE user_id = $1 AND source = $2")
            .bind(user)
            .bind(source)
            .fetch_optional(pool)
            .await?;
    row.map(|c| secrets.open(&c, &aad(user, source)).map_err(|e| ApiError::Internal(e.into()))).transpose()
}

/// A device's service refused the session with `fp`. Returns whether the
/// report counted. The server asks the service itself first: only its own
/// "signed in" overrules the device.
pub async fn report_expired(st: &AppState, user: Uuid, source: &str, fp: &str) -> Result<bool, ApiError> {
    let (_, site) = site_of(source)?;
    let current: Option<String> =
        sqlx::query_scalar("SELECT fingerprint FROM service_sessions WHERE user_id = $1 AND source = $2")
            .bind(user)
            .bind(source)
            .fetch_optional(&st.pool)
            .await?;
    if current.as_deref() != Some(fp) {
        return Ok(false);
    }
    let Some(value) = value_of(&st.pool, &st.secrets, user, source).await? else { return Ok(false) };
    let check = sites::check(&st.http, &st.site_base(site), site.session_cookie, &value).await;
    let expired = !matches!(check, Check::Valid { .. });
    let status = if expired { SessionStatus::Expired } else { SessionStatus::Ok };
    set_status(&st.pool, user, source, fp, status, !matches!(check, Check::Unknown(_))).await?;
    Ok(expired)
}

/// Only while the session is still the one with fingerprint `fp`: a renewal
/// that lands meanwhile is never marked by a check of its predecessor.
async fn set_status(
    pool: &PgPool,
    user: Uuid,
    source: &str,
    fp: &str,
    status: SessionStatus,
    checked: bool,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE service_sessions SET status = $4, updated_at = CASE WHEN status <> $4 THEN $5 ELSE updated_at END,
           checked_at = CASE WHEN $6 THEN now() ELSE checked_at END
         WHERE user_id = $1 AND source = $2 AND fingerprint = $3",
    )
    .bind(user)
    .bind(source)
    .bind(fp)
    .bind(status_str(status))
    .bind(now_ms())
    .bind(checked)
    .execute(pool)
    .await?;
    Ok(())
}

/// Signs out everywhere: at the service (when `at_service`), then forgets it.
pub async fn remove(st: &AppState, user: Uuid, source: &str, at_service: bool) -> Result<(), ApiError> {
    let (_, site) = site_of(source)?;
    if at_service && let Some(value) = value_of(&st.pool, &st.secrets, user, source).await? {
        sites::logout(&st.http, &st.site_base(site), site.session_cookie, &value).await;
    }
    sqlx::query("DELETE FROM service_sessions WHERE user_id = $1 AND source = $2")
        .bind(user)
        .bind(source)
        .execute(&st.pool)
        .await?;
    Ok(())
}

/// What `/sync` returns: every session, with the value only where the
/// device's fingerprint differs.
pub async fn for_sync(
    conn: &mut PgConnection,
    secrets: &Secrets,
    user: Uuid,
    held: &BTreeMap<String, String>,
) -> Result<Vec<SessionInfo>, ApiError> {
    #[derive(sqlx::FromRow)]
    struct Stored {
        source: String,
        ciphertext: Vec<u8>,
        fingerprint: String,
        username: Option<String>,
        status: String,
        updated_at: i64,
    }
    let rows: Vec<Stored> = sqlx::query_as(
        "SELECT source, ciphertext, fingerprint, username, status, updated_at FROM service_sessions
          WHERE user_id = $1 ORDER BY source",
    )
    .bind(user)
    .fetch_all(conn)
    .await?;
    let mut out = Vec::new();
    for r in rows {
        let value = if held.get(&r.source) == Some(&r.fingerprint) {
            None
        } else {
            Some(secrets.open(&r.ciphertext, &aad(user, &r.source)).map_err(|e| ApiError::Internal(e.into()))?)
        };
        out.push(SessionInfo {
            status: parse_status(&r.status),
            source: r.source,
            username: r.username,
            fingerprint: r.fingerprint,
            value,
            updated_at: r.updated_at,
        });
    }
    Ok(out)
}

/// For the website: never the value.
#[derive(sqlx::FromRow)]
pub struct Summary {
    pub source: String,
    #[sqlx(try_from = "String")]
    pub status: Status,
    pub username: Option<String>,
    pub checked_at: Option<DateTime<Utc>>,
}

/// A stored status column.
pub struct Status(pub SessionStatus);

impl From<String> for Status {
    fn from(s: String) -> Self {
        Status(parse_status(&s))
    }
}

pub async fn summaries(pool: &PgPool, user: Uuid) -> Result<Vec<Summary>, sqlx::Error> {
    sqlx::query_as("SELECT source, status, username, checked_at FROM service_sessions WHERE user_id = $1")
        .bind(user)
        .fetch_all(pool)
        .await
}

/// Asks the service about one stored session now ("Check now").
pub async fn check_one(st: &AppState, user: Uuid, source: &str) -> Result<Check, ApiError> {
    let (_, site) = site_of(source)?;
    let fp: Option<String> =
        sqlx::query_scalar("SELECT fingerprint FROM service_sessions WHERE user_id = $1 AND source = $2")
            .bind(user)
            .bind(source)
            .fetch_optional(&st.pool)
            .await?;
    let (Some(fp), Some(value)) = (fp, value_of(&st.pool, &st.secrets, user, source).await?) else {
        return Err(ApiError::NotFound);
    };
    let check = sites::check(&st.http, &st.site_base(site), site.session_cookie, &value).await;
    apply(&st.pool, user, source, &fp, &check).await?;
    Ok(check)
}

async fn apply(pool: &PgPool, user: Uuid, source: &str, fp: &str, check: &Check) -> Result<(), sqlx::Error> {
    match check {
        Check::Valid { username } => {
            set_status(pool, user, source, fp, SessionStatus::Ok, true).await?;
            sqlx::query(
                "UPDATE service_sessions SET username = $4 WHERE user_id = $1 AND source = $2 AND fingerprint = $3",
            )
            .bind(user)
            .bind(source)
            .bind(fp)
            .bind(username)
            .execute(pool)
            .await?;
        }
        Check::Rejected => set_status(pool, user, source, fp, SessionStatus::Expired, true).await?,
        // A challenge or an outage says nothing about the session.
        Check::Unknown(_) => {}
    }
    Ok(())
}

/// The background checker: every stored session that is not known expired,
/// once per [`CHECK_EVERY`], spaced out so the services see no burst.
pub async fn run_checker(st: AppState) {
    loop {
        if let Err(e) = check_all(&st).await {
            tracing::warn!(error = %e, "session check round failed");
        }
        tokio::time::sleep(CHECK_EVERY).await;
    }
}

pub async fn check_all(st: &AppState) -> Result<(), ApiError> {
    let rows: Vec<(Uuid, String, String)> =
        sqlx::query_as("SELECT user_id, source, fingerprint FROM service_sessions WHERE status <> 'expired'")
            .fetch_all(&st.pool)
            .await?;
    for (user, source, fp) in rows {
        let Ok((_, site)) = site_of(&source) else { continue };
        let Some(value) = value_of(&st.pool, &st.secrets, user, &source).await? else { continue };
        let check = sites::check(&st.http, &st.site_base(site), site.session_cookie, &value).await;
        if let Check::Unknown(why) = &check {
            tracing::info!(source, why, "session check inconclusive");
        }
        apply(&st.pool, user, &source, &fp, &check).await?;
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Ok(())
}
