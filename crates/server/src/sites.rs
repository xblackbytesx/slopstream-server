//! The few calls the server makes to Floatplane's platform (Floatplane and
//! Sauce+): is this session still signed in, and sign it out. Never feeds,
//! never streams. The session value goes only into the Cookie header of
//! requests to the service's own host; errors are logged without URLs.

use std::time::Duration;

/// Same identity as the app (`Floatplane.USER_AGENT`).
const USER_AGENT: &str = concat!("Slopstream-server/", env!("CARGO_PKG_VERSION"));
const MAX_BODY: usize = 256 * 1024;

pub fn client() -> reqwest::Client {
    // reqwest is built without a default crypto provider (aws-lc needs cmake
    // everywhere); ring is installed once, here, before any client exists.
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(20))
        // A redirect could carry the Cookie header to another host.
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(USER_AGENT)
        .build()
        .expect("reqwest client")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    Valid {
        username: String,
    },
    /// The service said so explicitly (401, or 403 `notLoggedInError`).
    Rejected,
    /// Anything else: a challenge page, a network error, an odd answer.
    /// Never treated as signed out.
    Unknown(String),
}

/// `GET /api/v3/user/self` with the session. `base`: the site's base URL
/// (`AppState::site_base`), `cookie`: its session cookie's name.
pub async fn check(http: &reqwest::Client, base: &str, cookie: &str, value: &str) -> Check {
    let resp = http
        .get(format!("{base}/api/v3/user/self"))
        .header("Cookie", format!("{cookie}={value}"))
        .header("Accept", "application/json")
        .send()
        .await;
    let resp = match resp {
        Ok(r) => r,
        Err(e) => return Check::Unknown(format!("could not reach the service: {}", e.without_url())),
    };
    let status = resp.status().as_u16();
    let body = read(resp).await;
    classify(status, &body)
}

pub fn classify(status: u16, body: &str) -> Check {
    match status {
        200 => match serde_json::from_str::<serde_json::Value>(body) {
            Ok(v) => match v.get("username").and_then(|u| u.as_str()) {
                Some(u) => Check::Valid { username: u.to_string() },
                None => Check::Unknown("the service answered without a username".into()),
            },
            Err(_) => Check::Unknown("the service did not answer with JSON".into()),
        },
        401 => Check::Rejected,
        403 if error_names(body).iter().any(|n| n == "notLoggedInError") => Check::Rejected,
        s => Check::Unknown(format!("the service answered HTTP {s}")),
    }
}

/// `errors[].name` of a Floatplane error body.
fn error_names(body: &str) -> Vec<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("errors").and_then(|e| e.as_array()).cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|e| e.get("name").and_then(|n| n.as_str()).map(String::from))
        .collect()
}

/// `POST /api/v3/auth/logout`: ends the session at the service, for every
/// device at once. Best effort: the caller forgets the session either way.
pub async fn logout(http: &reqwest::Client, base: &str, cookie: &str, value: &str) {
    let r = http
        .post(format!("{base}/api/v3/auth/logout"))
        .header("Cookie", format!("{cookie}={value}"))
        .header("Content-Length", "0")
        .send()
        .await;
    match r {
        Ok(r) => tracing::info!(status = r.status().as_u16(), base, "signed out at the service"),
        Err(e) => tracing::warn!(error = %e.without_url(), base, "sign-out at the service failed"),
    }
}

async fn read(mut resp: reqwest::Response) -> String {
    let mut out = Vec::new();
    while let Ok(Some(chunk)) = resp.chunk().await {
        if out.len() + chunk.len() > MAX_BODY {
            break;
        }
        out.extend_from_slice(&chunk);
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_explicit_refusal_is_signed_out() {
        assert_eq!(classify(200, r#"{"id":"1","username":"sam"}"#), Check::Valid { username: "sam".into() });
        assert_eq!(classify(401, ""), Check::Rejected);
        let refused = r#"{"id":"x","errors":[{"id":"y","name":"notLoggedInError","message":"..."}]}"#;
        assert_eq!(classify(403, refused), Check::Rejected);
        // Cloudflare's challenge is a 403 too, with HTML.
        assert!(matches!(classify(403, "<html>Just a moment...</html>"), Check::Unknown(_)));
        assert!(matches!(classify(403, r#"{"errors":[{"name":"notSubscribedError"}]}"#), Check::Unknown(_)));
        assert!(matches!(classify(500, ""), Check::Unknown(_)));
        assert!(matches!(classify(200, "{}"), Check::Unknown(_)));
    }
}
