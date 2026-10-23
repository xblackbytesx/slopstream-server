//! Service sessions: per person, sent only where a device lacks the current
//! one, renewed once for every device, never expired by a stale report, and
//! never shown on the website. Needs `SLOPSTREAM_TEST_DATABASE_URL`.

mod common;

use axum::http::StatusCode;
use common::{Browser, Db};
use slopstream_core::fingerprint;
use slopstream_core::wire::{ExpiredReport, SessionPut, SessionStatus};
use slopstream_server::auth::Role;

const GOOD: &str = "good-session-aaaaaaaaaaaa";
const RENEWED: &str = "good-session-bbbbbbbbbbbb";

fn put(value: &str) -> SessionPut {
    SessionPut { value: value.into(), username: None }
}

#[tokio::test]
async fn one_sign_in_reaches_every_device_of_that_person_only() {
    let Some(db) = Db::new().await else { return };
    let sam = db.person("sam@example.com", Role::Admin).await;
    let kim = db.person("kim@example.com", Role::User).await;
    let phone = db.device(sam, "phone").await;
    let mut tv = db.device(sam, "tv").await;
    let mut kims = db.device(kim, "kim").await;

    let r: serde_json::Value = phone.json("PUT", "/api/v1/sessions/fp", &put(GOOD)).await;
    assert_eq!(r["status"], "ok");

    let got = tv.pull().await;
    assert_eq!(got.sessions.len(), 1);
    let s = &got.sessions[0];
    assert_eq!((s.source.as_str(), s.status, s.value.as_deref()), ("fp", SessionStatus::Ok, Some(GOOD)));
    assert_eq!(s.username.as_deref(), Some("sam_fp"));
    assert_eq!(s.fingerprint, fingerprint(GOOD));
    // The TV now holds it: the value is not sent again.
    assert_eq!(tv.pull().await.sessions[0].value, None);
    assert!(kims.pull().await.sessions.is_empty(), "never another person's");

    let stored: Vec<u8> =
        sqlx::query_scalar("SELECT ciphertext FROM service_sessions").fetch_one(&db.pool).await.unwrap();
    assert!(!String::from_utf8_lossy(&stored).contains(GOOD), "encrypted at rest");
    db.drop().await;
}

#[tokio::test]
async fn refused_and_unverifiable_sessions() {
    let Some(db) = Db::new().await else { return };
    let sam = db.person("sam@example.com", Role::Admin).await;
    let phone = db.device(sam, "phone").await;
    let (status, body) = phone.call("PUT", "/api/v1/sessions/fp", Some(&put("bad-session-aaaaaaaaaaa"))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("signed out"));
    let (status, _) = phone.call("PUT", "/api/v1/sessions/fp", Some(&put("short"))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = phone.call("PUT", "/api/v1/sessions/yt", Some(&put(GOOD))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "YouTube has no sign-in");

    // A challenge page: stored, marked unchecked, with the device's username.
    let r: serde_json::Value = phone
        .json(
            "PUT",
            "/api/v1/sessions/sp",
            &SessionPut { value: "challenge-aaaaaaaaaaaa".into(), username: Some("sam_sp".into()) },
        )
        .await;
    assert_eq!(r["status"], "unchecked");
    db.drop().await;
}

#[tokio::test]
async fn expiry_needs_the_current_fingerprint_and_renewal_clears_it() {
    let Some(db) = Db::new().await else { return };
    let sam = db.person("sam@example.com", Role::Admin).await;
    let mut phone = db.device(sam, "phone").await;
    let mut tv = db.device(sam, "tv").await;
    let _: serde_json::Value = phone.json("PUT", "/api/v1/sessions/fp", &put(GOOD)).await;
    tv.pull().await;

    // The phone renews; the TV, still holding the old one, reports it expired.
    let _: serde_json::Value = phone.json("PUT", "/api/v1/sessions/fp", &put(RENEWED)).await;
    db.site.revoke(GOOD);
    let r: serde_json::Value =
        tv.json("POST", "/api/v1/sessions/fp/expired", &ExpiredReport { fingerprint: fingerprint(GOOD) }).await;
    assert_eq!(r["counted"], false, "a stale copy never expires the renewed session");
    let got = tv.pull().await;
    assert_eq!((got.sessions[0].status, got.sessions[0].value.as_deref()), (SessionStatus::Ok, Some(RENEWED)));

    // A report about a session the service still accepts does not count either.
    let r: serde_json::Value =
        tv.json("POST", "/api/v1/sessions/fp/expired", &ExpiredReport { fingerprint: fingerprint(RENEWED) }).await;
    assert_eq!(r["counted"], false);

    // Now it really ends: every device sees "expired".
    db.site.revoke(RENEWED);
    let r: serde_json::Value =
        tv.json("POST", "/api/v1/sessions/fp/expired", &ExpiredReport { fingerprint: fingerprint(RENEWED) }).await;
    assert_eq!(r["counted"], true);
    assert_eq!(phone.pull().await.sessions[0].status, SessionStatus::Expired);

    // A paste on the website renews it for both.
    let mut web = Browser::logged_in(db.app(), "sam@example.com").await;
    let fresh = "good-session-cccccccccccc";
    let r = web.post("/services/session", &[("source", "fp"), ("session", &format!("sails.sid={fresh}"))]).await;
    assert_eq!(r.status, axum::http::StatusCode::SEE_OTHER, "{}", r.body);
    let got = tv.pull().await;
    assert_eq!((got.sessions[0].status, got.sessions[0].value.as_deref()), (SessionStatus::Ok, Some(fresh)));
    assert!(!web.get("/services").await.body.contains(fresh), "write-only on the website");
    db.drop().await;
}

#[tokio::test]
async fn the_checker_marks_only_explicit_refusals() {
    let Some(db) = Db::new().await else { return };
    let sam = db.person("sam@example.com", Role::Admin).await;
    let mut phone = db.device(sam, "phone").await;
    let _: serde_json::Value = phone.json("PUT", "/api/v1/sessions/fp", &put(GOOD)).await;
    let _: serde_json::Value = phone
        .json("PUT", "/api/v1/sessions/sp", &SessionPut { value: "challenge-aaaaaaaaaaaa".into(), username: None })
        .await;
    db.site.revoke(GOOD);
    slopstream_server::sessions::check_all(&db.state()).await.unwrap();
    let got = phone.pull().await;
    let status = |k: &str| got.sessions.iter().find(|s| s.source == k).unwrap().status;
    assert_eq!(status("fp"), SessionStatus::Expired);
    assert_eq!(status("sp"), SessionStatus::Unchecked, "a challenge changes nothing");
    db.drop().await;
}

#[tokio::test]
async fn signing_out_ends_it_everywhere() {
    let Some(db) = Db::new().await else { return };
    let sam = db.person("sam@example.com", Role::Admin).await;
    let phone = db.device(sam, "phone").await;
    let mut tv = db.device(sam, "tv").await;
    let _: serde_json::Value = phone.json("PUT", "/api/v1/sessions/fp", &put(GOOD)).await;
    let _: serde_json::Value = phone.json("PUT", "/api/v1/sessions/sp", &put(RENEWED)).await;
    assert_eq!(tv.pull().await.sessions.len(), 2);

    // From a device that already signed out at the service itself.
    let (status, _) = phone.call::<()>("DELETE", "/api/v1/sessions/fp", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(db.site.logouts.lock().unwrap().is_empty());
    // From the website: the server signs out at the service.
    let mut web = Browser::logged_in(db.app(), "sam@example.com").await;
    web.post("/services/signout", &[("source", "sp")]).await;
    assert_eq!(*db.site.logouts.lock().unwrap(), vec![RENEWED.to_string()]);
    assert!(tv.pull().await.sessions.is_empty(), "the TV drops both");
    db.drop().await;
}
