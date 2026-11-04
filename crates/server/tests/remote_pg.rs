//! Playing on a TV (remote.rs). Needs `SLOPSTREAM_TEST_DATABASE_URL`.

mod common;

use axum::http::StatusCode;
use common::{Browser, Db, Device};
use slopstream_core::remote::{
    ControlRequest, PlayState, ReceiveAnswer, ReceiveRequest, RemoteAction, RemoteItem, RemoteStatus, SendAnswer,
    SendRequest, StateReport, Targets,
};
use slopstream_server::auth::Role;
use uuid::Uuid;

fn item(source: &str) -> RemoteItem {
    RemoteItem {
        source: source.into(),
        video_id: "dQw4w9WgXcQ".into(),
        title: "A video".into(),
        channel_name: "A channel".into(),
        channel_id: Some("UCuAXFkgsw1L7xaCfnd5JJOw".into()),
        thumbnail_url: None,
        kind: Some("VIDEO".into()),
    }
}

async fn receive(tv: &Device, since: Option<u64>) -> ReceiveAnswer {
    tv.json("POST", "/api/v1/remote/receive", &ReceiveRequest { since, services: vec!["yt".into()] }).await
}

async fn targets(d: &Device) -> Targets {
    let (status, body) = d.call::<()>("GET", "/api/v1/remote/targets", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    serde_json::from_str(&body).unwrap()
}

async fn play(d: &Device, tv: Uuid, item: RemoteItem) -> (StatusCode, String) {
    d.call("POST", &format!("/api/v1/remote/{tv}/play"), Some(&SendRequest { item, start_ms: 61_000 })).await
}

async fn report(tv: &Device, session: &str, state: PlayState, position_ms: i64) {
    let rep =
        StateReport { session: session.into(), state, position_ms, duration_ms: 212_000, watched: false, error: None };
    let (status, body) = tv.call("POST", "/api/v1/remote/state", Some(&rep)).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
}

async fn token_id(db: &Db, label: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM api_tokens WHERE label = $1").bind(label).fetch_one(&db.pool).await.unwrap()
}

#[tokio::test]
async fn a_phone_plays_on_its_own_tv_and_steers_it() {
    let Some(db) = Db::new().await else { return };
    let sam = db.person("sam@example.com", Role::Admin).await;
    let mut phone = db.device(sam, "Pixel").await;
    let tv = db.device(sam, "Living room TV").await;
    let tv_id = token_id(&db, "Living room TV").await;

    let first = receive(&tv, None).await;
    assert!(first.commands.is_empty());
    let list = targets(&phone).await.targets;
    assert_eq!(list.len(), 1);
    assert!(list[0].own && list[0].name == "Living room TV" && list[0].now_playing.is_none());
    // A TV never lists itself.
    assert!(targets(&tv).await.targets.is_empty());

    let waiting = tokio::spawn(async move {
        let a = receive(&tv, Some(first.seq)).await;
        (tv, a)
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let (status, body) = play(&phone, tv_id, item("yt")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let sent: SendAnswer = serde_json::from_str(&body).unwrap();
    let (tv, woke) = waiting.await.unwrap();
    assert_eq!(woke.commands.len(), 1);
    let cmd = &woke.commands[0];
    assert!(cmd.action == RemoteAction::Play && !cmd.guest && cmd.position_ms == Some(61_000));
    assert_eq!(cmd.session, sent.session);

    report(&tv, &sent.session, PlayState::Playing, 62_000).await;
    let (_, body) = phone.call::<()>("GET", &format!("/api/v1/remote/{tv_id}/status"), None).await;
    let status: RemoteStatus = serde_json::from_str(&body).unwrap();
    let now = status.now_playing.unwrap();
    assert!(status.online && now.state == PlayState::Playing && now.position_ms == 62_000 && !now.guest);

    let pause = ControlRequest { session: sent.session.clone(), action: RemoteAction::Pause, position_ms: None };
    let (s, body) = phone.call("POST", &format!("/api/v1/remote/{tv_id}/control"), Some(&pause)).await;
    assert_eq!(s, StatusCode::NO_CONTENT, "{body}");
    let next = receive(&tv, Some(woke.seq)).await;
    assert_eq!(next.commands.iter().map(|c| c.action).collect::<Vec<_>>(), vec![RemoteAction::Pause]);

    // Its own TV keeps its own library: the server writes nothing for it.
    report(&tv, &sent.session, PlayState::Paused, 63_000).await;
    assert!(phone.pull().await.watch_state.is_empty());

    // A stopped session takes no more steering.
    report(&tv, &sent.session, PlayState::Stopped, 63_000).await;
    let (s, _) = phone.call("POST", &format!("/api/v1/remote/{tv_id}/control"), Some(&pause)).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    db.drop().await;
}

#[tokio::test]
async fn others_play_on_a_tv_once_it_is_shared_and_keep_their_own_progress() {
    let Some(db) = Db::new().await else { return };
    let sam = db.person("sam@example.com", Role::Admin).await;
    let alex = db.person("alex@example.com", Role::User).await;
    let mut sams_phone = db.device(sam, "Sam's phone").await;
    let mut alexs_phone = db.device(alex, "Alex's phone").await;
    let tv = db.device(sam, "Living room TV").await;
    let tv_id = token_id(&db, "Living room TV").await;
    receive(&tv, None).await;

    assert!(targets(&alexs_phone).await.targets.is_empty());
    assert_eq!(play(&alexs_phone, tv_id, item("yt")).await.0, StatusCode::NOT_FOUND);

    let mut web = Browser::logged_in(db.app(), "sam@example.com").await;
    let r = web.post(&format!("/devices/tokens/{tv_id}/shared"), &[("shared", "true")]).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.body);
    // Only the owner shares a device.
    let mut alex_web = Browser::logged_in(db.app(), "alex@example.com").await;
    let r = alex_web.post(&format!("/devices/tokens/{tv_id}/shared"), &[("shared", "false")]).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    let list = targets(&alexs_phone).await.targets;
    assert!(list.len() == 1 && !list[0].own);
    let (status, body) = play(&alexs_phone, tv_id, item("yt")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let sent: SendAnswer = serde_json::from_str(&body).unwrap();
    let cmds = receive(&tv, Some(0)).await.commands;
    assert!(cmds.len() == 1 && cmds[0].guest);

    // Progress goes to Alex, never to the TV's account.
    report(&tv, &sent.session, PlayState::Playing, 70_000).await;
    let states = alexs_phone.pull().await.watch_state;
    assert_eq!(states.len(), 1);
    let s = &states[0].row;
    assert_eq!((s.position_ms, s.duration_ms, s.watched), (70_000, 212_000, false));
    assert_eq!(s.title.as_deref(), Some("A video"));
    assert_eq!(s.device_id, format!("tv:{tv_id}"));
    // Within 10 s of the last write, playing on writes nothing; a pause does.
    report(&tv, &sent.session, PlayState::Playing, 75_000).await;
    assert!(alexs_phone.pull().await.watch_state.is_empty());
    report(&tv, &sent.session, PlayState::Paused, 80_000).await;
    assert_eq!(alexs_phone.pull().await.watch_state[0].row.position_ms, 80_000);
    report(&tv, &sent.session, PlayState::Ended, 211_000).await;
    let end = &alexs_phone.pull().await.watch_state[0].row;
    assert!(end.watched && end.position_ms == 212_000);
    assert!(sams_phone.pull().await.watch_state.is_empty());

    // Unshared: gone for Alex at once.
    web.post(&format!("/devices/tokens/{tv_id}/shared"), &[("shared", "false")]).await;
    assert!(targets(&alexs_phone).await.targets.is_empty());
    db.drop().await;
}

#[tokio::test]
async fn a_tv_takes_only_what_it_can_play_and_only_while_open() {
    let Some(db) = Db::new().await else { return };
    let sam = db.person("sam@example.com", Role::Admin).await;
    let phone = db.device(sam, "Pixel").await;
    let tv = db.device(sam, "Living room TV").await;
    let tv_id = token_id(&db, "Living room TV").await;
    receive(&tv, None).await;

    let (status, body) = play(&phone, tv_id, item("fp")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body.contains("not signed in to Floatplane"), "{body}");

    // A first receive replays nothing that was sent before.
    play(&phone, tv_id, item("yt")).await;
    assert!(receive(&tv, None).await.commands.is_empty());

    let (s, _) = tv.call::<()>("POST", "/api/v1/remote/offline", None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert!(targets(&phone).await.targets.is_empty());
    assert_eq!(play(&phone, tv_id, item("yt")).await.0, StatusCode::TOO_MANY_REQUESTS);

    // A revoked TV is gone, not offline.
    receive(&tv, None).await;
    let mut web = Browser::logged_in(db.app(), "sam@example.com").await;
    web.post(&format!("/devices/tokens/{tv_id}/revoke"), &[]).await;
    assert!(targets(&phone).await.targets.is_empty());
    assert_eq!(play(&phone, tv_id, item("yt")).await.0, StatusCode::NOT_FOUND);
    db.drop().await;
}
