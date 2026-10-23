//! Floatplane's platform (Floatplane, Sauce+): a creator's video posts as feed
//! items, and its livestream while it is on. Same rules as the app's
//! `FloatplaneMapping`: only video posts the account's plan includes (a locked
//! tile is a dead end); one item per creator stream, `live:<creatorId>`.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::Value;

use super::{FeedItem, LIVE, VIDEO, parse_time};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Image {
    #[serde(default)]
    pub width: i64,
    pub path: String,
    #[serde(default)]
    pub child_images: Option<Vec<Image>>,
}

impl Image {
    /// The smallest rendition at least `min_width` wide, else the largest.
    pub fn pick(&self, min_width: i64) -> String {
        let mut all = vec![self];
        all.extend(self.child_images.iter().flatten());
        all.iter()
            .filter(|i| i.width >= min_width)
            .min_by_key(|i| i.width)
            .or_else(|| all.iter().max_by_key(|i| i.width))
            .map(|i| i.path.clone())
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveStream {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub thumbnail: Option<Image>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Creator {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub live_stream: Option<LiveStream>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Metadata {
    #[serde(default)]
    pub has_video: bool,
    #[serde(default)]
    pub video_duration: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Post {
    pub id: String,
    pub title: String,
    /// A channel object or a bare id, depending on the endpoint.
    #[serde(default)]
    pub channel: Value,
    #[serde(default)]
    pub metadata: Metadata,
    pub release_date: String,
    #[serde(default)]
    pub thumbnail: Option<Image>,
    #[serde(default = "yes")]
    pub is_accessible: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct SubChannel {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Origin {
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Variant {
    #[serde(default)]
    pub url: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub origins: Option<Vec<Origin>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Group {
    #[serde(default)]
    pub origins: Vec<Origin>,
    #[serde(default)]
    pub variants: Vec<Variant>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Delivery {
    #[serde(default)]
    pub groups: Vec<Group>,
}

fn channel_id_of(p: &Post) -> Option<&str> {
    match &p.channel {
        Value::Object(o) => o.get("id").and_then(Value::as_str),
        Value::String(s) => Some(s),
        _ => None,
    }
}

fn channel_title_of(p: &Post) -> Option<&str> {
    p.channel.as_object().and_then(|o| o.get("title")).and_then(Value::as_str)
}

/// Some post names its sub-channel by id alone: the channel list is needed.
pub fn needs_channel_titles(posts: &[Post]) -> bool {
    posts.iter().any(|p| !p.channel.is_object() && channel_id_of(p).is_some())
}

/// Video posts the account can play, newest first. `titles`: sub-channel id
/// to title (only needed per [`needs_channel_titles`]).
pub fn feed_items(source: &str, creator: &Creator, posts: &[Post], titles: &HashMap<String, String>) -> Vec<FeedItem> {
    let mut items: Vec<FeedItem> = posts
        .iter()
        .filter(|p| p.metadata.has_video && p.is_accessible)
        .filter_map(|p| {
            Some(FeedItem {
                source: source.to_string(),
                remote_id: p.id.clone(),
                channel_id: creator.id.clone(),
                channel_name: channel_title_of(p)
                    .map(String::from)
                    .or_else(|| channel_id_of(p).and_then(|id| titles.get(id).cloned()))
                    .unwrap_or_else(|| creator.title.clone()),
                title: p.title.clone(),
                published_at: parse_time(&p.release_date)?,
                kind: VIDEO.into(),
                thumbnail_url: p.thumbnail.as_ref().map(|t| t.pick(640)),
                duration_sec: Some(p.metadata.video_duration.round() as i64).filter(|d| *d > 0),
            })
        })
        .collect();
    items.sort_by(|a, b| b.published_at.cmp(&a.published_at));
    items
}

pub const LIVE_PREFIX: &str = "live:";

pub fn live_id(creator_id: &str) -> String {
    format!("{LIVE_PREFIX}{creator_id}")
}

/// The livestream's playlist URL (to check it answers 200: it is 404 while
/// the creator is offline).
pub fn live_url(d: &Delivery) -> Option<String> {
    d.groups.iter().find_map(|g| {
        g.variants.iter().find(|v| v.enabled && !v.url.is_empty()).and_then(|v| {
            let origin = v.origins.as_ref().unwrap_or(&g.origins).first()?;
            Some(format!("{}{}", origin.url.trim_end_matches('/'), v.url))
        })
    })
}

/// The stream on air, dated `now_ms` so it leads the feed while it lasts.
pub fn live_item(source: &str, creator: &Creator, stream: &LiveStream, now_ms: i64) -> FeedItem {
    FeedItem {
        source: source.to_string(),
        remote_id: live_id(&creator.id),
        channel_id: creator.id.clone(),
        channel_name: creator.title.clone(),
        title: if stream.title.trim().is_empty() { format!("{} is live", creator.title) } else { stream.title.clone() },
        published_at: now_ms,
        kind: LIVE.into(),
        thumbnail_url: stream.thumbnail.as_ref().map(|t| t.pick(640)),
        duration_sec: None,
    }
}
