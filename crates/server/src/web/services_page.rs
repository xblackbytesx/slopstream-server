//! A person's Floatplane and Sauce+ sessions: their state, a paste box to
//! renew one, "Check now", and "Sign out everywhere". The value itself is
//! write-only here: never rendered, not even masked.

use askama::Template;
use axum::Form;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;
use slopstream_core::services::{self, Service};
use slopstream_core::wire::SessionStatus;

use super::session::WebUser;
use super::{Nav, html, local_day};
use crate::AppState;
use crate::error::{ApiError, WebError};
use crate::sessions;
use crate::sites::Check;

type Page = Result<Response, WebError>;

pub struct ServiceRow {
    pub key: &'static str,
    pub label: &'static str,
    pub cookie: &'static str,
    /// None: no session stored.
    pub status: Option<&'static str>,
    pub username: Option<String>,
    pub checked: Option<String>,
}

#[derive(Template)]
#[template(path = "services.html")]
struct ServicesPage {
    nav: Nav,
    rows: Vec<ServiceRow>,
    notice: Option<String>,
}

#[derive(Deserialize)]
pub struct NoticeQuery {
    done: Option<String>,
    service: Option<String>,
}

fn label(s: SessionStatus) -> &'static str {
    match s {
        SessionStatus::Ok => "Signed in",
        SessionStatus::Expired => "Expired: sign in again",
        SessionStatus::Unchecked => "Stored, not checked by the server",
    }
}

pub async fn page(State(st): State<AppState>, user: WebUser, Query(q): Query<NoticeQuery>) -> Page {
    let stored = sessions::summaries(&st.pool, user.id).await?;
    let rows = services::with_sessions()
        .map(|svc| {
            let s = stored.iter().find(|s| s.source == svc.key);
            ServiceRow {
                key: svc.key,
                label: svc.label,
                cookie: svc.site.map(|s| s.session_cookie).unwrap_or(""),
                status: s.map(|s| label(s.status.0)),
                username: s.and_then(|s| s.username.clone()),
                checked: s.and_then(|s| s.checked_at).map(|t| local_day(t, user.tz)),
            }
        })
        .collect();
    let svc = q.service.as_deref().and_then(services::by_key).map(|s| s.label).unwrap_or("The service");
    let notice = match q.done.as_deref() {
        Some("saved") => Some(format!("{svc}: saved. Your devices pick it up on their next sync.")),
        Some("unchecked") => Some(format!(
            "{svc}: saved, but the server could not check it (the service may be asking this server for a challenge). \
             Your devices will try it."
        )),
        Some("valid") => Some(format!("{svc}: still signed in.")),
        Some("expired") => {
            Some(format!("{svc}: the session has expired. Sign in again in the app, or paste a new one."))
        }
        Some("unknown") => Some(format!("{svc}: the server could not ask; nothing changed.")),
        Some("signedout") => Some(format!("{svc}: signed out everywhere.")),
        _ => None,
    };
    html(ServicesPage { nav: Nav::new(&user, "services"), rows, notice })
}

fn site_service(key: &str) -> Result<&'static Service, ApiError> {
    sessions::site_of(key).map(|(svc, _)| svc)
}

#[derive(Deserialize)]
pub struct PasteForm {
    source: String,
    session: String,
}

pub async fn paste(State(st): State<AppState>, user: WebUser, Form(f): Form<PasteForm>) -> Page {
    let svc = site_service(&f.source)?;
    let status = sessions::put(&st, user.id, svc.key, &f.session, None).await?;
    let done = if status == SessionStatus::Ok { "saved" } else { "unchecked" };
    Ok(Redirect::to(&format!("/services?done={done}&service={}", svc.key)).into_response())
}

#[derive(Deserialize)]
pub struct SourceForm {
    source: String,
}

pub async fn check(State(st): State<AppState>, user: WebUser, Form(f): Form<SourceForm>) -> Page {
    let svc = site_service(&f.source)?;
    let done = match sessions::check_one(&st, user.id, svc.key).await? {
        Check::Valid { .. } => "valid",
        Check::Rejected => "expired",
        Check::Unknown(_) => "unknown",
    };
    Ok(Redirect::to(&format!("/services?done={done}&service={}", svc.key)).into_response())
}

pub async fn sign_out(State(st): State<AppState>, user: WebUser, Form(f): Form<SourceForm>) -> Page {
    let svc = site_service(&f.source)?;
    sessions::remove(&st, user.id, svc.key, true).await?;
    Ok(Redirect::to(&format!("/services?done=signedout&service={}", svc.key)).into_response())
}
