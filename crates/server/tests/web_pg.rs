//! Accounts, the web guard and every page. Needs
//! `SLOPSTREAM_TEST_DATABASE_URL`; skipped when unset.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{Browser, Db, PASSWORD};
use slopstream_server::auth::Role;

#[tokio::test]
async fn login_logout_and_security_headers() {
    let Some(db) = Db::new().await else { return };
    db.person("admin@example.com", Role::Admin).await;
    let mut b = Browser::logged_in(db.app(), "admin@example.com").await;
    let r = b.get("/settings").await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.contains("admin@example.com"));
    let csp = r.headers.get("content-security-policy").unwrap().to_str().unwrap();
    assert!(csp.contains("script-src 'self'") && !csp.contains("unsafe"), "{csp}");
    assert_eq!(r.headers.get("referrer-policy").unwrap(), "same-origin");

    let js = b.get("/static/htmx.min.js").await;
    let etag = js.headers.get("etag").unwrap().to_str().unwrap().to_string();
    let r304 = b.send(Request::get("/static/htmx.min.js").header("if-none-match", &etag), Body::empty(), None).await;
    assert_eq!(r304.status, StatusCode::NOT_MODIFIED);

    let r = b.post("/logout", &[]).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert!(b.cookie.is_none());
    let r = b.get("/subscriptions").await;
    assert_eq!(r.headers.get("location").unwrap(), "/login");
    let r = b.send(Request::get("/settings").header("hx-request", "true"), Body::empty(), None).await;
    assert_eq!(r.headers.get("hx-redirect").unwrap(), "/login");

    let r = b.post("/login", &[("email", "admin@example.com"), ("password", "wrong password!")]).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = b.post("/login", &[("email", "nobody@example.com"), ("password", PASSWORD)]).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    db.drop().await;
}

#[tokio::test]
async fn cross_site_writes_are_refused() {
    let Some(db) = Db::new().await else { return };
    db.person("admin@example.com", Role::Admin).await;
    let mut b = Browser::logged_in(db.app(), "admin@example.com").await;
    for path in ["/admin/invites", "/subscriptions/add", "/services/session"] {
        let r = b
            .send(
                Request::post(path).header("content-type", "application/x-www-form-urlencoded"),
                Body::from("role=admin&note=x&source=yt&channel=x&session=x"),
                Some("https://evil.example"),
            )
            .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{path}");
    }
    let open: i64 = sqlx::query_scalar("SELECT count(*) FROM invites").fetch_one(&db.pool).await.unwrap();
    assert_eq!(open, 0);
    db.drop().await;
}

#[tokio::test]
async fn invites_are_single_use_and_roles_are_enforced() {
    let Some(db) = Db::new().await else { return };
    db.person("admin@example.com", Role::Admin).await;
    let mut admin = Browser::logged_in(db.app(), "admin@example.com").await;
    let r = admin.htmx_post("/admin/invites", &[("role", "user"), ("note", "Kim")]).await;
    let link = r.body.split("<code id=\"invite-link\">").nth(1).unwrap().split('<').next().unwrap().to_string();
    assert!(link.starts_with("http://test.local/invite/ssi_"), "{link}");
    let path = link.trim_start_matches("http://test.local");

    let mut kim = Browser::new(db.app());
    let form = [
        ("email", "kim@example.com"),
        ("name", "Kim"),
        ("password", "a long password"),
        ("password2", "a long password"),
    ];
    let r = kim.post(path, &form).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.body);
    assert_eq!(kim.get("/subscriptions").await.status, StatusCode::OK);
    let mut eve = Browser::new(db.app());
    assert_eq!(eve.get(path).await.status, StatusCode::NOT_FOUND);
    assert_eq!(kim.get("/admin/users").await.status, StatusCode::FORBIDDEN);

    let kim_id: uuid::Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE email = 'kim@example.com'").fetch_one(&db.pool).await.unwrap();
    admin.post(&format!("/admin/users/{kim_id}/disable"), &[]).await;
    assert_eq!(kim.get("/subscriptions").await.status, StatusCode::SEE_OTHER);
    db.drop().await;
}

/// Every page renders for a person with data in it, with no inline script
/// or style (the CSP would block them silently).
#[tokio::test]
async fn every_page_renders_under_the_csp() {
    let Some(db) = Db::new().await else { return };
    let sam = db.person("admin@example.com", Role::Admin).await;
    let phone = db.device(sam, "Pixel").await;
    let _: serde_json::Value = phone
        .json(
            "PUT",
            "/api/v1/sessions/fp",
            &slopstream_core::wire::SessionPut { value: "good-session-aaaaaaaaaaaa".into(), username: None },
        )
        .await;
    let mut b = Browser::logged_in(db.app(), "admin@example.com").await;
    b.post("/subscriptions/add", &[("source", "yt"), ("channel", "@LinusTechTips")]).await;
    let mut pages = vec!["/login".to_string(), "/settings".into(), "/devices".into(), "/pair".into()];
    pages.extend(["/admin/users".into(), "/services".into()]);
    for svc in ["yt", "fp", "sp"] {
        for p in ["/subscriptions", "/watch-later", "/history"] {
            pages.push(format!("{p}?service={svc}"));
        }
    }
    for p in &pages {
        let r = b.get(p).await;
        assert_eq!(r.status, StatusCode::OK, "{p}: {}", r.body);
        assert!(!r.body.contains("style=\"") && !r.body.contains("onclick"), "{p} has inline style or script");
        assert!(!r.body.contains("good-session"), "{p} shows a session value");
    }
    assert!(b.get("/subscriptions?service=yt").await.body.contains("Linus Tech Tips"));
    assert!(b.get("/services").await.body.contains("sam_fp"));
    assert_eq!(b.get("/").await.headers.get("location").unwrap(), "/subscriptions");
    db.drop().await;
}
