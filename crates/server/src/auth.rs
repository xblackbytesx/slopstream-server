//! Bearer-token auth for `/api/v1`, and the token format shared with sessions,
//! invites and pairing: 256 random bits, shown once, stored only as SHA-256.

use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use base64::Engine as _;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::AppState;
use crate::error::ApiError;

pub const TOKEN_PREFIX: &str = "ss_";
pub const INVITE_PREFIX: &str = "ssi_";
pub const DEVICE_CODE_PREFIX: &str = "ssd_";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Admin,
    User,
}

impl Role {
    pub fn parse(s: &str) -> Role {
        if s == "admin" { Role::Admin } else { Role::User }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::User => "user",
        }
    }
}

/// A fresh 256-bit secret with `prefix`.
pub fn new_secret(prefix: &str) -> String {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).expect("OS randomness");
    format!("{prefix}{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf))
}

pub fn new_token() -> String {
    new_secret(TOKEN_PREFIX)
}

/// Plain SHA-256 is enough for 256-bit random secrets (no dictionary to try).
pub fn token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

#[derive(Debug, Clone)]
pub struct AuthUser {
    pub id: Uuid,
    pub role: Role,
    /// The token used: each device has its own.
    pub token_id: Uuid,
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Self, Self::Rejection> {
        let token = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .filter(|t| t.starts_with(TOKEN_PREFIX))
            .ok_or(ApiError::Unauthorized)?;
        let row: Option<(Uuid, String, Uuid)> = sqlx::query_as(
            "UPDATE api_tokens t SET last_used_at = now() FROM users u
             WHERE t.token_sha256 = $1 AND t.revoked_at IS NULL AND u.id = t.user_id AND u.disabled_at IS NULL
             RETURNING u.id, u.role, t.id",
        )
        .bind(token_hash(token))
        .fetch_optional(&st.pool)
        .await?;
        let (id, role, token_id) = row.ok_or(ApiError::Unauthorized)?;
        Ok(AuthUser { id, role: Role::parse(&role), token_id })
    }
}
