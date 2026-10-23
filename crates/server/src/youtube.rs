//! Following a YouTube channel from the website: turn what was typed into a
//! channel id, name and avatar. One page fetch for a handle or URL, then the
//! uploads feed, which proves the channel exists. Feeds are never refreshed
//! here; the devices do that.

use std::time::Duration;

use slopstream_core::channel::{self, ChannelRef};

use crate::error::ApiError;

pub const BASE: &str = "https://www.youtube.com";
const MAX_BODY: usize = 4 * 1024 * 1024;
/// YouTube's consent wall for visitors without cookies (EU) is skipped the
/// way NewPipe does it.
const CONSENT: &str = "SOCS=CAE=";

pub fn client() -> reqwest::Client {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(20))
        // /c/Name and /user/Name redirect to /@handle; never off the host.
        .redirect(reqwest::redirect::Policy::custom(|a| {
            let same = a.previous().first().map(|u| u.host_str()) == Some(a.url().host_str());
            if same && a.previous().len() < 5 { a.follow() } else { a.stop() }
        }))
        .user_agent("Mozilla/5.0 (X11; Linux x86_64; rv:140.0) Gecko/20100101 Firefox/140.0")
        .build()
        .expect("reqwest client")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Channel {
    pub id: String,
    pub name: String,
    pub avatar_url: Option<String>,
}

/// `base`: www.youtube.com, or a stand-in in tests.
pub async fn resolve(http: &reqwest::Client, base: &str, input: &str) -> Result<Channel, ApiError> {
    let reference = channel::parse(input)
        .ok_or_else(|| ApiError::bad("that is not a YouTube channel: paste its link, its @handle or its UC… id"))?;
    let (id, avatar_url) = match reference {
        ChannelRef::Id(id) => (id, None),
        ChannelRef::Page(path) => {
            let html = fetch(http, base, &path).await?.ok_or_else(|| ApiError::bad("YouTube has no such channel"))?;
            let id = channel::id_from_page(&html)
                .ok_or_else(|| ApiError::Upstream("YouTube's page did not name the channel; try its UC… id".into()))?;
            (id, channel::avatar_from_page(&html))
        }
    };
    let feed = fetch(http, base, &channel::feed_path(&id))
        .await?
        .ok_or_else(|| ApiError::bad("YouTube has no such channel"))?;
    let name = channel::name_from_feed(&feed).unwrap_or_else(|| id.clone());
    Ok(Channel { id, name, avatar_url })
}

/// The body of a 200, None for a 404, an error otherwise.
async fn fetch(http: &reqwest::Client, base: &str, path: &str) -> Result<Option<String>, ApiError> {
    let resp = http
        .get(format!("{base}{path}"))
        .header("Cookie", CONSENT)
        .header("Accept-Language", "en")
        .send()
        .await
        .map_err(|e| ApiError::Upstream(format!("could not reach YouTube: {}", e.without_url())))?;
    match resp.status().as_u16() {
        200 => {}
        404 => return Ok(None),
        s => return Err(ApiError::Upstream(format!("YouTube answered HTTP {s}; try again later"))),
    }
    let mut resp = resp;
    let mut out = Vec::new();
    while let Some(chunk) =
        resp.chunk().await.map_err(|e| ApiError::Upstream(format!("YouTube stopped answering: {}", e.without_url())))?
    {
        if out.len() + chunk.len() > MAX_BODY {
            break;
        }
        out.extend_from_slice(&chunk);
    }
    Ok(Some(String::from_utf8_lossy(&out).into_owned()))
}
