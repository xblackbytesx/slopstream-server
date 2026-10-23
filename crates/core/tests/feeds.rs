//! The feed rules against the app's own fixtures (`source-youtube` and
//! `source-floatplane` test resources), ported from `RssFeedFetcherTest` and
//! `FloatplaneSourceTest`: both sides must build the same list.

use std::collections::HashMap;

use slopstream_core::feeds::floatplane::{self, Creator, Delivery, Post, SubChannel};
use slopstream_core::feeds::youtube::{self, Upload};
use slopstream_core::feeds::{SHORT, VIDEO, parse_time};

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

const CHANNEL: &str = "UCaaaaaaaaaaaaaaaaaaaaaa";

#[test]
fn kinds_split_by_playlist_newest_first() {
    let feed = youtube::merge(
        CHANNEL,
        Some(youtube::parse(&fixture("uulf.xml")).unwrap()),
        Some(youtube::parse(&fixture("uush.xml")).unwrap()),
        None,
    )
    .unwrap();
    assert_eq!(feed.channel_name.as_deref(), Some("Fixture Channel"));
    let ids: Vec<&str> = feed.items.iter().map(|i| i.remote_id.as_str()).collect();
    assert_eq!(ids, ["shtAAAAAAA1", "vidAAAAAAA1", "vidAAAAAAA2"]);
    let by: HashMap<&str, _> = feed.items.iter().map(|i| (i.remote_id.as_str(), i)).collect();
    assert_eq!(by["shtAAAAAAA1"].kind, SHORT);
    assert_eq!(by["vidAAAAAAA1"].kind, VIDEO);
    assert_eq!(by["vidAAAAAAA1"].title, "Long video one & more");
    assert_eq!(by["vidAAAAAAA1"].published_at, parse_time("2026-10-20T15:00:05Z").unwrap());
    assert_eq!(by["vidAAAAAAA1"].thumbnail_url.as_deref(), Some("https://i.ytimg.com/vi/vidAAAAAAA1/hq720.jpg"));
    assert_eq!(by["shtAAAAAAA1"].thumbnail_url.as_deref(), Some("https://i.ytimg.com/vi/shtAAAAAAA1/oar2.jpg"));
    assert_eq!(by["vidAAAAAAA2"].kind, SHORT, "an id in both Shorts and Videos stays a Short");
    assert_eq!(by["vidAAAAAAA1"].channel_id, CHANNEL);
}

#[test]
fn without_split_playlists_the_channel_feed_is_classified_by_link() {
    assert!(youtube::merge(CHANNEL, None, None, None).is_none());
    let feed = youtube::from_channel_feed(CHANNEL, youtube::parse(&fixture("channel.xml")).unwrap());
    let kind = |id: &str| feed.items.iter().find(|i| i.remote_id == id).unwrap().kind.clone();
    assert_eq!(kind("shtAAAAAAA1"), SHORT);
    assert_eq!(kind("vidAAAAAAA1"), VIDEO);
    assert!(youtube::playlist_path(CHANNEL, Upload::Live).unwrap().ends_with("UULVaaaaaaaaaaaaaaaaaaaaaa"));
}

#[test]
fn floatplane_keeps_playable_video_posts_named_by_sub_channel() {
    let creator: Creator = serde_json::from_str(&fixture("creator.json")).unwrap();
    let posts: Vec<Post> = serde_json::from_str(&fixture("posts.json")).unwrap();
    assert!(floatplane::needs_channel_titles(&posts));
    let titles: HashMap<String, String> = serde_json::from_str::<Vec<SubChannel>>(&fixture("channels.json"))
        .unwrap()
        .into_iter()
        .map(|c| (c.id, c.title))
        .collect();
    let items = floatplane::feed_items("fp", &creator, &posts, &titles);
    let ids: Vec<&str> = items.iter().map(|i| i.remote_id.as_str()).collect();
    assert_eq!(ids, ["postSubChan", "postSingle1"], "locked and text posts are left out");
    let sub = &items[0];
    assert_eq!((sub.channel_name.as_str(), sub.channel_id.as_str()), ("Sub Channel", "creatorA"));
    assert_eq!(sub.thumbnail_url, None);
    let single = &items[1];
    assert_eq!(single.channel_name, "Main Channel");
    assert_eq!(single.duration_sec, Some(755));
    assert_eq!(single.thumbnail_url.as_deref(), Some("https://pbs.example/thumb_1280.jpeg"));
    assert_eq!(single.published_at, parse_time("2026-10-20T16:00:00Z").unwrap());
}

#[test]
fn floatplane_live() {
    let creator: Creator = serde_json::from_str(&fixture("creator.json")).unwrap();
    let delivery: Delivery = serde_json::from_str(&fixture("delivery_live.json")).unwrap();
    assert_eq!(
        floatplane::live_url(&delivery).as_deref(),
        Some("https://live.example/api/video/v1/channel.m3u8?allow_source=false&token=TOKEN")
    );
    let item = floatplane::live_item("fp", &creator, creator.live_stream.as_ref().unwrap(), 42);
    assert_eq!(
        (item.remote_id.as_str(), item.title.as_str(), item.kind.as_str()),
        ("live:creatorA", "Weekly show", "LIVE")
    );
    assert_eq!(item.published_at, 42);
}
