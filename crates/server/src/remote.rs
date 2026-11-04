//! Playing on another device (AGENTS.md "Playing on a TV"): TVs with
//! Slopstream open wait here for commands, phones send them and follow what
//! the TV reports. One relay per device token, in memory: the server is a
//! single instance (like `changes.rs`), and after a restart a TV is back
//! within one long-poll.
//!
//! Who may play on a TV: its own person, and everyone once its owner shares
//! it. A guest's progress goes into the guest's library ([`report`]), never
//! the TV account's.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use slopstream_core::remote::{
    ControlRequest, NowPlaying, PlayState, ReceiveAnswer, ReceiveRequest, RemoteAction, RemoteCommand, RemoteItem,
    RemoteStatus, SendRequest, StateReport, Target,
};
use slopstream_core::services;
use tokio::sync::watch;
use uuid::Uuid;

use crate::AppState;
use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::library::{self, now_ms};

/// How long a long-poll (receive or status) is held.
pub const WAIT: Duration = Duration::from_secs(25);
/// A TV counts as open this long after its last receive ended.
const ONLINE: Duration = Duration::from_secs(35);
/// A command not picked up in this time is dropped: a TV coming back later
/// must not start a video someone sent a minute ago.
const COMMAND_TTL: Duration = Duration::from_secs(30);
const MAX_COMMANDS: usize = 32;
/// A guest's progress is written at most this often while it plays (and on
/// every pause and at the end).
const PROGRESS_EVERY: Duration = Duration::from_secs(10);
/// Sessions are kept this long for late reports, then forgotten.
const SESSION_TTL: Duration = Duration::from_secs(6 * 3600);

#[derive(Default)]
pub struct Remote {
    tvs: Mutex<HashMap<Uuid, Tv>>,
}

struct Session {
    sender: Uuid,
    item: RemoteItem,
    guest: bool,
    touched: Instant,
    written: Option<Instant>,
}

struct Tv {
    owner: Uuid,
    name: String,
    shared: bool,
    services: Vec<String>,
    waiting: usize,
    last_seen: Instant,
    /// Said it closed; open again with its next receive.
    closed: bool,
    seq: u64,
    commands: VecDeque<(Instant, RemoteCommand)>,
    sessions: HashMap<String, Session>,
    now: Option<NowPlaying>,
    /// Bumped by every new command (wakes `receive`).
    command_tx: watch::Sender<u64>,
    /// Bumped by every change a sender should see (wakes `status`).
    status_tx: watch::Sender<u64>,
}

impl Tv {
    fn new(owner: Uuid, name: String, shared: bool) -> Tv {
        Tv {
            owner,
            name,
            shared,
            services: vec![],
            waiting: 0,
            last_seen: Instant::now(),
            closed: false,
            seq: 0,
            commands: VecDeque::new(),
            sessions: HashMap::new(),
            now: None,
            command_tx: watch::channel(0).0,
            status_tx: watch::channel(0).0,
        }
    }

    fn online(&self) -> bool {
        !self.closed && (self.waiting > 0 || self.last_seen.elapsed() < ONLINE)
    }

    fn allows(&self, user: Uuid) -> bool {
        self.owner == user || self.shared
    }

    fn push(&mut self, session: &str, action: RemoteAction, item: Option<RemoteItem>, position_ms: Option<i64>) {
        let guest = self.sessions.get(session).is_some_and(|s| s.guest);
        self.seq += 1;
        let cmd = RemoteCommand { seq: self.seq, session: session.to_string(), action, item, position_ms, guest };
        self.commands.push_back((Instant::now(), cmd));
        self.prune();
        self.command_tx.send_replace(self.seq);
    }

    fn prune(&mut self) {
        while self.commands.len() > MAX_COMMANDS || self.commands.front().is_some_and(|c| c.0.elapsed() > COMMAND_TTL) {
            self.commands.pop_front();
        }
        self.sessions.retain(|_, s| s.touched.elapsed() < SESSION_TTL);
    }

    fn pending(&self, since: u64) -> Vec<RemoteCommand> {
        self.commands.iter().filter(|c| c.1.seq > since && c.0.elapsed() <= COMMAND_TTL).map(|c| c.1.clone()).collect()
    }

    fn changed(&self) {
        self.status_tx.send_modify(|g| *g = g.wrapping_add(1));
    }

    fn status(&self) -> RemoteStatus {
        RemoteStatus { generation: *self.status_tx.borrow(), online: self.online(), now_playing: self.now.clone() }
    }
}

/// Counts a waiting receive while its future lives, so a TV is open for as
/// long as it waits, and a dropped connection ends that.
struct Waiting<'a> {
    remote: &'a Remote,
    tv: Uuid,
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        if let Some(tv) = self.remote.tvs.lock().unwrap().get_mut(&self.tv) {
            tv.waiting = tv.waiting.saturating_sub(1);
            tv.last_seen = Instant::now();
        }
    }
}

impl Remote {
    fn with<R>(&self, tv: Uuid, f: impl FnOnce(&mut Tv) -> R) -> Option<R> {
        self.tvs.lock().unwrap().get_mut(&tv).map(f)
    }

    /// A TV the caller may play on and that is open now.
    fn reachable<R>(
        &self,
        tv: Uuid,
        user: &AuthUser,
        f: impl FnOnce(&mut Tv) -> Result<R, ApiError>,
    ) -> Result<R, ApiError> {
        let mut tvs = self.tvs.lock().unwrap();
        match tvs.get_mut(&tv) {
            Some(t) if t.allows(user.id) && tv != user.token_id => {
                if !t.online() {
                    return Err(ApiError::Busy(format!("{} is not open in Slopstream right now", t.name)));
                }
                f(t)
            }
            _ => Err(ApiError::NotFound),
        }
    }

    pub fn is_online(&self, tv: Uuid) -> bool {
        self.with(tv, |t| t.online()).unwrap_or(false)
    }

    /// The owner shared or unshared the device on the website.
    pub fn set_shared(&self, tv: Uuid, shared: bool) {
        self.with(tv, |t| {
            t.shared = shared;
            t.changed();
        });
    }

    /// The token was revoked: it is no TV any more.
    pub fn forget(&self, tv: Uuid) {
        if let Some(t) = self.tvs.lock().unwrap().remove(&tv) {
            t.changed();
        }
    }

    /// The TV's long-poll. Registers it (name, sharing, services) and answers
    /// with the commands after `since`, waiting up to `wait` for one.
    pub async fn receive(
        &self,
        st: &AppState,
        user: &AuthUser,
        req: ReceiveRequest,
        wait: Duration,
    ) -> Result<ReceiveAnswer, ApiError> {
        req.validate().map_err(|e| ApiError::bad(e.0))?;
        let (name, shared): (String, bool) = sqlx::query_as("SELECT label, shared FROM api_tokens WHERE id = $1")
            .bind(user.token_id)
            .fetch_one(&st.pool)
            .await?;
        let mut rx = {
            let mut tvs = self.tvs.lock().unwrap();
            let tv = tvs.entry(user.token_id).or_insert_with(|| Tv::new(user.id, name.clone(), shared));
            let came_online = !tv.online();
            tv.name = name;
            tv.shared = shared;
            tv.services = req.services;
            tv.closed = false;
            tv.waiting += 1;
            tv.prune();
            if came_online {
                tv.changed();
            }
            // Subscribed before looking: a command sent in between still wakes this wait.
            tv.command_tx.subscribe()
        };
        let _waiting = Waiting { remote: self, tv: user.token_id };
        let answer = |seq_seen: Option<u64>| {
            self.with(user.token_id, |t| match seq_seen {
                // First call, or this server restarted under the TV: nothing old is replayed.
                None => ReceiveAnswer { seq: t.seq, commands: vec![] },
                Some(s) if s > t.seq => ReceiveAnswer { seq: t.seq, commands: vec![] },
                Some(s) => ReceiveAnswer { seq: t.seq, commands: t.pending(s) },
            })
            .unwrap_or(ReceiveAnswer { seq: 0, commands: vec![] })
        };
        let Some(since) = req.since else { return Ok(answer(None)) };
        let first = answer(Some(since));
        if !first.commands.is_empty() || since > first.seq {
            return Ok(first);
        }
        let _ = tokio::time::timeout(wait, rx.changed()).await;
        Ok(answer(Some(since)))
    }

    /// The TV closed (went to the background): gone from every list at once.
    pub fn offline(&self, user: &AuthUser) {
        self.with(user.token_id, |t| {
            t.closed = true;
            t.changed();
        });
    }

    /// The TVs this person may play on, open now, by name.
    pub fn targets(&self, user: &AuthUser) -> Vec<Target> {
        let tvs = self.tvs.lock().unwrap();
        let mut out: Vec<Target> = tvs
            .iter()
            .filter(|(id, t)| **id != user.token_id && t.allows(user.id) && t.online())
            .map(|(id, t)| Target {
                id: id.to_string(),
                name: t.name.clone(),
                own: t.owner == user.id,
                services: t.services.clone(),
                now_playing: t.now.clone(),
            })
            .collect();
        out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.id.cmp(&b.id)));
        out
    }

    /// Starts a new session on the TV.
    pub fn send(&self, tv: Uuid, user: &AuthUser, req: SendRequest) -> Result<String, ApiError> {
        req.item.validate().map_err(|e| ApiError::bad(e.0))?;
        self.reachable(tv, user, |t| {
            if !t.services.contains(&req.item.source) {
                let label = services::by_key(&req.item.source).map_or("that service", |s| s.label);
                return Err(ApiError::Forbidden(format!("{} is not signed in to {label}", t.name)));
            }
            let session = Uuid::new_v4().to_string();
            let guest = t.owner != user.id;
            let start = req.start_ms.max(0);
            t.sessions.insert(
                session.clone(),
                Session { sender: user.id, item: req.item.clone(), guest, touched: Instant::now(), written: None },
            );
            t.now = Some(NowPlaying {
                session: session.clone(),
                item: req.item.clone(),
                state: PlayState::Starting,
                position_ms: start,
                duration_ms: 0,
                error: None,
                at: now_ms(),
                guest,
            });
            t.push(&session, RemoteAction::Play, Some(req.item), Some(start));
            t.changed();
            Ok(session)
        })
    }

    /// Pause, resume, seek or stop what plays now. Anyone who may play on the
    /// TV may steer it, as with the remote on the couch.
    pub fn control(&self, tv: Uuid, user: &AuthUser, req: ControlRequest) -> Result<(), ApiError> {
        req.validate().map_err(|e| ApiError::bad(e.0))?;
        self.reachable(tv, user, |t| {
            match &t.now {
                Some(n) if n.session == req.session && !n.state.is_final() => {}
                _ => return Err(ApiError::Busy("that is no longer playing on this TV".into())),
            }
            t.push(&req.session, req.action, None, req.position_ms);
            Ok(())
        })
    }

    pub async fn status(
        &self,
        tv: Uuid,
        user: &AuthUser,
        since: Option<u64>,
        wait: Duration,
    ) -> Result<RemoteStatus, ApiError> {
        let mut rx = {
            let tvs = self.tvs.lock().unwrap();
            match tvs.get(&tv) {
                Some(t) if t.allows(user.id) && tv != user.token_id => {
                    if since != Some(*t.status_tx.borrow()) {
                        return Ok(t.status());
                    }
                    t.status_tx.subscribe()
                }
                _ => return Err(ApiError::NotFound),
            }
        };
        let _ = tokio::time::timeout(wait, rx.changed()).await;
        // Unshared or revoked while waiting: say so as not found.
        self.with(tv, |t| t.allows(user.id).then(|| t.status())).flatten().ok_or(ApiError::NotFound)
    }

    /// The TV says how a session plays. A guest's progress goes into the
    /// sender's library (and wakes the sender's devices).
    pub async fn report(&self, st: &AppState, user: &AuthUser, rep: StateReport) -> Result<(), ApiError> {
        rep.validate().map_err(|e| ApiError::bad(e.0))?;
        let progress = self
            .with(user.token_id, |t| {
                let s = t.sessions.get_mut(&rep.session)?;
                s.touched = Instant::now();
                if let Some(n) = t.now.as_mut().filter(|n| n.session == rep.session) {
                    n.state = rep.state;
                    n.position_ms = rep.position_ms;
                    n.duration_ms = rep.duration_ms;
                    n.error = rep.error.clone();
                    n.at = now_ms();
                    t.changed();
                }
                let s = t.sessions.get_mut(&rep.session)?;
                let settled = matches!(rep.state, PlayState::Paused | PlayState::Ended | PlayState::Stopped);
                let due = s.written.is_none_or(|w| w.elapsed() >= PROGRESS_EVERY);
                let useful = rep.duration_ms > 0 && rep.state != PlayState::Starting && rep.state != PlayState::Error;
                (s.guest && useful && (settled || due)).then(|| {
                    s.written = Some(Instant::now());
                    (s.sender, s.item.clone())
                })
            })
            .flatten();
        if let Some((sender, item)) = progress {
            let ended = rep.state == PlayState::Ended;
            let position = if ended { rep.duration_ms } else { rep.position_ms };
            let device = format!("tv:{}", user.token_id);
            if library::record_progress(
                &st.pool,
                sender,
                &item,
                position,
                rep.duration_ms,
                rep.watched || ended,
                &device,
            )
            .await?
            {
                st.changes.bump(sender);
            }
        }
        Ok(())
    }
}
