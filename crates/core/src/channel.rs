//! A YouTube channel as someone types it on the website: a channel id, an
//! @handle, or a channel URL. Turning a handle or URL into an id needs the
//! channel page; these functions only read what was fetched.

/// What the input names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelRef {
    /// `UC` + 22 characters: usable as is.
    Id(String),
    /// A page on www.youtube.com to fetch for the id: its path (`/@handle`,
    /// `/c/Name`, `/user/Name`). Only ever a path, so the host is ours to set.
    Page(String),
}

/// `UC` followed by 22 URL-safe base64 characters.
pub fn is_channel_id(s: &str) -> bool {
    s.len() == 24 && s.starts_with("UC") && s[2..].bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// None when the input is not a YouTube channel reference.
pub fn parse(input: &str) -> Option<ChannelRef> {
    let s = input.trim();
    if is_channel_id(s) {
        return Some(ChannelRef::Id(s.to_string()));
    }
    if let Some(handle) = s.strip_prefix('@') {
        return handle_ok(handle).then(|| ChannelRef::Page(format!("/@{handle}")));
    }
    let rest = s.strip_prefix("https://").or_else(|| s.strip_prefix("http://")).unwrap_or(s);
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host.to_ascii_lowercase();
    if !matches!(host.as_str(), "youtube.com" | "www.youtube.com" | "m.youtube.com" | "music.youtube.com") {
        return None;
    }
    let path = path.split(['?', '#']).next().unwrap_or("");
    let mut parts = path.split('/').filter(|p| !p.is_empty());
    match (parts.next(), parts.next()) {
        (Some("channel"), Some(id)) if is_channel_id(id) => Some(ChannelRef::Id(id.to_string())),
        (Some(h), _) if h.starts_with('@') && handle_ok(&h[1..]) => Some(ChannelRef::Page(format!("/{h}"))),
        (Some(kind @ ("c" | "user")), Some(name)) if name_ok(name) => Some(ChannelRef::Page(format!("/{kind}/{name}"))),
        _ => None,
    }
}

fn handle_ok(h: &str) -> bool {
    (3..=100).contains(&h.len()) && h.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

fn name_ok(n: &str) -> bool {
    !n.is_empty() && n.len() <= 100 && n.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.%".contains(&b))
}

/// The channel id on a channel page: its canonical link, else the
/// `externalId` in the page data.
pub fn id_from_page(html: &str) -> Option<String> {
    let canonical =
        html.split("<link rel=\"canonical\" href=\"").nth(1).and_then(|r| r.split('"').next()).and_then(|href| {
            href.strip_prefix("https://www.youtube.com/channel/").map(|id| id.split(['/', '?']).next().unwrap_or(""))
        });
    let external = || html.split("\"externalId\":\"").nth(1).and_then(|r| r.split('"').next());
    canonical.filter(|id| is_channel_id(id)).or_else(|| external().filter(|id| is_channel_id(id))).map(String::from)
}

/// The avatar on a channel page (`og:image`), when there is one.
pub fn avatar_from_page(html: &str) -> Option<String> {
    let url = html.split("<meta property=\"og:image\" content=\"").nth(1)?.split('"').next()?;
    let url = unescape(url);
    url.starts_with("https://").then_some(url)
}

/// The channel's name from its uploads feed: the feed's own `<title>`, which
/// comes before the first `<entry>`.
pub fn name_from_feed(xml: &str) -> Option<String> {
    let head = xml.split("<entry>").next()?;
    let title = head.split("<title>").nth(1)?.split("</title>").next()?;
    let title = unescape(title.trim());
    (!title.is_empty()).then_some(title)
}

/// The five XML entities and numeric references.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let Some(end) = tail.find(';').filter(|&e| e <= 10) else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let entity = &tail[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            e if e.starts_with("#x") => u32::from_str_radix(&e[2..], 16).ok().and_then(char::from_u32),
            e if e.starts_with('#') => e[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &tail[end + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The path of a channel's uploads feed on www.youtube.com (cheap, and proof
/// the channel exists).
pub fn feed_path(id: &str) -> String {
    format!("/feeds/videos.xml?channel_id={id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "UCXuqSBlHAE6Xw-yeJA0Tunw";

    #[test]
    fn references_people_type() {
        assert_eq!(parse(ID), Some(ChannelRef::Id(ID.into())));
        assert_eq!(parse(&format!(" https://www.youtube.com/channel/{ID}/videos ")), Some(ChannelRef::Id(ID.into())));
        assert_eq!(parse(&format!("youtube.com/channel/{ID}?si=x")), Some(ChannelRef::Id(ID.into())));
        let page = |p: &str| Some(ChannelRef::Page(format!("/{p}")));
        assert_eq!(parse("@LinusTechTips"), page("@LinusTechTips"));
        assert_eq!(parse("https://m.youtube.com/@LinusTechTips/featured"), page("@LinusTechTips"));
        assert_eq!(parse("https://www.youtube.com/c/LinusTechTips"), page("c/LinusTechTips"));
        assert_eq!(parse("http://youtube.com/user/LinusTechTips"), page("user/LinusTechTips"));
        for bad in ["", "hello", "@a", "https://evil.example/@x", "https://www.youtube.com/watch?v=abc", "UCshort"] {
            assert_eq!(parse(bad), None, "{bad}");
        }
        // Never a different host smuggled into the fetched URL.
        assert_eq!(parse("@x/../../evil"), None);
    }

    #[test]
    fn id_name_and_avatar_from_fetched_pages() {
        let html = format!(
            "<html><link rel=\"canonical\" href=\"https://www.youtube.com/channel/{ID}\">\
             <meta property=\"og:image\" content=\"https://yt3.googleusercontent.com/a=s900?x&amp;y\"></html>"
        );
        assert_eq!(id_from_page(&html).as_deref(), Some(ID));
        assert_eq!(avatar_from_page(&html).as_deref(), Some("https://yt3.googleusercontent.com/a=s900?x&y"));
        let data = format!("<script>var d = {{\"metadata\":{{\"externalId\":\"{ID}\"}}}}</script>");
        assert_eq!(id_from_page(&data).as_deref(), Some(ID));
        assert_eq!(id_from_page("<html>consent</html>"), None);

        let feed = "<feed><title>Tom &amp; Jerry&#39;s</title><entry><title>A video</title></entry></feed>";
        assert_eq!(name_from_feed(feed).as_deref(), Some("Tom & Jerry's"));
        assert_eq!(name_from_feed("<feed><entry><title>x</title></entry></feed>"), None);
        assert_eq!(unescape("a & b &bogus; &#x41;"), "a & b &bogus; A");
    }
}
