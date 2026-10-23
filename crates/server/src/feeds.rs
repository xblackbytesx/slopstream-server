//! The video lists, fetched here all day so devices find them ready (app
//! decision 19, AGENTS.md "Feeds"). Lists only: streams are always resolved on
//! the device. One task: reconcile what is followed, claim the channels that
//! are due, fetch, store, schedule the next fetch. Postgres is the queue.
//!
//! - YouTube: the channel's three uploads-playlist RSS feeds, as the app
//!   fetches them; one fetch serves everyone who follows the channel.
//! - Floatplane, Sauce+: per person, with their own session (what a post
//!   includes depends on their plan). Never to another host with the cookie.
//! - The last good copy stays: a failed fetch only reschedules.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use slopstream_core::feeds::floatplane::{self as fp, Creator, Delivery, Post, SubChannel};
use slopstream_core::feeds::youtube::{self, Upload};
use slopstream_core::feeds::{ChannelFeed, FeedItem, KEEP_PER_KIND};
use slopstream_core::services::{self, Site};
use slopstream_core::wire::{
    ChannelStatus, FEED_PAGE, FeedRefresh, FeedRequest, FeedResponse, FeedRow, MAX_FEED_CHANNELS, PROTOCOL,
};
use sqlx::{PgConnection, PgPool};
use tokio::sync::{Notify, Semaphore};
use uuid::Uuid;

use crate::AppState;
use crate::error::ApiError;
use crate::library::now_ms;
use crate::sessions;

/// The owner of YouTube rows: everyone's.
pub const SHARED: Uuid = Uuid::nil();
const MAX_BODY: usize = 2 * 1024 * 1024;
const YT_PARALLEL: usize = 4;
const YT_BATCH: i64 = 40;
const SITE_BATCH: i64 = 10;
/// A claimed channel's lease: if the server stops mid-fetch, it is due again.
const LEASE: Duration = Duration::from_secs(10 * 60);
const YT_PAUSE: Duration = Duration::from_secs(3600);
const SITE_PAUSE: Duration = Duration::from_secs(15 * 60);
/// Followed by nobody this long: the channel and its items go.
const UNFOLLOWED_DAYS: i64 = 7;
const TOMBSTONE_DAYS: i64 = 7;

pub struct FeedsState {
    /// `SLOPSTREAM_FEEDS=false`: nothing is fetched; devices fetch everything.
    pub enabled: bool,
    pub wake: Notify,
    /// Pause between two Floatplane-platform requests (0 in tests).
    pub site_gap: Duration,
    paused: Mutex<HashMap<&'static str, Instant>>,
    refreshed: Mutex<HashMap<Uuid, Instant>>,
}

impl FeedsState {
    pub fn new(enabled: bool) -> Self {
        FeedsState {
            enabled,
            wake: Notify::new(),
            site_gap: Duration::from_secs(1),
            paused: Mutex::new(HashMap::new()),
            refreshed: Mutex::new(HashMap::new()),
        }
    }

    /// No pause between Floatplane requests (tests).
    pub fn without_gaps(mut self) -> Self {
        self.site_gap = Duration::ZERO;
        self
    }

    fn paused(&self, source: &'static str) -> bool {
        self.paused.lock().unwrap().get(source).is_some_and(|until| Instant::now() < *until)
    }

    fn pause(&self, source: &'static str, d: Duration) {
        tracing::warn!(source, minutes = d.as_secs() / 60, "the service refused us; pausing its feeds");
        self.paused.lock().unwrap().insert(source, Instant::now() + d);
    }

    /// Pull-to-refresh: at most once a minute per person.
    pub fn may_refresh(&self, user: Uuid) -> bool {
        let mut m = self.refreshed.lock().unwrap();
        let now = Instant::now();
        if m.get(&user).is_some_and(|t| now.duration_since(*t) < Duration::from_secs(60)) {
            return false;
        }
        m.insert(user, now);
        true
    }
}

/// How long until the next fetch. `jitter` in [-1, 1] spreads channels out.
pub fn interval(favorite: bool, last_upload_ms: Option<i64>, now_ms: i64, jitter: f64) -> Duration {
    let quiet_days = last_upload_ms.map(|t| (now_ms - t) / 86_400_000);
    let base = match quiet_days {
        Some(d) if d > 180 => 12 * 3600,
        Some(d) if d > 30 => 3 * 3600,
        _ if favorite => 15 * 60,
        _ => 30 * 60,
    } as f64;
    Duration::from_secs_f64(base * (1.0 + 0.1 * jitter.clamp(-1.0, 1.0)))
}

/// After `failures` failures in a row: 30 min doubling, at most 12 h.
pub fn backoff(failures: i32) -> Duration {
    let steps = failures.clamp(1, 10) - 1;
    Duration::from_secs(((30 * 60u64) << steps).min(12 * 3600))
}

fn jitter() -> f64 {
    let mut b = [0u8; 2];
    getrandom::fill(&mut b).expect("OS randomness");
    (u16::from_le_bytes(b) as f64 / u16::MAX as f64) * 2.0 - 1.0
}

/// Runs for the life of the server: a round, then wait to be woken (a refresh,
/// a new follow) or 30 s.
pub async fn run(st: AppState) {
    if !st.feeds.enabled {
        tracing::info!("feeds are off (SLOPSTREAM_FEEDS=false); devices fetch their own lists");
        return;
    }
    loop {
        if let Err(e) = run_once(&st).await {
            tracing::warn!(error = %e, "feed round failed");
        }
        let _ = tokio::time::timeout(Duration::from_secs(30), st.feeds.wake.notified()).await;
    }
}

/// One round: reconcile, then fetch whatever is due. Returns how many
/// channels were fetched.
pub async fn run_once(st: &AppState) -> Result<usize, ApiError> {
    reconcile(&st.pool).await?;
    let mut done = 0;
    if !st.feeds.paused("yt") {
        let due = claim(&st.pool, "yt", YT_BATCH).await?;
        let gate = std::sync::Arc::new(Semaphore::new(YT_PARALLEL));
        let mut set = tokio::task::JoinSet::new();
        for ch in due {
            let st = st.clone();
            let gate = gate.clone();
            set.spawn(async move {
                let _permit = gate.acquire_owned().await;
                if st.feeds.paused("yt") {
                    return release(&st.pool, &ch).await;
                }
                let result = fetch_youtube(&st, &ch.remote_id).await;
                settle(&st, &ch, result).await
            });
        }
        while let Some(r) = set.join_next().await {
            r.map_err(|e| ApiError::Internal(e.into()))??;
            done += 1;
        }
    }
    for svc in services::with_sessions() {
        let Some(site) = svc.site.as_ref() else { continue };
        if st.feeds.paused(svc.key) {
            continue;
        }
        for ch in claim(&st.pool, svc.key, SITE_BATCH).await? {
            if st.feeds.paused(svc.key) {
                release(&st.pool, &ch).await?;
                continue;
            }
            let result = fetch_site(st, svc.key, site, &ch).await;
            settle(st, &ch, result).await?;
            done += 1;
            tokio::time::sleep(st.feeds.site_gap).await;
        }
    }
    Ok(done)
}

/// Channels follow subscriptions: added when someone follows one, marked when
/// nobody does, dropped with their items a week later.
pub async fn reconcile(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO feed_channels (source, remote_id, owner, name)
         SELECT DISTINCT ON (s.source, s.remote_id, o.owner) s.source, s.remote_id, o.owner, s.name
           FROM subscriptions s
           JOIN users u ON u.id = s.user_id AND u.disabled_at IS NULL
           CROSS JOIN LATERAL (SELECT CASE WHEN s.source = 'yt' THEN $1::uuid ELSE s.user_id END AS owner) o
          WHERE s.deleted_at IS NULL
         ON CONFLICT (source, remote_id, owner) DO UPDATE SET unfollowed_at = NULL
          WHERE feed_channels.unfollowed_at IS NOT NULL",
    )
    .bind(SHARED)
    .execute(pool)
    .await?;
    sqlx::query(
        "UPDATE feed_channels c SET unfollowed_at = now()
          WHERE c.unfollowed_at IS NULL AND NOT EXISTS (
            SELECT 1 FROM subscriptions s JOIN users u ON u.id = s.user_id AND u.disabled_at IS NULL
             WHERE s.source = c.source AND s.remote_id = c.remote_id AND s.deleted_at IS NULL
               AND (c.owner = $1 OR s.user_id = c.owner))",
    )
    .bind(SHARED)
    .execute(pool)
    .await?;
    let gone = format!("now() - interval '{UNFOLLOWED_DAYS} days'");
    sqlx::query(&format!(
        "DELETE FROM feed_items i USING feed_channels c
          WHERE c.unfollowed_at < {gone} AND i.owner = c.owner AND i.source = c.source AND i.channel_id = c.remote_id"
    ))
    .execute(pool)
    .await?;
    sqlx::query(&format!("DELETE FROM feed_channels WHERE unfollowed_at < {gone}")).execute(pool).await?;
    sqlx::query("DELETE FROM feed_items WHERE deleted_at < $1")
        .bind(now_ms() - TOMBSTONE_DAYS * 86_400_000)
        .execute(pool)
        .await?;
    Ok(())
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Due {
    pub source: String,
    pub remote_id: String,
    pub owner: Uuid,
    pub last_upload_at: Option<i64>,
    pub failures: i32,
    pub favorite: bool,
}

/// Due channels of one service, favorites first, leased so a crash frees them.
/// Floatplane and Sauce+ only while the owner's session is not known expired.
async fn claim(pool: &PgPool, source: &str, limit: i64) -> Result<Vec<Due>, sqlx::Error> {
    let due: Vec<Due> = sqlx::query_as(
        "SELECT c.source, c.remote_id, c.owner, c.last_upload_at, c.failures,
                EXISTS (SELECT 1 FROM subscriptions s
                         WHERE s.source = c.source AND s.remote_id = c.remote_id AND s.deleted_at IS NULL
                           AND s.favorite AND (c.owner = $3 OR s.user_id = c.owner)) AS favorite
           FROM feed_channels c
          WHERE c.source = $1 AND c.unfollowed_at IS NULL AND c.next_fetch_at <= now()
            AND (c.source = 'yt' OR EXISTS (SELECT 1 FROM service_sessions ss
                  WHERE ss.user_id = c.owner AND ss.source = c.source AND ss.status <> 'expired'))
          ORDER BY favorite DESC, c.next_fetch_at
          LIMIT $2",
    )
    .bind(source)
    .bind(limit)
    .bind(SHARED)
    .fetch_all(pool)
    .await?;
    for d in &due {
        sqlx::query("UPDATE feed_channels SET next_fetch_at = $4 WHERE source = $1 AND remote_id = $2 AND owner = $3")
            .bind(&d.source)
            .bind(&d.remote_id)
            .bind(d.owner)
            .bind(Utc::now() + LEASE)
            .execute(pool)
            .await?;
    }
    Ok(due)
}

/// Claimed but not fetched (a pause began): due again right away.
async fn release(pool: &PgPool, ch: &Due) -> Result<(), ApiError> {
    sqlx::query("UPDATE feed_channels SET next_fetch_at = now() WHERE source = $1 AND remote_id = $2 AND owner = $3")
        .bind(&ch.source)
        .bind(&ch.remote_id)
        .bind(ch.owner)
        .execute(pool)
        .await?;
    Ok(())
}

/// Why a fetch did not produce a list.
#[derive(Debug)]
pub enum FetchError {
    /// The service refuses us for now (429, or YouTube's 403): pause it all.
    Blocked(String),
    /// The session is signed out: mark it, the devices will show it.
    SignedOut,
    /// This channel only; back off.
    Failed(String),
}

async fn get(http: &reqwest::Client, url: &str, cookie: Option<&str>) -> Result<(u16, String), FetchError> {
    let mut req = http.get(url).header("Accept", "application/json, application/atom+xml");
    if let Some(c) = cookie {
        req = req.header("Cookie", c);
    }
    let mut resp = req.send().await.map_err(|e| FetchError::Failed(format!("unreachable: {}", e.without_url())))?;
    let status = resp.status().as_u16();
    let mut out = Vec::new();
    while let Some(chunk) =
        resp.chunk().await.map_err(|e| FetchError::Failed(format!("cut off: {}", e.without_url())))?
    {
        if out.len() + chunk.len() > MAX_BODY {
            return Err(FetchError::Failed("answer too large".into()));
        }
        out.extend_from_slice(&chunk);
    }
    Ok((status, String::from_utf8_lossy(&out).into_owned()))
}

/// The three uploads playlists, else the channel feed.
pub async fn fetch_youtube(st: &AppState, channel_id: &str) -> Result<ChannelFeed, FetchError> {
    let mut answers = Vec::new();
    for upload in Upload::ALL {
        let path =
            youtube::playlist_path(channel_id, upload).ok_or_else(|| FetchError::Failed("not a channel id".into()))?;
        answers.push(yt_feed(st, &path).await?);
    }
    let live = answers.pop().flatten();
    let shorts = answers.pop().flatten();
    let videos = answers.pop().flatten();
    if let Some(feed) = youtube::merge(channel_id, videos, shorts, live) {
        return Ok(feed);
    }
    match yt_feed(st, &youtube::channel_path(channel_id)).await? {
        Some(atom) => Ok(youtube::from_channel_feed(channel_id, atom)),
        None => Err(FetchError::Failed("the channel has no feed (removed or terminated?)".into())),
    }
}

async fn yt_feed(st: &AppState, path: &str) -> Result<Option<youtube::Atom>, FetchError> {
    let (status, body) = get(&st.yt_http, &format!("{}{path}", st.youtube_base), None).await?;
    match status {
        404 => Ok(None),
        429 | 403 => Err(FetchError::Blocked(format!("YouTube refused the feed (HTTP {status})"))),
        200 => youtube::parse(&body).map(Some).map_err(FetchError::Failed),
        s => Err(FetchError::Failed(format!("YouTube answered HTTP {s}"))),
    }
}

/// A creator's posts and, where the site has them, its live stream.
async fn fetch_site(st: &AppState, source: &'static str, site: &Site, ch: &Due) -> Result<ChannelFeed, FetchError> {
    let Some((value, _)) = sessions::current(&st.pool, &st.secrets, ch.owner, source).await.ok().flatten() else {
        return Err(FetchError::SignedOut);
    };
    let base = st.site_base(site);
    let cookie = format!("{}={value}", site.session_cookie);
    let api = |path: String| {
        let url = format!("{base}{path}");
        let cookie = cookie.clone();
        async move {
            let (status, body) = get(&st.http, &url, Some(&cookie)).await?;
            match sites_status(status, &body) {
                None => Ok(body),
                Some(e) => Err(e),
            }
        }
    };
    let id = encode(&ch.remote_id);
    let creator: Creator = json(&api(format!("/api/v3/creator/info?id={id}")).await?)?;
    let posts: Vec<Post> = json(&api(format!("/api/v3/content/creator?id={id}&limit=20&hasVideo=true")).await?)?;
    let titles: HashMap<String, String> = if fp::needs_channel_titles(&posts) {
        json::<Vec<SubChannel>>(&api(format!("/api/v3/creator/channels/list?ids={id}")).await?)?
            .into_iter()
            .map(|c| (c.id, c.title))
            .collect()
    } else {
        HashMap::new()
    };
    let mut items = fp::feed_items(source, &creator, &posts, &titles);
    let mut complete = vec![];
    // Sauce+ has no livestreams (its site config says so; the app's `hasLive`).
    if source == "fp" {
        match &creator.live_stream {
            None => complete.push(slopstream_core::feeds::LIVE),
            Some(stream) => {
                // Only a clear answer either way may end an earlier live item.
                let on = async {
                    let d: Delivery = json(
                        &api(format!("/api/v3/delivery/info?scenario=live&entityId={}", encode(&stream.id))).await?,
                    )?;
                    let Some(url) = fp::live_url(&d) else { return Ok::<bool, FetchError>(false) };
                    // The playlist is on a CDN: never with the session cookie.
                    Ok(get(&st.http, &url, None).await?.0 == 200)
                }
                .await;
                match on {
                    Ok(true) => {
                        items.insert(0, fp::live_item(source, &creator, stream, now_ms()));
                        complete.push(slopstream_core::feeds::LIVE);
                    }
                    Ok(false) => complete.push(slopstream_core::feeds::LIVE),
                    Err(FetchError::SignedOut) => return Err(FetchError::SignedOut),
                    Err(_) => {}
                }
            }
        }
    }
    Ok(ChannelFeed { channel_name: Some(creator.title), items, complete_kinds: complete })
}

/// Floatplane's answer as an error, None for a 2xx.
fn sites_status(status: u16, body: &str) -> Option<FetchError> {
    match status {
        200..=299 => None,
        401 => Some(FetchError::SignedOut),
        403 if body.contains("notLoggedInError") => Some(FetchError::SignedOut),
        429 => Some(FetchError::Blocked("rate limited (HTTP 429)".into())),
        403 => Some(FetchError::Failed("not included in the plan (HTTP 403)".into())),
        s => Some(FetchError::Failed(format!("answered HTTP {s}"))),
    }
}

fn json<T: serde::de::DeserializeOwned>(body: &str) -> Result<T, FetchError> {
    serde_json::from_str(body).map_err(|e| FetchError::Failed(format!("unexpected answer: {e}")))
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Stores a fetch's outcome and schedules the next one.
async fn settle(st: &AppState, ch: &Due, result: Result<ChannelFeed, FetchError>) -> Result<(), ApiError> {
    let now = now_ms();
    match result {
        Ok(feed) => {
            let mut tx = st.pool.begin().await?;
            store(&mut tx, ch.owner, &ch.source, &ch.remote_id, &feed, now).await?;
            let newest =
                feed.items.iter().filter(|i| i.kind != slopstream_core::feeds::LIVE).map(|i| i.published_at).max();
            let last_upload = newest.max(ch.last_upload_at);
            let next = Utc::now() + interval(ch.favorite, last_upload, now, jitter());
            sqlx::query(
                "UPDATE feed_channels SET name = COALESCE($4, name), last_fetch_at = now(), last_ok_at = now(),
                        failures = 0, last_error = NULL, last_upload_at = $5, next_fetch_at = $6
                  WHERE source = $1 AND remote_id = $2 AND owner = $3",
            )
            .bind(&ch.source)
            .bind(&ch.remote_id)
            .bind(ch.owner)
            .bind(feed.channel_name.as_deref().filter(|n| !n.is_empty()))
            .bind(last_upload)
            .bind(next)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
        }
        Err(e) => {
            let source: &'static str = services::by_key(&ch.source).map(|s| s.key).unwrap_or("yt");
            let message = match &e {
                FetchError::Blocked(m) => {
                    st.feeds.pause(source, if source == "yt" { YT_PAUSE } else { SITE_PAUSE });
                    m.clone()
                }
                FetchError::SignedOut => {
                    sessions::server_saw_signed_out(&st.pool, ch.owner, &ch.source).await?;
                    "the session is signed out".to_string()
                }
                FetchError::Failed(m) => m.clone(),
            };
            let failures = ch.failures + 1;
            sqlx::query(
                "UPDATE feed_channels SET last_fetch_at = now(), failures = $4, last_error = $5, next_fetch_at = $6
                  WHERE source = $1 AND remote_id = $2 AND owner = $3",
            )
            .bind(&ch.source)
            .bind(&ch.remote_id)
            .bind(ch.owner)
            .bind(failures)
            .bind(&message)
            .bind(Utc::now() + backoff(failures))
            .execute(&st.pool)
            .await?;
            tracing::info!(source = ch.source, channel = ch.remote_id, error = message, "feed fetch failed");
        }
    }
    Ok(())
}

/// Writes a channel's list: a new rev only for what changed, tombstones for
/// what a complete kind no longer lists, the newest [`KEEP_PER_KIND`] kept.
pub async fn store(
    conn: &mut PgConnection,
    owner: Uuid,
    source: &str,
    channel_id: &str,
    feed: &ChannelFeed,
    now: i64,
) -> Result<(), sqlx::Error> {
    for i in &feed.items {
        upsert(conn, owner, i, now).await?;
    }
    for kind in &feed.complete_kinds {
        let keep: Vec<String> = feed.items.iter().filter(|i| i.kind == *kind).map(|i| i.remote_id.clone()).collect();
        sqlx::query(
            "UPDATE feed_items SET deleted_at = $5, rev = nextval('feed_rev')
              WHERE owner = $1 AND source = $2 AND channel_id = $3 AND kind = $4 AND deleted_at IS NULL
                AND NOT (remote_id = ANY($6))",
        )
        .bind(owner)
        .bind(source)
        .bind(channel_id)
        .bind(kind)
        .bind(now)
        .bind(&keep)
        .execute(&mut *conn)
        .await?;
    }
    let mut kinds: Vec<&str> = feed.items.iter().map(|i| i.kind.as_str()).collect();
    kinds.sort_unstable();
    kinds.dedup();
    for kind in kinds {
        // Devices prune their own caches the same way, so this needs no tombstone.
        sqlx::query(
            "DELETE FROM feed_items WHERE owner = $1 AND source = $2 AND channel_id = $3 AND kind = $4
               AND remote_id NOT IN (SELECT remote_id FROM feed_items
                  WHERE owner = $1 AND source = $2 AND channel_id = $3 AND kind = $4
                  ORDER BY published_at DESC LIMIT $5)",
        )
        .bind(owner)
        .bind(source)
        .bind(channel_id)
        .bind(kind)
        .bind(KEEP_PER_KIND)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// Inserts or updates one item; the rev moves only when something a device
/// shows changed. A known duration is kept; a live item keeps the time it was
/// first seen on air, so it does not re-date every fetch.
async fn upsert(conn: &mut PgConnection, owner: Uuid, i: &FeedItem, now: i64) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO feed_items (owner, source, remote_id, channel_id, channel_name, title, published_at, kind,
                                 thumbnail_url, duration_sec, fetched_at, rev)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, nextval('feed_rev'))
         ON CONFLICT (owner, source, remote_id) DO UPDATE SET
            channel_id = EXCLUDED.channel_id, channel_name = EXCLUDED.channel_name, title = EXCLUDED.title,
            published_at = CASE WHEN EXCLUDED.kind = 'LIVE' AND feed_items.deleted_at IS NULL
                                THEN feed_items.published_at ELSE EXCLUDED.published_at END,
            kind = EXCLUDED.kind, thumbnail_url = EXCLUDED.thumbnail_url,
            duration_sec = COALESCE(EXCLUDED.duration_sec, feed_items.duration_sec),
            fetched_at = EXCLUDED.fetched_at, deleted_at = NULL, rev = nextval('feed_rev')
          WHERE feed_items.deleted_at IS NOT NULL
             OR (feed_items.channel_id, feed_items.channel_name, feed_items.title, feed_items.kind, feed_items.thumbnail_url)
                IS DISTINCT FROM (EXCLUDED.channel_id, EXCLUDED.channel_name, EXCLUDED.title, EXCLUDED.kind, EXCLUDED.thumbnail_url)
             OR (EXCLUDED.kind <> 'LIVE' AND feed_items.published_at <> EXCLUDED.published_at)
             OR (EXCLUDED.duration_sec IS NOT NULL AND feed_items.duration_sec IS DISTINCT FROM EXCLUDED.duration_sec)",
    )
    .bind(owner)
    .bind(&i.source)
    .bind(&i.remote_id)
    .bind(&i.channel_id)
    .bind(&i.channel_name)
    .bind(&i.title)
    .bind(i.published_at)
    .bind(&i.kind)
    .bind(&i.thumbnail_url)
    .bind(i.duration_sec)
    .bind(now)
    .execute(conn)
    .await?;
    Ok(())
}

/// `POST /api/v1/feed`: items above the device's rev for each channel the
/// person follows; channels they do not follow are ignored, never an error
/// (an unfollow may not have synced yet).
pub async fn feed_for(pool: &PgPool, user: Uuid, req: &FeedRequest) -> Result<FeedResponse, ApiError> {
    if req.protocol != PROTOCOL {
        return Err(ApiError::bad(format!("protocol {} is not supported", req.protocol)));
    }
    if req.channels.len() > MAX_FEED_CHANNELS {
        return Err(ApiError::bad(format!("at most {MAX_FEED_CHANNELS} channels per request")));
    }
    let followed: Vec<(String, String)> =
        sqlx::query_as("SELECT source, remote_id FROM subscriptions WHERE user_id = $1 AND deleted_at IS NULL")
            .bind(user)
            .fetch_all(pool)
            .await?;
    let followed: std::collections::HashSet<(String, String)> = followed.into_iter().collect();
    let (last, called): (i64, bool) =
        sqlx::query_as("SELECT last_value, is_called FROM feed_rev").fetch_one(pool).await?;
    let highest = if called { last } else { 0 };
    let (mut sources, mut ids, mut owners, mut revs) = (vec![], vec![], vec![], vec![]);
    for (key, rev) in &req.channels {
        let Some((source, id)) = key.split_once(':') else { continue };
        if !followed.contains(&(source.to_string(), id.to_string())) {
            continue;
        }
        sources.push(source.to_string());
        ids.push(id.to_string());
        owners.push(if source == "yt" { SHARED } else { user });
        // Ahead of this server (restored, or another one): from the start.
        revs.push(if *rev > highest || *rev < 0 { 0 } else { *rev });
    }
    #[derive(sqlx::FromRow)]
    struct Row {
        source: String,
        remote_id: String,
        channel_id: String,
        channel_name: String,
        title: String,
        published_at: i64,
        kind: String,
        thumbnail_url: Option<String>,
        duration_sec: Option<i64>,
        deleted_at: Option<i64>,
        rev: i64,
    }
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT i.source, i.remote_id, i.channel_id, i.channel_name, i.title, i.published_at, i.kind,
                i.thumbnail_url, i.duration_sec, i.deleted_at, i.rev
           FROM feed_items i
           JOIN unnest($1::text[], $2::text[], $3::uuid[], $4::bigint[]) AS q(source, channel_id, owner, rev)
             ON i.owner = q.owner AND i.source = q.source AND i.channel_id = q.channel_id AND i.rev > q.rev
          ORDER BY i.rev LIMIT $5",
    )
    .bind(&sources)
    .bind(&ids)
    .bind(&owners)
    .bind(&revs)
    .bind(FEED_PAGE as i64 + 1)
    .fetch_all(pool)
    .await?;
    let more = rows.len() > FEED_PAGE;
    let items = rows
        .into_iter()
        .take(FEED_PAGE)
        .map(|r| FeedRow {
            rev: r.rev,
            deleted: r.deleted_at.is_some(),
            item: FeedItem {
                source: r.source,
                remote_id: r.remote_id,
                channel_id: r.channel_id,
                channel_name: r.channel_name,
                title: r.title,
                published_at: r.published_at,
                kind: r.kind,
                thumbnail_url: r.thumbnail_url,
                duration_sec: r.duration_sec,
            },
        })
        .collect();
    #[derive(sqlx::FromRow)]
    struct Status {
        source: String,
        remote_id: String,
        name: Option<String>,
        last_ok_at: Option<DateTime<Utc>>,
        last_error: Option<String>,
        rev: Option<i64>,
    }
    let statuses: Vec<Status> = sqlx::query_as(
        "SELECT c.source, c.remote_id, c.name, c.last_ok_at, c.last_error,
                (SELECT max(i.rev) FROM feed_items i
                  WHERE i.owner = c.owner AND i.source = c.source AND i.channel_id = c.remote_id) AS rev
           FROM feed_channels c
           JOIN unnest($1::text[], $2::text[], $3::uuid[]) AS q(source, remote_id, owner)
             ON c.source = q.source AND c.remote_id = q.remote_id AND c.owner = q.owner",
    )
    .bind(&sources)
    .bind(&ids)
    .bind(&owners)
    .fetch_all(pool)
    .await?;
    let channels = statuses
        .into_iter()
        .map(|s| ChannelStatus {
            source: s.source,
            remote_id: s.remote_id,
            name: s.name,
            last_ok_at: s.last_ok_at.map(|t| t.timestamp_millis()),
            last_error: s.last_error,
            rev: s.rev.unwrap_or(0),
        })
        .collect();
    Ok(FeedResponse { protocol: PROTOCOL, more, items, channels })
}

/// Pull-to-refresh: this person's channels not fetched in the last 2 minutes
/// become due now; waits up to 15 s for them.
pub async fn refresh(st: &AppState, user: Uuid) -> Result<FeedRefresh, ApiError> {
    if !st.feeds.enabled || !st.feeds.may_refresh(user) {
        return Ok(FeedRefresh { fetched: 0, pending: 0 });
    }
    reconcile(&st.pool).await?;
    let asked_at = Utc::now();
    let marked: Vec<(String, String, Uuid)> = sqlx::query_as(
        "UPDATE feed_channels c SET next_fetch_at = now()
          WHERE c.unfollowed_at IS NULL
            AND (c.last_fetch_at IS NULL OR c.last_fetch_at < now() - interval '2 minutes')
            AND EXISTS (SELECT 1 FROM subscriptions s
                         WHERE s.user_id = $1 AND s.deleted_at IS NULL AND s.source = c.source
                           AND s.remote_id = c.remote_id AND (c.owner = $2 OR c.owner = $1))
          RETURNING c.source, c.remote_id, c.owner",
    )
    .bind(user)
    .bind(SHARED)
    .fetch_all(&st.pool)
    .await?;
    if marked.is_empty() {
        return Ok(FeedRefresh { fetched: 0, pending: 0 });
    }
    st.feeds.wake.notify_one();
    let (sources, ids, owners): (Vec<_>, Vec<_>, Vec<_>) =
        marked.iter().cloned().fold((vec![], vec![], vec![]), |(mut a, mut b, mut c), (s, i, o)| {
            a.push(s);
            b.push(i);
            c.push(o);
            (a, b, c)
        });
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let pending: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM feed_channels c
               JOIN unnest($1::text[], $2::text[], $3::uuid[]) AS q(source, remote_id, owner)
                 ON c.source = q.source AND c.remote_id = q.remote_id AND c.owner = q.owner
              WHERE c.last_fetch_at IS NULL OR c.last_fetch_at < $4",
        )
        .bind(&sources)
        .bind(&ids)
        .bind(&owners)
        .bind(asked_at)
        .fetch_one(&st.pool)
        .await?;
        if pending == 0 || Instant::now() >= deadline {
            return Ok(FeedRefresh { fetched: marked.len() as i64 - pending, pending });
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

/// For the website: when each of a person's channels was last fetched.
pub async fn status_of(
    pool: &PgPool,
    user: Uuid,
    source: &str,
) -> Result<HashMap<String, (Option<DateTime<Utc>>, Option<String>)>, sqlx::Error> {
    let owner = if source == "yt" { SHARED } else { user };
    let rows: Vec<(String, Option<DateTime<Utc>>, Option<String>)> = sqlx::query_as(
        "SELECT remote_id, last_ok_at, last_error FROM feed_channels WHERE source = $1 AND owner = $2 AND unfollowed_at IS NULL",
    )
    .bind(source)
    .bind(owner)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(id, ok, err)| (id, (ok, err))).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedule() {
        let now = 1_800_000_000_000;
        let day = 86_400_000;
        assert_eq!(interval(false, Some(now - day), now, 0.0), Duration::from_secs(1800));
        assert_eq!(interval(true, Some(now - day), now, 0.0), Duration::from_secs(900));
        assert_eq!(
            interval(true, Some(now - 40 * day), now, 0.0),
            Duration::from_secs(3 * 3600),
            "quiet beats favorite"
        );
        assert_eq!(interval(false, Some(now - 200 * day), now, 0.0), Duration::from_secs(12 * 3600));
        assert_eq!(interval(false, None, now, 1.0), Duration::from_secs(1980));
        assert_eq!(backoff(1), Duration::from_secs(1800));
        assert_eq!(backoff(2), Duration::from_secs(3600));
        assert_eq!(backoff(9), Duration::from_secs(12 * 3600));
        assert!((-1.0..=1.0).contains(&jitter()));
    }

    #[test]
    fn only_explicit_refusals_count() {
        assert!(sites_status(200, "").is_none());
        assert!(matches!(
            sites_status(403, r#"{"errors":[{"name":"notLoggedInError"}]}"#),
            Some(FetchError::SignedOut)
        ));
        assert!(matches!(sites_status(403, "<html>challenge</html>"), Some(FetchError::Failed(_))));
        assert!(matches!(sites_status(429, ""), Some(FetchError::Blocked(_))));
        assert_eq!(encode("a b/c"), "a%20b%2Fc");
    }
}
