//! Slopstream server library: the router and stores, split from `main` so
//! integration tests drive the real app.

pub mod api;
pub mod auth;
pub mod config;
pub mod crypto;
pub mod error;
pub mod library;
pub mod pairing;
pub mod sessions;
pub mod sites;
pub mod users;
pub mod web;
pub mod youtube;

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::get;
use slopstream_core::services::Site;
use sqlx::PgPool;
use tower_http::compression::CompressionLayer;
use tower_http::trace::TraceLayer;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    /// `Secure` flag on the web session cookie (and https in invite links).
    pub cookie_secure: bool,
    /// `SLOPSTREAM_PUBLIC_URL`, for the pairing page a TV points at.
    pub public_url: Option<String>,
    /// Service sessions at rest.
    pub secrets: Arc<crypto::Secrets>,
    /// Floatplane and Sauce+ (no redirects: the Cookie header stays put).
    pub http: reqwest::Client,
    /// YouTube channel pages and feeds (redirects on the same host only).
    pub yt_http: reqwest::Client,
    /// Tests: one stand-in base URL for every site, instead of the real ones.
    pub site_override: Option<String>,
    /// www.youtube.com, or a stand-in in tests.
    pub youtube_base: String,
}

impl AppState {
    pub fn new(pool: PgPool, secret_key: &[u8; 32]) -> Self {
        Self {
            pool,
            cookie_secure: true,
            public_url: None,
            secrets: Arc::new(crypto::Secrets::new(secret_key)),
            http: sites::client(),
            yt_http: youtube::client(),
            site_override: None,
            youtube_base: youtube::BASE.to_string(),
        }
    }

    pub fn site_base(&self, site: &Site) -> String {
        self.site_override.clone().unwrap_or_else(|| site.base_url.to_string())
    }

    /// This server as devices and invitees reach it.
    pub fn public_base(&self, headers: &HeaderMap) -> String {
        self.public_url.clone().unwrap_or_else(|| web::base_url(headers, self.cookie_secure))
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .nest("/api/v1", api::routes())
        .merge(web::routes())
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn healthz(State(st): State<AppState>) -> Result<&'static str, error::ApiError> {
    sqlx::query("SELECT 1").execute(&st.pool).await?;
    Ok("ok")
}
