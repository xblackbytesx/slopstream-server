//! The `/api/v1` wire format. JSON, camelCase, times in epoch milliseconds,
//! field names matching the app's Room columns. Pinned by
//! `tests/golden/*.json`, which the app's contract test parses.
//!
//! One sync is one `POST /api/v1/sync`: the device sends its unsynced rows,
//! each with the `rev` it last saw for that row; the server merges them and
//! answers with every row of this person changed since the device's cursor.
//! The device applies the answer as is: the server leads.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::merge::Clock;
use crate::services;

/// Bumped only for a breaking change, together with a new API prefix.
pub const PROTOCOL: u32 = 1;
/// Most changes of one kind accepted in one request; a device sends the rest
/// in the next round.
pub const MAX_CHANGES: usize = 500;
/// Most rows returned by one sync; `more` asks the device to sync again.
pub const PAGE: usize = 1000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncRequest {
    pub protocol: u32,
    /// The `cursor` of the last answer the device applied; 0 the first time.
    pub cursor: i64,
    /// The device's clock when it sent this, for [`crate::merge::Clock`].
    pub client_now: i64,
    #[serde(default)]
    pub subscriptions: Vec<Change<Subscription>>,
    #[serde(default)]
    pub watch_later: Vec<Change<WatchLater>>,
    #[serde(default)]
    pub watch_state: Vec<Change<WatchState>>,
    /// Service key to the [`crate::fingerprint`] of the session the device
    /// holds. A session value is sent only when it differs.
    #[serde(default)]
    pub session_fingerprints: BTreeMap<String, String>,
}

/// A row the device changed, with the server `rev` it was based on (0: it
/// never saw this row on the server).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Change<T> {
    pub base_rev: i64,
    #[serde(flatten)]
    pub row: T,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncResponse {
    pub protocol: u32,
    /// Store it and send it next time.
    pub cursor: i64,
    /// More rows are waiting: sync again right away (with no changes).
    pub more: bool,
    /// The server is behind the device's cursor (restored from a backup, or
    /// a different server). Nothing was merged: the device sets its cursor
    /// to 0, marks every row unsynced with `baseRev` 0, and syncs again, so
    /// the server gets back what it lost and stays leading for the rest.
    #[serde(default)]
    pub reset: bool,
    pub server_now: i64,
    pub subscriptions: Vec<Synced<Subscription>>,
    pub watch_later: Vec<Synced<WatchLater>>,
    pub watch_state: Vec<Synced<WatchState>>,
    /// Every session this person keeps on the server, always the full list:
    /// a service missing here has no session (signed out everywhere).
    pub sessions: Vec<SessionInfo>,
}

/// A row as the server holds it, with its `rev`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Synced<T> {
    pub rev: i64,
    #[serde(flatten)]
    pub row: T,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Subscription {
    pub source: String,
    pub remote_id: String,
    pub name: String,
    pub avatar_url: Option<String>,
    pub favorite: bool,
    pub added_at: i64,
    pub updated_at: i64,
    pub deleted_at: Option<i64>,
    pub device_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchLater {
    pub source: String,
    pub remote_id: String,
    pub title: String,
    pub channel_id: String,
    pub channel_name: String,
    pub thumbnail_url: Option<String>,
    pub duration_sec: Option<i64>,
    pub published_at: i64,
    /// The app's `VideoKind` name (VIDEO, SHORT, LIVE, UPCOMING).
    pub kind: String,
    pub added_at: i64,
    pub updated_at: i64,
    pub deleted_at: Option<i64>,
    pub device_id: String,
}

/// Progress on one video, and its history entry (newest `updatedAt` first).
/// The display fields are optional: rows written before the app kept them
/// have none.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchState {
    pub source: String,
    pub remote_id: String,
    pub position_ms: i64,
    pub duration_ms: i64,
    pub watched: bool,
    pub title: Option<String>,
    pub channel_id: Option<String>,
    pub channel_name: Option<String>,
    pub thumbnail_url: Option<String>,
    pub kind: Option<String>,
    pub updated_at: i64,
    /// Removed from history.
    pub deleted_at: Option<i64>,
    pub device_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionStatus {
    /// The service accepted it at the last check.
    Ok,
    /// The service refused it: sign in again on any device, or paste it on
    /// the website.
    Expired,
    /// The server could not ask the service (a challenge page, a network
    /// error); the device that sent it had checked it.
    Unchecked,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub source: String,
    pub status: SessionStatus,
    pub username: Option<String>,
    pub fingerprint: String,
    /// The cookie value; only when the device's fingerprint differs.
    pub value: Option<String>,
    pub updated_at: i64,
}

/// `PUT /api/v1/sessions/{source}`: the device signed in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionPut {
    pub value: String,
    pub username: Option<String>,
}

/// `POST /api/v1/sessions/{source}/expired`: the service refused the session
/// with this fingerprint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpiredReport {
    pub fingerprint: String,
}

/// `POST /api/v1/pair/start`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairStart {
    pub device_name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairStarted {
    /// Secret; only the device knows it. Sent back with every poll.
    pub device_code: String,
    /// What the person types on the website, e.g. `KJ7W-3MPQ`.
    pub user_code: String,
    /// Where to type it.
    pub verification_url: String,
    pub expires_in_sec: i64,
    pub interval_sec: i64,
}

/// `POST /api/v1/pair/poll`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairPoll {
    pub device_code: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PairStatus {
    Pending,
    Approved,
    Denied,
    Expired,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairResult {
    pub status: PairStatus,
    /// Once, on the first poll after approval.
    pub token: Option<String>,
    pub person: Option<Person>,
}

/// `GET /api/v1/me`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Person {
    pub name: String,
    pub email: String,
}

/// `POST /api/v1/feed`: the video lists the server fetched (AGENTS.md, Feeds).
/// The device names every channel it follows (`"<source>:<channelId>"`) with
/// the highest feed rev it holds for it, 0 for a new follow. No cursor lives on
/// the server, so a new follow backfills by itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedRequest {
    pub protocol: u32,
    pub channels: BTreeMap<String, i64>,
}

/// Most channels named in one feed request.
pub const MAX_FEED_CHANNELS: usize = 5000;
/// Most items in one feed answer; `more` says ask again.
pub const FEED_PAGE: usize = 2000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedResponse {
    pub protocol: u32,
    /// More items are waiting. Until it is false, a channel's new rev is the
    /// highest one received for it, not [`ChannelStatus::rev`].
    pub more: bool,
    /// Lowest rev first.
    pub items: Vec<FeedRow>,
    /// One per requested channel the server fetches; a channel missing here
    /// (just followed, a session the server lacks) is the device's to fetch.
    pub channels: Vec<ChannelStatus>,
    /// YouTube channels this device is asked to look up durations for (its
    /// tabs), leased to it alone for a while; report with [`FeedDurations`].
    #[serde(default)]
    pub duration_jobs: Vec<String>,
}

/// `POST /api/v1/feed/durations`: what a duration job found. Every new video of
/// the channel counts as checked afterwards, found or not.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedDurations {
    pub channel_id: String,
    /// Video id to seconds.
    pub durations: BTreeMap<String, i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DurationsStored {
    pub updated: i64,
}

/// `GET /api/v1/changes?since=N`: answered at once when the person's counter
/// is not N, else after something changes or ~25 s (`changed` false). Sync
/// (and pull the feed) on `changed`, then wait again with `generation`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangesAnswer {
    pub generation: u64,
    pub changed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedRow {
    pub rev: i64,
    #[serde(flatten)]
    pub item: crate::feeds::FeedItem,
    /// Gone (an ended livestream): the device drops it.
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelStatus {
    pub source: String,
    pub remote_id: String,
    pub name: Option<String>,
    /// When the server last fetched it successfully; null: never.
    pub last_ok_at: Option<i64>,
    pub last_error: Option<String>,
    /// The highest feed rev of this channel on the server.
    pub rev: i64,
}

/// `POST /api/v1/feed/refresh`: what pull-to-refresh got.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedRefresh {
    /// Channels fetched for this request (0: all were fresh, or too soon again).
    pub fetched: i64,
    /// Still running when the server stopped waiting.
    pub pending: i64,
}

/// Why a request was refused. Each check names the field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invalid(pub String);

impl std::fmt::Display for Invalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

const MAX_ID: usize = 200;
const MAX_TEXT: usize = 500;
const MAX_URL: usize = 2048;

fn text(field: &str, v: &str, max: usize, required: bool) -> Result<(), Invalid> {
    if required && v.trim().is_empty() {
        return Err(Invalid(format!("{field} is empty")));
    }
    if v.chars().count() > max {
        return Err(Invalid(format!("{field} is longer than {max} characters")));
    }
    Ok(())
}

fn opt(field: &str, v: &Option<String>, max: usize) -> Result<(), Invalid> {
    v.as_deref().map_or(Ok(()), |v| text(field, v, max, false))
}

fn time(field: &str, v: i64) -> Result<(), Invalid> {
    if v < 0 { Err(Invalid(format!("{field} is negative"))) } else { Ok(()) }
}

fn key(source: &str, remote_id: &str) -> Result<(), Invalid> {
    if services::by_key(source).is_none() {
        return Err(Invalid(format!("unknown service {source:?}")));
    }
    text("remoteId", remote_id, MAX_ID, true)
}

/// A synced row: its identity, its clock and its checks.
pub trait Row: Clone {
    fn source(&self) -> &str;
    fn remote_id(&self) -> &str;
    fn updated_at(&self) -> i64;
    /// Puts the device's stamps on the server's clock: `updatedAt` (the
    /// conflict clock) is shifted, the other dates only capped at now. They
    /// travel back and forth unchanged, so shifting them would drift.
    fn correct(&mut self, clock: &Clock);
    /// Marks the row as written by `device_id` at `updated_at`.
    fn stamp(&mut self, updated_at: i64, device_id: &str);
    fn validate(&self) -> Result<(), Invalid>;
    /// Equal apart from who wrote it and when: writing it would change nothing.
    fn same_content(&self, other: &Self) -> bool;
}

macro_rules! same_content_except_stamp {
    () => {
        fn same_content(&self, other: &Self) -> bool {
            let strip = |r: &Self| {
                let mut r = r.clone();
                r.updated_at = 0;
                r.device_id = String::new();
                r
            };
            strip(self) == strip(other)
        }
    };
}

impl Row for Subscription {
    fn source(&self) -> &str {
        &self.source
    }
    fn remote_id(&self) -> &str {
        &self.remote_id
    }
    fn updated_at(&self) -> i64 {
        self.updated_at
    }
    fn correct(&mut self, clock: &Clock) {
        self.updated_at = clock.adjust(self.updated_at);
        self.added_at = clock.cap(self.added_at);
        self.deleted_at = self.deleted_at.map(|t| clock.cap(t));
    }
    fn stamp(&mut self, updated_at: i64, device_id: &str) {
        self.updated_at = updated_at;
        self.device_id = device_id.to_string();
    }
    fn validate(&self) -> Result<(), Invalid> {
        key(&self.source, &self.remote_id)?;
        text("name", &self.name, MAX_TEXT, false)?;
        opt("avatarUrl", &self.avatar_url, MAX_URL)?;
        text("deviceId", &self.device_id, MAX_ID, false)?;
        time("addedAt", self.added_at)?;
        time("updatedAt", self.updated_at)
    }
    same_content_except_stamp!();
}

impl Row for WatchLater {
    fn source(&self) -> &str {
        &self.source
    }
    fn remote_id(&self) -> &str {
        &self.remote_id
    }
    fn updated_at(&self) -> i64 {
        self.updated_at
    }
    fn correct(&mut self, clock: &Clock) {
        self.updated_at = clock.adjust(self.updated_at);
        self.added_at = clock.cap(self.added_at);
        self.deleted_at = self.deleted_at.map(|t| clock.cap(t));
    }
    fn stamp(&mut self, updated_at: i64, device_id: &str) {
        self.updated_at = updated_at;
        self.device_id = device_id.to_string();
    }
    fn validate(&self) -> Result<(), Invalid> {
        key(&self.source, &self.remote_id)?;
        text("title", &self.title, MAX_TEXT, false)?;
        text("channelId", &self.channel_id, MAX_ID, false)?;
        text("channelName", &self.channel_name, MAX_TEXT, false)?;
        opt("thumbnailUrl", &self.thumbnail_url, MAX_URL)?;
        text("kind", &self.kind, 32, false)?;
        text("deviceId", &self.device_id, MAX_ID, false)?;
        time("addedAt", self.added_at)?;
        time("updatedAt", self.updated_at)
    }
    same_content_except_stamp!();
}

impl Row for WatchState {
    fn source(&self) -> &str {
        &self.source
    }
    fn remote_id(&self) -> &str {
        &self.remote_id
    }
    fn updated_at(&self) -> i64 {
        self.updated_at
    }
    fn correct(&mut self, clock: &Clock) {
        self.updated_at = clock.adjust(self.updated_at);
        self.deleted_at = self.deleted_at.map(|t| clock.cap(t));
    }
    fn stamp(&mut self, updated_at: i64, device_id: &str) {
        self.updated_at = updated_at;
        self.device_id = device_id.to_string();
    }
    fn validate(&self) -> Result<(), Invalid> {
        key(&self.source, &self.remote_id)?;
        if self.position_ms < 0 || self.duration_ms < 0 {
            return Err(Invalid("positionMs and durationMs must not be negative".into()));
        }
        opt("title", &self.title, MAX_TEXT)?;
        opt("channelId", &self.channel_id, MAX_ID)?;
        opt("channelName", &self.channel_name, MAX_TEXT)?;
        opt("thumbnailUrl", &self.thumbnail_url, MAX_URL)?;
        opt("kind", &self.kind, 32)?;
        text("deviceId", &self.device_id, MAX_ID, false)?;
        time("updatedAt", self.updated_at)
    }
    same_content_except_stamp!();
}

impl SyncRequest {
    /// Every check that does not need the database. A bad row refuses the
    /// whole request: the device must not mark anything synced that was not
    /// stored.
    pub fn validate(&self) -> Result<(), Invalid> {
        if self.protocol != PROTOCOL {
            return Err(Invalid(format!(
                "protocol {} is not supported (this server speaks {PROTOCOL})",
                self.protocol
            )));
        }
        if self.cursor < 0 {
            return Err(Invalid("cursor is negative".into()));
        }
        for (name, n) in [
            ("subscriptions", self.subscriptions.len()),
            ("watchLater", self.watch_later.len()),
            ("watchState", self.watch_state.len()),
        ] {
            if n > MAX_CHANGES {
                return Err(Invalid(format!("{name}: at most {MAX_CHANGES} changes per request")));
            }
        }
        each(&self.subscriptions)?;
        each(&self.watch_later)?;
        each(&self.watch_state)
    }
}

fn each<T: Row>(changes: &[Change<T>]) -> Result<(), Invalid> {
    let mut seen = std::collections::HashSet::new();
    for c in changes {
        c.row.validate().map_err(|e| Invalid(format!("{}:{}: {e}", c.row.source(), c.row.remote_id())))?;
        if c.base_rev < 0 {
            return Err(Invalid("baseRev is negative".into()));
        }
        if !seen.insert((c.row.source(), c.row.remote_id())) {
            return Err(Invalid(format!("{}:{} is listed twice", c.row.source(), c.row.remote_id())));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn sub(id: &str) -> Subscription {
        Subscription {
            source: "yt".into(),
            remote_id: id.into(),
            name: "Channel".into(),
            avatar_url: None,
            favorite: false,
            added_at: 1,
            updated_at: 2,
            deleted_at: None,
            device_id: "d".into(),
        }
    }

    fn request(changes: Vec<Change<Subscription>>) -> SyncRequest {
        SyncRequest {
            protocol: PROTOCOL,
            cursor: 0,
            client_now: 0,
            subscriptions: changes,
            watch_later: vec![],
            watch_state: vec![],
            session_fingerprints: BTreeMap::new(),
        }
    }

    #[test]
    fn requests_are_checked_whole() {
        assert!(request(vec![Change { base_rev: 0, row: sub("UC1") }]).validate().is_ok());
        let twice = request(vec![Change { base_rev: 0, row: sub("UC1") }, Change { base_rev: 3, row: sub("UC1") }]);
        assert!(twice.validate().unwrap_err().0.contains("twice"));
        let mut unknown = sub("UC1");
        unknown.source = "tw".into();
        assert!(request(vec![Change { base_rev: 0, row: unknown }]).validate().unwrap_err().0.contains("unknown"));
        let mut empty = sub(" ");
        empty.name = String::new();
        assert!(request(vec![Change { base_rev: 0, row: empty }]).validate().is_err());
        let mut old = request(vec![]);
        old.protocol = 0;
        assert!(old.validate().is_err());
    }

    #[test]
    fn same_content_ignores_who_and_when() {
        let a = sub("UC1");
        let mut b = a.clone();
        b.updated_at = 99;
        b.device_id = "other".into();
        assert!(a.same_content(&b));
        b.favorite = true;
        assert!(!a.same_content(&b));
    }

    #[test]
    fn missing_lists_default_to_empty() {
        let r: SyncRequest = serde_json::from_str(r#"{"protocol":1,"cursor":5,"clientNow":7}"#).unwrap();
        assert!(r.subscriptions.is_empty() && r.session_fingerprints.is_empty());
    }
}
