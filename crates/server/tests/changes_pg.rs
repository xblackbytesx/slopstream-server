//! Long-poll and duration jobs (0.3.0). Needs `SLOPSTREAM_TEST_DATABASE_URL`.

mod common;

use std::collections::BTreeMap;

use common::{Browser, CHANNEL_ID, Db, Device, FIXTURE_CHANNEL, now_ms};
use slopstream_core::wire::{
    Change, ChangesAnswer, DurationsStored, FeedDurations, FeedRequest, FeedResponse, PROTOCOL, Subscription,
};
use slopstream_server::auth::Role;
use slopstream_server::{feeds, library};

fn sub(id: &str, at: i64) -> Subscription {
    Subscription {
        source: "yt".into(),
        remote_id: id.into(),
        name: "C".into(),
        avatar_url: None,
        favorite: false,
        added_at: at,
        updated_at: at,
        deleted_at: None,
        device_id: "d".into(),
    }
}

async fn changes(d: &Device, since: u64) -> ChangesAnswer {
    let (status, body) = d.call::<()>("GET", &format!("/api/v1/changes?since={since}"), None).await;
    assert!(status.is_success(), "{body}");
    serde_json::from_str(&body).unwrap()
}

#[tokio::test]
async fn a_waiting_device_wakes_when_another_one_writes() {
    let Some(db) = Db::new().await else { return };
    let sam = db.person("sam@example.com", Role::Admin).await;
    let mut phone = db.device(sam, "phone").await;
    let tv = db.device(sam, "tv").await;

    // A number the server never handed out (a restart, a first call): at once.
    let first = changes(&tv, 12345).await;
    assert!(first.changed);

    let waiting = tokio::spawn(async move { changes(&tv, first.generation).await });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let mut req = phone.request(now_ms());
    req.subscriptions = vec![Change { base_rev: 0, row: sub("UC1", now_ms()) }];
    phone.sync(req).await;
    let woke = waiting.await.unwrap();
    assert!(woke.changed && woke.generation > first.generation);

    // A sync that writes nothing wakes nobody.
    let before = db.changes.current(sam);
    phone.pull().await;
    assert_eq!(db.changes.current(sam), before);
    db.drop().await;
}

#[tokio::test]
async fn website_edits_and_fresh_lists_wake_the_right_people() {
    let Some(db) = Db::new().await else { return };
    let sam = db.person("sam@example.com", Role::Admin).await;
    let kim = db.person("kim@example.com", Role::User).await;
    let mut web = Browser::logged_in(db.app(), "sam@example.com").await;

    let (s0, k0) = (db.changes.current(sam), db.changes.current(kim));
    web.post("/subscriptions/add", &[("source", "yt"), ("channel", CHANNEL_ID)]).await;
    assert!(db.changes.current(sam) > s0);
    assert_eq!(db.changes.current(kim), k0, "never another person's library");

    // Kim follows the fixture channel; its first list wakes Kim only.
    library::follow(&db.pool, kim, sub(FIXTURE_CHANNEL, 0)).await.unwrap();
    let (s1, k1) = (db.changes.current(sam), db.changes.current(kim));
    feeds::run_once(&db.state()).await.unwrap();
    assert!(db.changes.current(kim) > k1);
    assert_eq!(db.changes.current(sam), s1);
    db.drop().await;
}

async fn feed(d: &Device, rev: i64) -> FeedResponse {
    let req = FeedRequest { protocol: PROTOCOL, channels: BTreeMap::from([(format!("yt:{FIXTURE_CHANNEL}"), rev)]) };
    d.json("POST", "/api/v1/feed", &req).await
}

#[tokio::test]
async fn one_device_at_a_time_looks_up_durations_and_everyone_gets_them() {
    let Some(db) = Db::new().await else { return };
    let sam = db.person("sam@example.com", Role::Admin).await;
    library::follow(&db.pool, sam, sub(FIXTURE_CHANNEL, 0)).await.unwrap();
    feeds::run_once(&db.state()).await.unwrap();
    let tv = db.device(sam, "tv").await;
    let phone = db.device(sam, "phone").await;

    let t = feed(&tv, 0).await;
    assert_eq!(t.duration_jobs, [FIXTURE_CHANNEL], "RSS has no durations");
    assert!(feed(&phone, 0).await.duration_jobs.is_empty(), "the TV has it");
    assert_eq!(feed(&tv, 0).await.duration_jobs, [FIXTURE_CHANNEL], "still the TV's");

    let rev = t.channels[0].rev;
    let report = FeedDurations {
        channel_id: FIXTURE_CHANNEL.into(),
        durations: BTreeMap::from([("vidAAAAAAA1".to_string(), 754), ("notInThisFeed".to_string(), 10)]),
    };
    let stored: DurationsStored = tv.json("POST", "/api/v1/feed/durations", &report).await;
    assert_eq!(stored.updated, 1);

    let p = feed(&phone, rev).await;
    assert_eq!(p.items.len(), 1);
    assert_eq!((p.items[0].item.remote_id.as_str(), p.items[0].item.duration_sec), ("vidAAAAAAA1", Some(754)));
    assert!(p.duration_jobs.is_empty(), "the rest were checked, found or not: never asked again");
    assert!(feed(&tv, 0).await.duration_jobs.is_empty());

    // A durations report for a channel you don't follow is refused.
    let other = FeedDurations { channel_id: "UCbbbbbbbbbbbbbbbbbbbbbb".into(), durations: BTreeMap::new() };
    let (status, _) = tv.call("POST", "/api/v1/feed/durations", Some(&other)).await;
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
    db.drop().await;
}
