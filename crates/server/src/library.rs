//! Each person's library: subscriptions, Watch later and watch state
//! (history). Devices change it through [`sync`]; the website through
//! [`edit`] and [`follow`]. Both take the same per-person lock and stamp
//! every accepted write with the next `rev`.

use std::collections::HashSet;

use slopstream_core::merge::{Clock, Decision, decide};
use slopstream_core::wire::{
    Change, PAGE, PROTOCOL, Row, Subscription, SyncRequest, SyncResponse, Synced, WatchLater, WatchState,
};
use sqlx::postgres::{PgArguments, PgRow};
use sqlx::query::Query;
use sqlx::{PgConnection, PgPool, Postgres, Row as _};
use uuid::Uuid;

use crate::crypto::Secrets;
use crate::error::ApiError;
use crate::sessions;

/// The device id the website writes with.
pub const WEB_DEVICE: &str = "web";

/// A synced table: its columns (besides `user_id` and `rev`) in bind order.
pub trait Stored: Row + Send + Unpin + 'static {
    const TABLE: &'static str;
    const COLUMNS: &'static [&'static str];
    fn read(row: &PgRow) -> Result<Self, sqlx::Error>;
    fn bind<'q>(&'q self, q: Query<'q, Postgres, PgArguments>) -> Query<'q, Postgres, PgArguments>;
}

impl Stored for Subscription {
    const TABLE: &'static str = "subscriptions";
    const COLUMNS: &'static [&'static str] =
        &["source", "remote_id", "name", "avatar_url", "favorite", "added_at", "updated_at", "deleted_at", "device_id"];
    fn read(r: &PgRow) -> Result<Self, sqlx::Error> {
        Ok(Subscription {
            source: r.try_get("source")?,
            remote_id: r.try_get("remote_id")?,
            name: r.try_get("name")?,
            avatar_url: r.try_get("avatar_url")?,
            favorite: r.try_get("favorite")?,
            added_at: r.try_get("added_at")?,
            updated_at: r.try_get("updated_at")?,
            deleted_at: r.try_get("deleted_at")?,
            device_id: r.try_get("device_id")?,
        })
    }
    fn bind<'q>(&'q self, q: Query<'q, Postgres, PgArguments>) -> Query<'q, Postgres, PgArguments> {
        q.bind(&self.source)
            .bind(&self.remote_id)
            .bind(&self.name)
            .bind(&self.avatar_url)
            .bind(self.favorite)
            .bind(self.added_at)
            .bind(self.updated_at)
            .bind(self.deleted_at)
            .bind(&self.device_id)
    }
}

impl Stored for WatchLater {
    const TABLE: &'static str = "watch_later";
    const COLUMNS: &'static [&'static str] = &[
        "source",
        "remote_id",
        "title",
        "channel_id",
        "channel_name",
        "thumbnail_url",
        "duration_sec",
        "published_at",
        "kind",
        "added_at",
        "updated_at",
        "deleted_at",
        "device_id",
    ];
    fn read(r: &PgRow) -> Result<Self, sqlx::Error> {
        Ok(WatchLater {
            source: r.try_get("source")?,
            remote_id: r.try_get("remote_id")?,
            title: r.try_get("title")?,
            channel_id: r.try_get("channel_id")?,
            channel_name: r.try_get("channel_name")?,
            thumbnail_url: r.try_get("thumbnail_url")?,
            duration_sec: r.try_get("duration_sec")?,
            published_at: r.try_get("published_at")?,
            kind: r.try_get("kind")?,
            added_at: r.try_get("added_at")?,
            updated_at: r.try_get("updated_at")?,
            deleted_at: r.try_get("deleted_at")?,
            device_id: r.try_get("device_id")?,
        })
    }
    fn bind<'q>(&'q self, q: Query<'q, Postgres, PgArguments>) -> Query<'q, Postgres, PgArguments> {
        q.bind(&self.source)
            .bind(&self.remote_id)
            .bind(&self.title)
            .bind(&self.channel_id)
            .bind(&self.channel_name)
            .bind(&self.thumbnail_url)
            .bind(self.duration_sec)
            .bind(self.published_at)
            .bind(&self.kind)
            .bind(self.added_at)
            .bind(self.updated_at)
            .bind(self.deleted_at)
            .bind(&self.device_id)
    }
}

impl Stored for WatchState {
    const TABLE: &'static str = "watch_state";
    const COLUMNS: &'static [&'static str] = &[
        "source",
        "remote_id",
        "position_ms",
        "duration_ms",
        "watched",
        "title",
        "channel_id",
        "channel_name",
        "thumbnail_url",
        "kind",
        "updated_at",
        "deleted_at",
        "device_id",
    ];
    fn read(r: &PgRow) -> Result<Self, sqlx::Error> {
        Ok(WatchState {
            source: r.try_get("source")?,
            remote_id: r.try_get("remote_id")?,
            position_ms: r.try_get("position_ms")?,
            duration_ms: r.try_get("duration_ms")?,
            watched: r.try_get("watched")?,
            title: r.try_get("title")?,
            channel_id: r.try_get("channel_id")?,
            channel_name: r.try_get("channel_name")?,
            thumbnail_url: r.try_get("thumbnail_url")?,
            kind: r.try_get("kind")?,
            updated_at: r.try_get("updated_at")?,
            deleted_at: r.try_get("deleted_at")?,
            device_id: r.try_get("device_id")?,
        })
    }
    fn bind<'q>(&'q self, q: Query<'q, Postgres, PgArguments>) -> Query<'q, Postgres, PgArguments> {
        q.bind(&self.source)
            .bind(&self.remote_id)
            .bind(self.position_ms)
            .bind(self.duration_ms)
            .bind(self.watched)
            .bind(&self.title)
            .bind(&self.channel_id)
            .bind(&self.channel_name)
            .bind(&self.thumbnail_url)
            .bind(&self.kind)
            .bind(self.updated_at)
            .bind(self.deleted_at)
            .bind(&self.device_id)
    }
}

fn columns<T: Stored>() -> String {
    T::COLUMNS.join(", ")
}

/// Serializes every write (and the sync read) for one person until the
/// transaction ends. Revs are then handed out in commit order per person, so
/// a cursor can never step over a row that commits later.
pub async fn lock(conn: &mut PgConnection, user: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 7))").bind(user).execute(conn).await?;
    Ok(())
}

pub async fn get<T: Stored>(
    conn: &mut PgConnection,
    user: Uuid,
    source: &str,
    remote_id: &str,
) -> Result<Option<Synced<T>>, sqlx::Error> {
    let sql = format!(
        "SELECT {}, rev FROM {} WHERE user_id = $1 AND source = $2 AND remote_id = $3",
        columns::<T>(),
        T::TABLE
    );
    let row = sqlx::query(&sql).bind(user).bind(source).bind(remote_id).fetch_optional(conn).await?;
    row.map(|r| Ok(Synced { rev: r.try_get("rev")?, row: T::read(&r)? })).transpose()
}

/// Inserts or replaces the row with the next rev. Callers hold [`lock`].
async fn write<T: Stored>(conn: &mut PgConnection, user: Uuid, row: &T) -> Result<i64, sqlx::Error> {
    let cols = T::COLUMNS;
    let params: Vec<String> = (2..cols.len() + 2).map(|i| format!("${i}")).collect();
    let updates: Vec<String> = cols[2..].iter().map(|c| format!("{c} = EXCLUDED.{c}")).collect();
    let sql = format!(
        "INSERT INTO {t} (user_id, {c}, rev) VALUES ($1, {p}, nextval('sync_rev'))
         ON CONFLICT (user_id, source, remote_id) DO UPDATE SET {u}, rev = EXCLUDED.rev RETURNING rev",
        t = T::TABLE,
        c = cols.join(", "),
        p = params.join(", "),
        u = updates.join(", "),
    );
    let rev: i64 = row.bind(sqlx::query(&sql).bind(user)).fetch_one(conn).await?.try_get("rev")?;
    Ok(rev)
}

async fn since<T: Stored>(
    conn: &mut PgConnection,
    user: Uuid,
    cursor: i64,
    limit: usize,
) -> Result<Vec<Synced<T>>, sqlx::Error> {
    let sql = format!(
        "SELECT {}, rev FROM {} WHERE user_id = $1 AND rev > $2 ORDER BY rev LIMIT $3",
        columns::<T>(),
        T::TABLE
    );
    let rows = sqlx::query(&sql).bind(user).bind(cursor).bind(limit as i64).fetch_all(conn).await?;
    rows.iter().map(|r| Ok(Synced { rev: r.try_get("rev")?, row: T::read(r)? })).collect()
}

/// Merges a device's changes. Returns the keys whose server row the device
/// must get back even if it is older than its cursor (it lost, or wrote
/// nothing new).
async fn merge<T: Stored>(
    conn: &mut PgConnection,
    user: Uuid,
    clock: &Clock,
    changes: Vec<Change<T>>,
) -> Result<Vec<(String, String)>, sqlx::Error> {
    let mut echo = Vec::new();
    for Change { base_rev, mut row } in changes {
        row.correct(clock);
        let server = get::<T>(conn, user, row.source(), row.remote_id()).await?;
        match decide(server.as_ref().map(|s| (s.rev, s.row.updated_at())), base_rev, row.updated_at()) {
            Decision::Insert | Decision::Apply => match server {
                // Nothing new: no rev bump, no echo churn on other devices.
                Some(s) if s.row.same_content(&row) => {
                    echo.push((row.source().to_string(), row.remote_id().to_string()))
                }
                _ => {
                    write(conn, user, &row).await?;
                }
            },
            Decision::Keep => echo.push((row.source().to_string(), row.remote_id().to_string())),
        }
    }
    Ok(echo)
}

async fn echoed<T: Stored>(
    conn: &mut PgConnection,
    user: Uuid,
    keys: &[(String, String)],
    have: &mut Vec<Synced<T>>,
) -> Result<(), sqlx::Error> {
    let present: HashSet<(String, String)> =
        have.iter().map(|s| (s.row.source().to_string(), s.row.remote_id().to_string())).collect();
    for (source, id) in keys {
        if !present.contains(&(source.clone(), id.clone()))
            && let Some(s) = get::<T>(conn, user, source, id).await?
        {
            have.push(s);
        }
    }
    Ok(())
}

/// One sync round for `user`, all in one transaction.
pub async fn sync(
    pool: &PgPool,
    secrets: &Secrets,
    user: Uuid,
    req: SyncRequest,
    server_now: i64,
) -> Result<SyncResponse, ApiError> {
    req.validate().map_err(|e| ApiError::bad(e.0))?;
    let clock = Clock::new(req.client_now, server_now);
    let mut tx = pool.begin().await?;
    lock(&mut tx, user).await?;

    // A cursor beyond every rev ever handed out: this server forgot rows the
    // device saw. Its base revs mean nothing here, so merge nothing.
    let (last, called): (i64, bool) =
        sqlx::query_as("SELECT last_value, is_called FROM sync_rev").fetch_one(&mut *tx).await?;
    if req.cursor > if called { last } else { 0 } {
        tracing::warn!(%user, cursor = req.cursor, last, "a device is ahead of this server; asking it to resend");
        return Ok(SyncResponse {
            protocol: PROTOCOL,
            cursor: 0,
            more: false,
            reset: true,
            server_now,
            subscriptions: vec![],
            watch_later: vec![],
            watch_state: vec![],
            sessions: vec![],
        });
    }

    let echo_subs = merge(&mut tx, user, &clock, req.subscriptions).await?;
    let echo_later = merge(&mut tx, user, &clock, req.watch_later).await?;
    let echo_state = merge(&mut tx, user, &clock, req.watch_state).await?;

    // Up to PAGE + 1 from each table: enough to know the PAGE lowest revs
    // overall and whether anything is left after them.
    let mut subscriptions = since::<Subscription>(&mut tx, user, req.cursor, PAGE + 1).await?;
    let mut watch_later = since::<WatchLater>(&mut tx, user, req.cursor, PAGE + 1).await?;
    let mut watch_state = since::<WatchState>(&mut tx, user, req.cursor, PAGE + 1).await?;
    let mut revs: Vec<i64> = subscriptions
        .iter()
        .map(|s| s.rev)
        .chain(watch_later.iter().map(|s| s.rev))
        .chain(watch_state.iter().map(|s| s.rev))
        .collect();
    revs.sort_unstable();
    let more = revs.len() > PAGE;
    let cursor = if more { revs[PAGE - 1] } else { revs.last().copied().unwrap_or(req.cursor) };
    if more {
        subscriptions.retain(|s| s.rev <= cursor);
        watch_later.retain(|s| s.rev <= cursor);
        watch_state.retain(|s| s.rev <= cursor);
    }
    echoed(&mut tx, user, &echo_subs, &mut subscriptions).await?;
    echoed(&mut tx, user, &echo_later, &mut watch_later).await?;
    echoed(&mut tx, user, &echo_state, &mut watch_state).await?;

    let sessions = sessions::for_sync(&mut tx, secrets, user, &req.session_fingerprints).await?;
    tx.commit().await?;
    Ok(SyncResponse {
        protocol: PROTOCOL,
        cursor: cursor.max(req.cursor),
        more,
        reset: false,
        server_now,
        subscriptions,
        watch_later,
        watch_state,
        sessions,
    })
}

/// A change made on the website: load the row, let `f` change it, stamp it
/// as the website's and write it with a new rev. NotFound when there is no
/// such row.
pub async fn edit<T: Stored>(
    pool: &PgPool,
    user: Uuid,
    source: &str,
    remote_id: &str,
    f: impl FnOnce(&mut T, i64),
) -> Result<T, ApiError> {
    let now = now_ms();
    let mut tx = pool.begin().await?;
    lock(&mut tx, user).await?;
    let Some(Synced { mut row, .. }) = get::<T>(&mut tx, user, source, remote_id).await? else {
        return Err(ApiError::NotFound);
    };
    f(&mut row, now);
    row.stamp(now, WEB_DEVICE);
    write(&mut tx, user, &row).await?;
    tx.commit().await?;
    Ok(row)
}

/// Follows a channel from the website, or un-tombstones it. Returns false
/// when it was followed already.
pub async fn follow(pool: &PgPool, user: Uuid, mut sub: Subscription) -> Result<bool, ApiError> {
    let now = now_ms();
    let mut tx = pool.begin().await?;
    lock(&mut tx, user).await?;
    let old = get::<Subscription>(&mut tx, user, &sub.source, &sub.remote_id).await?;
    if let Some(old) = &old {
        if old.row.deleted_at.is_none() {
            return Ok(false);
        }
        // Back after an unfollow: keep the favorite and the first follow date.
        sub.favorite = old.row.favorite;
        sub.added_at = old.row.added_at;
    } else {
        sub.added_at = now;
    }
    sub.deleted_at = None;
    sub.stamp(now, WEB_DEVICE);
    write(&mut tx, user, &sub).await?;
    tx.commit().await?;
    Ok(true)
}

/// Live (not tombstoned) rows of one service for the website, in `order`
/// (an ORDER BY clause from this crate, never from a request).
pub async fn list<T: Stored>(pool: &PgPool, user: Uuid, source: &str, order: &str) -> Result<Vec<T>, sqlx::Error> {
    let sql = format!(
        "SELECT {} FROM {} WHERE user_id = $1 AND source = $2 AND deleted_at IS NULL ORDER BY {order} LIMIT 500",
        columns::<T>(),
        T::TABLE
    );
    let rows = sqlx::query(&sql).bind(user).bind(source).fetch_all(pool).await?;
    rows.iter().map(T::read).collect()
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
