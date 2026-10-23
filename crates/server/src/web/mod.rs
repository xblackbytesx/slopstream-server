//! The website: server-rendered askama pages, enhanced with HTMX. It manages
//! state only; nothing plays here.
//!
//! No inline script or style anywhere (the CSP forbids both); HTMX and the
//! small `app.js` are served from here, and HTMX's own inline indicator
//! styles and eval-based features are turned off in `base.html`.

mod admin;
mod devices;
mod library_pages;
mod pages;
mod services_page;
pub mod session;

use std::sync::LazyLock;

use axum::Router;
use axum::extract::{DefaultBodyLimit, Request};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, ETAG, HOST, IF_NONE_MATCH, ORIGIN};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use sha2::{Digest, Sha256};
use slopstream_core::services::{self, Service};

use crate::AppState;
use session::WebUser;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(|| async { axum::response::Redirect::to("/subscriptions") }))
        .route("/subscriptions", get(library_pages::subscriptions))
        .route("/subscriptions/add", post(library_pages::follow))
        .route("/subscriptions/favorite", post(library_pages::favorite))
        .route("/subscriptions/unfollow", post(library_pages::unfollow))
        .route("/watch-later", get(library_pages::watch_later))
        .route("/watch-later/remove", post(library_pages::remove_later))
        .route("/history", get(library_pages::history))
        .route("/history/remove", post(library_pages::remove_history))
        .route("/services", get(services_page::page))
        .route("/services/session", post(services_page::paste))
        .route("/services/check", post(services_page::check))
        .route("/services/signout", post(services_page::sign_out))
        .route("/devices", get(devices::page))
        .route("/devices/tokens", post(devices::create_token))
        .route("/devices/tokens/{id}/revoke", post(devices::revoke))
        .route("/pair", get(devices::pair_page).post(devices::pair_code))
        .route("/pair/{id}/approve", post(devices::approve))
        .route("/pair/{id}/deny", post(devices::deny))
        .route("/login", get(pages::login_page).post(pages::login))
        .route("/logout", post(pages::logout))
        .route("/invite/{token}", get(pages::invite_page).post(pages::accept_invite))
        .route("/settings", get(pages::settings))
        .route("/settings/profile", post(pages::save_profile))
        .route("/settings/password", post(pages::change_password))
        .route("/admin", get(|| async { axum::response::Redirect::to("/admin/users") }))
        .route("/admin/users", get(admin::users))
        .route("/admin/invites", post(admin::create_invite))
        .route("/admin/invites/{id}/delete", post(admin::delete_invite))
        .route("/admin/users/{id}/role", post(admin::set_role))
        .route("/admin/users/{id}/disable", post(admin::disable))
        .route("/admin/users/{id}/enable", post(admin::enable))
        .route("/static/app.css", get(|h: HeaderMap| asset(h, &CSS, "text/css")))
        .route("/static/app.js", get(|h: HeaderMap| asset(h, &JS, "text/javascript")))
        .route("/static/htmx.min.js", get(|h: HeaderMap| asset(h, &HTMX, "text/javascript")))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(middleware::from_fn(guard))
}

/// An embedded static file with its ETag, computed once.
pub struct Asset {
    body: &'static [u8],
    etag: String,
}

fn embed(body: &'static [u8]) -> Asset {
    Asset { body, etag: format!("\"{}\"", &hex::encode(Sha256::digest(body))[..16]) }
}

static CSS: LazyLock<Asset> = LazyLock::new(|| embed(include_bytes!("../../static/app.css")));
static JS: LazyLock<Asset> = LazyLock::new(|| embed(include_bytes!("../../static/app.js")));
/// htmx 2.0.4, vendored. sha256 in AGENTS.md.
static HTMX: LazyLock<Asset> = LazyLock::new(|| embed(include_bytes!("../../static/htmx.min.js")));

/// `no-cache` + ETag: a deploy is picked up on the next page view, and every
/// other view is a 304 with no body.
async fn asset(headers: HeaderMap, a: &Asset, content_type: &'static str) -> Response {
    let etag = HeaderValue::from_str(&a.etag).expect("hex etag");
    let common = [(CACHE_CONTROL, HeaderValue::from_static("no-cache")), (ETAG, etag)];
    if headers.get(IF_NONE_MATCH).is_some_and(|v| v.as_bytes() == a.etag.as_bytes()) {
        return (StatusCode::NOT_MODIFIED, common).into_response();
    }
    (common, [(CONTENT_TYPE, HeaderValue::from_static(content_type))], a.body).into_response()
}

/// Thumbnails and avatars load from the hosts the app gets them from; with
/// `same-origin` referrers they learn nothing about this server.
pub const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; \
     img-src 'self' https://i.ytimg.com https://yt3.ggpht.com https://yt3.googleusercontent.com https://pbs.floatplane.com; \
     connect-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'";

/// Refuses cross-site writes (Origin must name this host when sent) and sets
/// the page security headers. SameSite=Strict cookies already stop most CSRF;
/// this is the second lock. Every method except GET/HEAD counts as a write,
/// since HTMX can send DELETE and PUT. `/api/v1` is not behind it: bearer
/// tokens are not sent by browsers on their own.
async fn guard(req: Request, next: Next) -> Response {
    if !matches!(*req.method(), Method::GET | Method::HEAD)
        && let Some(origin) = req.headers().get(ORIGIN).and_then(|v| v.to_str().ok())
    {
        let host = req.headers().get(HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
        let origin_host = origin.split("://").nth(1).unwrap_or("");
        if origin_host != host {
            return (StatusCode::FORBIDDEN, "cross-site request refused").into_response();
        }
    }
    let htmx = is_htmx(req.headers());
    let mut resp = next.run(req).await;
    if !htmx && resp.headers().contains_key("hx-retarget") {
        resp = error_page(resp).await;
    }
    let h = resp.headers_mut();
    h.insert("content-security-policy", HeaderValue::from_static(CSP));
    h.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    // Not `no-referrer`: under it browsers send `Origin: null` on every form
    // POST (Fetch spec), which the check above refuses. `same-origin` keeps
    // the real Origin on our own posts and still sends nothing cross-site.
    h.insert("referrer-policy", HeaderValue::from_static("same-origin"));
    resp
}

/// An error fragment (see `WebError`) reached a plain browser request (no
/// JavaScript): wrap it in a minimal page with a way back.
async fn error_page(resp: Response) -> Response {
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024).await.unwrap_or_default();
    let fragment = String::from_utf8_lossy(&bytes);
    let page = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" \
         content=\"width=device-width, initial-scale=1\"><title>Slopstream</title><link rel=\"stylesheet\" \
         href=\"/static/app.css\"></head><body><main class=\"narrow\"><section class=\"card\">{fragment}\
         <p>Use your browser's back button, or <a href=\"/\">start over</a>.</p></section></main></body></html>"
    );
    (status, axum::response::Html(page)).into_response()
}

/// What every logged-in page needs for the header.
pub struct Nav {
    pub label: String,
    pub admin: bool,
    pub active: &'static str,
}

impl Nav {
    pub fn new(u: &WebUser, active: &'static str) -> Self {
        Nav { label: u.label().to_string(), admin: u.is_admin(), active }
    }

    /// `class="active"` for the current section's link.
    pub fn on(&self, section: &str) -> &'static str {
        if self.active == section { "active" } else { "" }
    }
}

/// One service tab. Services stay apart here as in the app.
pub struct Tab {
    pub key: &'static str,
    pub label: &'static str,
    pub active: bool,
}

pub fn tabs(current: &Service) -> Vec<Tab> {
    services::ALL.iter().map(|s| Tab { key: s.key, label: s.label, active: s.key == current.key }).collect()
}

/// The service named by `?service=`, YouTube by default.
pub fn service(key: Option<&str>) -> &'static Service {
    key.and_then(services::by_key).unwrap_or(&services::YOUTUBE)
}

pub fn html(t: impl askama::Template) -> Result<Response, crate::error::WebError> {
    t.render()
        .map(|s| axum::response::Html(s).into_response())
        .map_err(|e| crate::error::ApiError::Internal(anyhow::anyhow!("template: {e}")).into())
}

/// Whether the request came from HTMX (fragment) or a full page load.
pub fn is_htmx(headers: &HeaderMap) -> bool {
    headers.contains_key("hx-request")
}

/// This server's own base URL as the browser sees it (invite links).
pub fn base_url(headers: &HeaderMap, secure: bool) -> String {
    let host = headers.get(HOST).and_then(|v| v.to_str().ok()).unwrap_or("localhost");
    format!("{}://{host}", if secure { "https" } else { "http" })
}

/// A date in the viewer's time zone.
pub fn local_day(t: chrono::DateTime<chrono::Utc>, tz: chrono_tz::Tz) -> String {
    t.with_timezone(&tz).format("%Y-%m-%d").to_string()
}

/// Epoch milliseconds as a local date and time.
pub fn local_time_ms(ms: i64, tz: chrono_tz::Tz) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|t| t.with_timezone(&tz).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}
