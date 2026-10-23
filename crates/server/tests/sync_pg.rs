//! The sync contract against a real Postgres: the server leads, clocks are
//! corrected, people never see each other's rows, and paging loses nothing.
//! Needs `SLOPSTREAM_TEST_DATABASE_URL`; skipped when unset.

mod common;

use axum::http::StatusCode;
use common::{Browser, CHANNEL_ID, Db, now_ms};
use slopstream_core::merge::DOUBT_MS;
use slopstream_core::wire::{Change, PAGE, Subscription, WatchLater, WatchState};
use slopstream_server::auth::Role;

fn sub(id: &str, name: &str, updated_at: i64, device: &str) -> Subscription {
    Subscription {
        source: "yt".into(),
        remote_id: id.into(),
        name: name.into(),
        avatar_url: None,
        favorite: false,
        added_at: updated_at,
        updated_at,
        deleted_at: None,
        device_id: device.into(),
    }
}

fn progress(id: &str, position_ms: i64, updated_at: i64) -> WatchState {
    WatchState {
        source: "yt".into(),
        remote_id: id.into(),
        position_ms,
        duration_ms: 600_000,
        watched: false,
        title: Some(format!("Video {id}")),
        channel_id: None,
        channel_name: Some("Channel".into()),
        thumbnail_url: None,
        kind: Some("VIDEO".into()),
        updated_at,
        deleted_at: None,
        device_id: "tv".into(),
    }
}

#[tokio::test]
async fn the_first_device_seeds_and_the_second_receives() {
    let Some(db) = Db::new().await else { return };
    let sam = db.person("sam@example.com", Role::Admin).await;
    let mut phone = db.device(sam, "phone").await;
    let mut tv = db.device(sam, "tv").await;
    let now = now_ms();

    let mut req = phone.request(now);
    req.subscriptions = vec![Change { base_rev: 0, row: sub("UC1", "One", now - 1000, "phone") }];
    req.watch_state = vec![Change { base_rev: 0, row: progress("v1", 61_000, now - 500) }];
    let r = phone.sync(req).await;
    assert_eq!(r.subscriptions.len(), 1, "the device gets its own rows back with their rev");
    assert!(r.subscriptions[0].rev > 0 && r.cursor >= r.subscriptions[0].rev);

    let got = tv.pull().await;
    assert_eq!(got.subscriptions.len(), 1);
    assert_eq!(got.subscriptions[0].row.name, "One");
    assert_eq!(got.watch_state[0].row.position_ms, 61_000);
    assert_eq!(got.watch_state[0].row.title.as_deref(), Some("Video v1"));

    // Nothing new: an empty answer, the cursor stays.
    let again = tv.pull().await;
    assert!(again.subscriptions.is_empty() && again.watch_state.is_empty());
    assert_eq!(again.cursor, got.cursor);
    db.drop().await;
}

#[tokio::test]
async fn a_stale_device_loses_unless_clearly_newer() {
    let Some(db) = Db::new().await else { return };
    let sam = db.person("sam@example.com", Role::Admin).await;
    let mut phone = db.device(sam, "phone").await;
    let mut tv = db.device(sam, "tv").await;
    let now = now_ms();

    let mut req = phone.request(now);
    req.subscriptions = vec![Change { base_rev: 0, row: sub("UC1", "One", now - 60_000, "phone") }];
    let rev1 = phone.sync(req).await.subscriptions[0].rev;
    tv.pull().await;

    // The phone favorites it (it saw rev1): applied.
    let mut fav = sub("UC1", "One", now - 30_000, "phone");
    fav.favorite = true;
    let mut req = phone.request(now);
    req.subscriptions = vec![Change { base_rev: rev1, row: fav }];
    let rev2 = phone.sync(req).await.subscriptions[0].rev;
    assert!(rev2 > rev1);

    // The TV, still on rev1, unfollows with a stamp only 1 s newer: in doubt,
    // the server's row stays and comes back to the TV.
    let mut gone = sub("UC1", "One", now - 30_000 + DOUBT_MS / 2, "tv");
    gone.deleted_at = Some(gone.updated_at);
    let mut req = tv.request(now);
    req.subscriptions = vec![Change { base_rev: rev1, row: gone.clone() }];
    let r = tv.sync(req).await;
    assert_eq!(r.subscriptions.len(), 1);
    assert!(r.subscriptions[0].row.favorite && r.subscriptions[0].row.deleted_at.is_none(), "server wins");
    assert_eq!(r.subscriptions[0].rev, rev2);

    // Clearly newer (and still based on the old rev): the TV wins.
    gone.updated_at = now - 30_000 + DOUBT_MS + 5_000;
    gone.deleted_at = Some(gone.updated_at);
    let mut req = tv.request(now);
    req.subscriptions = vec![Change { base_rev: rev1, row: gone }];
    let r = tv.sync(req).await;
    assert!(r.subscriptions[0].row.deleted_at.is_some());
    assert!(phone.pull().await.subscriptions[0].row.deleted_at.is_some(), "the tombstone reaches the phone");
    db.drop().await;
}

#[tokio::test]
async fn clocks_are_corrected_and_capped() {
    let Some(db) = Db::new().await else { return };
    let sam = db.person("sam@example.com", Role::Admin).await;
    let mut fast = db.device(sam, "fast").await;
    let now = now_ms();
    let hour = 3_600_000;

    // A device an hour ahead: its stamps arrive shifted back onto our clock.
    let mut req = fast.request(now + hour);
    req.watch_state = vec![Change { base_rev: 0, row: progress("v1", 10_000, now + hour - 5_000) }];
    let r = fast.sync(req).await;
    let stored = r.watch_state[0].row.updated_at;
    assert!((stored - (now - 5_000)).abs() < 2_000, "shifted by the device's offset: {stored} vs {now}");

    // A stamp in the future of the device's own clock is capped at now.
    let mut req = fast.request(now);
    req.watch_state = vec![Change { base_rev: r.watch_state[0].rev, row: progress("v1", 20_000, now + 10 * hour) }];
    let r = fast.sync(req).await;
    assert!(r.watch_state[0].row.updated_at <= now_ms());
    db.drop().await;
}

#[tokio::test]
async fn unchanged_rows_get_no_new_rev() {
    let Some(db) = Db::new().await else { return };
    let sam = db.person("sam@example.com", Role::Admin).await;
    let mut phone = db.device(sam, "phone").await;
    let mut tv = db.device(sam, "tv").await;
    let now = now_ms();
    let mut req = phone.request(now);
    req.subscriptions = vec![Change { base_rev: 0, row: sub("UC1", "One", now - 9_000, "phone") }];
    let rev = phone.sync(req).await.subscriptions[0].rev;
    let mut row = tv.pull().await.subscriptions.remove(0).row;

    // The TV writes the same row again with its own stamp (an account
    // mirror, a favorite toggled twice): nothing changes.
    row.updated_at = now - 1_000;
    row.device_id = "tv".into();
    let mut req = tv.request(now);
    req.subscriptions = vec![Change { base_rev: rev, row }];
    let r = tv.sync(req).await;
    assert_eq!(r.subscriptions[0].rev, rev, "echoed, not rewritten");
    assert!(phone.pull().await.subscriptions.is_empty(), "no churn on the other device");
    db.drop().await;
}

#[tokio::test]
async fn people_never_see_each_others_library() {
    let Some(db) = Db::new().await else { return };
    let sam = db.person("sam@example.com", Role::Admin).await;
    let kim = db.person("kim@example.com", Role::User).await;
    let mut sams = db.device(sam, "sam-phone").await;
    let mut kims = db.device(kim, "kim-phone").await;
    let now = now_ms();
    let mut req = sams.request(now);
    req.subscriptions = vec![Change { base_rev: 0, row: sub("UC1", "Sam's", now, "sam-phone") }];
    sams.sync(req).await;

    let r = kims.pull().await;
    assert!(r.subscriptions.is_empty());
    // Kim's own row with the same key is Kim's alone.
    let mut req = kims.request(now);
    req.subscriptions = vec![Change { base_rev: 0, row: sub("UC1", "Kim's", now, "kim-phone") }];
    kims.sync(req).await;
    let mut fresh = db.device(sam, "sam-tv").await;
    assert_eq!(fresh.pull().await.subscriptions[0].row.name, "Sam's");
    db.drop().await;
}

#[tokio::test]
async fn paging_hands_out_everything_once() {
    let Some(db) = Db::new().await else { return };
    let sam = db.person("sam@example.com", Role::Admin).await;
    let mut phone = db.device(sam, "phone").await;
    let now = now_ms();
    // 3 x 400 rows: more than one page.
    let mut req = phone.request(now);
    req.subscriptions = (0..400).map(|i| Change { base_rev: 0, row: sub(&format!("UC{i}"), "c", now, "p") }).collect();
    req.watch_state = (0..400).map(|i| Change { base_rev: 0, row: progress(&format!("v{i}"), i, now) }).collect();
    req.watch_later = (0..400)
        .map(|i| Change {
            base_rev: 0,
            row: WatchLater {
                source: "fp".into(),
                remote_id: format!("p{i}"),
                title: "t".into(),
                channel_id: "c".into(),
                channel_name: "c".into(),
                thumbnail_url: None,
                duration_sec: None,
                published_at: 1,
                kind: "VIDEO".into(),
                added_at: now,
                updated_at: now,
                deleted_at: None,
                device_id: "p".into(),
            },
        })
        .collect();
    let first = phone.sync(req).await;
    assert!(first.more, "1200 rows do not fit in one page of {PAGE}");

    let mut tv = db.device(sam, "tv").await;
    let page = tv.sync(tv.request(now_ms())).await;
    assert!(page.more);
    assert_eq!(page.subscriptions.len() + page.watch_later.len() + page.watch_state.len(), PAGE);
    let all = {
        let mut rest = tv.pull().await;
        rest.subscriptions.extend(page.subscriptions);
        rest.watch_later.extend(page.watch_later);
        rest.watch_state.extend(page.watch_state);
        rest
    };
    let mut keys: Vec<String> = all
        .subscriptions
        .iter()
        .map(|s| s.row.remote_id.clone())
        .chain(all.watch_later.iter().map(|s| s.row.remote_id.clone()))
        .chain(all.watch_state.iter().map(|s| s.row.remote_id.clone()))
        .collect();
    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), 1200, "every row arrived, none twice");
    db.drop().await;
}

#[tokio::test]
async fn a_bad_row_refuses_the_whole_request() {
    let Some(db) = Db::new().await else { return };
    let sam = db.person("sam@example.com", Role::Admin).await;
    let phone = db.device(sam, "phone").await;
    let now = now_ms();
    let mut bad = sub("UC2", "Two", now, "phone");
    bad.source = "tw".into();
    let mut req = phone.request(now);
    req.subscriptions =
        vec![Change { base_rev: 0, row: sub("UC1", "One", now, "phone") }, Change { base_rev: 0, row: bad }];
    let (status, body) = phone.call("POST", "/api/v1/sync", Some(&req)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("unknown service"));
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM subscriptions").fetch_one(&db.pool).await.unwrap();
    assert_eq!(n, 0, "nothing stored");

    let (status, _) = common::anon(&db.app(), "/api/v1/sync", &req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "no token, no sync");
    db.drop().await;
}

#[tokio::test]
async fn website_changes_reach_the_devices() {
    let Some(db) = Db::new().await else { return };
    let sam = db.person("sam@example.com", Role::Admin).await;
    let mut tv = db.device(sam, "tv").await;
    let mut web = Browser::logged_in(db.app(), "sam@example.com").await;

    let r = web
        .post("/subscriptions/add", &[("source", "yt"), ("channel", "https://www.youtube.com/@LinusTechTips")])
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.body);
    let page = web.get("/subscriptions?service=yt").await;
    assert!(page.body.contains("Linus Tech Tips"));

    let got = tv.pull().await;
    let s = &got.subscriptions[0].row;
    assert_eq!((s.remote_id.as_str(), s.name.as_str()), (CHANNEL_ID, "Linus Tech Tips"));
    assert_eq!(s.avatar_url.as_deref(), Some("https://yt3.googleusercontent.com/ltt=s900"));
    assert_eq!(s.device_id, "web");

    // Favorite and unfollow on the website; the TV follows along.
    web.htmx_post("/subscriptions/favorite", &[("source", "yt"), ("remote_id", CHANNEL_ID)]).await;
    assert!(tv.pull().await.subscriptions[0].row.favorite);
    let r = web.htmx_post("/subscriptions/unfollow", &[("source", "yt"), ("remote_id", CHANNEL_ID)]).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(tv.pull().await.subscriptions[0].row.deleted_at.is_some());
    assert!(!web.get("/subscriptions?service=yt").await.body.contains("Linus Tech Tips"));

    // Following it again clears the tombstone and keeps the favorite.
    web.post("/subscriptions/add", &[("source", "yt"), ("channel", CHANNEL_ID)]).await;
    let back = tv.pull().await;
    assert!(back.subscriptions[0].row.deleted_at.is_none() && back.subscriptions[0].row.favorite);

    // Not a channel, an unknown one, and a service that is not added here.
    let r = web.htmx_post("/subscriptions/add", &[("source", "yt"), ("channel", "hello")]).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = web.htmx_post("/subscriptions/add", &[("source", "yt"), ("channel", "UCaaaaaaaaaaaaaaaaaaaaaa")]).await;
    assert!(r.body.contains("no such channel"), "{}", r.body);
    let r = web.htmx_post("/subscriptions/add", &[("source", "fp"), ("channel", CHANNEL_ID)]).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    // Floatplane follows come from the account: no unfollow here.
    let r = web.htmx_post("/subscriptions/unfollow", &[("source", "fp"), ("remote_id", "x")]).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    db.drop().await;
}

#[tokio::test]
async fn watch_later_and_history_on_the_website() {
    let Some(db) = Db::new().await else { return };
    let sam = db.person("sam@example.com", Role::Admin).await;
    let mut tv = db.device(sam, "tv").await;
    let now = now_ms();
    let mut req = tv.request(now);
    req.watch_state = vec![Change { base_rev: 0, row: progress("v1", 300_000, now) }];
    tv.sync(req).await;

    let mut web = Browser::logged_in(db.app(), "sam@example.com").await;
    let page = web.get("/history?service=yt").await;
    assert!(page.body.contains("Video v1") && page.body.contains("value=\"50\""), "{}", page.body);
    assert!(!web.get("/history?service=fp").await.body.contains("Video v1"), "services stay apart");
    web.htmx_post("/history/remove", &[("source", "yt"), ("remote_id", "v1")]).await;
    assert!(tv.pull().await.watch_state[0].row.deleted_at.is_some());
    assert!(!web.get("/history?service=yt").await.body.contains("Video v1"));
    db.drop().await;
}

#[tokio::test]
async fn a_device_ahead_of_a_restored_server_is_asked_to_resend() {
    let Some(db) = Db::new().await else { return };
    let sam = db.person("sam@example.com", Role::Admin).await;
    let mut phone = db.device(sam, "phone").await;
    let now = now_ms();
    let mut req = phone.request(now);
    req.subscriptions = vec![Change { base_rev: 0, row: sub("UC1", "One", now, "phone") }];
    let first = phone.sync(req).await;

    // The server loses that row (an older backup): the sequence is behind.
    sqlx::query("DELETE FROM subscriptions").execute(&db.pool).await.unwrap();
    sqlx::query("SELECT setval('sync_rev', 1, false)").execute(&db.pool).await.unwrap();
    let r = phone.sync(phone.request(now)).await;
    assert!(r.reset && r.cursor == 0, "{r:?}");

    // The device resends everything with base rev 0 and the server has it again.
    let mut req = phone.request(now);
    req.subscriptions = vec![Change { base_rev: 0, row: first.subscriptions[0].row.clone() }];
    let r = phone.sync(req).await;
    assert!(!r.reset);
    assert_eq!(r.subscriptions[0].row.name, "One");
    db.drop().await;
}
