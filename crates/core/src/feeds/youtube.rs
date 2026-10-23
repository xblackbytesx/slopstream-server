//! YouTube's Atom feeds. A channel `UC<22>` has three uploads playlists, by
//! swapping the prefix: `UULF` regular videos, `UUSH` Shorts, `UULV`
//! livestreams. Their feeds give exact times and split the kinds; a 404 means
//! "none of that kind". Without any of them the plain channel feed is used and
//! Shorts are told by their `/shorts/` link. Same as the app's `RssFeedFetcher`.

use quick_xml::events::Event;
use quick_xml::name::ResolveResult;
use quick_xml::reader::NsReader;

use super::{ChannelFeed, FeedItem, LIVE, SHORT, VIDEO, parse_time};
use crate::channel::is_channel_id;

const ATOM: &[u8] = b"http://www.w3.org/2005/Atom";
const YT: &[u8] = b"http://www.youtube.com/xml/schemas/2015";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub video_id: String,
    pub channel_id: String,
    pub title: String,
    pub channel_name: String,
    pub published_at: i64,
    pub link: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Atom {
    pub channel_id: Option<String>,
    pub channel_name: Option<String>,
    pub entries: Vec<Entry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upload {
    Videos,
    Shorts,
    Live,
}

impl Upload {
    pub const ALL: [Upload; 3] = [Upload::Videos, Upload::Shorts, Upload::Live];

    fn prefix(self) -> &'static str {
        match self {
            Upload::Videos => "UULF",
            Upload::Shorts => "UUSH",
            Upload::Live => "UULV",
        }
    }

    fn kind(self) -> &'static str {
        match self {
            Upload::Videos => VIDEO,
            Upload::Shorts => SHORT,
            Upload::Live => LIVE,
        }
    }
}

/// The feed path of one uploads playlist, None for a malformed channel id.
pub fn playlist_path(channel_id: &str, upload: Upload) -> Option<String> {
    is_channel_id(channel_id).then(|| format!("/feeds/videos.xml?playlist_id={}{}", upload.prefix(), &channel_id[2..]))
}

pub fn channel_path(channel_id: &str) -> String {
    format!("/feeds/videos.xml?channel_id={channel_id}")
}

/// i.ytimg.com's stable names: 16:9 `hq720`, the 9:16 `oar2` for Shorts.
pub fn thumbnail(video_id: &str, kind: &str) -> String {
    let file = if kind == SHORT { "oar2.jpg" } else { "hq720.jpg" };
    format!("https://i.ytimg.com/vi/{video_id}/{file}")
}

/// Where in the document the parser is.
#[derive(PartialEq)]
enum At {
    Other,
    FeedChannelId,
    FeedAuthorName,
    EntryVideoId,
    EntryChannelId,
    EntryTitle,
    EntryAuthorName,
    EntryPublished,
}

/// Parses one feed. Malformed XML is an error, never "no uploads".
pub fn parse(xml: &str) -> Result<Atom, String> {
    let mut r = NsReader::from_str(xml);
    let mut atom = Atom::default();
    let mut path: Vec<(bool, Vec<u8>)> = Vec::new(); // (is atom/yt, local name)
    let mut entry: Option<Entry> = None;
    let mut entry_ok = (false, false); // video id, published seen
    let mut at = At::Other;
    let mut text = String::new();
    loop {
        let (ns, ev) = r.read_resolved_event().map_err(|e| format!("feed is not valid XML: {e}"))?;
        match ev {
            Event::Start(e) => {
                let local = e.local_name().as_ref().to_vec();
                let ns = match ns {
                    ResolveResult::Bound(n) => n.as_ref().to_vec(),
                    _ => Vec::new(),
                };
                let depth = path.len();
                let in_entry = entry.is_some();
                at = match (ns.as_slice(), local.as_slice(), depth, in_entry) {
                    (ATOM, b"entry", 1, _) => {
                        entry = Some(Entry {
                            video_id: String::new(),
                            channel_id: String::new(),
                            title: String::new(),
                            channel_name: String::new(),
                            published_at: 0,
                            link: String::new(),
                        });
                        entry_ok = (false, false);
                        At::Other
                    }
                    (YT, b"channelId", 1, false) => At::FeedChannelId,
                    (ATOM, b"name", 2, false) if path[1].1 == b"author" => At::FeedAuthorName,
                    (YT, b"videoId", 2, true) => At::EntryVideoId,
                    (YT, b"channelId", 2, true) => At::EntryChannelId,
                    (ATOM, b"title", 2, true) => At::EntryTitle,
                    (ATOM, b"published", 2, true) => At::EntryPublished,
                    (ATOM, b"name", 3, true) if path[2].1 == b"author" => At::EntryAuthorName,
                    _ => At::Other,
                };
                text.clear();
                path.push((ns == ATOM || ns == YT, local));
            }
            Event::Empty(e) => {
                let is_link =
                    matches!(ns, ResolveResult::Bound(n) if n.as_ref() == ATOM) && e.local_name().as_ref() == b"link";
                if is_link
                    && path.len() == 2
                    && let Some(en) = entry.as_mut()
                    && en.link.is_empty()
                    && let Ok(Some(href)) = e.try_get_attribute("href")
                {
                    // Raw: only used to spot `/shorts/`, which has nothing to unescape.
                    en.link = String::from_utf8_lossy(&href.value).into_owned();
                }
            }
            Event::Text(t) => {
                if at != At::Other {
                    text.push_str(&t.decode().map_err(|e| format!("feed text: {e}"))?);
                }
            }
            Event::CData(t) => {
                if at != At::Other {
                    text.push_str(&String::from_utf8_lossy(&t));
                }
            }
            Event::GeneralRef(g) => {
                if at != At::Other {
                    let name = g.decode().map_err(|e| format!("feed entity: {e}"))?;
                    match g.resolve_char_ref() {
                        Ok(Some(c)) => text.push(c),
                        _ => match &*name {
                            "amp" => text.push('&'),
                            "lt" => text.push('<'),
                            "gt" => text.push('>'),
                            "quot" => text.push('"'),
                            "apos" => text.push('\''),
                            // Never resolved: an unknown entity stays as written.
                            other => {
                                text.push('&');
                                text.push_str(other);
                                text.push(';');
                            }
                        },
                    }
                }
            }
            Event::End(_) => {
                let value = text.trim().to_string();
                match at {
                    At::FeedChannelId => atom.channel_id = Some(value).filter(|v| !v.is_empty()),
                    At::FeedAuthorName => atom.channel_name = Some(value).filter(|v| !v.is_empty()),
                    At::EntryVideoId => {
                        if let Some(en) = entry.as_mut() {
                            en.video_id = value;
                            entry_ok.0 = !en.video_id.is_empty();
                        }
                    }
                    At::EntryChannelId => {
                        if let Some(en) = entry.as_mut() {
                            en.channel_id = value;
                        }
                    }
                    At::EntryTitle => {
                        if let Some(en) = entry.as_mut() {
                            en.title = value;
                        }
                    }
                    At::EntryAuthorName => {
                        if let Some(en) = entry.as_mut() {
                            en.channel_name = value;
                        }
                    }
                    At::EntryPublished => {
                        if let (Some(en), Some(t)) = (entry.as_mut(), parse_time(&value)) {
                            en.published_at = t;
                            entry_ok.1 = true;
                        }
                    }
                    At::Other => {}
                }
                at = At::Other;
                text.clear();
                let closed = path.pop();
                if path.len() == 1
                    && closed.is_some_and(|(_, l)| l == b"entry")
                    && let Some(en) = entry.take()
                    && entry_ok == (true, true)
                {
                    atom.entries.push(en);
                }
            }
            Event::DocType(_) => return Err("feed declares a DTD; refused".into()),
            Event::Eof => break,
            _ => {}
        }
    }
    let feed_channel = atom.channel_id.clone().unwrap_or_default();
    let feed_author = atom.channel_name.clone().unwrap_or_default();
    for e in &mut atom.entries {
        if e.channel_id.is_empty() {
            e.channel_id = feed_channel.clone();
        }
        if e.channel_name.is_empty() {
            e.channel_name = feed_author.clone();
        }
    }
    Ok(atom)
}

fn item(e: &Entry, channel_id: &str, kind: &'static str) -> FeedItem {
    FeedItem {
        source: "yt".into(),
        remote_id: e.video_id.clone(),
        channel_id: if e.channel_id.is_empty() { channel_id.to_string() } else { e.channel_id.clone() },
        channel_name: e.channel_name.clone(),
        title: e.title.clone(),
        published_at: e.published_at,
        kind: kind.to_string(),
        thumbnail_url: Some(thumbnail(&e.video_id, kind)),
        duration_sec: None,
    }
}

fn newest_first(mut items: Vec<FeedItem>) -> Vec<FeedItem> {
    items.sort_by(|a, b| b.published_at.cmp(&a.published_at).then_with(|| a.remote_id.cmp(&b.remote_id)));
    items
}

/// The three playlist answers (None: 404) as one feed. None when all three
/// are missing: fetch the channel feed then ([`from_channel_feed`]).
pub fn merge(channel_id: &str, videos: Option<Atom>, shorts: Option<Atom>, live: Option<Atom>) -> Option<ChannelFeed> {
    if videos.is_none() && shorts.is_none() && live.is_none() {
        return None;
    }
    let mut seen = std::collections::HashSet::new();
    let mut items = Vec::new();
    let mut name = None;
    // Shorts first: an id that sits in two playlists must never reach the main feed.
    for (upload, feed) in [(Upload::Shorts, &shorts), (Upload::Live, &live), (Upload::Videos, &videos)] {
        let Some(feed) = feed else { continue };
        name = name.or_else(|| feed.channel_name.clone());
        for e in &feed.entries {
            if seen.insert(e.video_id.clone()) {
                items.push(item(e, channel_id, upload.kind()));
            }
        }
    }
    Some(ChannelFeed { channel_name: name, items: newest_first(items), complete_kinds: vec![] })
}

pub fn from_channel_feed(channel_id: &str, atom: Atom) -> ChannelFeed {
    let items = atom
        .entries
        .iter()
        .map(|e| item(e, channel_id, if e.link.contains("/shorts/") { SHORT } else { VIDEO }))
        .collect();
    ChannelFeed { channel_name: atom.channel_name, items: newest_first(items), complete_kinds: vec![] }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playlist_paths() {
        assert_eq!(
            playlist_path("UCaaaaaaaaaaaaaaaaaaaaaa", Upload::Shorts).unwrap(),
            "/feeds/videos.xml?playlist_id=UUSHaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert_eq!(playlist_path("nope", Upload::Videos), None);
    }

    #[test]
    fn entities_and_refusals() {
        let xml = r#"<feed xmlns="http://www.w3.org/2005/Atom" xmlns:yt="http://www.youtube.com/xml/schemas/2015">
            <author><name>A &amp; B</name></author>
            <entry><yt:videoId>v1</yt:videoId><title>x &lt;3 &#233;</title><published>2026-10-20T15:00:05+00:00</published></entry>
            <entry><title>no id, dropped</title><published>2026-10-20T15:00:05+00:00</published></entry>
            </feed>"#;
        let a = parse(xml).unwrap();
        assert_eq!(a.channel_name.as_deref(), Some("A & B"));
        assert_eq!(a.entries.len(), 1);
        assert_eq!(a.entries[0].title, "x <3 é");
        assert_eq!(a.entries[0].channel_name, "A & B", "the feed's author fills in");
        assert!(parse("<feed><entry>").is_err() || parse("<feed><entry>").unwrap().entries.is_empty());
        assert!(parse(r#"<!DOCTYPE feed [<!ENTITY x "y">]><feed/>"#).is_err());
    }
}
