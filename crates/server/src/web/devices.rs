//! Devices: the tokens a person's devices use, pairing a new one with the
//! code it shows, and tokens to paste by hand.

use askama::Template;
use axum::Form;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use uuid::Uuid;

use super::session::WebUser;
use super::{Nav, html, is_htmx, local_day};
use crate::AppState;
use crate::auth::token_hash;
use crate::error::{ApiError, WebError};
use crate::pairing::{self, MAX_FAILURES};
use crate::users;

type Page = Result<Response, WebError>;

pub struct TokenRow {
    pub id: Uuid,
    pub label: String,
    pub paired: bool,
    pub created: String,
    pub last_used: String,
}

#[derive(Template)]
#[template(path = "devices.html")]
struct DevicesPage {
    nav: Nav,
    tokens: Vec<TokenRow>,
    server: String,
    paired: Option<String>,
}

#[derive(Deserialize)]
pub struct PairedQuery {
    paired: Option<String>,
}

pub async fn page(
    State(st): State<AppState>,
    user: WebUser,
    headers: HeaderMap,
    axum::extract::Query(q): axum::extract::Query<PairedQuery>,
) -> Page {
    #[derive(sqlx::FromRow)]
    struct Stored {
        id: Uuid,
        label: String,
        paired: bool,
        created_at: DateTime<Utc>,
        last_used_at: Option<DateTime<Utc>>,
    }
    let rows: Vec<Stored> = sqlx::query_as(
        "SELECT id, label, paired, created_at, last_used_at FROM api_tokens
          WHERE user_id = $1 AND revoked_at IS NULL ORDER BY created_at DESC",
    )
    .bind(user.id)
    .fetch_all(&st.pool)
    .await?;
    let tokens = rows
        .into_iter()
        .map(|r| TokenRow {
            id: r.id,
            label: r.label,
            paired: r.paired,
            created: local_day(r.created_at, user.tz),
            last_used: r.last_used_at.map(|t| local_day(t, user.tz)).unwrap_or_else(|| "never".into()),
        })
        .collect();
    html(DevicesPage { nav: Nav::new(&user, "devices"), tokens, server: st.public_base(&headers), paired: q.paired })
}

#[derive(Template)]
#[template(path = "token_created.html")]
struct TokenCreated {
    label: String,
    token: String,
}

#[derive(Deserialize)]
pub struct TokenForm {
    label: String,
}

/// HTMX: the new token replaces the form, shown once.
pub async fn create_token(State(st): State<AppState>, user: WebUser, Form(f): Form<TokenForm>) -> Page {
    let token = users::new_token(&st.pool, user.id, &f.label, false).await.map_err(|e| ApiError::bad(e.to_string()))?;
    html(TokenCreated { label: f.label.trim().to_string(), token })
}

/// The device stops syncing at once. Its library stays on the device.
pub async fn revoke(State(st): State<AppState>, user: WebUser, Path(id): Path<Uuid>, headers: HeaderMap) -> Page {
    sqlx::query("UPDATE api_tokens SET revoked_at = now() WHERE id = $1 AND user_id = $2 AND revoked_at IS NULL")
        .bind(id)
        .bind(user.id)
        .execute(&st.pool)
        .await?;
    if is_htmx(&headers) {
        return Ok("".into_response());
    }
    Ok(Redirect::to("/devices").into_response())
}

#[derive(Template)]
#[template(path = "pair.html")]
struct PairPage {
    nav: Nav,
    code: String,
    error: Option<&'static str>,
    /// Found: (pairing id, device name).
    found: Option<(Uuid, String)>,
}

pub async fn pair_page(user: WebUser) -> Page {
    html(PairPage { nav: Nav::new(&user, "devices"), code: String::new(), error: None, found: None })
}

#[derive(Deserialize)]
pub struct CodeForm {
    code: String,
}

/// Finds the device by the code it shows. Wrong codes count against this
/// browser session; after [`MAX_FAILURES`] it takes no more.
pub async fn pair_code(State(st): State<AppState>, user: WebUser, Form(f): Form<CodeForm>) -> Page {
    let failures: i32 = sqlx::query_scalar("SELECT pair_failures FROM web_sessions WHERE token_sha256 = $1")
        .bind(token_hash(&user.token))
        .fetch_one(&st.pool)
        .await?;
    let nav = Nav::new(&user, "devices");
    if failures >= MAX_FAILURES {
        let error = Some("Too many wrong codes. Log out and in again to try more.");
        return html(PairPage { nav, code: f.code, error, found: None });
    }
    match pairing::find(&st.pool, &f.code).await? {
        Some(found) => html(PairPage { nav, code: f.code, error: None, found: Some(found) }),
        None => {
            sqlx::query("UPDATE web_sessions SET pair_failures = pair_failures + 1 WHERE token_sha256 = $1")
                .bind(token_hash(&user.token))
                .execute(&st.pool)
                .await?;
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            let error = Some("No device is waiting with that code. Check it, or start pairing again on the device.");
            html(PairPage { nav, code: f.code, error, found: None })
        }
    }
}

pub async fn approve(State(st): State<AppState>, user: WebUser, Path(id): Path<Uuid>) -> Page {
    if !pairing::decide(&st.pool, id, user.id, true).await? {
        return Err(ApiError::bad("that pairing request has expired; start again on the device").into());
    }
    Ok(Redirect::to("/devices?paired=1").into_response())
}

pub async fn deny(State(st): State<AppState>, user: WebUser, Path(id): Path<Uuid>) -> Page {
    pairing::decide(&st.pool, id, user.id, false).await?;
    Ok(Redirect::to("/devices").into_response())
}
