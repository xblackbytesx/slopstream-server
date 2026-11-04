//! `/api/v1`: what the app talks to. Bearer tokens (one per device); every
//! route acts as the token's owner and sees only that person's library and
//! sessions. Pairing is the only part that needs no token. Versioned: a
//! breaking change gets a new prefix, never an edit here.

use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use slopstream_core::remote::{
    ControlRequest, ReceiveAnswer, ReceiveRequest, RemoteStatus, SendAnswer, SendRequest, StateReport, Targets,
};
use slopstream_core::wire::{
    ChangesAnswer, DurationsStored, ExpiredReport, FeedDurations, FeedRefresh, FeedRequest, FeedResponse, PairPoll,
    PairResult, PairStart, PairStarted, Person, SessionPut, SessionStatus, SyncRequest, SyncResponse,
};

use crate::AppState;
use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::library::{self, now_ms};
use crate::{feeds, pairing, remote, sessions};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/me", get(me))
        .route("/sync", post(sync))
        .route("/pair/start", post(pair_start))
        .route("/pair/poll", post(pair_poll))
        .route("/sessions/{source}", put(put_session).delete(delete_session))
        .route("/sessions/{source}/expired", post(expired))
        .route("/feed", post(feed))
        .route("/feed/refresh", post(feed_refresh))
        .route("/feed/durations", post(feed_durations))
        .route("/changes", get(changes))
        .route("/remote/receive", post(remote_receive))
        .route("/remote/offline", post(remote_offline))
        .route("/remote/state", post(remote_state))
        .route("/remote/targets", get(remote_targets))
        .route("/remote/{target}/play", post(remote_play))
        .route("/remote/{target}/control", post(remote_control))
        .route("/remote/{target}/status", get(remote_status))
        // A full sync request: 3 x 500 rows of a few hundred bytes each.
        .layer(DefaultBodyLimit::max(4 * 1024 * 1024))
}

async fn me(State(st): State<AppState>, u: AuthUser) -> Result<Json<Person>, ApiError> {
    let (name, email): (String, String) =
        sqlx::query_as("SELECT display_name, email FROM users WHERE id = $1").bind(u.id).fetch_one(&st.pool).await?;
    Ok(Json(Person { name, email }))
}

async fn sync(
    State(st): State<AppState>,
    u: AuthUser,
    Json(req): Json<SyncRequest>,
) -> Result<Json<SyncResponse>, ApiError> {
    let followed = !req.subscriptions.is_empty();
    let (resp, wrote) = library::sync(&st.pool, &st.secrets, u.id, req, now_ms()).await?;
    if wrote {
        // The person's other devices, waiting on /changes, fetch it now.
        st.changes.bump(u.id);
        // A follow from a device: its channel's list is fetched without waiting for the round.
        if followed {
            st.feeds.wake.notify_one();
        }
    }
    Ok(Json(resp))
}

#[derive(Deserialize)]
struct SinceQuery {
    since: Option<u64>,
}

/// Long-poll: answers when something changed for this person, or after ~25 s.
async fn changes(State(st): State<AppState>, u: AuthUser, Query(q): Query<SinceQuery>) -> Json<ChangesAnswer> {
    let (generation, changed) = st.changes.wait(u.id, q.since.unwrap_or(u64::MAX), crate::changes::WAIT).await;
    Json(ChangesAnswer { generation, changed })
}

async fn feed_durations(
    State(st): State<AppState>,
    u: AuthUser,
    Json(req): Json<FeedDurations>,
) -> Result<Json<DurationsStored>, ApiError> {
    Ok(Json(feeds::report_durations(&st, u.id, u.token_id, &req).await?))
}

async fn feed(
    State(st): State<AppState>,
    u: AuthUser,
    Json(req): Json<FeedRequest>,
) -> Result<Json<FeedResponse>, ApiError> {
    Ok(Json(feeds::feed_for(&st.pool, u.id, u.token_id, &req).await?))
}

async fn feed_refresh(State(st): State<AppState>, u: AuthUser) -> Result<Json<FeedRefresh>, ApiError> {
    Ok(Json(feeds::refresh(&st, u.id).await?))
}

async fn pair_start(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<PairStart>,
) -> Result<Json<PairStarted>, ApiError> {
    let url = format!("{}/pair", st.public_base(&headers));
    Ok(Json(pairing::start(&st.pool, &req.device_name, url).await?))
}

async fn pair_poll(State(st): State<AppState>, Json(req): Json<PairPoll>) -> Result<Json<PairResult>, ApiError> {
    Ok(Json(pairing::poll(&st.pool, &req.device_code).await?))
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Stored {
    status: SessionStatus,
}

/// The device signed in (or in again): every other device of this person
/// gets the session on its next sync.
async fn put_session(
    State(st): State<AppState>,
    u: AuthUser,
    Path(source): Path<String>,
    Json(req): Json<SessionPut>,
) -> Result<Json<Stored>, ApiError> {
    let status = sessions::put(&st, u.id, &source, &req.value, req.username.as_deref()).await?;
    Ok(Json(Stored { status }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeleteQuery {
    /// Also end the session at the service (the device did not manage to).
    #[serde(default)]
    at_service: bool,
}

/// Signed out everywhere.
async fn delete_session(
    State(st): State<AppState>,
    u: AuthUser,
    Path(source): Path<String>,
    Query(q): Query<DeleteQuery>,
) -> Result<Response, ApiError> {
    sessions::remove(&st, u.id, &source, q.at_service).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Counted {
    /// False: the report named an older session; sync for the current one.
    counted: bool,
}

async fn expired(
    State(st): State<AppState>,
    u: AuthUser,
    Path(source): Path<String>,
    Json(req): Json<ExpiredReport>,
) -> Result<Json<Counted>, ApiError> {
    Ok(Json(Counted { counted: sessions::report_expired(&st, u.id, &source, &req.fingerprint).await? }))
}

/// A TV waiting for something to play (the open app's loop).
async fn remote_receive(
    State(st): State<AppState>,
    u: AuthUser,
    Json(req): Json<ReceiveRequest>,
) -> Result<Json<ReceiveAnswer>, ApiError> {
    Ok(Json(st.remote.receive(&st, &u, req, remote::WAIT).await?))
}

async fn remote_offline(State(st): State<AppState>, u: AuthUser) -> StatusCode {
    st.remote.offline(&u);
    StatusCode::NO_CONTENT
}

async fn remote_state(
    State(st): State<AppState>,
    u: AuthUser,
    Json(rep): Json<StateReport>,
) -> Result<StatusCode, ApiError> {
    st.remote.report(&st, &u, rep).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn remote_targets(State(st): State<AppState>, u: AuthUser) -> Json<Targets> {
    Json(Targets { targets: st.remote.targets(&u) })
}

async fn remote_play(
    State(st): State<AppState>,
    u: AuthUser,
    Path(target): Path<uuid::Uuid>,
    Json(req): Json<SendRequest>,
) -> Result<Json<SendAnswer>, ApiError> {
    Ok(Json(SendAnswer { session: st.remote.send(target, &u, req)? }))
}

async fn remote_control(
    State(st): State<AppState>,
    u: AuthUser,
    Path(target): Path<uuid::Uuid>,
    Json(req): Json<ControlRequest>,
) -> Result<StatusCode, ApiError> {
    st.remote.control(target, &u, req)?;
    Ok(StatusCode::NO_CONTENT)
}

/// The remote screen's long-poll: what the TV plays.
async fn remote_status(
    State(st): State<AppState>,
    u: AuthUser,
    Path(target): Path<uuid::Uuid>,
    Query(q): Query<SinceQuery>,
) -> Result<Json<RemoteStatus>, ApiError> {
    Ok(Json(st.remote.status(target, &u, q.since, remote::WAIT).await?))
}
