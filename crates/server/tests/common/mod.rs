//! Shared harness: a fresh database per test (dropped at the end), the real
//! router, a tiny browser that keeps the session cookie, a device that talks
//! `/api/v1`, and stand-ins for Floatplane's platform and YouTube.
#![allow(dead_code)]

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use http_body_util::BodyExt as _;
use serde::Serialize;
use serde::de::DeserializeOwned;
use slopstream_core::wire::{PROTOCOL, SyncRequest, SyncResponse};
use slopstream_server::{AppState, MIGRATOR, router};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt as _;
use uuid::Uuid;

pub const PASSWORD: &str = "correct horse battery";

pub struct Db {
    pub pool: PgPool,
    admin: PgPool,
    name: String,
    /// Shared by every state of this test, like the one server process.
    pub feeds: std::sync::Arc<slopstream_server::feeds::FeedsState>,
    pub site: FakeSite,
    pub youtube: FakeYoutube,
}

impl Db {
    /// None (and a SKIP line) when `SLOPSTREAM_TEST_DATABASE_URL` is unset.
    pub async fn new() -> Option<Db> {
        let Ok(base) = std::env::var("SLOPSTREAM_TEST_DATABASE_URL") else {
            eprintln!("SKIP: SLOPSTREAM_TEST_DATABASE_URL unset");
            return None;
        };
        let admin = PgPoolOptions::new().max_connections(1).connect(&base).await.unwrap();
        let name = format!("slopstream_test_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE DATABASE {name}")).execute(&admin).await.unwrap();
        let url = format!("{}/{name}", base.rsplit_once('/').unwrap().0);
        let pool = PgPoolOptions::new().max_connections(8).connect(&url).await.unwrap();
        MIGRATOR.run(&pool).await.unwrap();
        let site = FakeSite::start().await;
        let youtube = FakeYoutube::start().await;
        let feeds = std::sync::Arc::new(slopstream_server::feeds::FeedsState::new(true).without_gaps());
        Some(Db { pool, admin, name, feeds, site, youtube })
    }

    pub fn state(&self) -> AppState {
        AppState {
            cookie_secure: false,
            site_override: Some(self.site.base.clone()),
            youtube_base: self.youtube.base.clone(),
            feeds: self.feeds.clone(),
            ..AppState::new(self.pool.clone(), &[9u8; 32])
        }
    }

    pub fn app(&self) -> Router {
        router(self.state())
    }

    pub async fn drop(self) {
        self.pool.close().await;
        sqlx::query(&format!("DROP DATABASE {} WITH (FORCE)", self.name)).execute(&self.admin).await.unwrap();
    }

    /// A person with a password; returns their id.
    pub async fn person(&self, email: &str, role: slopstream_server::auth::Role) -> Uuid {
        let id = slopstream_server::users::create(&self.pool, email, role).await.unwrap();
        slopstream_server::users::set_password(&self.pool, email, PASSWORD).await.unwrap();
        id
    }

    /// A device of `user` with its own token.
    pub async fn device(&self, user: Uuid, name: &str) -> Device {
        let token = slopstream_server::users::new_token(&self.pool, user, name, true).await.unwrap();
        Device { app: self.app(), token, id: name.to_string(), cursor: 0, fingerprints: Default::default() }
    }
}

pub struct Resp {
    pub status: StatusCode,
    pub headers: axum::http::HeaderMap,
    pub body: String,
}

/// A browser: keeps the session cookie, sends same-origin Origin on writes.
pub struct Browser {
    pub app: Router,
    pub cookie: Option<String>,
}

impl Browser {
    pub fn new(app: Router) -> Self {
        Browser { app, cookie: None }
    }

    pub async fn logged_in(app: Router, email: &str) -> Self {
        let mut b = Browser::new(app);
        let r = b.post("/login", &[("email", email), ("password", PASSWORD)]).await;
        assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.body);
        b
    }

    pub async fn get(&mut self, path: &str) -> Resp {
        self.send(Request::get(path), Body::empty(), None).await
    }

    pub async fn post(&mut self, path: &str, form: &[(&str, &str)]) -> Resp {
        let body = form_body(form);
        self.send(
            Request::post(path).header(header::CONTENT_TYPE, "application/x-www-form-urlencoded"),
            Body::from(body),
            Some("http://test.local"),
        )
        .await
    }

    pub async fn htmx_post(&mut self, path: &str, form: &[(&str, &str)]) -> Resp {
        let body = form_body(form);
        self.send(
            Request::post(path)
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header("hx-request", "true"),
            Body::from(body),
            Some("http://test.local"),
        )
        .await
    }

    pub async fn send(&mut self, mut b: axum::http::request::Builder, body: Body, origin: Option<&str>) -> Resp {
        b = b.header(header::HOST, "test.local");
        if let Some(o) = origin {
            b = b.header(header::ORIGIN, o);
        }
        if let Some(c) = &self.cookie {
            b = b.header(header::COOKIE, c);
        }
        let resp = self.app.clone().oneshot(b.body(body).unwrap()).await.unwrap();
        let status = resp.status();
        let headers = resp.headers().clone();
        if let Some(set) = headers.get(header::SET_COOKIE).and_then(|v| v.to_str().ok()) {
            let kv = set.split(';').next().unwrap().to_string();
            self.cookie = if kv.ends_with('=') { None } else { Some(kv) };
        }
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        Resp { status, headers, body: String::from_utf8_lossy(&bytes).into_owned() }
    }
}

/// A device: its token, its cursor and the session fingerprints it holds,
/// like the app's sync engine keeps them.
pub struct Device {
    pub app: Router,
    pub token: String,
    pub id: String,
    pub cursor: i64,
    pub fingerprints: std::collections::BTreeMap<String, String>,
}

impl Device {
    pub fn request(&self, now: i64) -> SyncRequest {
        SyncRequest {
            protocol: PROTOCOL,
            cursor: self.cursor,
            client_now: now,
            subscriptions: vec![],
            watch_later: vec![],
            watch_state: vec![],
            session_fingerprints: self.fingerprints.clone(),
        }
    }

    /// Sends `req`, applies the cursor and the session fingerprints.
    pub async fn sync(&mut self, req: SyncRequest) -> SyncResponse {
        let (status, body) = self.call("POST", "/api/v1/sync", Some(&req)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let resp: SyncResponse = serde_json::from_str(&body).unwrap();
        self.cursor = resp.cursor;
        self.fingerprints = resp.sessions.iter().map(|s| (s.source.clone(), s.fingerprint.clone())).collect();
        resp
    }

    /// Syncs with no changes until nothing more is waiting.
    pub async fn pull(&mut self) -> SyncResponse {
        let mut all = self.sync(self.request(now_ms())).await;
        while all.more {
            let next = self.sync(self.request(now_ms())).await;
            all.subscriptions.extend(next.subscriptions);
            all.watch_later.extend(next.watch_later);
            all.watch_state.extend(next.watch_state);
            all.sessions = next.sessions;
            all.more = next.more;
            all.cursor = next.cursor;
        }
        all
    }

    pub async fn call<T: Serialize>(&self, method: &str, path: &str, body: Option<&T>) -> (StatusCode, String) {
        let b = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, "test.local")
            .header(header::AUTHORIZATION, format!("Bearer {}", self.token))
            .header(header::CONTENT_TYPE, "application/json");
        let body = body.map(|b| Body::from(serde_json::to_vec(b).unwrap())).unwrap_or_else(Body::empty);
        let resp = self.app.clone().oneshot(b.body(body).unwrap()).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    pub async fn json<T: Serialize, R: DeserializeOwned>(&self, method: &str, path: &str, body: &T) -> R {
        let (status, text) = self.call(method, path, Some(body)).await;
        assert!(status.is_success(), "{status}: {text}");
        serde_json::from_str(&text).unwrap()
    }
}

/// Unauthenticated JSON call (pairing).
pub async fn anon<T: Serialize>(app: &Router, path: &str, body: &T) -> (StatusCode, String) {
    let b = Request::post(path).header(header::HOST, "test.local").header(header::CONTENT_TYPE, "application/json");
    let resp = app.clone().oneshot(b.body(Body::from(serde_json::to_vec(body).unwrap())).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub fn form_body(form: &[(&str, &str)]) -> String {
    form.iter().map(|(k, v)| format!("{}={}", enc(k), enc(v))).collect::<Vec<_>>().join("&")
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            b' ' => "+".into(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Floatplane's platform, as far as the server uses it. Session values that
/// start with `good` are signed in until [`FakeSite::revoke`]; `challenge`
/// gets Cloudflare's HTML 403; anything else is signed out.
#[derive(Clone)]
pub struct FakeSite {
    pub base: String,
    revoked: Arc<Mutex<HashSet<String>>>,
    pub logouts: Arc<Mutex<Vec<String>>>,
    /// Whether creatorA's stream is on air.
    pub live: Arc<std::sync::atomic::AtomicBool>,
    /// Requests to the stream playlist that carried a Cookie (must stay 0).
    pub cookie_leaks: Arc<std::sync::atomic::AtomicUsize>,
    pub api_hits: Arc<std::sync::atomic::AtomicUsize>,
}

impl FakeSite {
    async fn start() -> FakeSite {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let site = FakeSite {
            base,
            revoked: Default::default(),
            logouts: Default::default(),
            live: Default::default(),
            cookie_leaks: Default::default(),
            api_hits: Default::default(),
        };
        let app = Router::new()
            .route("/api/v3/user/self", get(fake_self))
            .route("/api/v3/auth/logout", post(fake_logout))
            .route("/api/v3/creator/info", get(fake_creator))
            .route("/api/v3/content/creator", get(fake_posts))
            .route("/api/v3/creator/channels/list", get(fake_channels))
            .route("/api/v3/delivery/info", get(fake_delivery))
            .route("/live/playlist.m3u8", get(fake_live_playlist))
            .with_state(site.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        site
    }

    pub fn revoke(&self, value: &str) {
        self.revoked.lock().unwrap().insert(value.to_string());
    }
}

fn cookie_value(headers: &HeaderMap) -> String {
    let c = headers.get(header::COOKIE).and_then(|v| v.to_str().ok()).unwrap_or("");
    c.split_once('=').map(|(_, v)| v.to_string()).unwrap_or_default()
}

async fn fake_self(State(site): State<FakeSite>, headers: HeaderMap) -> Response {
    let value = cookie_value(&headers);
    if value.starts_with("challenge") {
        return (StatusCode::FORBIDDEN, "<html>Just a moment...</html>").into_response();
    }
    if value.starts_with("good") && !site.revoked.lock().unwrap().contains(&value) {
        return axum::Json(serde_json::json!({"id": "u1", "username": "sam_fp"})).into_response();
    }
    let body = serde_json::json!({"id": "e", "errors": [{"id": "x", "name": "notLoggedInError", "message": "no"}]});
    (StatusCode::FORBIDDEN, axum::Json(body)).into_response()
}

/// Signed in (`good…`, not revoked) or the explicit refusal.
fn signed_in(site: &FakeSite, headers: &HeaderMap) -> Option<String> {
    site.api_hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let value = cookie_value(headers);
    (value.starts_with("good") && !site.revoked.lock().unwrap().contains(&value)).then_some(value)
}

fn refused() -> Response {
    let body = serde_json::json!({"errors": [{"name": "notLoggedInError"}]});
    (StatusCode::FORBIDDEN, axum::Json(body)).into_response()
}

pub fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{}/../core/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

async fn fake_creator(State(site): State<FakeSite>, headers: HeaderMap) -> Response {
    if signed_in(&site, &headers).is_none() {
        return refused();
    }
    fixture("creator.json").into_response()
}

/// `good-limited…` sessions are on a plan that only includes the sub-channel's post.
async fn fake_posts(State(site): State<FakeSite>, headers: HeaderMap) -> Response {
    let Some(value) = signed_in(&site, &headers) else { return refused() };
    let mut posts: Vec<serde_json::Value> = serde_json::from_str(&fixture("posts.json")).unwrap();
    if value.starts_with("good-limited") {
        for p in &mut posts {
            if p["id"] != "postSubChan" {
                p["isAccessible"] = false.into();
            }
        }
    }
    axum::Json(posts).into_response()
}

async fn fake_channels(State(site): State<FakeSite>, headers: HeaderMap) -> Response {
    if signed_in(&site, &headers).is_none() {
        return refused();
    }
    fixture("channels.json").into_response()
}

async fn fake_delivery(State(site): State<FakeSite>, headers: HeaderMap) -> Response {
    if signed_in(&site, &headers).is_none() {
        return refused();
    }
    let body = serde_json::json!({"groups": [{"origins": [{"url": site.base}], "variants": [{"url": "/live/playlist.m3u8", "enabled": true}]}]});
    axum::Json(body).into_response()
}

async fn fake_live_playlist(State(site): State<FakeSite>, headers: HeaderMap) -> StatusCode {
    if headers.contains_key(header::COOKIE) {
        site.cookie_leaks.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
    if site.live.load(std::sync::atomic::Ordering::SeqCst) { StatusCode::OK } else { StatusCode::NOT_FOUND }
}

async fn fake_logout(State(site): State<FakeSite>, headers: HeaderMap) -> StatusCode {
    site.logouts.lock().unwrap().push(cookie_value(&headers));
    StatusCode::OK
}

pub const CHANNEL_ID: &str = "UCXuqSBlHAE6Xw-yeJA0Tunw";
/// The fixture channel of the app's RSS tests: Videos and Shorts playlists, no streams.
pub const FIXTURE_CHANNEL: &str = "UCaaaaaaaaaaaaaaaaaaaaaa";

/// YouTube: `/@LinusTechTips` names [`CHANNEL_ID`]; the fixture channel's
/// playlists answer with the app's fixtures. Counts feed requests; `status`
/// set to e.g. 429 answers every feed request with it.
#[derive(Clone)]
pub struct FakeYoutube {
    pub base: String,
    pub feed_hits: Arc<std::sync::atomic::AtomicUsize>,
    pub status: Arc<std::sync::atomic::AtomicU16>,
}

impl FakeYoutube {
    async fn start() -> FakeYoutube {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let yt = FakeYoutube {
            base: format!("http://{}", listener.local_addr().unwrap()),
            feed_hits: Default::default(),
            status: Default::default(),
        };
        #[derive(serde::Deserialize)]
        struct FeedQuery {
            channel_id: Option<String>,
            playlist_id: Option<String>,
        }
        let app = Router::new()
            .route(
                "/@LinusTechTips",
                get(|| async {
                    axum::response::Html(format!(
                        "<html><link rel=\"canonical\" href=\"https://www.youtube.com/channel/{CHANNEL_ID}\">\
                         <meta property=\"og:image\" content=\"https://yt3.googleusercontent.com/ltt=s900\"></html>"
                    ))
                }),
            )
            .route(
                "/feeds/videos.xml",
                get(|State(yt): State<FakeYoutube>, Query(q): Query<FeedQuery>| async move {
                    yt.feed_hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let forced = yt.status.load(std::sync::atomic::Ordering::SeqCst);
                    if forced != 0 {
                        return StatusCode::from_u16(forced).unwrap().into_response();
                    }
                    let tail = &FIXTURE_CHANNEL[2..];
                    match (q.channel_id.as_deref(), q.playlist_id.as_deref()) {
                        (Some(CHANNEL_ID), _) => "<feed xmlns=\"http://www.w3.org/2005/Atom\"><author><name>Linus Tech Tips</name></author><title>Linus Tech Tips</title></feed>".into_response(),
                        (_, Some(p)) if p == format!("UULF{tail}") => fixture("uulf.xml").into_response(),
                        (_, Some(p)) if p == format!("UUSH{tail}") => fixture("uush.xml").into_response(),
                        _ => StatusCode::NOT_FOUND.into_response(),
                    }
                }),
            )
            .with_state(yt.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        yt
    }
}
