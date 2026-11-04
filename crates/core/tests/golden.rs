//! The wire contract, pinned as JSON. The app's contract test parses copies of
//! these files, so a change here is a change for every installed app.
//! Regenerate only on purpose: `UPDATE_GOLDEN=1 cargo test -p slopstream-core`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Serialize;
use serde::de::DeserializeOwned;
use slopstream_core::fingerprint;
use slopstream_core::wire::*;

fn check<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(name: &str, value: &T) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden").join(format!("{name}.json"));
    let json = serde_json::to_string_pretty(value).unwrap() + "\n";
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, &json).unwrap();
    }
    let stored =
        std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("{} missing (UPDATE_GOLDEN=1)", path.display()));
    assert_eq!(stored, json, "{name}.json changed; regenerate on purpose with UPDATE_GOLDEN=1");
    let back: T = serde_json::from_str(&stored).unwrap();
    assert_eq!(&back, value, "{name}.json round trip");
}

fn subscription() -> Subscription {
    Subscription {
        source: "yt".into(),
        remote_id: "UCXuqSBlHAE6Xw-yeJA0Tunw".into(),
        name: "Linus Tech Tips".into(),
        avatar_url: Some("https://yt3.googleusercontent.com/example=s176".into()),
        favorite: true,
        added_at: 1_761_200_000_000,
        updated_at: 1_761_210_000_000,
        deleted_at: None,
        device_id: "3f6c1d2e-shield".into(),
    }
}

fn watch_later() -> WatchLater {
    WatchLater {
        source: "fp".into(),
        remote_id: "Xk2PqWl9aB".into(),
        title: "A Floatplane exclusive".into(),
        channel_id: "59f94c0bdd241b70349eb72b".into(),
        channel_name: "Linus Tech Tips".into(),
        thumbnail_url: Some("https://pbs.floatplane.com/example.jpg".into()),
        duration_sec: Some(1234),
        published_at: 1_761_100_000_000,
        kind: "VIDEO".into(),
        added_at: 1_761_200_000_000,
        updated_at: 1_761_200_000_000,
        deleted_at: Some(1_761_220_000_000),
        device_id: "9a1b-phone".into(),
    }
}

fn watch_state() -> WatchState {
    WatchState {
        source: "yt".into(),
        remote_id: "dQw4w9WgXcQ".into(),
        position_ms: 61_000,
        duration_ms: 212_000,
        watched: false,
        title: Some("A video".into()),
        channel_id: Some("UCuAXFkgsw1L7xaCfnd5JJOw".into()),
        channel_name: Some("A channel".into()),
        thumbnail_url: Some("https://i.ytimg.com/vi/dQw4w9WgXcQ/hq720.jpg".into()),
        kind: Some("VIDEO".into()),
        updated_at: 1_761_230_000_000,
        deleted_at: None,
        device_id: "3f6c1d2e-shield".into(),
    }
}

#[test]
fn sync_request() {
    check(
        "sync_request",
        &SyncRequest {
            protocol: PROTOCOL,
            cursor: 41,
            client_now: 1_761_230_005_000,
            subscriptions: vec![Change { base_rev: 12, row: subscription() }],
            watch_later: vec![Change { base_rev: 0, row: watch_later() }],
            watch_state: vec![Change { base_rev: 40, row: watch_state() }],
            session_fingerprints: BTreeMap::from([("fp".to_string(), fingerprint("old-session-value"))]),
        },
    );
}

#[test]
fn sync_response() {
    check(
        "sync_response",
        &SyncResponse {
            protocol: PROTOCOL,
            cursor: 45,
            more: false,
            reset: false,
            server_now: 1_761_230_005_120,
            subscriptions: vec![Synced { rev: 43, row: subscription() }],
            watch_later: vec![Synced { rev: 44, row: watch_later() }],
            watch_state: vec![Synced { rev: 45, row: watch_state() }],
            sessions: vec![
                SessionInfo {
                    source: "fp".into(),
                    status: SessionStatus::Ok,
                    username: Some("someone".into()),
                    fingerprint: fingerprint("example-session-value"),
                    value: Some("example-session-value".into()),
                    updated_at: 1_761_225_000_000,
                },
                SessionInfo {
                    source: "sp".into(),
                    status: SessionStatus::Expired,
                    username: None,
                    fingerprint: fingerprint("same-as-the-device"),
                    value: None,
                    updated_at: 1_761_000_000_000,
                },
            ],
        },
    );
}

#[test]
fn pairing() {
    check(
        "pair_started",
        &PairStarted {
            device_code: "sd_8Jd7...device-secret".into(),
            user_code: "KJ7W-3MPQ".into(),
            verification_url: "https://slopstream.example.com/pair".into(),
            expires_in_sec: 600,
            interval_sec: 5,
        },
    );
    check(
        "pair_result",
        &PairResult {
            status: PairStatus::Approved,
            token: Some("ss_example-token".into()),
            person: Some(Person { name: "Sam".into(), email: "sam@example.com".into() }),
        },
    );
}

#[test]
fn sessions() {
    check("session_put", &SessionPut { value: "example-session-value".into(), username: Some("someone".into()) });
    check("expired_report", &ExpiredReport { fingerprint: fingerprint("example-session-value") });
}

#[test]
fn feeds() {
    check(
        "feed_request",
        &FeedRequest {
            protocol: PROTOCOL,
            channels: BTreeMap::from([
                ("yt:UCXuqSBlHAE6Xw-yeJA0Tunw".to_string(), 120),
                ("fp:59f94c0bdd241b70349eb72b".to_string(), 0),
            ]),
        },
    );
    check(
        "feed_response",
        &FeedResponse {
            protocol: PROTOCOL,
            more: false,
            items: vec![
                FeedRow {
                    rev: 121,
                    item: slopstream_core::feeds::FeedItem {
                        source: "yt".into(),
                        remote_id: "dQw4w9WgXcQ".into(),
                        channel_id: "UCXuqSBlHAE6Xw-yeJA0Tunw".into(),
                        channel_name: "Linus Tech Tips".into(),
                        title: "A video".into(),
                        published_at: 1_761_230_000_000,
                        kind: "VIDEO".into(),
                        thumbnail_url: Some("https://i.ytimg.com/vi/dQw4w9WgXcQ/hq720.jpg".into()),
                        duration_sec: None,
                    },
                    deleted: false,
                },
                FeedRow {
                    rev: 122,
                    item: slopstream_core::feeds::FeedItem {
                        source: "fp".into(),
                        remote_id: "live:59f94c0bdd241b70349eb72b".into(),
                        channel_id: "59f94c0bdd241b70349eb72b".into(),
                        channel_name: "Linus Tech Tips".into(),
                        title: "The WAN Show".into(),
                        published_at: 1_761_231_000_000,
                        kind: "LIVE".into(),
                        thumbnail_url: None,
                        duration_sec: None,
                    },
                    deleted: true,
                },
            ],
            channels: vec![ChannelStatus {
                source: "yt".into(),
                remote_id: "UCXuqSBlHAE6Xw-yeJA0Tunw".into(),
                name: Some("Linus Tech Tips".into()),
                last_ok_at: Some(1_761_230_500_000),
                last_error: None,
                rev: 121,
            }],
            duration_jobs: vec!["UCXuqSBlHAE6Xw-yeJA0Tunw".into()],
        },
    );
    check(
        "feed_durations",
        &FeedDurations {
            channel_id: "UCXuqSBlHAE6Xw-yeJA0Tunw".into(),
            durations: BTreeMap::from([("dQw4w9WgXcQ".to_string(), 212)]),
        },
    );
    check("durations_stored", &DurationsStored { updated: 1 });
    check("changes", &ChangesAnswer { generation: 7, changed: true });
    check("feed_refresh", &FeedRefresh { fetched: 12, pending: 0 });
}

fn remote_item() -> slopstream_core::remote::RemoteItem {
    slopstream_core::remote::RemoteItem {
        source: "yt".into(),
        video_id: "dQw4w9WgXcQ".into(),
        title: "A video".into(),
        channel_name: "A channel".into(),
        channel_id: Some("UCuAXFkgsw1L7xaCfnd5JJOw".into()),
        thumbnail_url: Some("https://i.ytimg.com/vi/dQw4w9WgXcQ/hq720.jpg".into()),
        kind: Some("VIDEO".into()),
    }
}

#[test]
fn remote() {
    use slopstream_core::remote::*;
    let session = "6f1c2d3e-4b5a-4c6d-8e7f-0a1b2c3d4e5f".to_string();
    let now_playing = NowPlaying {
        session: session.clone(),
        item: remote_item(),
        state: PlayState::Playing,
        position_ms: 61_000,
        duration_ms: 212_000,
        error: None,
        at: 1_761_230_000_000,
        guest: true,
    };
    check("remote_receive", &ReceiveRequest { since: Some(4), services: vec!["yt".into(), "fp".into()] });
    check(
        "remote_commands",
        &ReceiveAnswer {
            seq: 6,
            commands: vec![
                RemoteCommand {
                    seq: 5,
                    session: session.clone(),
                    action: RemoteAction::Play,
                    item: Some(remote_item()),
                    position_ms: Some(61_000),
                    guest: true,
                },
                RemoteCommand {
                    seq: 6,
                    session: session.clone(),
                    action: RemoteAction::Seek,
                    item: None,
                    position_ms: Some(90_000),
                    guest: true,
                },
            ],
        },
    );
    check("remote_send", &SendRequest { item: remote_item(), start_ms: 61_000 });
    check("remote_sent", &SendAnswer { session: session.clone() });
    check(
        "remote_control",
        &ControlRequest { session: session.clone(), action: RemoteAction::Pause, position_ms: None },
    );
    check(
        "remote_state",
        &StateReport {
            session: session.clone(),
            state: PlayState::Paused,
            position_ms: 61_000,
            duration_ms: 212_000,
            watched: false,
            error: None,
        },
    );
    check(
        "remote_targets",
        &Targets {
            targets: vec![Target {
                id: "0b7d9a8e-1c2f-4e3d-9a5b-6c7d8e9f0a1b".into(),
                name: "Living room TV".into(),
                own: false,
                services: vec!["yt".into(), "fp".into()],
                now_playing: Some(now_playing.clone()),
            }],
        },
    );
    check("remote_status", &RemoteStatus { generation: 3, online: true, now_playing: Some(now_playing) });
}
