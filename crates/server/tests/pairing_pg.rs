//! Pairing a TV with a code typed on the website. Needs
//! `SLOPSTREAM_TEST_DATABASE_URL`; skipped when unset.

mod common;

use axum::http::StatusCode;
use common::{Browser, Db, anon};
use slopstream_core::wire::{PairPoll, PairResult, PairStart, PairStarted, PairStatus, Person};
use slopstream_server::auth::Role;

async fn start(db: &Db) -> PairStarted {
    let (status, body) =
        anon(&db.app(), "/api/v1/pair/start", &PairStart { device_name: "Living room Shield".into() }).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    serde_json::from_str(&body).unwrap()
}

async fn poll(db: &Db, code: &str) -> PairResult {
    let (status, body) = anon(&db.app(), "/api/v1/pair/poll", &PairPoll { device_code: code.into() }).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    serde_json::from_str(&body).unwrap()
}

#[tokio::test]
async fn a_tv_pairs_with_the_code_it_shows() {
    let Some(db) = Db::new().await else { return };
    db.person("sam@example.com", Role::Admin).await;
    let started = start(&db).await;
    assert_eq!(started.verification_url, "http://test.local/pair");
    assert_eq!(poll(&db, &started.device_code).await.status, PairStatus::Pending);

    let mut web = Browser::logged_in(db.app(), "sam@example.com").await;
    // Typed in lower case without the dash: still found.
    let typed = started.user_code.replace('-', "").to_lowercase();
    let r = web.post("/pair", &[("code", &typed)]).await;
    assert!(r.body.contains("Living room Shield"), "{}", r.body);
    let id = r.body.split("/pair/").nth(1).unwrap().split('/').next().unwrap().to_string();
    let r = web.post(&format!("/pair/{id}/approve"), &[]).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);

    let done = poll(&db, &started.device_code).await;
    assert_eq!(done.status, PairStatus::Approved);
    let token = done.token.unwrap();
    assert_eq!(done.person.unwrap().email, "sam@example.com");
    // Delivered once.
    let again = poll(&db, &started.device_code).await;
    assert_eq!((again.status, again.token), (PairStatus::Expired, None));

    let tv = common::Device { app: db.app(), token, id: "tv".into(), cursor: 0, fingerprints: Default::default() };
    let (status, body) = tv.call::<()>("GET", "/api/v1/me", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(serde_json::from_str::<Person>(&body).unwrap().email, "sam@example.com");
    let devices = web.get("/devices").await;
    assert!(devices.body.contains("Living room Shield"));
    // The code cannot be used a second time.
    assert!(web.post("/pair", &[("code", &started.user_code)]).await.body.contains("No device is waiting"));
    db.drop().await;
}

#[tokio::test]
async fn denied_and_guessed_codes() {
    let Some(db) = Db::new().await else { return };
    db.person("sam@example.com", Role::Admin).await;
    let started = start(&db).await;
    let mut web = Browser::logged_in(db.app(), "sam@example.com").await;
    let r = web.post("/pair", &[("code", &started.user_code)]).await;
    let id = r.body.split("/pair/").nth(1).unwrap().split('/').next().unwrap().to_string();
    web.post(&format!("/pair/{id}/deny"), &[]).await;
    assert_eq!(poll(&db, &started.device_code).await.status, PairStatus::Denied);
    assert_eq!(poll(&db, "ssd_unknown").await.status, PairStatus::Expired);

    // Five wrong codes, then even the right one is refused in this session.
    let other = start(&db).await;
    for _ in 0..5 {
        assert!(web.post("/pair", &[("code", "ZZZZ-ZZZZ")]).await.body.contains("No device is waiting"));
    }
    let r = web.post("/pair", &[("code", &other.user_code)]).await;
    assert!(r.body.contains("Too many wrong codes"), "{}", r.body);
    // A fresh login can try again.
    let mut again = Browser::logged_in(db.app(), "sam@example.com").await;
    assert!(again.post("/pair", &[("code", &other.user_code)]).await.body.contains("Living room Shield"));
    db.drop().await;
}

#[tokio::test]
async fn a_revoked_device_stops_syncing() {
    let Some(db) = Db::new().await else { return };
    let sam = db.person("sam@example.com", Role::Admin).await;
    let tv = db.device(sam, "tv").await;
    let mut web = Browser::logged_in(db.app(), "sam@example.com").await;
    let id: uuid::Uuid = sqlx::query_scalar("SELECT id FROM api_tokens").fetch_one(&db.pool).await.unwrap();
    web.htmx_post(&format!("/devices/tokens/{id}/revoke"), &[]).await;
    let (status, _) = tv.call::<()>("GET", "/api/v1/me", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // A pasted token works like a paired one.
    let r = web.htmx_post("/devices/tokens", &[("label", "Pixel")]).await;
    let token = r.body.split("<code id=\"new-token-value\">").nth(1).unwrap().split('<').next().unwrap().to_string();
    assert!(token.starts_with("ss_"));
    let phone = common::Device { app: db.app(), token, id: "p".into(), cursor: 0, fingerprints: Default::default() };
    assert_eq!(phone.call::<()>("GET", "/api/v1/me", None).await.0, StatusCode::OK);
    db.drop().await;
}
