//! The server's feed fetcher and `/api/v1/feed` against a real Postgres and
//! stand-ins for YouTube and Floatplane. Needs `SLOPSTREAM_TEST_DATABASE_URL`.

mod common;

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;

use axum::http::StatusCode;
use common::{Db, Device, FIXTURE_CHANNEL};
use slopstream_core::feeds::{ChannelFeed, FeedItem};
use slopstream_core::wire::{FeedRefresh, FeedRequest, FeedResponse, PROTOCOL, SessionPut, Subscription};
use slopstream_server::auth::Role;
use slopstream_server::{feeds, library};
use uuid::Uuid;

fn sub(source: &str, id: &str) -> Subscription {
    Subscription {
        source: source.into(),
        remote_id: id.into(),
        name: String::new(),
        avatar_url: None,
        favorite: false,
        added_at: 0,
        updated_at: 0,
        deleted_at: None,
        device_id: "test".into(),
    }
}

async fn follow(db: &Db, user: Uuid, source: &str, id: &str) {
    library::follow(&db.pool, user, sub(source, id)).await.unwrap();
}

async fn feed(device: &Device, channels: &[(&str, i64)]) -> FeedResponse {
    let req = FeedRequest {
        protocol: PROTOCOL,
        channels: channels.iter().map(|(k, r)| (k.to_string(), *r)).collect::<BTreeMap<_, _>>(),
    };
    device.json("POST", "/api/v1/feed", &req).await
}

fn yt() -> String {
    format!("yt:{FIXTURE_CHANNEL}")
}

#[tokio::test]
async fn one_youtube_fetch_serves_everyone_who_follows_the_channel() {
    let Some(db) = Db::new().await else { return };
    let sam = db.person("sam@example.com", Role::Admin).await;
    let kim = db.person("kim@example.com", Role::User).await;
    follow(&db, sam, "yt", FIXTURE_CHANNEL).await;
    follow(&db, kim, "yt", FIXTURE_CHANNEL).await;

    assert_eq!(feeds::run_once(&db.state()).await.unwrap(), 1, "one channel, once");
    assert_eq!(db.youtube.feed_hits.load(Ordering::SeqCst), 3, "the three uploads playlists");
    assert_eq!(feeds::run_once(&db.state()).await.unwrap(), 0, "not due again yet");

    for user in [sam, kim] {
        let r = feed(&db.device(user, "tv").await, &[(&yt(), 0)]).await;
        let ids: Vec<&str> = r.items.iter().map(|i| i.item.remote_id.as_str()).collect();
        assert_eq!(ids.len(), 3, "{ids:?}");
        assert!(r.items.iter().all(|i| !i.deleted && i.item.thumbnail_url.is_some()));
        let status = &r.channels[0];
        assert!(status.last_ok_at.is_some() && status.last_error.is_none());
        assert_eq!(status.name.as_deref(), Some("Fixture Channel"));
        assert_eq!(status.rev, r.items.iter().map(|i| i.rev).max().unwrap());

        // Up to date: nothing more for this channel.
        let again = feed(&db.device(user, "tv2").await, &[(&yt(), status.rev)]).await;
        assert!(again.items.is_empty());
    }
    db.drop().await;
}

#[tokio::test]
async fn a_person_gets_only_channels_they_follow() {
    let Some(db) = Db::new().await else { return };
    let sam = db.person("sam@example.com", Role::Admin).await;
    let kim = db.person("kim@example.com", Role::User).await;
    follow(&db, sam, "yt", FIXTURE_CHANNEL).await;
    feeds::run_once(&db.state()).await.unwrap();
    let r = feed(&db.device(kim, "kim").await, &[(&yt(), 0)]).await;
    assert!(r.items.is_empty() && r.channels.is_empty());

    // A rev beyond anything this server handed out (it was restored): from the start.
    let r = feed(&db.device(sam, "sam").await, &[(&yt(), 9_999_999)]).await;
    assert_eq!(r.items.len(), 3);
    db.drop().await;
}

#[tokio::test]
async fn floatplane_lists_are_per_person_and_the_stream_comes_and_goes() {
    let Some(db) = Db::new().await else { return };
    let sam = db.person("sam@example.com", Role::Admin).await;
    let kim = db.person("kim@example.com", Role::User).await;
    let sams = db.device(sam, "sam").await;
    let kims = db.device(kim, "kim").await;
    let _: serde_json::Value = sams
        .json("PUT", "/api/v1/sessions/fp", &SessionPut { value: "good-session-sam-aaaa".into(), username: None })
        .await;
    let _: serde_json::Value = kims
        .json("PUT", "/api/v1/sessions/fp", &SessionPut { value: "good-limited-kim-aaaa".into(), username: None })
        .await;
    follow(&db, sam, "fp", "creatorA").await;
    follow(&db, kim, "fp", "creatorA").await;
    db.site.live.store(true, Ordering::SeqCst);

    assert_eq!(feeds::run_once(&db.state()).await.unwrap(), 2, "one fetch per person");
    let ids = |r: &FeedResponse| {
        let mut v = r.items.iter().filter(|i| !i.deleted).map(|i| i.item.remote_id.clone()).collect::<Vec<_>>();
        v.sort();
        v
    };
    let s = feed(&sams, &[("fp:creatorA", 0)]).await;
    assert!(s.items.windows(2).all(|w| w[0].rev < w[1].rev), "lowest rev first");
    assert_eq!(ids(&s), ["live:creatorA", "postSingle1", "postSubChan"]);
    let k = feed(&kims, &[("fp:creatorA", 0)]).await;
    assert_eq!(ids(&k), ["live:creatorA", "postSubChan"], "Kim's plan, Kim's list");
    assert_eq!(db.site.cookie_leaks.load(Ordering::SeqCst), 0, "the stream's playlist never gets the session");

    // The stream ends: the next fetch tombstones it.
    db.site.live.store(false, Ordering::SeqCst);
    sqlx::query("UPDATE feed_channels SET next_fetch_at = now()").execute(&db.pool).await.unwrap();
    feeds::run_once(&db.state()).await.unwrap();
    let rev = s.channels[0].rev;
    let after = feed(&sams, &[("fp:creatorA", rev)]).await;
    assert_eq!(after.items.len(), 1);
    assert!(after.items[0].deleted && after.items[0].item.remote_id == "live:creatorA");
    db.drop().await;
}

#[tokio::test]
async fn a_signed_out_session_stops_its_fetches_and_shows_everywhere() {
    let Some(db) = Db::new().await else { return };
    let sam = db.person("sam@example.com", Role::Admin).await;
    let mut sams = db.device(sam, "sam").await;
    let value = "good-session-sam-bbbb";
    let _: serde_json::Value =
        sams.json("PUT", "/api/v1/sessions/fp", &SessionPut { value: value.into(), username: None }).await;
    follow(&db, sam, "fp", "creatorA").await;
    db.site.revoke(value);
    feeds::run_once(&db.state()).await.unwrap();
    assert_eq!(sams.pull().await.sessions[0].status, slopstream_core::wire::SessionStatus::Expired);
    sqlx::query("UPDATE feed_channels SET next_fetch_at = now()").execute(&db.pool).await.unwrap();
    let hits = db.site.api_hits.load(Ordering::SeqCst);
    assert_eq!(feeds::run_once(&db.state()).await.unwrap(), 0, "not fetched while expired");
    assert_eq!(db.site.api_hits.load(Ordering::SeqCst), hits);
    db.drop().await;
}

#[tokio::test]
async fn a_refusal_pauses_youtube_and_keeps_the_last_good_copy() {
    let Some(db) = Db::new().await else { return };
    let sam = db.person("sam@example.com", Role::Admin).await;
    follow(&db, sam, "yt", FIXTURE_CHANNEL).await;
    feeds::run_once(&db.state()).await.unwrap();

    db.youtube.status.store(429, Ordering::SeqCst);
    sqlx::query("UPDATE feed_channels SET next_fetch_at = now()").execute(&db.pool).await.unwrap();
    feeds::run_once(&db.state()).await.unwrap();
    let r = feed(&db.device(sam, "tv").await, &[(&yt(), 0)]).await;
    assert_eq!(r.items.len(), 3, "kept");
    assert!(r.channels[0].last_error.as_deref().unwrap().contains("429"));

    sqlx::query("UPDATE feed_channels SET next_fetch_at = now()").execute(&db.pool).await.unwrap();
    let hits = db.youtube.feed_hits.load(Ordering::SeqCst);
    feeds::run_once(&db.state()).await.unwrap();
    assert_eq!(db.youtube.feed_hits.load(Ordering::SeqCst), hits, "paused: no request at all");
    db.drop().await;
}

#[tokio::test]
async fn failures_back_off() {
    let Some(db) = Db::new().await else { return };
    let sam = db.person("sam@example.com", Role::Admin).await;
    follow(&db, sam, "yt", FIXTURE_CHANNEL).await;
    db.youtube.status.store(500, Ordering::SeqCst);
    feeds::run_once(&db.state()).await.unwrap();
    let (failures, wait): (i32, f64) =
        sqlx::query_as("SELECT failures, extract(epoch FROM next_fetch_at - now())::float8 FROM feed_channels")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(failures, 1);
    assert!((1700.0..=1800.0).contains(&wait), "{wait}");
    db.drop().await;
}

#[tokio::test]
async fn unfollowed_channels_go_after_a_week() {
    let Some(db) = Db::new().await else { return };
    let sam = db.person("sam@example.com", Role::Admin).await;
    follow(&db, sam, "yt", FIXTURE_CHANNEL).await;
    feeds::run_once(&db.state()).await.unwrap();
    library::edit::<Subscription>(&db.pool, sam, "yt", FIXTURE_CHANNEL, |r, now| r.deleted_at = Some(now))
        .await
        .unwrap();
    feeds::reconcile(&db.pool).await.unwrap();
    let marked: bool =
        sqlx::query_scalar("SELECT unfollowed_at IS NOT NULL FROM feed_channels").fetch_one(&db.pool).await.unwrap();
    assert!(marked);
    sqlx::query("UPDATE feed_channels SET unfollowed_at = now() - interval '8 days'").execute(&db.pool).await.unwrap();
    feeds::reconcile(&db.pool).await.unwrap();
    let left: (i64, i64) =
        sqlx::query_as("SELECT (SELECT count(*) FROM feed_channels), (SELECT count(*) FROM feed_items)")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(left, (0, 0));
    db.drop().await;
}

fn item(id: &str, at: i64, duration: Option<i64>) -> FeedItem {
    FeedItem {
        source: "yt".into(),
        remote_id: id.into(),
        channel_id: FIXTURE_CHANNEL.into(),
        channel_name: "C".into(),
        title: format!("Video {id}"),
        published_at: at,
        kind: "VIDEO".into(),
        thumbnail_url: None,
        duration_sec: duration,
    }
}

async fn rev(conn: &mut sqlx::PgConnection) -> (i64, Option<i64>) {
    sqlx::query_as("SELECT rev, duration_sec FROM feed_items WHERE remote_id = 'a'").fetch_one(conn).await.unwrap()
}

#[tokio::test]
async fn storing_keeps_durations_moves_revs_only_on_change_and_prunes() {
    let Some(db) = Db::new().await else { return };
    let owner = feeds::SHARED;
    let mut conn = db.pool.acquire().await.unwrap();
    let feed = |items| ChannelFeed { channel_name: None, items, complete_kinds: vec![] };
    feeds::store(&mut conn, owner, "yt", FIXTURE_CHANNEL, &feed(vec![item("a", 1, Some(600))]), 1).await.unwrap();
    let first = rev(&mut conn).await;
    // RSS again, without a duration: kept, and nothing changed.
    feeds::store(&mut conn, owner, "yt", FIXTURE_CHANNEL, &feed(vec![item("a", 1, None)]), 2).await.unwrap();
    assert_eq!(rev(&mut conn).await, first);
    // A new title moves the rev.
    let mut renamed = item("a", 1, None);
    renamed.title = "Renamed".into();
    feeds::store(&mut conn, owner, "yt", FIXTURE_CHANNEL, &feed(vec![renamed]), 3).await.unwrap();
    let after = rev(&mut conn).await;
    assert!(after.0 > first.0 && after.1 == Some(600));

    let many: Vec<FeedItem> = (0..205).map(|i| item(&format!("v{i}"), 1000 + i, None)).collect();
    feeds::store(&mut conn, owner, "yt", FIXTURE_CHANNEL, &feed(many), 4).await.unwrap();
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM feed_items").fetch_one(&mut *conn).await.unwrap();
    assert_eq!(n, 200, "the newest 200 of the kind");
    drop(conn);
    db.drop().await;
}

#[tokio::test]
async fn refresh_fetches_now_but_not_twice_a_minute() {
    let Some(db) = Db::new().await else { return };
    let sam = db.person("sam@example.com", Role::Admin).await;
    follow(&db, sam, "yt", FIXTURE_CHANNEL).await;
    let state = db.state();
    tokio::spawn(feeds::run(state.clone()));
    let tv = db.device(sam, "tv").await;
    // Wait for the loop's first round.
    for _ in 0..50 {
        if !feed(&tv, &[(&yt(), 0)]).await.items.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    sqlx::query("UPDATE feed_channels SET last_fetch_at = now() - interval '10 minutes', next_fetch_at = now() + interval '1 hour'")
        .execute(&db.pool)
        .await
        .unwrap();
    let r: FeedRefresh = tv.json("POST", "/api/v1/feed/refresh", &()).await;
    assert_eq!((r.fetched, r.pending), (1, 0));
    let again: FeedRefresh = tv.json("POST", "/api/v1/feed/refresh", &()).await;
    assert_eq!(again.fetched, 0, "once a minute");

    let (status, _) =
        common::anon(&db.app(), "/api/v1/feed", &FeedRequest { protocol: PROTOCOL, channels: BTreeMap::new() }).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    db.drop().await;
}
