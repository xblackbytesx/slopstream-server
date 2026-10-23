//! Pairing a device: it asks for a code, shows it, and polls; a signed-in
//! person types the code on the website and approves. The device then gets
//! its own token, labelled with its name. Like OAuth's device flow, but for
//! our own server only.

use chrono::{Duration, Utc};
use slopstream_core::wire::{PairResult, PairStarted, PairStatus, Person};
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::{self, DEVICE_CODE_PREFIX};
use crate::error::ApiError;
use crate::users;

pub const TTL_SEC: i64 = 600;
pub const INTERVAL_SEC: i64 = 5;
/// Wrong codes one web session may type before it stops accepting codes.
pub const MAX_FAILURES: i32 = 5;
/// Pending pairings at once (the start endpoint needs no login).
const MAX_OPEN: i64 = 50;
/// No 0/O, 1/I: read off a TV across the room.
const ALPHABET: &[u8] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";

pub fn new_user_code() -> String {
    let mut buf = [0u8; 8];
    getrandom::fill(&mut buf).expect("OS randomness");
    // 32 symbols: the low five bits of each byte are uniform.
    let s: String = buf.iter().map(|b| ALPHABET[(*b & 31) as usize] as char).collect();
    format!("{}-{}", &s[..4], &s[4..])
}

/// The code as typed (any case, with or without the dash) in its stored
/// form, or None when it cannot be one.
pub fn normalize(input: &str) -> Option<String> {
    let s: String = input.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_uppercase()).collect();
    (s.len() == 8 && s.bytes().all(|b| ALPHABET.contains(&b))).then_some(s)
}

pub async fn start(pool: &PgPool, device_name: &str, verification_url: String) -> Result<PairStarted, ApiError> {
    let name: String = device_name.trim().chars().filter(|c| !c.is_control()).take(60).collect();
    if name.is_empty() {
        return Err(ApiError::bad("give the device a name"));
    }
    sqlx::query("DELETE FROM pairings WHERE expires_at < now() - interval '1 day'").execute(pool).await?;
    let open: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pairings WHERE expires_at > now() AND approved_at IS NULL AND denied_at IS NULL",
    )
    .fetch_one(pool)
    .await?;
    if open >= MAX_OPEN {
        return Err(ApiError::Busy("too many devices are waiting to pair; try again in a few minutes".into()));
    }
    let device_code = auth::new_secret(DEVICE_CODE_PREFIX);
    // A clash of two pending 40-bit codes is unlikely; retry rather than fail.
    for _ in 0..5 {
        let user_code = new_user_code();
        let inserted = sqlx::query(
            "INSERT INTO pairings (id, device_code_sha256, user_code_sha256, device_name, expires_at)
             VALUES ($1, $2, $3, $4, $5) ON CONFLICT (user_code_sha256) DO NOTHING",
        )
        .bind(Uuid::new_v4())
        .bind(auth::token_hash(&device_code))
        .bind(auth::token_hash(&normalize(&user_code).expect("own code")))
        .bind(&name)
        .bind(Utc::now() + Duration::seconds(TTL_SEC))
        .execute(pool)
        .await?;
        if inserted.rows_affected() == 1 {
            return Ok(PairStarted {
                device_code,
                user_code,
                verification_url,
                expires_in_sec: TTL_SEC,
                interval_sec: INTERVAL_SEC,
            });
        }
    }
    Err(ApiError::Busy("could not make a pairing code; try again".into()))
}

/// What the device learns when it polls. The token is made here, once.
pub async fn poll(pool: &PgPool, device_code: &str) -> Result<PairResult, ApiError> {
    let pending = PairResult { status: PairStatus::Pending, token: None, person: None };
    let ended = |status| PairResult { status, token: None, person: None };
    let row: Option<(Uuid, String, bool, bool, bool, bool)> = sqlx::query_as(
        "SELECT id, device_name, expires_at < now(), approved_at IS NOT NULL, denied_at IS NOT NULL,
                delivered_at IS NOT NULL
           FROM pairings WHERE device_code_sha256 = $1",
    )
    .bind(auth::token_hash(device_code))
    .fetch_optional(pool)
    .await?;
    let Some((id, name, expired, approved, denied, delivered)) = row else { return Ok(ended(PairStatus::Expired)) };
    if denied {
        return Ok(ended(PairStatus::Denied));
    }
    if delivered || (expired && !approved) {
        return Ok(ended(PairStatus::Expired));
    }
    if !approved {
        return Ok(pending);
    }
    // Claim it first, so two racing polls cannot both get a token.
    let user: Option<Uuid> = sqlx::query_scalar(
        "UPDATE pairings SET delivered_at = now() WHERE id = $1 AND delivered_at IS NULL RETURNING approved_by",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .flatten();
    let Some(user) = user else { return Ok(ended(PairStatus::Expired)) };
    let token = users::new_token(pool, user, &name, true).await.map_err(ApiError::Internal)?;
    let (name, email): (String, String) =
        sqlx::query_as("SELECT display_name, email FROM users WHERE id = $1").bind(user).fetch_one(pool).await?;
    Ok(PairResult { status: PairStatus::Approved, token: Some(token), person: Some(Person { name, email }) })
}

/// A pending pairing by its user code: (id, device name).
pub async fn find(pool: &PgPool, user_code: &str) -> Result<Option<(Uuid, String)>, sqlx::Error> {
    let Some(code) = normalize(user_code) else { return Ok(None) };
    sqlx::query_as(
        "SELECT id, device_name FROM pairings WHERE user_code_sha256 = $1 AND expires_at > now()
            AND approved_at IS NULL AND denied_at IS NULL",
    )
    .bind(auth::token_hash(&code))
    .fetch_optional(pool)
    .await
}

/// Approves (or denies) a still-pending pairing; false when it is gone.
pub async fn decide(pool: &PgPool, id: Uuid, user: Uuid, approve: bool) -> Result<bool, sqlx::Error> {
    let sql = if approve {
        "UPDATE pairings SET approved_at = now(), approved_by = $2
          WHERE id = $1 AND expires_at > now() AND approved_at IS NULL AND denied_at IS NULL"
    } else {
        "UPDATE pairings SET denied_at = now()
          WHERE id = $1 AND expires_at > now() AND approved_at IS NULL AND denied_at IS NULL AND $2::uuid IS NOT NULL"
    };
    Ok(sqlx::query(sql).bind(id).bind(user).execute(pool).await?.rows_affected() == 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_read_well_and_normalize() {
        for _ in 0..200 {
            let c = new_user_code();
            assert_eq!(c.len(), 9);
            assert_eq!(&c[4..5], "-");
            assert!(!c.contains(['0', 'O', '1', 'I']), "{c}");
            assert_eq!(normalize(&c).unwrap(), c.replace('-', ""));
        }
        assert_eq!(normalize(" kj7w 3mpq ").as_deref(), Some("KJ7W3MPQ"));
        assert_eq!(normalize("KJ7W-3MP"), None);
        assert_eq!(normalize("OOOO-IIII"), None, "letters a code never has");
    }
}
