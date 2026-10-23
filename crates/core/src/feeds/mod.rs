//! A channel's recent uploads, from what YouTube and Floatplane's platform
//! answer, as the app's `feed_item` rows. The same rules as the app's
//! `RssFeedFetcher` and `FloatplaneMapping`, so a list fetched here and one
//! fetched on a device are the same list.

pub mod floatplane;
pub mod youtube;

use serde::{Deserialize, Serialize};

/// The app's `VideoKind` names.
pub const VIDEO: &str = "VIDEO";
pub const SHORT: &str = "SHORT";
pub const LIVE: &str = "LIVE";

/// Newest items kept per channel and kind, as the app's `KEEP_PER_KIND`.
pub const KEEP_PER_KIND: i64 = 200;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedItem {
    pub source: String,
    pub remote_id: String,
    pub channel_id: String,
    pub channel_name: String,
    pub title: String,
    /// Epoch milliseconds.
    pub published_at: i64,
    pub kind: String,
    pub thumbnail_url: Option<String>,
    pub duration_sec: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelFeed {
    pub channel_name: Option<String>,
    /// Newest first.
    pub items: Vec<FeedItem>,
    /// Kinds this answer lists completely: a cached item of such a kind that is
    /// missing here is gone (an ended livestream). Other kinds only ever add.
    pub complete_kinds: Vec<&'static str>,
}

/// RFC 3339 to epoch milliseconds.
pub fn parse_time(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s.trim()).ok().map(|t| t.timestamp_millis())
}

#[cfg(test)]
mod tests {
    #[test]
    fn times() {
        assert_eq!(super::parse_time("2026-10-20T15:00:05+00:00"), Some(1_792_508_405_000));
        assert_eq!(super::parse_time("2026-10-20T16:00:00.000Z"), Some(1_792_512_000_000));
        assert_eq!(super::parse_time("yesterday"), None);
    }
}
