//! Subscriptions, Watch later and History, one service at a time. Every
//! change here is a write with a new rev, so the person's devices pick it up
//! on their next sync.

use askama::Template;
use axum::Form;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;
use slopstream_core::services::{self, Service};
use slopstream_core::wire::{Subscription, WatchLater, WatchState};

use super::session::WebUser;
use super::{Nav, Tab, html, is_htmx, local_time_ms, service, tabs};
use crate::AppState;
use crate::error::{ApiError, WebError};
use crate::library::{self, WEB_DEVICE};
use crate::{feeds, youtube};

type Page = Result<Response, WebError>;

#[derive(Deserialize)]
pub struct ServiceQuery {
    service: Option<String>,
}

/// A row named by a form: its service and id (ids can hold `:`, so never
/// in the path).
#[derive(Deserialize)]
pub struct KeyForm {
    source: String,
    remote_id: String,
}

impl KeyForm {
    fn service(&self) -> Result<&'static Service, ApiError> {
        services::by_key(&self.source).ok_or(ApiError::NotFound)
    }
}

pub struct SubRow {
    pub source: String,
    pub remote_id: String,
    pub name: String,
    pub avatar_url: Option<String>,
    pub favorite: bool,
    pub can_unfollow: bool,
    /// How fresh the server's list of this channel is; None before the first fetch.
    pub fetched: Option<String>,
    pub fetch_error: Option<String>,
}

impl SubRow {
    fn of(s: Subscription, svc: &Service) -> Self {
        SubRow {
            fetched: None,
            fetch_error: None,
            name: if s.name.is_empty() { s.remote_id.clone() } else { s.name },
            source: s.source,
            remote_id: s.remote_id,
            avatar_url: s.avatar_url,
            favorite: s.favorite,
            can_unfollow: svc.can_unsubscribe,
        }
    }
}

#[derive(Template)]
#[template(path = "subscriptions.html")]
struct SubscriptionsPage {
    nav: Nav,
    tabs: Vec<Tab>,
    service: &'static Service,
    rows: Vec<SubRow>,
    added: bool,
}

#[derive(Template)]
#[template(path = "sub_row.html")]
struct SubRowFragment {
    r: SubRow,
}

#[derive(Deserialize)]
pub struct SubsQuery {
    service: Option<String>,
    added: Option<String>,
}

pub async fn subscriptions(State(st): State<AppState>, user: WebUser, Query(q): Query<SubsQuery>) -> Page {
    let svc = service(q.service.as_deref());
    let rows = library::list::<Subscription>(&st.pool, user.id, svc.key, "favorite DESC, lower(name)").await?;
    let status = feeds::status_of(&st.pool, user.id, svc.key).await?;
    let rows = rows
        .into_iter()
        .map(|s| {
            let st = status.get(&s.remote_id).cloned();
            let mut r = SubRow::of(s, svc);
            if let Some((ok, err)) = st {
                r.fetched = ok.map(ago);
                r.fetch_error = err;
            }
            r
        })
        .collect();
    html(SubscriptionsPage {
        nav: Nav::new(&user, "subscriptions"),
        tabs: tabs(svc),
        service: svc,
        rows,
        added: q.added.is_some(),
    })
}

#[derive(Deserialize)]
pub struct FollowForm {
    source: String,
    channel: String,
}

pub async fn follow(State(st): State<AppState>, user: WebUser, Form(f): Form<FollowForm>) -> Page {
    let svc = services::by_key(&f.source).ok_or(ApiError::NotFound)?;
    if !svc.add_on_web {
        return Err(ApiError::bad(format!("{} channels are added in the app", svc.label)).into());
    }
    let ch = youtube::resolve(&st.yt_http, &st.youtube_base, &f.channel).await?;
    let sub = Subscription {
        source: svc.key.to_string(),
        remote_id: ch.id,
        name: ch.name,
        avatar_url: ch.avatar_url,
        favorite: false,
        added_at: 0,
        updated_at: 0,
        deleted_at: None,
        device_id: WEB_DEVICE.to_string(),
    };
    library::follow(&st.pool, user.id, sub).await?;
    st.changes.bump(user.id);
    st.feeds.wake.notify_one();
    Ok(Redirect::to(&format!("/subscriptions?service={}&added=1", svc.key)).into_response())
}

pub async fn favorite(State(st): State<AppState>, user: WebUser, headers: HeaderMap, Form(f): Form<KeyForm>) -> Page {
    let svc = f.service()?;
    let row =
        library::edit::<Subscription>(&st.pool, user.id, &f.source, &f.remote_id, |r, _| r.favorite = !r.favorite)
            .await?;
    st.changes.bump(user.id);
    if is_htmx(&headers) {
        let status = feeds::status_of(&st.pool, user.id, svc.key).await?;
        let mut r = SubRow::of(row, svc);
        if let Some((ok, err)) = status.get(&r.remote_id).cloned() {
            r.fetched = ok.map(ago);
            r.fetch_error = err;
        }
        return html(SubRowFragment { r });
    }
    Ok(Redirect::to(&format!("/subscriptions?service={}", svc.key)).into_response())
}

pub async fn unfollow(State(st): State<AppState>, user: WebUser, headers: HeaderMap, Form(f): Form<KeyForm>) -> Page {
    let svc = f.service()?;
    if !svc.can_unsubscribe {
        return Err(ApiError::bad(format!(
            "{} follows come from the account; unsubscribe on {}",
            svc.label, svc.label
        ))
        .into());
    }
    library::edit::<Subscription>(&st.pool, user.id, &f.source, &f.remote_id, |r, now| r.deleted_at = Some(now))
        .await?;
    st.changes.bump(user.id);
    gone(&headers, "/subscriptions", svc)
}

fn ago(t: chrono::DateTime<chrono::Utc>) -> String {
    let min = (chrono::Utc::now() - t).num_minutes();
    match min {
        ..1 => "just now".into(),
        1..60 => format!("{min} min ago"),
        60..1440 => format!("{} h ago", min / 60),
        _ => format!("{} days ago", min / 1440),
    }
}

/// HTMX removes the row with an empty swap; a plain form goes back.
fn gone(headers: &HeaderMap, page: &str, svc: &Service) -> Page {
    if is_htmx(headers) {
        return Ok("".into_response());
    }
    Ok(Redirect::to(&format!("{page}?service={}", svc.key)).into_response())
}

pub struct VideoRow {
    pub source: String,
    pub remote_id: String,
    pub title: String,
    pub channel: String,
    pub thumbnail_url: Option<String>,
    /// Watch later: published; History: last watched.
    pub when: String,
    /// History only.
    pub progress: Option<u8>,
    pub watched: bool,
}

#[derive(Template)]
#[template(path = "videos.html")]
struct VideosPage {
    nav: Nav,
    tabs: Vec<Tab>,
    service: &'static Service,
    /// `watch-later` or `history`: the actions' path.
    page: &'static str,
    /// The page's own path, for the service tabs.
    base: &'static str,
    heading: &'static str,
    empty: &'static str,
    remove_label: &'static str,
    rows: Vec<VideoRow>,
}

pub async fn watch_later(State(st): State<AppState>, user: WebUser, Query(q): Query<ServiceQuery>) -> Page {
    let svc = service(q.service.as_deref());
    let rows = library::list::<WatchLater>(&st.pool, user.id, svc.key, "added_at DESC").await?;
    let rows = rows
        .into_iter()
        .map(|r| VideoRow {
            when: local_time_ms(r.published_at, user.tz),
            source: r.source,
            remote_id: r.remote_id,
            title: r.title,
            channel: r.channel_name,
            thumbnail_url: r.thumbnail_url,
            progress: None,
            watched: false,
        })
        .collect();
    html(VideosPage {
        nav: Nav::new(&user, "watch-later"),
        tabs: tabs(svc),
        service: svc,
        page: "watch-later",
        base: "/watch-later",
        heading: "Watch later",
        empty: "Nothing saved for later. Long-press a video in the app to add it.",
        remove_label: "Remove",
        rows,
    })
}

pub async fn history(State(st): State<AppState>, user: WebUser, Query(q): Query<ServiceQuery>) -> Page {
    let svc = service(q.service.as_deref());
    let rows = library::list::<WatchState>(&st.pool, user.id, svc.key, "updated_at DESC").await?;
    let rows = rows
        .into_iter()
        .map(|r| VideoRow {
            when: local_time_ms(r.updated_at, user.tz),
            progress: (r.duration_ms > 0).then(|| ((r.position_ms.min(r.duration_ms) * 100) / r.duration_ms) as u8),
            watched: r.watched,
            title: r.title.unwrap_or_else(|| r.remote_id.clone()),
            channel: r.channel_name.unwrap_or_default(),
            thumbnail_url: r.thumbnail_url,
            source: r.source,
            remote_id: r.remote_id,
        })
        .collect();
    html(VideosPage {
        nav: Nav::new(&user, "history"),
        tabs: tabs(svc),
        service: svc,
        page: "history",
        base: "/history",
        heading: "History",
        empty: "Nothing watched yet on a synced device.",
        remove_label: "Forget",
        rows,
    })
}

pub async fn remove_later(
    State(st): State<AppState>,
    user: WebUser,
    headers: HeaderMap,
    Form(f): Form<KeyForm>,
) -> Page {
    let svc = f.service()?;
    library::edit::<WatchLater>(&st.pool, user.id, &f.source, &f.remote_id, |r, now| r.deleted_at = Some(now)).await?;
    st.changes.bump(user.id);
    gone(&headers, "/watch-later", svc)
}

/// Forgets the progress too: on every device the video looks unwatched.
pub async fn remove_history(
    State(st): State<AppState>,
    user: WebUser,
    headers: HeaderMap,
    Form(f): Form<KeyForm>,
) -> Page {
    let svc = f.service()?;
    library::edit::<WatchState>(&st.pool, user.id, &f.source, &f.remote_id, |r, now| r.deleted_at = Some(now)).await?;
    st.changes.bump(user.id);
    gone(&headers, "/history", svc)
}
