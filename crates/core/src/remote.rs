//! Playing on another device (decision 7): a phone sends a video to a TV
//! that has Slopstream open, then works as its remote. The server relays;
//! nothing here plays or stores anything but a guest's progress.
//!
//! Receivers (TVs) long-poll `POST /api/v1/remote/receive` for commands and
//! report what they play to `POST /api/v1/remote/state`. Senders list
//! `GET /api/v1/remote/targets`, send `POST /api/v1/remote/{target}/play`,
//! steer with `.../control` and follow `GET .../status?since=N`.

use serde::{Deserialize, Serialize};

use crate::services;
use crate::wire::Invalid;

/// The services one device can name: every one the app knows.
const MAX_SERVICES: usize = services::ALL.len();
const MAX_ID: usize = 200;
const MAX_TEXT: usize = 500;
const MAX_URL: usize = 2048;

/// What to play: the player's header on the TV, and the sender's history row
/// when the TV plays it as a guest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteItem {
    pub source: String,
    pub video_id: String,
    pub title: String,
    pub channel_name: String,
    pub channel_id: Option<String>,
    pub thumbnail_url: Option<String>,
    /// `VIDEO`, `LIVE` (the app's VideoKind); Shorts are not sent.
    pub kind: Option<String>,
}

impl RemoteItem {
    pub fn validate(&self) -> Result<(), Invalid> {
        if services::by_key(&self.source).is_none() {
            return Err(Invalid(format!("unknown service {:?}", self.source)));
        }
        text("videoId", &self.video_id, MAX_ID, true)?;
        text("title", &self.title, MAX_TEXT, false)?;
        text("channelName", &self.channel_name, MAX_TEXT, false)?;
        opt("channelId", &self.channel_id, MAX_ID)?;
        opt("thumbnailUrl", &self.thumbnail_url, MAX_URL)?;
        opt("kind", &self.kind, 20)
    }
}

/// `POST /api/v1/remote/receive`: a TV waiting for commands. `since` is the
/// last `seq` it saw (none on its first call: nothing old is replayed);
/// `services` are the ones it can play now (YouTube, plus each signed in).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiveRequest {
    pub since: Option<u64>,
    #[serde(default)]
    pub services: Vec<String>,
}

impl ReceiveRequest {
    pub fn validate(&self) -> Result<(), Invalid> {
        if self.services.len() > MAX_SERVICES {
            return Err(Invalid(format!("at most {MAX_SERVICES} services")));
        }
        match self.services.iter().find(|s| services::by_key(s).is_none()) {
            Some(s) => Err(Invalid(format!("unknown service {s:?}"))),
            None => Ok(()),
        }
    }
}

/// Answered at once when commands newer than `since` wait, else after one
/// arrives or ~25 s (empty). Ask again with `seq`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiveAnswer {
    pub seq: u64,
    pub commands: Vec<RemoteCommand>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RemoteAction {
    /// Open the player on `item` at `positionMs` (a new session).
    Play,
    Pause,
    Resume,
    /// To `positionMs`.
    Seek,
    /// Close the player.
    Stop,
}

/// One command for a TV. `guest`: the sender is someone other than the TV's
/// account, so the TV keeps its own library out of it and reports progress
/// to the server, which writes it into the sender's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteCommand {
    pub seq: u64,
    pub session: String,
    pub action: RemoteAction,
    pub item: Option<RemoteItem>,
    pub position_ms: Option<i64>,
    pub guest: bool,
}

/// `POST /api/v1/remote/{target}/play`: start at `startMs` (the sender's
/// own resume position, 0 for the start).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendRequest {
    pub item: RemoteItem,
    pub start_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendAnswer {
    pub session: String,
}

/// `POST /api/v1/remote/{target}/control`: steers the session playing now.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlRequest {
    pub session: String,
    pub action: RemoteAction,
    pub position_ms: Option<i64>,
}

impl ControlRequest {
    pub fn validate(&self) -> Result<(), Invalid> {
        match self.action {
            RemoteAction::Play => Err(Invalid("play starts a session: use /play".into())),
            RemoteAction::Seek if self.position_ms.is_none_or(|p| p < 0) => {
                Err(Invalid("seek needs positionMs".into()))
            }
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlayState {
    /// Sent; the TV has not reported yet.
    Starting,
    Buffering,
    Playing,
    Paused,
    Ended,
    /// The player closed.
    Stopped,
    Error,
}

impl PlayState {
    /// The session is over: no more reports follow.
    pub fn is_final(self) -> bool {
        matches!(self, PlayState::Ended | PlayState::Stopped | PlayState::Error)
    }
}

/// `POST /api/v1/remote/state`: the TV says how a session plays, on every
/// change and every few seconds while it plays. `durationMs` is 0 for a live
/// stream (no progress is kept for those).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StateReport {
    pub session: String,
    pub state: PlayState,
    pub position_ms: i64,
    pub duration_ms: i64,
    /// By the app's rule (90 %, or under 30 s left of a long video).
    pub watched: bool,
    pub error: Option<String>,
}

impl StateReport {
    pub fn validate(&self) -> Result<(), Invalid> {
        text("session", &self.session, MAX_ID, true)?;
        if self.position_ms < 0 || self.duration_ms < 0 {
            return Err(Invalid("positionMs and durationMs cannot be negative".into()));
        }
        opt("error", &self.error, MAX_TEXT)
    }
}

/// What a TV plays: as last reported, `at` the server's time of that report
/// (a sender moves `positionMs` on from it while `playing`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NowPlaying {
    pub session: String,
    pub item: RemoteItem,
    pub state: PlayState,
    pub position_ms: i64,
    pub duration_ms: i64,
    pub error: Option<String>,
    pub at: i64,
    pub guest: bool,
}

/// A TV this person may play on: their own, or one its owner shares.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Target {
    pub id: String,
    pub name: String,
    /// Paired to this person (progress stays in the TV's own library).
    pub own: bool,
    pub services: Vec<String>,
    pub now_playing: Option<NowPlaying>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Targets {
    pub targets: Vec<Target>,
}

/// `GET /api/v1/remote/{target}/status?since=N`: answered at once when the
/// TV's counter is not N, else on its next report or after ~25 s.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteStatus {
    pub generation: u64,
    pub online: bool,
    pub now_playing: Option<NowPlaying>,
}

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

#[cfg(test)]
mod tests {
    use super::*;

    pub fn item() -> RemoteItem {
        RemoteItem {
            source: "yt".into(),
            video_id: "dQw4w9WgXcQ".into(),
            title: "A video".into(),
            channel_name: "A channel".into(),
            channel_id: None,
            thumbnail_url: None,
            kind: Some("VIDEO".into()),
        }
    }

    #[test]
    fn items_and_requests_are_checked() {
        assert!(item().validate().is_ok());
        assert!(RemoteItem { source: "tw".into(), ..item() }.validate().is_err());
        assert!(RemoteItem { video_id: " ".into(), ..item() }.validate().is_err());
        let receive = |s: &[&str]| ReceiveRequest { since: None, services: s.iter().map(|s| s.to_string()).collect() };
        assert!(receive(&["yt", "fp"]).validate().is_ok());
        assert!(receive(&["yt", "tw"]).validate().is_err());
        let control = |action, position_ms| ControlRequest { session: "s".into(), action, position_ms };
        assert!(control(RemoteAction::Pause, None).validate().is_ok());
        assert!(control(RemoteAction::Seek, None).validate().is_err());
        assert!(control(RemoteAction::Seek, Some(-1)).validate().is_err());
        assert!(control(RemoteAction::Play, None).validate().is_err());
    }
}
